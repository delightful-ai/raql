use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Instant, UNIX_EPOCH};

use base_db::SourceDatabase;
use camino::Utf8PathBuf;
use hir::{Adt, AssocItem, HasSource, HasVisibility, Impl, Module, ModuleDef};
use ide::AnalysisHost;
use ide_db::symbol_index::{Query, world_symbols};
use project_model::ProjectManifest;
use raql_compiler::{PlannedProgram, required_extern_capabilities};
use raql_engine::{EvalResult, execute};
use raql_host::{
    CapabilityId, CapabilitySet, ExternLookupRequest, ExternLookupShape, ExternLookupValue,
    MissingCapabilitiesError, SpanKey,
    is_engine_managed_extern, is_runtime_scalar_input_predicate,
};
use hir::import_map::AssocSearchMode;
use syntax::ast::{HasGenericArgs, HasName};
use syntax::{ast, AstNode, Edition};
use vfs::{AbsPathBuf, VfsPath};

use crate::capability::{day_one_supported_capabilities, supports_day_one_capability};
use crate::lazy_runtime::LazyRaRuntime;
use crate::provider::calls::{lookup_call_edge_rows as provider_lookup_call_edge_rows, method_dispatch_kind};
use crate::provider::core_index::CoreLookupIndex;
use crate::provider::defs::{
    LocalFile, LookupDefRecord, canonical_function_path, lookup_def_kind_rows,
    lookup_def_name_rows, lookup_def_path_rows, lookup_def_rows, lookup_def_span_rows,
    module_def_in_test, module_def_is_public, module_def_kind,
};
use crate::provider::syntax::{
    extract_syntax_nodes, lookup_span_allowed_rows, lookup_span_key_rows,
};
use crate::workspace_loader;
use crate::{
    DefId, DefKind, DeterministicRaHost, GenericArg, Mutability, RaHostInitError, SpanId,
    TypeShape, WorldStamp,
};

#[derive(Debug)]
pub struct WorkspaceService {
    manifest_path: PathBuf,
    workspace_root: PathBuf,
    analysis_host: AnalysisHost,
    vfs: vfs::Vfs,
    _proc_macro_client: Option<Box<dyn workspace_loader::ProcMacroClientHandle>>,
    core_host: Option<DeterministicRaHost>,
    core_index: Option<CoreLookupIndex>,
    core_host_spec: Option<CoreHostBuildSpec>,
    tracked_files: BTreeSet<PathBuf>,
    tracked_file_states: BTreeMap<PathBuf, WatchedFileState>,
    tracked_dirs: BTreeSet<PathBuf>,
    tracked_dir_states: BTreeMap<PathBuf, WatchedFileState>,
    watched_entries: Vec<vfs::loader::Entry>,
    build_script_rerun_paths: BTreeSet<PathBuf>,
    auxiliary_build_inputs: BTreeMap<PathBuf, Option<WatchedFileState>>,
    lookup_defs: BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: BTreeMap<SpanId, SpanKey>,
    workspace_epoch: u64,
    content_revision: u64,
}

pub struct WarmupSnapshot(ide::Analysis);

impl WarmupSnapshot {
    pub fn parallel_prime_caches(&self, worker_threads: usize) -> Result<(), String> {
        self.0
            .parallel_prime_caches(worker_threads, |_| {})
            .map_err(|err| format!("rust-analyzer cache priming cancelled: {err:?}"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WatchedFileState {
    len: u64,
    modified_unix_nanos: Option<u128>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CoreHostBuildSpec {
    impls: bool,
    call_graph: bool,
    syntax_nodes: bool,
    type_facts: bool,
    adt_structure: bool,
    def_paths: bool,
    def_spans: bool,
    def_handles: bool,
    def_publicity: bool,
    def_test_flags: bool,
}

impl CoreHostBuildSpec {
    fn from_planned(planned: &PlannedProgram) -> Self {
        let mut spec = Self::default();
        for capability in required_extern_capabilities(planned) {
            match capability.as_str() {
                "field" => {
                    spec.adt_structure = true;
                    spec.type_facts = true;
                }
                "variant" => spec.adt_structure = true,
                "method" | "trait_method" | "implements" | "from_impl" | "method_of" | "impl_id" => {
                    spec.impls = true;
                }
                "call_edge" | "call_id" => {
                    spec.call_graph = true;
                    spec.def_spans = true;
                }
                "fn_error_type" | "fn_return_type" | "ty_app" | "ty_arg" | "ty_ref"
                | "ty_ptr" | "ty_tuple" | "ty_slice" | "ty_param" | "ty_prim"
                | "ty_unknown" | "typeref_id" => spec.type_facts = true,
                "def_path" => spec.def_paths = true,
                "node_at" | "node_kind" | "node_span" | "node_parent" | "enclosing_control"
                | "node_id" => {
                    spec.syntax_nodes = true;
                    spec.def_spans = true;
                }
                "def_span" | "span_key" | "span_allowed" => spec.def_spans = true,
                "handle" => spec.def_handles = true,
                "is_public" => spec.def_publicity = true,
                "in_test" => spec.def_test_flags = true,
                _ => {}
            }
        }
        spec
    }

    fn covers(&self, other: &Self) -> bool {
        (!other.impls || self.impls)
            && (!other.call_graph || self.call_graph)
            && (!other.syntax_nodes || self.syntax_nodes)
            && (!other.type_facts || self.type_facts)
            && (!other.adt_structure || self.adt_structure)
            && (!other.def_paths || self.def_paths)
            && (!other.def_spans || self.def_spans)
            && (!other.def_handles || self.def_handles)
            && (!other.def_publicity || self.def_publicity)
            && (!other.def_test_flags || self.def_test_flags)
    }

    fn union(&self, other: &Self) -> Self {
        Self {
            impls: self.impls || other.impls,
            call_graph: self.call_graph || other.call_graph,
            syntax_nodes: self.syntax_nodes || other.syntax_nodes,
            type_facts: self.type_facts || other.type_facts,
            adt_structure: self.adt_structure || other.adt_structure,
            def_paths: self.def_paths || other.def_paths,
            def_spans: self.def_spans || other.def_spans,
            def_handles: self.def_handles || other.def_handles,
            def_publicity: self.def_publicity || other.def_publicity,
            def_test_flags: self.def_test_flags || other.def_test_flags,
        }
    }

    pub(crate) fn supports_lookup_only_fast_path(&self) -> bool {
        !self.impls
            && !self.syntax_nodes
            && !self.type_facts
            && !self.adt_structure
            && !self.def_handles
            && !self.def_publicity
            && !self.def_test_flags
    }
}

impl WorkspaceService {
    pub fn from_workspace_root(root: &Path) -> Result<Self, RaHostInitError> {
        let loaded = workspace_loader::load_from_workspace_root(root)?;
        Self::from_loaded(loaded)
    }

    pub fn from_manifest_path(manifest: &Path) -> Result<Self, RaHostInitError> {
        let loaded = workspace_loader::load_from_manifest_path(manifest)?;
        Self::from_loaded(loaded)
    }

    fn from_loaded(loaded: workspace_loader::LoadedWorkspace) -> Result<Self, RaHostInitError> {
        let init_started = Instant::now();
        let tracked_files_started = Instant::now();
        let tracked_files = tracked_workspace_state(
            &loaded.db,
            &loaded.vfs,
            &loaded.watched_entries,
            loaded.manifest_path.as_path(),
            loaded.workspace_root.as_path(),
            &loaded.local_tracking_roots,
        )?;
        trace_timing("workspace_service.from_loaded.tracked_workspace_state", tracked_files_started.elapsed());
        let tracked_file_states_started = Instant::now();
        let tracked_file_states = tracked_file_state_map(&tracked_files)?;
        trace_timing("workspace_service.from_loaded.tracked_file_state_map", tracked_file_states_started.elapsed());
        let tracked_dirs_started = Instant::now();
        let tracked_dirs =
            tracked_directory_watch_set(&tracked_files, loaded.manifest_path.as_path(), loaded.workspace_root.as_path());
        trace_timing("workspace_service.from_loaded.tracked_directory_watch_set", tracked_dirs_started.elapsed());
        let tracked_dir_states_started = Instant::now();
        let tracked_dir_states = tracked_file_state_map(&tracked_dirs)?;
        trace_timing("workspace_service.from_loaded.tracked_dir_state_map", tracked_dir_states_started.elapsed());
        let build_scripts_started = Instant::now();
        let build_script_rerun_paths = build_script_rerun_paths(&tracked_files)?;
        trace_timing("workspace_service.from_loaded.build_script_rerun_paths", build_scripts_started.elapsed());
        let aux_inputs_started = Instant::now();
        let auxiliary_build_inputs =
            auxiliary_build_input_state(&build_script_rerun_paths, &tracked_files)?;
        trace_timing("workspace_service.from_loaded.auxiliary_build_input_state", aux_inputs_started.elapsed());
        Ok(Self {
            manifest_path: loaded.manifest_path,
            workspace_root: loaded.workspace_root,
            analysis_host: AnalysisHost::with_database(loaded.db),
            vfs: loaded.vfs,
            _proc_macro_client: loaded.proc_macro_client,
            core_host: None,
            core_index: None,
            core_host_spec: None,
            tracked_files,
            tracked_file_states,
            tracked_dirs,
            tracked_dir_states,
            watched_entries: loaded.watched_entries,
            build_script_rerun_paths,
            auxiliary_build_inputs,
            lookup_defs: BTreeMap::new(),
            lookup_spans: BTreeMap::new(),
            workspace_epoch: 0,
            content_revision: 0,
        })
        .inspect(|_| trace_timing("workspace_service.from_loaded.total", init_started.elapsed()))
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn workspace_epoch(&self) -> u64 {
        self.workspace_epoch
    }

    pub fn analysis_snapshot(&self) -> WarmupSnapshot {
        WarmupSnapshot(self.analysis_host.analysis())
    }

    pub fn content_revision(&self) -> u64 {
        self.content_revision
    }

    pub(crate) fn current_world_stamp(&self) -> WorldStamp {
        WorldStamp::new(format!(
            "ra-workspace:{}:{}:{}",
            self.workspace_root.display(),
            self.workspace_epoch,
            self.content_revision
        ))
    }

    pub fn supported_capabilities(&self) -> CapabilitySet {
        day_one_supported_capabilities()
    }

    pub fn sync(&mut self) -> Result<(), RaHostInitError> {
        self.sync_workspace()
    }

    pub fn prewarm_semantics(&mut self) -> Result<(), RaHostInitError> {
        let started = Instant::now();
        let sync_started = Instant::now();
        self.sync_workspace()?;
        trace_timing(
            "workspace_service.prewarm_semantics.sync_workspace",
            sync_started.elapsed(),
        );
        let prime_started = Instant::now();
        let worker_threads = std::env::var("RAQL_PRIME_CACHE_THREADS")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|threads| *threads > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|threads| threads.get().min(4))
                    .unwrap_or(1)
            });
        self.analysis_host
            .analysis()
            .parallel_prime_caches(worker_threads, |_| {})
            .map_err(|err| RaHostInitError::SemanticBuild {
                details: format!("rust-analyzer cache priming cancelled: {err:?}"),
            })?;
        trace_timing(
            "workspace_service.prewarm_semantics.parallel_prime_caches",
            prime_started.elapsed(),
        );
        trace_timing("workspace_service.prewarm_semantics.total", started.elapsed());
        Ok(())
    }

    pub(crate) fn supports_extern_predicate(&self, predicate: &str) -> bool {
        is_engine_managed_extern(predicate)
            || is_runtime_scalar_input_predicate(predicate)
            || supports_day_one_capability(predicate)
    }

    pub(crate) fn ensure_supported_planned(
        &self,
        planned: &PlannedProgram,
    ) -> Result<(), RaHostInitError> {
        MissingCapabilitiesError::from_required_and_supported(
            required_extern_capabilities(planned)
                .into_iter()
                .map(CapabilityId::from),
            self.supported_capabilities(),
        )
        .map_err(|err| RaHostInitError::SemanticBuild {
            details: err.to_string(),
        })
    }

    pub(crate) fn extern_lookup_rows(
        &mut self,
        request: &ExternLookupRequest,
    ) -> Result<Option<Vec<Vec<ExternLookupValue>>>, RaHostInitError> {
        let lookup_started = Instant::now();
        let result = match (request.predicate(), request.shape()) {
            ("def_name", ExternLookupShape::FunctionExactBindings) => {
                lookup_def_name_rows(
                    request,
                    self.analysis_host.raw_database(),
                    &self.vfs,
                    self.workspace_root.as_path(),
                    self.core_index.as_ref(),
                    &mut self.lookup_defs,
                    &mut self.lookup_spans,
                )
                .map(Some)
            }
            ("def", ExternLookupShape::RelationExactBindings) => {
                Ok(Some(lookup_def_rows(request, &self.lookup_defs)))
            }
            ("def_kind", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_def_kind_rows(
                    request,
                    self.core_index.as_ref(),
                    &self.lookup_defs,
                )))
            }
            ("def_span", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_def_span_rows(request, &self.lookup_defs)))
            }
            ("def_path", ExternLookupShape::FunctionExactBindings) => lookup_def_path_rows(
                request,
                self.analysis_host.raw_database(),
                self.core_index.as_ref(),
                &mut self.lookup_defs,
            ),
            ("call_edge", ExternLookupShape::RelationExactBindings) => {
                self.lookup_call_edge_rows(request)
            }
            ("span_allowed", ExternLookupShape::RelationExactBindings) => {
                Ok(Some(lookup_span_allowed_rows(request, &self.lookup_spans)))
            }
            ("span_key", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_span_key_rows(request, &self.lookup_spans)))
            }
            _ => Ok(None),
        };
        trace_lookup_timing(request, lookup_started.elapsed());
        result
    }

    pub(crate) fn take_runtime_notes_if_ready(&mut self) -> Vec<String> {
        self.core_host
            .as_mut()
            .map(|host| host.drain_runtime_notes())
            .unwrap_or_default()
    }

    pub(crate) fn set_control_max_depth_if_ready(&mut self, depth: u32) {
        if let Some(host) = self.core_host.as_mut() {
            host.set_control_max_depth(depth);
        }
    }

    pub fn run_planned(&mut self, planned: &PlannedProgram) -> Result<EvalResult, RaHostInitError> {
        let run_started = Instant::now();
        self.ensure_supported_planned(planned)?;
        trace_timing(
            "workspace_service.ensure_supported_planned",
            run_started.elapsed(),
        );
        let sync_started = Instant::now();
        self.sync_workspace()?;
        trace_timing("workspace_service.sync_workspace", sync_started.elapsed());
        let core_host_spec = CoreHostBuildSpec::from_planned(planned);
        let shared = Rc::new(RefCell::new(self));
        let mut runtime = LazyRaRuntime::new(shared, core_host_spec);
        let execute_started = Instant::now();
        let result = execute(planned, &mut runtime);
        trace_timing("workspace_service.execute", execute_started.elapsed());
        trace_timing("workspace_service.run_planned.total", run_started.elapsed());
        Ok(result)
    }

    fn lookup_call_edge_rows(
        &mut self,
        request: &ExternLookupRequest,
    ) -> Result<Option<Vec<Vec<ExternLookupValue>>>, RaHostInitError> {
        let lookup_started = Instant::now();
        let rows = provider_lookup_call_edge_rows(
            request,
            self.analysis_host.raw_database(),
            &self.vfs,
            self.workspace_root.as_path(),
            &mut self.lookup_defs,
            &mut self.lookup_spans,
        );
        trace_timing(
            "workspace_service.lookup_call_edge_rows.total",
            lookup_started.elapsed(),
        );
        rows
    }

    pub(crate) fn ensure_core_host(
        &mut self,
        required_spec: &CoreHostBuildSpec,
    ) -> Result<&mut DeterministicRaHost, RaHostInitError> {
        let ensure_started = Instant::now();
        let build_spec = self
            .core_host_spec
            .as_ref()
            .map(|current| current.union(required_spec))
            .unwrap_or_else(|| required_spec.clone());
        if self.core_host.is_none()
            || self
                .core_host_spec
                .as_ref()
                .is_none_or(|current| !current.covers(required_spec))
        {
            let build_started = Instant::now();
            let artifacts = build_core_host(
                &self.analysis_host,
                &self.vfs,
                &self.workspace_root,
                &self.tracked_files,
                self.workspace_epoch,
                self.content_revision,
                &build_spec,
            )?;
            self.core_host = Some(artifacts.host);
            self.core_index = Some(artifacts.index);
            trace_timing("workspace_service.ensure_core_host.build_core_host", build_started.elapsed());
            self.core_host_spec = Some(build_spec);
        }
        trace_timing("workspace_service.ensure_core_host.total", ensure_started.elapsed());
        Ok(self.core_host.as_mut().expect("core host initialized"))
    }

    fn sync_workspace(&mut self) -> Result<(), RaHostInitError> {
        // TODO(ra-native-audit): replace raw filesystem rescans with RA/Cargo/VFS-backed truth.
        // This warm-path sweep is both a correctness risk and the main latency offender.
        let auxiliary_build_inputs =
            auxiliary_build_input_state(&self.build_script_rerun_paths, &self.tracked_files)?;
        if auxiliary_build_inputs != self.auxiliary_build_inputs {
            return self.reload_full();
        }

        let tracked_dir_states = tracked_file_state_map(&self.tracked_dirs)?;
        if tracked_dir_states != self.tracked_dir_states {
            return self.reload_full();
        }
        let tracked_file_states = tracked_file_state_map(&self.tracked_files)?;
        if tracked_file_states == self.tracked_file_states {
            return Ok(());
        }

        let mut change = hir::ChangeWithProcMacros::default();
        let mut saw_change = false;
        for path in &self.tracked_files {
            let changed_on_disk = tracked_file_states
                .get(path)
                .is_some_and(|state| self.tracked_file_states.get(path) != Some(state));
            if !changed_on_disk {
                continue;
            }
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
            if path_requires_reload(path.as_path(), &self.build_script_rerun_paths) {
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
            // TODO(ra-native-audit): stop dropping the entire deterministic host for ordinary
            // source edits. Preserve or incrementally reconcile host state where safe.
            self.core_host = None;
            self.core_index = None;
            self.core_host_spec = None;
            self.lookup_defs.clear();
            self.lookup_spans.clear();
        }
        self.tracked_file_states = tracked_file_states;
        Ok(())
    }

    fn reload_full(&mut self) -> Result<(), RaHostInitError> {
        let loaded = workspace_loader::load_from_manifest_path(&self.manifest_path)?;
        let workspace_loader::LoadedWorkspace {
            manifest_path,
            workspace_root,
            db,
            vfs,
            watched_entries,
            local_tracking_roots,
            proc_macro_client,
            ..
        } = loaded;
        let tracked_files =
            tracked_workspace_state(
                &db,
                &vfs,
                &watched_entries,
                manifest_path.as_path(),
                workspace_root.as_path(),
                &local_tracking_roots,
            )?;
        let tracked_file_states = tracked_file_state_map(&tracked_files)?;
        let tracked_dirs = tracked_directory_watch_set(&tracked_files, manifest_path.as_path(), workspace_root.as_path());
        let tracked_dir_states = tracked_file_state_map(&tracked_dirs)?;
        let build_script_rerun_paths = build_script_rerun_paths(&tracked_files)?;
        let auxiliary_build_inputs =
            auxiliary_build_input_state(&build_script_rerun_paths, &tracked_files)?;
        self.manifest_path = manifest_path;
        self.workspace_root = workspace_root;
        self.analysis_host = AnalysisHost::with_database(db);
        self.vfs = vfs;
        self._proc_macro_client = proc_macro_client;
        self.core_host = None;
        self.core_index = None;
        self.core_host_spec = None;
        self.tracked_files = tracked_files;
        self.tracked_file_states = tracked_file_states;
        self.tracked_dirs = tracked_dirs;
        self.tracked_dir_states = tracked_dir_states;
        self.watched_entries = watched_entries;
        self.build_script_rerun_paths = build_script_rerun_paths;
        self.auxiliary_build_inputs = auxiliary_build_inputs;
        self.lookup_defs.clear();
        self.lookup_spans.clear();
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
    workspace_loader::true_workspace_root(Path::new(&manifest.manifest_path().to_string()))
}

struct CoreFactsBuilder<'db> {
    db: &'db ide::RootDatabase,
    build_spec: CoreHostBuildSpec,
    files: BTreeMap<vfs::FileId, LocalFile>,
    host: DeterministicRaHost,
    core_index: CoreLookupIndex,
    def_path_by_id: BTreeMap<DefId, String>,
    local_adts: Vec<Adt>,
    local_traits: Vec<hir::Trait>,
    local_functions: Vec<hir::Function>,
}

impl<'db> CoreFactsBuilder<'db> {
    fn populate(&mut self, build_spec: &CoreHostBuildSpec) {
        let populate_started = Instant::now();
        let defs_started = Instant::now();
        self.populate_defs_from_symbols();
        trace_timing("workspace_service.populate.defs_from_symbols", defs_started.elapsed());
        let semantic_started = Instant::now();
        hir::attach_db(self.db, || {
            if build_spec.impls || build_spec.call_graph || build_spec.type_facts || build_spec.adt_structure {
                for krate in hir::Crate::all(self.db)
                    .into_iter()
                    .filter(|krate| krate.origin(self.db).is_local())
                {
                    self.visit_module(krate.root_module(self.db), false);
                }
            }
            if build_spec.impls {
                self.extract_impls();
            }
            if build_spec.call_graph {
                self.extract_call_edges();
            }
            if build_spec.syntax_nodes {
                extract_syntax_nodes(self.db, &self.files, &mut self.host);
            }
        });
        trace_timing("workspace_service.populate.semantic_phases", semantic_started.elapsed());
        trace_timing("workspace_service.populate.total", populate_started.elapsed());
    }

    fn populate_defs_from_symbols(&mut self) {
        let query_started = Instant::now();
        let mut query = Query::new(String::new());
        query.exclude_imports();
        query.assoc_search_mode(AssocSearchMode::Exclude);
        let symbols = world_symbols(self.db, query);
        trace_timing("workspace_service.populate_defs_from_symbols.world_symbols", query_started.elapsed());
        let lower_started = Instant::now();
        hir::attach_db(self.db, || {
            let sema = hir::Semantics::new(self.db);
            let mut parsed_by_file = HashMap::new();
            for symbol in symbols {
                if symbol.is_import || symbol.is_alias {
                    continue;
                }
                if let ModuleDef::Function(function) = symbol.def {
                    let _ = self.register_function_def(function);
                    continue;
                }
                let editioned = symbol.loc.hir_file_id.original_file(self.db);
                let Some(local) = self.files.get(&editioned.file_id(self.db)) else {
                    continue;
                };
                let Some(kind) = module_def_kind(symbol.def) else {
                    continue;
                };
                let name = symbol.name.as_str();
                let token = format!("symbol:{kind:?}:{name}:{:?}", symbol.loc);
                let def_id = self.host.intern_def_from_token(token.as_str());
                let path = if self.build_spec.def_paths {
                    symbol
                        .def
                        .canonical_path(self.db, Edition::CURRENT)
                        .unwrap_or_else(|| crate::fallback_def_path(def_id))
                } else {
                    crate::fallback_def_path(def_id)
                };
                if self.def_path_by_id.contains_key(&def_id) {
                    continue;
                }
                if self.build_spec.def_spans {
                    let root = parsed_by_file
                        .entry(symbol.loc.hir_file_id)
                        .or_insert_with(|| sema.parse_or_expand(symbol.loc.hir_file_id));
                    let syntax = symbol.loc.ptr.to_node(root);
                    let span = self
                        .host
                        .intern_span_from_text(
                            editioned.editioned_file_id(self.db),
                            local.rel_path.clone(),
                            local.text.as_str(),
                            syntax.text_range(),
                        )
                        .ok();
                    if let Some(span) = span {
                        self.host.insert_def(def_id, name, kind, span, path.as_str());
                    } else {
                        self.host.insert_synthetic_def(def_id, name, kind, path.as_str());
                    }
                } else {
                    self.host.insert_synthetic_def(def_id, name, kind, path.as_str());
                }
                if self.build_spec.def_handles {
                    self.host.insert_handle(def_id, format!("def://{path}"));
                }
                self.record_core_def(def_id, name, kind, path.as_str());
                if self.build_spec.def_publicity {
                    let is_public = module_def_is_public(symbol.def, self.db);
                    self.host.mark_public(def_id, is_public);
                    self.core_index.mark_public(def_id, is_public);
                }
                if self.build_spec.def_test_flags {
                    let in_test = module_def_in_test(symbol.def, self.db);
                    self.host.mark_in_test(def_id, in_test);
                    self.core_index.mark_in_test(def_id, in_test);
                }
            }
        });
        trace_timing("workspace_service.populate_defs_from_symbols.lower", lower_started.elapsed());
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
                ModuleDef::Function(function) => {
                    if self.build_spec.call_graph {
                        self.local_functions.push(function);
                    }
                    if self.build_spec.type_facts {
                        self.register_function_types(def_id, function);
                    }
                }
                ModuleDef::Adt(adt) => {
                    if self.build_spec.impls || self.build_spec.adt_structure {
                        self.process_adt(adt, def_id);
                    }
                }
                ModuleDef::Trait(trait_def) => {
                    if self.build_spec.impls {
                        self.local_traits.push(trait_def);
                    }
                    if self.build_spec.impls || self.build_spec.type_facts || self.build_spec.call_graph {
                        for assoc in trait_def.items(self.db) {
                            if let AssocItem::Function(function) = assoc {
                                let Some(method_def) =
                                    self.register_module_def(ModuleDef::Function(function), module_test)
                                else {
                                    continue;
                                };
                                if self.build_spec.impls {
                                    self.host.insert_trait_method(def_id, method_def);
                                }
                                if self.build_spec.call_graph {
                                    self.local_functions.push(function);
                                }
                                if self.build_spec.type_facts {
                                    self.register_function_types(method_def, function);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn process_adt(&mut self, adt: Adt, owner_def: DefId) {
        self.local_adts.push(adt);
        if !self.build_spec.adt_structure {
            return;
        }
        match adt {
            Adt::Struct(strukt) => self.insert_struct_fields(owner_def, strukt),
            Adt::Union(union) => self.insert_union_fields(owner_def, union),
            Adt::Enum(enum_) => {
                for variant in enum_.variants(self.db) {
                    let Some(variant_def) = self.register_module_def(ModuleDef::Variant(variant), false) else {
                        continue;
                    };
                    let variant_name = variant.name(self.db).display(self.db, Edition::CURRENT).to_string();
                    self.host.insert_variant(owner_def, variant_name, variant_def);
                }
            }
        }
    }

    fn insert_struct_fields(&mut self, owner_def: DefId, strukt: hir::Struct) {
        let Some(source) = strukt.source(self.db) else {
            return;
        };
        match source.value.field_list() {
            Some(ast::FieldList::RecordFieldList(fields)) => {
                for (source_field, hir_field) in fields.fields().zip(strukt.fields(self.db)) {
                    let Some(name) = source_field.name().map(|name| name.text().to_string()) else {
                        continue;
                    };
                    self.insert_field(
                        owner_def,
                        name.as_str(),
                        hir_field.ty(self.db).to_type(self.db),
                        source_field.ty(),
                    );
                }
            }
            Some(ast::FieldList::TupleFieldList(fields)) => {
                for ((index, source_field), hir_field) in
                    fields.fields().enumerate().zip(strukt.fields(self.db))
                {
                    let name = index.to_string();
                    self.insert_field(
                        owner_def,
                        name.as_str(),
                        hir_field.ty(self.db).to_type(self.db),
                        source_field.ty(),
                    );
                }
            }
            None => {}
        }
    }

    fn insert_union_fields(&mut self, owner_def: DefId, union: hir::Union) {
        let Some(source) = union.source(self.db) else {
            return;
        };
        let Some(fields) = source.value.record_field_list() else {
            return;
        };
        for (source_field, hir_field) in fields.fields().zip(union.fields(self.db)) {
            let Some(name) = source_field.name().map(|name| name.text().to_string()) else {
                continue;
            };
            self.insert_field(
                owner_def,
                name.as_str(),
                hir_field.ty(self.db).to_type(self.db),
                source_field.ty(),
            );
        }
    }

    fn insert_field(
        &mut self,
        owner_def: DefId,
        name: &str,
        ty: hir::Type,
        source_ty: Option<ast::Type>,
    ) {
        let owner_path = self
            .def_path_by_id
            .get(&owner_def)
            .map(String::as_str)
            .unwrap_or("unknown_owner");
        let type_ref =
            self.lower_type(format!("field:{owner_path}:{name}").as_str(), ty, source_ty.as_ref());
        self.host.insert_field(owner_def, name.to_string(), type_ref);
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
            let impl_record_def = self.register_impl_def(impl_def);
            let self_ty_def = impl_def
                .self_ty(self.db)
                .as_adt()
                .and_then(|adt| self.register_module_def(ModuleDef::Adt(adt), false));
            if let (Some(owner), Some(trait_def), Some(impl_record)) = (
                self_ty_def,
                impl_def
                    .trait_(self.db)
                    .and_then(|trait_| self.register_module_def(ModuleDef::Trait(trait_), false)),
                impl_record_def,
            ) {
                self.host.insert_implements(owner, trait_def, impl_record);
            }
            let is_from_impl = impl_def
                .trait_(self.db)
                .is_some_and(|trait_| {
                    ModuleDef::Trait(trait_)
                        .canonical_path(self.db, Edition::CURRENT)
                        .is_some_and(|path| path.ends_with("::From"))
                        || trait_.name(self.db).display(self.db, Edition::CURRENT).to_string() == "From"
                });
            if is_from_impl
                && let (Some(dst), Some(impl_record), Some(src)) = (
                    self_ty_def,
                    impl_record_def,
                    impl_def
                        .trait_ref(self.db)
                        .and_then(|trait_ref| trait_ref.get_type_argument(1))
                        .and_then(|src_ty| src_ty.to_type(self.db).as_adt())
                        .and_then(|adt| self.register_module_def(ModuleDef::Adt(adt), false)),
                )
            {
                self.host.insert_from_impl(src, dst, impl_record);
            }
            for assoc in impl_def.items(self.db) {
                if let AssocItem::Function(function) = assoc {
                    let Some(method_def) = self.register_module_def(ModuleDef::Function(function), false) else {
                        continue;
                    };
                    self.local_functions.push(function);
                    self.register_function_types(method_def, function);
                    if let Some(owner) = self_ty_def {
                        self.host.insert_method(owner, method_def);
                    }
                }
            }
        }
    }

    fn register_function_types(&mut self, function_def: DefId, function: hir::Function) {
        let source_ty = function
            .source(self.db)
            .and_then(|source| source.value.ret_type())
            .and_then(|ret| ret.ty());
        let function_path = self
            .def_path_by_id
            .get(&function_def)
            .cloned()
            .unwrap_or_else(|| format!("fn:{:#018x}", function_def.stable_id().as_u64()));
        let return_ty = function
            .async_ret_type(self.db)
            .unwrap_or_else(|| function.ret_type(self.db));
        let return_ref = self.lower_type(
            format!("fn_return:{function_path}").as_str(),
            return_ty.clone(),
            source_ty.as_ref(),
        );
        let error_def = self.result_error_def(return_ty);
        self.host.set_fn_return_type(function_def, Some(return_ref));
        self.host.set_fn_error_type(function_def, error_def);
    }

    fn extract_call_edges(&mut self) {
        // TODO(ra-native-audit): keep tightening this toward RA's outgoing-call semantics.
        // This now preserves closure bodies while excluding nested item bodies from the outer
        // function's call set.
        let sema = hir::Semantics::new(self.db);
        let local_functions = self.local_functions.clone();
        let mut parsed_by_file = HashMap::new();
        for function in local_functions {
            let Some(source) = function.source(self.db) else {
                continue;
            };
            let editioned = source.file_id.original_file(self.db);
            let Some(local) = self.files.get(&editioned.file_id(self.db)).cloned() else {
                continue;
            };
            let parsed = parsed_by_file
                .entry(source.file_id)
                .or_insert_with(|| sema.parse_or_expand(source.file_id));
            let lookup_offset = source
                .value
                .name()
                .map(|name| name.syntax().text_range().start())
                .unwrap_or_else(|| source.value.syntax().text_range().start());
            let Some(ast_fn) = sema.find_node_at_offset_with_descend::<ast::Fn>(parsed, lookup_offset) else {
                continue;
            };
            let Some(caller_def) = self.register_function_def(function) else {
                continue;
            };
            let Some(body) = ast_fn.body() else {
                continue;
            };
            let owner_item = ast::Item::Fn(ast_fn.clone());
            for callable_expr in body.syntax().descendants().filter_map(ast::CallableExpr::cast) {
                if !belongs_to_item(callable_expr.syntax(), owner_item.syntax()) {
                    continue;
                }
                match callable_expr {
                    ast::CallableExpr::Call(call) => {
                        self.record_call_expr(
                            &sema,
                            caller_def,
                            &call,
                            editioned.editioned_file_id(self.db),
                            &local,
                        );
                    }
                    ast::CallableExpr::MethodCall(method_call) => {
                        self.record_method_call(
                            &sema,
                            caller_def,
                            &method_call,
                            editioned.editioned_file_id(self.db),
                            &local,
                        );
                    }
                }
            }
        }
    }

    fn record_call_expr(
        &mut self,
        sema: &hir::Semantics<'_, ide::RootDatabase>,
        caller_def: DefId,
        call: &ast::CallExpr,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) {
        let Some(callee_expr) = call.expr() else {
            return;
        };
        let Some(type_info) = sema.type_of_expr(&callee_expr) else {
            return;
        };
        let Some(callable) = type_info.original.as_callable(self.db) else {
            return;
        };
        let Some((callee_def, dispatch)) =
            self.call_target_from_callable(&callable, &callee_expr, file_id, local)
        else {
            return;
        };
        let Ok(site) = self.host.intern_span_from_text(
            file_id,
            local.rel_path.clone(),
            local.text.as_str(),
            call.syntax().text_range(),
        ) else {
            return;
        };
        let range = call.syntax().text_range();
        let token = format!(
            "{}:{}..{}",
            local.rel_path,
            u32::from(range.start()),
            u32::from(range.end())
        );
        let call_id = crate::CallId::new(crate::deterministic_stable_id("call", token.as_str()));
        self.host.insert_call_id(call_id, format!("call:{token}"));
        self.host
            .insert_call_edge(caller_def, callee_def, site, dispatch);
    }

    fn record_method_call(
        &mut self,
        sema: &hir::Semantics<'_, ide::RootDatabase>,
        caller_def: DefId,
        method_call: &ast::MethodCallExpr,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) {
        let Some(function) = sema.resolve_method_call(method_call) else {
            return;
        };
        let Some(callee_def) = self.register_function_def(function) else {
            return;
        };
        let dispatch = method_dispatch_kind(sema, method_call, function, self.db);
        let Ok(site) = self.host.intern_span_from_text(
            file_id,
            local.rel_path.clone(),
            local.text.as_str(),
            method_call.syntax().text_range(),
        ) else {
            return;
        };
        let range = method_call.syntax().text_range();
        let token = format!(
            "{}:{}..{}",
            local.rel_path,
            u32::from(range.start()),
            u32::from(range.end())
        );
        let call_id = crate::CallId::new(crate::deterministic_stable_id("call", token.as_str()));
        self.host.insert_call_id(call_id, format!("call:{token}"));
        self.host
            .insert_call_edge(caller_def, callee_def, site, dispatch);
    }

    fn call_target_from_callable(
        &mut self,
        callable: &hir::Callable<'_>,
        callee_expr: &ast::Expr,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) -> Option<(DefId, crate::DispatchKind)> {
        match callable.kind() {
            hir::CallableKind::Function(function) => self
                .register_function_def(function)
                .map(|def| (def, crate::DispatchKind::Direct)),
            hir::CallableKind::TupleStruct(strukt) => Some((
                self.register_adt_def(Adt::Struct(strukt)),
                crate::DispatchKind::Direct,
            )),
            hir::CallableKind::TupleEnumVariant(variant) => self
                .register_module_def(ModuleDef::Variant(variant), false)
                .map(|def| (def, crate::DispatchKind::Direct)),
            hir::CallableKind::Closure(_) => Some((
                self.register_synthetic_callable_def("closure", callee_expr.syntax(), file_id, local),
                crate::DispatchKind::Closure,
            )),
            hir::CallableKind::FnPtr | hir::CallableKind::FnImpl(_) => Some((
                self.register_synthetic_callable_def("fn_pointer", callee_expr.syntax(), file_id, local),
                crate::DispatchKind::FnPointer,
            )),
        }
    }

    fn result_error_def(&mut self, ty: hir::Type) -> Option<DefId> {
        let head = ty.as_adt()?;
        let path = ModuleDef::Adt(head).canonical_path(self.db, Edition::CURRENT)?;
        if !(matches!(
            path.as_str(),
            "std::result::Result" | "core::result::Result" | "result::Result"
        ) || path.ends_with("::Result"))
        {
            return None;
        }
        let err_ty = ty.type_arguments().nth(1)?;
        if let Some(adt) = err_ty.as_adt() {
            return Some(self.register_adt_def(adt));
        }
        err_ty
            .as_type_param(self.db)
            .map(|param| self.register_type_param_def(param, "fn_error"))
    }

    fn lower_type(
        &mut self,
        key: &str,
        ty: hir::Type,
        source_ty: Option<&ast::Type>,
    ) -> crate::TypeRefId {
        let type_ref = self.host.intern_typeref_from_token(&format!("ty:{key}"));
        let normalized_source = source_ty.cloned().map(normalize_type_ast);
        let shape = if ty.is_unknown() {
            TypeShape::Unknown
        } else if let Some((inner, mutability)) = ty.as_reference() {
            let inner_ast = normalized_source.as_ref().and_then(ref_inner_type);
            TypeShape::Ref {
                mutability: map_mutability(mutability),
                inner: self.lower_type(format!("{key}/ref").as_str(), inner, inner_ast.as_ref()),
            }
        } else if ty.is_raw_ptr() {
            let inner = ty
                .remove_raw_ptr()
                .expect("raw pointer types should expose inner type");
            let (ptr_mutability, inner_ast) = normalized_source
                .as_ref()
                .map(ptr_parts)
                .unwrap_or((Mutability::Shared, None));
            TypeShape::Ptr {
                mutability: ptr_mutability,
                inner: self.lower_type(format!("{key}/ptr").as_str(), inner, inner_ast.as_ref()),
            }
        } else if ty.is_tuple() {
            let item_asts = normalized_source.as_ref().map(tuple_item_asts).unwrap_or_default();
            TypeShape::Tuple(
                ty.tuple_fields(self.db)
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let child_ast = item_asts.get(index);
                        self.lower_type(format!("{key}/tuple/{index}").as_str(), item, child_ast)
                    })
                    .collect(),
            )
        } else if let Some(inner) = ty.as_slice() {
            let inner_ast = normalized_source.as_ref().and_then(slice_inner_type);
            TypeShape::Slice(self.lower_type(
                format!("{key}/slice").as_str(),
                inner,
                inner_ast.as_ref(),
            ))
        } else if let Some(param) = ty.as_type_param(self.db) {
            TypeShape::Param(self.register_type_param_def(param, key))
        } else if ty.is_never() {
            TypeShape::Prim("!".to_string())
        } else if let Some(builtin) = ty.as_builtin() {
            TypeShape::Prim(builtin.name().as_str().to_string())
        } else if let Some(adt) = ty.as_adt() {
            let arg_asts = normalized_source
                .as_ref()
                .map(path_type_arg_asts)
                .unwrap_or_default();
            TypeShape::App {
                head: self.register_adt_def(adt),
                args: ty
                    .type_arguments()
                    .enumerate()
                    .map(|(index, arg)| {
                        GenericArg::Type(self.lower_type(
                            format!("{key}/arg/{index}").as_str(),
                            arg,
                            arg_asts.get(index),
                        ))
                    })
                    .collect(),
            }
        } else {
            TypeShape::Unknown
        };
        self.host.insert_type(type_ref, shape);
        type_ref
    }

    fn register_adt_def(&mut self, adt: Adt) -> DefId {
        if let Some(def_id) = self.register_module_def(ModuleDef::Adt(adt), false) {
            return def_id;
        }
        let name = ModuleDef::Adt(adt)
            .name(self.db)
            .map(|name| name.display(self.db, Edition::CURRENT).to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let raw_path = ModuleDef::Adt(adt)
            .canonical_path(self.db, Edition::CURRENT)
            .unwrap_or_else(|| format!("external::{name}"));
        let path = adt
            .module(self.db)
            .krate(self.db)
            .display_name(self.db)
            .map(|crate_name| crate_name.to_string())
            .filter(|crate_name| {
                raw_path != *crate_name && !raw_path.starts_with(format!("{crate_name}::").as_str())
            })
            .map(|crate_name| format!("{crate_name}::{raw_path}"))
            .unwrap_or(raw_path);
        let def_id = self
            .host
            .intern_def_from_token(format!("def:adt:{path}").as_str());
        if !self.def_path_by_id.contains_key(&def_id) {
            let kind = match adt {
                Adt::Struct(_) => DefKind::Struct,
                Adt::Enum(_) => DefKind::Enum,
                Adt::Union(_) => DefKind::Union,
            };
            self.host
                .insert_synthetic_def(def_id, name.as_str(), kind, path.as_str());
            self.host.mark_public(def_id, true);
            self.host.mark_in_test(def_id, false);
            self.record_core_def(def_id, name.as_str(), kind, path.as_str());
            self.core_index.mark_public(def_id, true);
            self.core_index.mark_in_test(def_id, false);
        }
        def_id
    }

    fn register_function_def(&mut self, function: hir::Function) -> Option<DefId> {
        self.register_module_def(ModuleDef::Function(function), false)
    }

    fn register_synthetic_callable_def(
        &mut self,
        prefix: &str,
        syntax: &syntax::SyntaxNode,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) -> DefId {
        let range = syntax.text_range();
        let label = syntax.text().to_string();
        let path = format!(
            "{prefix}::{}:{}..{}:{}",
            local.rel_path,
            u32::from(range.start()),
            u32::from(range.end()),
            label
        );
        let def_id = self
            .host
            .intern_def_from_token(format!("def:{prefix}:{path}").as_str());
        if !self.def_path_by_id.contains_key(&def_id) {
            let span = self
                .host
                .intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range)
                .ok();
            if let Some(span) = span {
                self.host
                    .insert_def(def_id, label.as_str(), DefKind::Other, span, path.as_str());
                self.host.insert_handle(def_id, format!("def://{path}"));
                self.host.mark_public(def_id, false);
                self.host.mark_in_test(def_id, false);
            } else {
                self.host
                    .insert_synthetic_def(def_id, label.as_str(), DefKind::Other, path.as_str());
                self.host.mark_public(def_id, false);
                self.host.mark_in_test(def_id, false);
            }
            self.record_core_def(def_id, label.as_str(), DefKind::Other, path.as_str());
            self.core_index.mark_public(def_id, false);
            self.core_index.mark_in_test(def_id, false);
        }
        def_id
    }

    fn register_type_param_def(&mut self, param: hir::TypeParam, key: &str) -> DefId {
        let name = param.name(self.db).display(self.db, Edition::CURRENT).to_string();
        let path = format!("type_param::{key}::{name}");
        let def_id = self
            .host
            .intern_def_from_token(format!("def:type_param:{path}").as_str());
        if !self.def_path_by_id.contains_key(&def_id) {
            self.host
                .insert_synthetic_def(def_id, name.as_str(), DefKind::Other, path.as_str());
            self.host.mark_public(def_id, false);
            self.host.mark_in_test(def_id, false);
            self.record_core_def(def_id, name.as_str(), DefKind::Other, path.as_str());
            self.core_index.mark_public(def_id, false);
            self.core_index.mark_in_test(def_id, false);
        }
        def_id
    }

    fn register_impl_def(&mut self, impl_def: Impl) -> Option<DefId> {
        let source = impl_def.source(self.db)?;
        let editioned = source.file_id.original_file(self.db);
        let local = self.files.get(&editioned.file_id(self.db))?;
        let rel_path = local.rel_path.clone();
        let range = source.value.syntax().text_range();
        let span = self
            .host
            .intern_span_from_text(
                editioned.editioned_file_id(self.db),
                rel_path.clone(),
                local.text.as_str(),
                range,
            )
            .ok()?;
        let path = format!(
            "impl::{rel_path}:{}..{}",
            u32::from(range.start()),
            u32::from(range.end())
        );
        let def_id = self.host.intern_def_from_token(
            format!(
                "def:Impl:{rel_path}:{}..{}",
                u32::from(range.start()),
                u32::from(range.end())
            )
            .as_str(),
        );
        self.host.insert_def(def_id, "impl", DefKind::Impl, span, path.as_str());
        self.host.insert_handle(def_id, format!("def://{path}"));
        let impl_token = format!(
            "{rel_path}:{}..{}",
            u32::from(range.start()),
            u32::from(range.end())
        );
        let impl_id = crate::ImplId::new(crate::deterministic_stable_id(
            "impl",
            impl_token.as_str(),
        ));
        self.host
            .insert_impl_id(impl_id, format!("impl:{impl_token}"));
        self.host.mark_public(def_id, false);
        self.host
            .mark_in_test(def_id, rel_path.starts_with("tests/") || rel_path.contains("/tests/"));
        self.record_core_def(def_id, "impl", DefKind::Impl, path.as_str());
        self.core_index.mark_public(def_id, false);
        self.core_index.mark_in_test(
            def_id,
            rel_path.starts_with("tests/") || rel_path.contains("/tests/"),
        );
        Some(def_id)
    }

    fn register_module_def(&mut self, def: ModuleDef, in_test: bool) -> Option<DefId> {
        let name = def.name(self.db)?.display(self.db, Edition::CURRENT).to_string();
        let (kind, path, def_id) = match def {
            ModuleDef::Function(function) => {
                let kind = if function.has_self_param(self.db) {
                    DefKind::Method
                } else {
                    DefKind::Fn
                };
                let path = canonical_function_path(self.db, function);
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:function:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Module(_) => {
                let kind = DefKind::Mod;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Adt(Adt::Struct(_)) => {
                let kind = DefKind::Struct;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Adt(Adt::Enum(_)) => {
                let kind = DefKind::Enum;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Adt(Adt::Union(_)) => {
                let kind = DefKind::Union;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Variant(_) => {
                let kind = DefKind::Variant;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Const(_) => {
                let kind = DefKind::Const;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Static(_) => {
                let kind = DefKind::Static;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Trait(_) => {
                let kind = DefKind::Trait;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::TypeAlias(_) => {
                let kind = DefKind::TypeAlias;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Macro(_) => {
                let kind = DefKind::Macro;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::BuiltinType(_) => return None,
        };
        if self.build_spec.def_spans {
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
                    rel_path,
                    local.text.as_str(),
                    range,
                )
                .ok()?;
            self.host.insert_def(def_id, name.as_str(), kind, span, path.as_str());
        } else {
            self.host
                .insert_synthetic_def(def_id, name.as_str(), kind, path.as_str());
        }
        if self.build_spec.def_handles {
            self.host.insert_handle(def_id, format!("def://{path}"));
        }
        self.record_core_def(def_id, name.as_str(), kind, path.as_str());

        if self.build_spec.def_publicity {
            let is_public = match def {
                ModuleDef::Module(module) => module.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Function(function) => function.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Adt(adt) => adt.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Variant(variant) => variant.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Const(const_) => const_.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Static(static_) => static_.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Trait(trait_) => trait_.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::TypeAlias(alias) => alias.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Macro(mac) => mac.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::BuiltinType(_) => false,
            };
            self.host.mark_public(def_id, is_public);
            self.core_index.mark_public(def_id, is_public);
        }
        if self.build_spec.def_test_flags {
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
            self.core_index.mark_in_test(def_id, in_test || in_test_scope);
        }
        Some(def_id)
    }

    fn record_core_def(&mut self, def_id: DefId, name: &str, kind: DefKind, path: &str) {
        self.def_path_by_id.insert(def_id, path.to_owned());
        self.core_index.record_def(def_id, name, kind, path);
    }

    fn module_is_test(&self, module: Module) -> bool {
        module.path_to_root(self.db).into_iter().any(|m| {
            m.name(self.db)
                .is_some_and(|name| name.display(self.db, Edition::CURRENT).to_string() == "tests")
        })
    }
}

struct CoreHostArtifacts {
    host: DeterministicRaHost,
    index: CoreLookupIndex,
}

fn build_core_host(
    analysis_host: &AnalysisHost,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    tracked_files: &BTreeSet<PathBuf>,
    workspace_epoch: u64,
    content_revision: u64,
    build_spec: &CoreHostBuildSpec,
) -> Result<CoreHostArtifacts, RaHostInitError> {
    // TODO(ra-native-audit): this still rebuilds a whole deterministic host snapshot after hot-path
    // invalidation. Replace with narrower incremental reconciliation where possible.
    let build_started = Instant::now();
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
        build_spec: build_spec.clone(),
        files,
        host,
        core_index: CoreLookupIndex::default(),
        def_path_by_id: BTreeMap::new(),
        local_adts: Vec::new(),
        local_traits: Vec::new(),
        local_functions: Vec::new(),
    };
    builder.populate(build_spec);
    trace_timing("workspace_service.build_core_host.total", build_started.elapsed());
    Ok(CoreHostArtifacts {
        host: builder.host,
        index: builder.core_index,
    })
}

fn trace_timing(label: &str, elapsed: std::time::Duration) {
    if std::env::var_os("RAQL_TRACE_TIMINGS").is_none() {
        return;
    }
    eprintln!("raql-timing {label} {}ms", elapsed.as_millis());
}

fn trace_lookup_timing(request: &ExternLookupRequest, elapsed: std::time::Duration) {
    if std::env::var_os("RAQL_TRACE_TIMINGS").is_none() {
        return;
    }
    eprintln!(
        "raql-timing extern_lookup {} {:?} bound={} {}ms",
        request.predicate(),
        request.shape(),
        request.bound_positions().len(),
        elapsed.as_millis()
    );
}

fn tracked_workspace_state(
    _db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    watched_entries: &[vfs::loader::Entry],
    manifest_path: &Path,
    workspace_root: &Path,
    local_tracking_roots: &[PathBuf],
) -> Result<BTreeSet<PathBuf>, RaHostInitError> {
    let mut tracked_files = vfs
        .iter()
        .filter_map(|(_, path)| path.as_path().map(|abs| {
            let path: &Path = abs.as_ref();
            path.to_path_buf()
        }))
        .filter(|path| is_local_workspace_file(path.as_path()) && path_is_in_tracking_roots(path, local_tracking_roots))
        .collect::<BTreeSet<_>>();
    tracked_files.extend(explicit_watched_files(watched_entries));
    tracked_files.extend(workspace_watch_files(manifest_path, workspace_root));
    Ok(tracked_files)
}

fn normalize_type_ast(ty: ast::Type) -> ast::Type {
    match ty {
        ast::Type::ParenType(paren) => paren.ty().map(normalize_type_ast).unwrap_or(ast::Type::ParenType(paren)),
        other => other,
    }
}

fn map_mutability(mutability: hir::Mutability) -> Mutability {
    match mutability {
        hir::Mutability::Mut => Mutability::Mut,
        hir::Mutability::Shared => Mutability::Shared,
    }
}

fn ref_inner_type(ty: &ast::Type) -> Option<ast::Type> {
    match ty {
        ast::Type::RefType(inner) => inner.ty().map(normalize_type_ast),
        _ => None,
    }
}

fn ptr_parts(ty: &ast::Type) -> (Mutability, Option<ast::Type>) {
    match ty {
        ast::Type::PtrType(ptr) => (
            if ptr.mut_token().is_some() {
                Mutability::Mut
            } else {
                Mutability::Shared
            },
            ptr.ty().map(normalize_type_ast),
        ),
        _ => (Mutability::Shared, None),
    }
}

fn tuple_item_asts(ty: &ast::Type) -> Vec<ast::Type> {
    match ty {
        ast::Type::TupleType(tuple) => tuple.fields().map(normalize_type_ast).collect(),
        _ => Vec::new(),
    }
}

fn slice_inner_type(ty: &ast::Type) -> Option<ast::Type> {
    match ty {
        ast::Type::SliceType(slice) => slice.ty().map(normalize_type_ast),
        _ => None,
    }
}

fn path_type_arg_asts(ty: &ast::Type) -> Vec<ast::Type> {
    match ty {
        ast::Type::PathType(path_ty) => path_ty
            .path()
            .and_then(|path| path.segment())
            .and_then(|segment| segment.generic_arg_list())
            .map(|args| {
                args.generic_args()
                    .filter_map(|arg| match arg {
                        ast::GenericArg::TypeArg(ty_arg) => ty_arg.ty().map(normalize_type_ast),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn belongs_to_item(node: &syntax::SyntaxNode, owner_item: &syntax::SyntaxNode) -> bool {
    node.ancestors()
        .find_map(ast::Item::cast)
        .is_some_and(|item| item.syntax() == owner_item)
}

fn explicit_watched_files(watched_entries: &[vfs::loader::Entry]) -> BTreeSet<PathBuf> {
    let mut files = BTreeSet::new();
    for entry in watched_entries {
        if let vfs::loader::Entry::Files(paths) = entry {
            for path in paths {
                let path: &Path = path.as_ref();
                let path = path.to_path_buf();
                if path.exists() {
                    files.insert(path);
                }
            }
        }
    }
    files
}

fn path_is_in_tracking_roots(path: &Path, local_tracking_roots: &[PathBuf]) -> bool {
    local_tracking_roots.iter().any(|root| path.starts_with(root))
}

fn workspace_watch_files(manifest_path: &Path, workspace_root: &Path) -> BTreeSet<PathBuf> {
    [
        manifest_path.to_path_buf(),
        workspace_root.join("Cargo.toml"),
        workspace_root.join("Cargo.lock"),
        workspace_root.join("rust-toolchain"),
        workspace_root.join("rust-toolchain.toml"),
        workspace_root.join(".cargo").join("config.toml"),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .collect()
}

fn tracked_directory_watch_set(
    tracked_files: &BTreeSet<PathBuf>,
    manifest_path: &Path,
    workspace_root: &Path,
) -> BTreeSet<PathBuf> {
    let mut dirs = BTreeSet::new();
    dirs.insert(workspace_root.to_path_buf());
    if let Some(parent) = manifest_path.parent() {
        dirs.insert(parent.to_path_buf());
    }
    for path in tracked_files {
        if let Some(parent) = path.parent() {
            dirs.insert(parent.to_path_buf());
        }
    }
    dirs
}

fn build_script_rerun_paths(
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, RaHostInitError> {
    // TODO(ra-native-audit): this is incomplete for env-driven and conditional build-script inputs.
    // Replace with tighter watched-input truth before trusting warm-daemon invalidation here.
    let mut watched = BTreeSet::new();
    for build_script in tracked_files
        .iter()
        .filter(|path| path.file_name().and_then(|value| value.to_str()) == Some("build.rs"))
    {
        watched.insert(build_script.clone());
        let package_root = build_script
            .parent()
            .ok_or_else(|| RaHostInitError::WorkspaceLoad {
                manifest: build_script.display().to_string(),
                details: "build.rs path has no parent directory".to_string(),
            })?;
        let declared = declared_rerun_if_changed_paths(build_script.as_path())?;
        if declared.is_empty() {
            watched.insert(package_root.to_path_buf());
            scan_all_files(package_root, &mut watched)?;
            continue;
        }
        for declared_path in declared {
            let watched_path = package_root.join(&declared_path);
            watched.insert(watched_path.clone());
            if watched_path.is_dir() {
                scan_all_files(watched_path.as_path(), &mut watched)?;
            }
        }
    }
    Ok(watched)
}

fn auxiliary_build_input_state(
    watched_paths: &BTreeSet<PathBuf>,
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<BTreeMap<PathBuf, Option<WatchedFileState>>, RaHostInitError> {
    let mut files = BTreeMap::new();
    for path in watched_paths {
        if tracked_files.contains(path) {
            continue;
        }
        files.insert(path.clone(), watched_path_state(path.as_path())?);
    }
    Ok(files)
}

fn tracked_file_state_map(
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<BTreeMap<PathBuf, WatchedFileState>, RaHostInitError> {
    let mut states = BTreeMap::new();
    for path in tracked_files {
        states.insert(path.clone(), watched_file_state(path)?);
    }
    Ok(states)
}

fn scan_all_files(dir: &Path, out: &mut BTreeSet<PathBuf>) -> Result<(), RaHostInitError> {
    // TODO(ra-native-audit): avoid recursive package-root scans for build inputs once watched
    // inputs are derived from a more authoritative source.
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
            out.insert(path.clone());
            scan_all_files(path.as_path(), out)?;
            continue;
        }
        out.insert(path);
    }
    Ok(())
}

fn watched_path_state(path: &Path) -> Result<Option<WatchedFileState>, RaHostInitError> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(watched_file_state(path)?))
}

fn watched_file_state(path: &Path) -> Result<WatchedFileState, RaHostInitError> {
    let metadata = fs::metadata(path).map_err(|err| RaHostInitError::WorkspaceLoad {
        manifest: path.display().to_string(),
        details: err.to_string(),
    })?;
    let modified_unix_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    Ok(WatchedFileState {
        len: metadata.len(),
        modified_unix_nanos,
    })
}

fn declared_rerun_if_changed_paths(build_script: &Path) -> Result<Vec<PathBuf>, RaHostInitError> {
    let text = fs::read_to_string(build_script).map_err(|err| RaHostInitError::WorkspaceLoad {
        manifest: build_script.display().to_string(),
        details: err.to_string(),
    })?;
    let mut paths = Vec::new();
    for segment in text.split("cargo:rerun-if-changed=").skip(1) {
        let candidate = segment
            .chars()
            .take_while(|ch| !matches!(ch, '"' | '\n' | '\r'))
            .collect::<String>();
        let candidate = candidate.trim();
        if candidate.is_empty() || candidate.contains('{') || candidate.contains('}') {
            continue;
        }
        paths.push(PathBuf::from(candidate));
    }
    Ok(paths)
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

fn path_requires_reload(path: &Path, build_script_rerun_paths: &BTreeSet<PathBuf>) -> bool {
    if path.file_name().and_then(|value| value.to_str()) == Some("build.rs") {
        return true;
    }
    if build_script_rerun_paths.contains(path) {
        return true;
    }
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
