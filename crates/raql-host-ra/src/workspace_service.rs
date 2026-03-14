use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use base_db::SourceDatabase;
use camino::Utf8PathBuf;
use hir::{Adt, AssocItem, HasSource, HasVisibility, Impl, Module, ModuleDef};
use ide::AnalysisHost;
use project_model::ProjectManifest;
use raql_compiler::{PlannedProgram, required_extern_capabilities};
use raql_engine::{EvalResult, execute};
use raql_host::{
    CapabilityId, CapabilitySet, MissingCapabilitiesError, is_engine_managed_extern,
    is_runtime_scalar_input_predicate,
};
use syntax::{AstNode, Edition};
use vfs::{AbsPathBuf, VfsPath};

use crate::capability::{day_one_supported_capabilities, supports_day_one_capability};
use crate::lazy_runtime::LazyRaRuntime;
use crate::workspace_loader;
use crate::{DefId, DefKind, DeterministicRaHost, RaHostInitError, WorldStamp};

#[derive(Debug)]
pub struct WorkspaceService {
    manifest_path: PathBuf,
    workspace_root: PathBuf,
    analysis_host: AnalysisHost,
    vfs: vfs::Vfs,
    _proc_macro_client: Option<Box<dyn workspace_loader::ProcMacroClientHandle>>,
    core_host: Option<DeterministicRaHost>,
    tracked_files: BTreeSet<PathBuf>,
    scan_roots: BTreeSet<PathBuf>,
    workspace_epoch: u64,
    content_revision: u64,
}

impl WorkspaceService {
    pub fn from_workspace_root(root: &Path) -> Result<Self, RaHostInitError> {
        let loaded = workspace_loader::load_from_workspace_root(root)?;
        Ok(Self::from_loaded(loaded))
    }

    pub fn from_manifest_path(manifest: &Path) -> Result<Self, RaHostInitError> {
        let loaded = workspace_loader::load_from_manifest_path(manifest)?;
        Ok(Self::from_loaded(loaded))
    }

    fn from_loaded(loaded: workspace_loader::LoadedWorkspace) -> Self {
        let (tracked_files, scan_roots) = tracked_workspace_state(
            &loaded.vfs,
            loaded.manifest_path.as_path(),
            loaded.workspace_root.as_path(),
        );
        Self {
            manifest_path: loaded.manifest_path,
            workspace_root: loaded.workspace_root,
            analysis_host: AnalysisHost::with_database(loaded.db),
            vfs: loaded.vfs,
            _proc_macro_client: loaded.proc_macro_client,
            core_host: None,
            tracked_files,
            scan_roots,
            workspace_epoch: 0,
            content_revision: 0,
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn workspace_epoch(&self) -> u64 {
        self.workspace_epoch
    }

    pub fn content_revision(&self) -> u64 {
        self.content_revision
    }

    pub fn supported_capabilities(&self) -> CapabilitySet {
        day_one_supported_capabilities()
    }

    pub(crate) fn supports_extern_predicate(&self, predicate: &str) -> bool {
        is_engine_managed_extern(predicate)
            || is_runtime_scalar_input_predicate(predicate)
            || supports_day_one_capability(predicate)
    }

    pub fn run_planned(&mut self, planned: &PlannedProgram) -> Result<EvalResult, RaHostInitError> {
        self.ensure_supported_planned(planned)?;
        self.sync_workspace()?;
        let shared = Rc::new(RefCell::new(self));
        let mut runtime = LazyRaRuntime::new(shared);
        Ok(execute(planned, &mut runtime))
    }

    fn ensure_supported_planned(&self, planned: &PlannedProgram) -> Result<(), RaHostInitError> {
        let required = required_extern_capabilities(planned)
            .into_iter()
            .map(CapabilityId::from)
            .collect::<Vec<_>>();
        MissingCapabilitiesError::from_required_and_supported(required, self.supported_capabilities())
            .map_err(|err| RaHostInitError::SemanticBuild {
                details: err.to_string(),
            })
    }

    pub(crate) fn ensure_core_host(&mut self) -> Result<&mut DeterministicRaHost, RaHostInitError> {
        if self.core_host.is_none() {
            self.core_host = Some(build_core_host(
                &self.analysis_host,
                &self.vfs,
                &self.workspace_root,
                &self.tracked_files,
                self.workspace_epoch,
                self.content_revision,
            )?);
        }
        Ok(self.core_host.as_mut().expect("core host initialized"))
    }

    fn sync_workspace(&mut self) -> Result<(), RaHostInitError> {
        let scanned = scan_relevant_files(&self.scan_roots, &self.tracked_files)?;
        if scanned != self.tracked_files {
            return self.reload_full();
        }

        let mut change = hir::ChangeWithProcMacros::default();
        let mut saw_change = false;
        for path in &self.tracked_files {
            let bytes = fs::read(path).map_err(|err| RaHostInitError::WorkspaceLoad {
                manifest: self.manifest_path.display().to_string(),
                details: err.to_string(),
            })?;
            let utf8 = Utf8PathBuf::from_path_buf(path.clone()).map_err(|path| RaHostInitError::WorkspaceLoad {
                manifest: self.manifest_path.display().to_string(),
                details: format!("non-utf8 path: {}", path.display()),
            })?;
            let abs = AbsPathBuf::assert(utf8);
            let vfs_path = VfsPath::from(abs);
            let changed = self.vfs.set_file_contents(vfs_path.clone(), Some(bytes.clone()));
            if !changed {
                continue;
            }
            if path_requires_reload(path.as_path()) {
                let _ = self.vfs.take_changes();
                return self.reload_full();
            }
            let Some((file_id, excluded)) = self.vfs.file_id(&vfs_path) else {
                let _ = self.vfs.take_changes();
                return self.reload_full();
            };
            if matches!(excluded, vfs::FileExcluded::Yes) {
                let _ = self.vfs.take_changes();
                return self.reload_full();
            }
            let text = String::from_utf8(bytes).map_err(|err| RaHostInitError::WorkspaceLoad {
                manifest: self.manifest_path.display().to_string(),
                details: err.to_string(),
            })?;
            change.change_file(file_id, Some(text));
            saw_change = true;
        }
        let _ = self.vfs.take_changes();
        if saw_change {
            self.analysis_host.apply_change(change);
            self.content_revision = self.content_revision.saturating_add(1);
            self.core_host = None;
        }
        Ok(())
    }

    fn reload_full(&mut self) -> Result<(), RaHostInitError> {
        let loaded = workspace_loader::load_from_manifest_path(&self.manifest_path)?;
        let workspace_loader::LoadedWorkspace {
            manifest_path,
            workspace_root,
            db,
            vfs,
            proc_macro_client,
            ..
        } = loaded;
        let (tracked_files, scan_roots) =
            tracked_workspace_state(&vfs, manifest_path.as_path(), workspace_root.as_path());
        self.manifest_path = manifest_path;
        self.workspace_root = workspace_root;
        self.analysis_host = AnalysisHost::with_database(db);
        self.vfs = vfs;
        self._proc_macro_client = proc_macro_client;
        self.core_host = None;
        self.tracked_files = tracked_files;
        self.scan_roots = scan_roots;
        self.workspace_epoch = self.workspace_epoch.saturating_add(1);
        self.content_revision = self.content_revision.saturating_add(1);
        Ok(())
    }
}

pub fn resolve_workspace_root(input: &Path) -> Result<PathBuf, RaHostInitError> {
    let input_path = input.to_string_lossy().to_string();
    let abs_input = std::fs::canonicalize(input).map_err(|err| RaHostInitError::WorkspaceNotFound {
        input_path: input_path.clone(),
        details: err.to_string(),
    })?;
    let utf8 = Utf8PathBuf::from_path_buf(abs_input.clone()).map_err(|path| RaHostInitError::WorkspaceNotFound {
        input_path: input_path.clone(),
        details: format!("non-utf8 path: {}", path.display()),
    })?;
    let abs = AbsPathBuf::assert(utf8);
    let manifest = if abs_input.file_name().is_some_and(|value| value == "Cargo.toml") {
        ProjectManifest::from_manifest_file(abs).map_err(|err| RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
            details: err.to_string(),
        })?
    } else {
        ProjectManifest::discover_single(abs.as_ref()).map_err(|err| RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
            details: err.to_string(),
        })?
    };
    Ok(PathBuf::from(manifest.manifest_path().parent().to_string()))
}

#[derive(Debug)]
struct LocalFile {
    rel_path: String,
    text: String,
}

struct CoreFactsBuilder<'db> {
    db: &'db ide::RootDatabase,
    files: BTreeMap<vfs::FileId, LocalFile>,
    host: DeterministicRaHost,
    def_path_by_id: BTreeMap<DefId, String>,
    local_adts: Vec<Adt>,
    local_traits: Vec<hir::Trait>,
}

impl<'db> CoreFactsBuilder<'db> {
    fn populate(&mut self) {
        for krate in hir::Crate::all(self.db) {
            self.visit_module(krate.root_module(self.db), false);
        }
        self.extract_impls();
    }

    fn visit_module(&mut self, module: Module, inherited_test: bool) {
        let module_test = inherited_test || self.module_is_test(module);
        let _ = self.register_module_def(ModuleDef::Module(module), module_test);
        for def in module.declarations(self.db) {
            let Some(def_id) = self.register_module_def(def, module_test) else {
                continue;
            };
            match def {
                ModuleDef::Module(child) => self.visit_module(child, module_test),
                ModuleDef::Adt(adt) => self.process_adt(adt),
                ModuleDef::Trait(trait_def) => {
                    for assoc in trait_def.items(self.db) {
                        if let AssocItem::Function(function) = assoc {
                            let Some(method_def) =
                                self.register_module_def(ModuleDef::Function(function), module_test)
                            else {
                                continue;
                            };
                            self.host.insert_trait_method(def_id, method_def);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn process_adt(&mut self, adt: Adt) {
        self.local_adts.push(adt);
        if let Adt::Enum(enum_) = adt {
            for variant in enum_.variants(self.db) {
                let _ = self.register_module_def(ModuleDef::Variant(variant), false);
            }
        }
    }

    fn extract_impls(&mut self) {
        let mut impls = Vec::new();
        for adt in self.local_adts.iter().copied() {
            let ty = match adt {
                Adt::Struct(strukt) => strukt.ty(self.db),
                Adt::Union(union) => union.ty(self.db),
                Adt::Enum(enum_) => enum_.ty(self.db),
            };
            impls.extend(Impl::all_for_type(self.db, ty));
        }
        for trait_ in self.local_traits.iter().copied() {
            impls.extend(Impl::all_for_trait(self.db, trait_));
        }
        for impl_def in impls {
            let self_ty_def = impl_def
                .self_ty(self.db)
                .as_adt()
                .and_then(|adt| self.register_module_def(ModuleDef::Adt(adt), false));
            for assoc in impl_def.items(self.db) {
                if let AssocItem::Function(function) = assoc {
                    let Some(method_def) = self.register_module_def(ModuleDef::Function(function), false) else {
                        continue;
                    };
                    if let Some(owner) = self_ty_def {
                        self.host.insert_method(owner, method_def);
                    }
                }
            }
        }
    }

    fn register_module_def(&mut self, def: ModuleDef, in_test: bool) -> Option<DefId> {
        let kind = match def {
            ModuleDef::Module(_) => DefKind::Mod,
            ModuleDef::Function(_) => DefKind::Fn,
            ModuleDef::Adt(Adt::Struct(_)) => DefKind::Struct,
            ModuleDef::Adt(Adt::Enum(_)) => DefKind::Enum,
            ModuleDef::Adt(Adt::Union(_)) => DefKind::Union,
            ModuleDef::Variant(_) => DefKind::Variant,
            ModuleDef::Const(_) => DefKind::Const,
            ModuleDef::Static(_) => DefKind::Static,
            ModuleDef::Trait(_) => DefKind::Trait,
            ModuleDef::TypeAlias(_) => DefKind::TypeAlias,
            ModuleDef::Macro(_) => DefKind::Macro,
            ModuleDef::BuiltinType(_) => return None,
        };
        let name = def.name(self.db)?.display(self.db, Edition::CURRENT).to_string();
        let path = def
            .canonical_path(self.db, Edition::CURRENT)
            .unwrap_or_else(|| format!("crate::{name}"));

        let (editioned_file, rel_path, range) = match def {
            ModuleDef::Module(module) => {
                let range = module
                    .declaration_source_range(self.db)
                    .unwrap_or_else(|| module.definition_source_range(self.db));
                let editioned = range.file_id.original_file(self.db);
                let local = self.files.get(&editioned.file_id(self.db))?;
                (editioned, local.rel_path.clone(), range.value)
            }
            ModuleDef::Function(function) => {
                let source = function.source(self.db)?;
                let editioned = source.file_id.original_file(self.db);
                let local = self.files.get(&editioned.file_id(self.db))?;
                (editioned, local.rel_path.clone(), source.value.syntax().text_range())
            }
            ModuleDef::Adt(adt) => {
                let source = adt.source(self.db)?;
                let editioned = source.file_id.original_file(self.db);
                let local = self.files.get(&editioned.file_id(self.db))?;
                (editioned, local.rel_path.clone(), source.value.syntax().text_range())
            }
            ModuleDef::Variant(variant) => {
                let source = variant.source(self.db)?;
                let editioned = source.file_id.original_file(self.db);
                let local = self.files.get(&editioned.file_id(self.db))?;
                (editioned, local.rel_path.clone(), source.value.syntax().text_range())
            }
            ModuleDef::Const(const_) => {
                let source = const_.source(self.db)?;
                let editioned = source.file_id.original_file(self.db);
                let local = self.files.get(&editioned.file_id(self.db))?;
                (editioned, local.rel_path.clone(), source.value.syntax().text_range())
            }
            ModuleDef::Static(static_) => {
                let source = static_.source(self.db)?;
                let editioned = source.file_id.original_file(self.db);
                let local = self.files.get(&editioned.file_id(self.db))?;
                (editioned, local.rel_path.clone(), source.value.syntax().text_range())
            }
            ModuleDef::Trait(trait_) => {
                let source = trait_.source(self.db)?;
                let editioned = source.file_id.original_file(self.db);
                let local = self.files.get(&editioned.file_id(self.db))?;
                (editioned, local.rel_path.clone(), source.value.syntax().text_range())
            }
            ModuleDef::TypeAlias(alias) => {
                let source = alias.source(self.db)?;
                let editioned = source.file_id.original_file(self.db);
                let local = self.files.get(&editioned.file_id(self.db))?;
                (editioned, local.rel_path.clone(), source.value.syntax().text_range())
            }
            ModuleDef::Macro(mac) => {
                let source = mac.source(self.db)?;
                let editioned = source.file_id.original_file(self.db);
                let local = self.files.get(&editioned.file_id(self.db))?;
                (editioned, local.rel_path.clone(), source.value.syntax().text_range())
            }
            ModuleDef::BuiltinType(_) => return None,
        };
        let local = self.files.get(&editioned_file.file_id(self.db))?;
        let span = self
            .host
            .intern_span_from_text(
                editioned_file.editioned_file_id(self.db),
                rel_path.clone(),
                local.text.as_str(),
                range,
            )
            .ok()?;
        let def_id = self.host.intern_def_from_token(
            format!(
                "def:{kind:?}:{path}:{rel_path}:{}..{}",
                u32::from(range.start()),
                u32::from(range.end())
            )
            .as_str(),
        );
        self.host.insert_def(def_id, name.as_str(), kind, span, path.as_str());
        self.host.insert_handle(def_id, format!("def://{path}"));
        self.def_path_by_id.insert(def_id, path.clone());

        let is_public = match def {
            ModuleDef::Module(module) => module.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Function(function) => function.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Adt(adt) => adt.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Variant(variant) => variant.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Const(const_) => const_.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Static(static_) => static_.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Trait(trait_) => {
                self.local_traits.push(trait_);
                trait_.visibility(self.db) == hir::Visibility::Public
            }
            ModuleDef::TypeAlias(alias) => alias.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Macro(mac) => mac.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::BuiltinType(_) => false,
        };
        self.host.mark_public(def_id, is_public);
        let in_test_scope = match def {
            ModuleDef::Function(function) => function.is_test(self.db) || self.module_is_test(function.module(self.db)),
            ModuleDef::Module(module) => self.module_is_test(module),
            ModuleDef::Adt(adt) => self.module_is_test(adt.module(self.db)),
            ModuleDef::Variant(variant) => self.module_is_test(variant.module(self.db)),
            ModuleDef::Const(const_) => self.module_is_test(const_.module(self.db)),
            ModuleDef::Static(static_) => self.module_is_test(static_.module(self.db)),
            ModuleDef::Trait(trait_) => self.module_is_test(trait_.module(self.db)),
            ModuleDef::TypeAlias(alias) => self.module_is_test(alias.module(self.db)),
            ModuleDef::Macro(mac) => self.module_is_test(mac.module(self.db)),
            ModuleDef::BuiltinType(_) => false,
        };
        self.host.mark_in_test(def_id, in_test || in_test_scope);
        Some(def_id)
    }

    fn module_is_test(&self, module: Module) -> bool {
        module.path_to_root(self.db).into_iter().any(|m| {
            m.name(self.db)
                .is_some_and(|name| name.display(self.db, Edition::CURRENT).to_string() == "tests")
        })
    }
}

fn build_core_host(
    analysis_host: &AnalysisHost,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    tracked_files: &BTreeSet<PathBuf>,
    workspace_epoch: u64,
    content_revision: u64,
) -> Result<DeterministicRaHost, RaHostInitError> {
    let db = analysis_host.raw_database();
    let mut host = DeterministicRaHost::new();
    host.set_world_stamp(WorldStamp::new(format!(
        "ra-workspace:{}:{}:{}",
        workspace_root.display(),
        workspace_epoch,
        content_revision
    )));
    let mut files = BTreeMap::new();
    for (file_id, path) in vfs.iter() {
        let Some(abs) = path.as_path() else {
            continue;
        };
        let abs_path: &Path = abs.as_ref();
        let owned_path = abs_path.to_path_buf();
        if !tracked_files.contains(&owned_path) {
            continue;
        }
        let rel_path = if abs_path.starts_with(workspace_root) {
            abs_path
                .strip_prefix(workspace_root)
                .unwrap_or(abs_path)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            abs_path.to_string_lossy().replace('\\', "/")
        };
        let text = db.file_text(file_id).text(db).to_string();
        files.insert(file_id, LocalFile { rel_path, text });
    }
    let mut builder = CoreFactsBuilder {
        db,
        files,
        host,
        def_path_by_id: BTreeMap::new(),
        local_adts: Vec::new(),
        local_traits: Vec::new(),
    };
    builder.populate();
    Ok(builder.host)
}

fn tracked_workspace_state(
    vfs: &vfs::Vfs,
    manifest_path: &Path,
    workspace_root: &Path,
) -> (BTreeSet<PathBuf>, BTreeSet<PathBuf>) {
    let tracked_files = vfs
        .iter()
        .filter_map(|(_, path)| path.as_path().map(|abs| {
            let path: &Path = abs.as_ref();
            path.to_path_buf()
        }))
        .filter(|path| is_local_workspace_file(path.as_path()))
        .collect::<BTreeSet<_>>();
    let mut scan_roots = BTreeSet::new();
    scan_roots.insert(workspace_root.to_path_buf());
    if let Some(parent) = manifest_path.parent() {
        scan_roots.insert(parent.to_path_buf());
    }
    for path in &tracked_files {
        if scan_root_seed(path.as_path()) && let Some(parent) = path.parent() {
            scan_roots.insert(parent.to_path_buf());
        }
    }
    (tracked_files, scan_roots)
}

fn scan_relevant_files(
    roots: &BTreeSet<PathBuf>,
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, RaHostInitError> {
    let mut files = tracked_files
        .iter()
        .filter(|path| path.exists())
        .cloned()
        .collect::<BTreeSet<_>>();
    for root in roots {
        if root.exists() {
            scan_dir(root.as_path(), &mut files)?;
        }
    }
    Ok(files)
}

fn scan_dir(dir: &Path, out: &mut BTreeSet<PathBuf>) -> Result<(), RaHostInitError> {
    let entries = fs::read_dir(dir).map_err(|err| RaHostInitError::WorkspaceLoad {
        manifest: dir.display().to_string(),
        details: err.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| RaHostInitError::WorkspaceLoad {
            manifest: dir.display().to_string(),
            details: err.to_string(),
        })?;
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if matches!(name, "target" | ".git" | ".jj") {
                continue;
            }
            scan_dir(path.as_path(), out)?;
            continue;
        }
        if is_relevant_workspace_file(path.as_path()) {
            out.insert(path);
        }
    }
    Ok(())
}

fn is_relevant_workspace_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if matches!(
        name,
        "Cargo.toml" | "Cargo.lock" | "build.rs" | "rust-toolchain" | "rust-toolchain.toml"
    ) {
        return true;
    }
    if name == "config.toml"
        && path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|value| value.to_str())
            == Some(".cargo")
    {
        return true;
    }
    path.extension().is_some_and(|ext| ext == "rs")
}

fn scan_root_seed(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        name,
        "Cargo.toml" | "Cargo.lock" | "build.rs" | "rust-toolchain" | "rust-toolchain.toml"
    ) || (name == "config.toml"
        && path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|value| value.to_str())
            == Some(".cargo"))
}

fn path_requires_reload(path: &Path) -> bool {
    !path.extension().is_some_and(|ext| ext == "rs")
}

fn is_local_workspace_file(path: &Path) -> bool {
    is_relevant_workspace_file(path) && !is_external_dependency_path(path)
}

fn is_external_dependency_path(path: &Path) -> bool {
    path.starts_with(cargo_home_dir().join("registry"))
        || path.starts_with(cargo_home_dir().join("git").join("checkouts"))
        || path.starts_with(rustup_home_dir().join("toolchains"))
        || has_component_sequence(path, &[".cargo", "registry"])
        || has_component_sequence(path, &[".cargo", "git", "checkouts"])
        || has_component_sequence(path, &[".rustup", "toolchains"])
        || has_registry_source_layout(path)
}

fn cargo_home_dir() -> PathBuf {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .unwrap_or_else(|| PathBuf::from(".cargo"))
}

fn rustup_home_dir() -> PathBuf {
    std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".rustup")))
        .unwrap_or_else(|| PathBuf::from(".rustup"))
}

fn has_component_sequence(path: &Path, sequence: &[&str]) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    components
        .windows(sequence.len())
        .any(|window| window == sequence)
}

fn has_registry_source_layout(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    components
        .windows(4)
        .any(|window| window[0] == "registry" && window[1] == "src")
}
