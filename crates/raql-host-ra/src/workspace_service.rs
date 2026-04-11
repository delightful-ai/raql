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
use ide::{AnalysisHost, LineIndex};
use ide_db::symbol_index::{Query, world_symbols};
use project_model::ProjectManifest;
use raql_compiler::{PlannedProgram, required_extern_capabilities};
use raql_engine::{EvalResult, execute};
use raql_host::{
    CapabilityId, CapabilitySet, ExternLookupHostValue, ExternLookupHostValueKind,
    ExternLookupRequest, ExternLookupShape, ExternLookupValue, MissingCapabilitiesError,
    SpanCoord, SpanKey, is_engine_managed_extern, is_runtime_scalar_input_predicate,
};
use hir::import_map::AssocSearchMode;
use syntax::ast::{HasGenericArgs, HasModuleItem, HasName};
use syntax::{ast, AstNode, Edition};
use vfs::{AbsPathBuf, VfsPath};

use crate::capability::{day_one_supported_capabilities, supports_day_one_capability};
use crate::lazy_runtime::LazyRaRuntime;
use crate::workspace_loader;
use crate::{
    DefId, DefKind, DeterministicRaHost, GenericArg, Mutability, NodeId, NodeKind,
    RaHostInitError, SpanId, TypeShape, WorldStamp,
};
use raql_engine::{EngineHostView, HostValueKind, RuntimeValue};

#[derive(Debug)]
pub struct WorkspaceService {
    manifest_path: PathBuf,
    workspace_root: PathBuf,
    analysis_host: AnalysisHost,
    vfs: vfs::Vfs,
    _proc_macro_client: Option<Box<dyn workspace_loader::ProcMacroClientHandle>>,
    core_host: Option<DeterministicRaHost>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum RaEntity {
    ModuleDef(ModuleDef),
}

impl RaEntity {
    fn as_function(&self) -> Option<hir::Function> {
        match self {
            Self::ModuleDef(ModuleDef::Function(function)) => Some(*function),
            _ => None,
        }
    }

    fn canonical_path(&self, db: &ide::RootDatabase) -> Option<String> {
        match self {
            Self::ModuleDef(ModuleDef::Function(function)) => Some(canonical_function_path(db, *function)),
            Self::ModuleDef(def) => def.canonical_path(db, Edition::CURRENT),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RaSpan {
    file_id: span::EditionedFileId,
    range: syntax::TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LookupDefRecord {
    name: Box<str>,
    kind: DefKind,
    span: SpanId,
    path: Option<Box<str>>,
    entity: Option<RaEntity>,
    ra_span: Option<RaSpan>,
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

    pub(crate) fn extern_lookup_rows(
        &mut self,
        request: &ExternLookupRequest,
    ) -> Result<Option<Vec<Vec<ExternLookupValue>>>, RaHostInitError> {
        let lookup_started = Instant::now();
        let result = match (request.predicate(), request.shape()) {
            ("def_name", ExternLookupShape::FunctionExactBindings) => {
                self.lookup_def_name_rows(request).map(Some)
            }
            ("def", ExternLookupShape::RelationExactBindings) => {
                Ok(Some(self.lookup_def_rows(request)))
            }
            ("def_kind", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(self.lookup_def_kind_rows(request)))
            }
            ("def_span", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(self.lookup_def_span_rows(request)))
            }
            ("def_path", ExternLookupShape::FunctionExactBindings) => self.lookup_def_path_rows(request),
            ("call_edge", ExternLookupShape::RelationExactBindings) => {
                self.lookup_call_edge_rows(request)
            }
            ("span_allowed", ExternLookupShape::RelationExactBindings) => {
                Ok(Some(self.lookup_span_allowed_rows(request)))
            }
            ("span_key", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(self.lookup_span_key_rows(request)))
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

    fn lookup_def_name_rows(
        &mut self,
        request: &ExternLookupRequest,
    ) -> Result<Vec<Vec<ExternLookupValue>>, RaHostInitError> {
        // TODO(ra-native-audit): exact-name lookup still falls back to tracked-file text scans,
        // path-prefix crate guessing, and RA AST walks over candidate files. Replace this with an
        // RA/HIR-native exact-name provider so cold def seeds stop depending on raw filesystem
        // reads and ad hoc candidate narrowing.
        let mut requested_name = None::<&str>;
        let mut def_filter = None::<DefId>;
        for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
            match (*idx, value) {
                (0, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Def =>
                {
                    def_filter = Some(DefId::new(host.stable_id()));
                }
                (1, ExternLookupValue::String(name)) => requested_name = Some(name.as_ref()),
                _ => return Ok(Vec::new()),
            }
        }
        if let Some(def) = def_filter {
            if let Some(record) = self.lookup_defs.get(&def) {
                if requested_name.is_some_and(|expected| expected != record.name.as_ref()) {
                    return Ok(Vec::new());
                }
                return Ok(vec![vec![
                    ExternLookupValue::Host(ExternLookupHostValue::new(
                        ExternLookupHostValueKind::Def,
                        def.stable_id(),
                    )),
                    ExternLookupValue::String(record.name.clone()),
                ]]);
            }
            if let Some(name) = self.lookup_bound_def_name_from_core_host(def) {
                if requested_name.is_some_and(|expected| expected != name.as_ref()) {
                    return Ok(Vec::new());
                }
                return Ok(vec![vec![
                    ExternLookupValue::Host(ExternLookupHostValue::new(
                        ExternLookupHostValueKind::Def,
                        def.stable_id(),
                    )),
                    ExternLookupValue::String(name),
                ]]);
            }
        }
        let Some(requested_name) = requested_name else {
            return Ok(Vec::new());
        };

        fn push_named_ast_def(
            rows: &mut BTreeSet<Vec<ExternLookupValue>>,
            id_host: &mut DeterministicRaHost,
            lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
            lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
            requested_name: &str,
            def_filter: Option<DefId>,
            rel_path: &str,
            file_id: span::EditionedFileId,
            source_text: &str,
            kind: DefKind,
            name: &str,
            range: syntax::TextRange,
            path: Option<Box<str>>,
            entity: Option<RaEntity>,
        ) {
            if name != requested_name {
                return;
            }
            let Some(span_key) = lookup_span_key_from_text(rel_path, source_text, range) else {
                return;
            };
            let Ok(span) = id_host.intern_span_from_text(file_id, rel_path, source_text, range)
            else {
                return;
            };
            let token = format!(
                "lookup_def:{kind:?}:{rel_path}:{}..{}",
                u32::from(range.start()),
                u32::from(range.end())
            );
            let def_id = id_host.intern_def_from_token(token.as_str());
            if def_filter.is_some_and(|expected| expected != def_id) {
                return;
            }
            lookup_defs.insert(
                def_id,
                LookupDefRecord {
                    name: name.to_string().into_boxed_str(),
                    kind,
                    span,
                    path,
                    entity,
                    ra_span: Some(RaSpan { file_id, range }),
                },
            );
            lookup_spans.insert(span, span_key);
            rows.insert(vec![
                ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Def,
                    def_id.stable_id(),
                )),
                ExternLookupValue::String(name.to_string().into_boxed_str()),
            ]);
        }

        fn push_named_function_def(
            sema: &hir::Semantics<'_, ide::RootDatabase>,
            rows: &mut BTreeSet<Vec<ExternLookupValue>>,
            id_host: &mut DeterministicRaHost,
            lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
            lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
            requested_name: &str,
            def_filter: Option<DefId>,
            rel_path: &str,
            file_id: span::EditionedFileId,
            source_text: &str,
            kind: DefKind,
            function_item: ast::Fn,
        ) {
            let Some(name) = function_item.name() else {
                return;
            };
            if name.text().as_str() != requested_name {
                return;
            }
            let entity = sema.to_def(&function_item).map(|function| RaEntity::ModuleDef(ModuleDef::Function(function)));
            push_named_ast_def(
                rows,
                id_host,
                lookup_defs,
                lookup_spans,
                requested_name,
                def_filter,
                rel_path,
                file_id,
                source_text,
                kind,
                name.text().as_str(),
                function_item.syntax().text_range(),
                None,
                entity,
            );
        }

        fn collect_named_assoc_defs(
            sema: &hir::Semantics<'_, ide::RootDatabase>,
            item: ast::AssocItem,
            rows: &mut BTreeSet<Vec<ExternLookupValue>>,
            id_host: &mut DeterministicRaHost,
            lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
            lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
            requested_name: &str,
            def_filter: Option<DefId>,
            rel_path: &str,
            file_id: span::EditionedFileId,
            source_text: &str,
        ) {
            if let ast::AssocItem::Fn(function_item) = item {
                push_named_function_def(
                    sema,
                    rows,
                    id_host,
                    lookup_defs,
                    lookup_spans,
                    requested_name,
                    def_filter,
                    rel_path,
                    file_id,
                    source_text,
                    DefKind::Method,
                    function_item,
                );
            }
        }

        fn collect_named_defs_from_item(
            sema: &hir::Semantics<'_, ide::RootDatabase>,
            item: ast::Item,
            rows: &mut BTreeSet<Vec<ExternLookupValue>>,
            id_host: &mut DeterministicRaHost,
            lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
            lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
            requested_name: &str,
            def_filter: Option<DefId>,
            rel_path: &str,
            file_id: span::EditionedFileId,
            source_text: &str,
        ) {
            match item {
                ast::Item::Fn(function_item) => {
                    push_named_function_def(
                        sema,
                        rows,
                        id_host,
                        lookup_defs,
                        lookup_spans,
                        requested_name,
                        def_filter,
                        rel_path,
                        file_id,
                        source_text,
                        DefKind::Fn,
                        function_item,
                    );
                }
                ast::Item::Struct(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::Struct,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                }
                ast::Item::Union(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::Union,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                }
                ast::Item::Enum(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::Enum,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                    if let Some(variant_list) = it.variant_list() {
                        for variant in variant_list.variants() {
                            if let Some(name) = variant.name() {
                                push_named_ast_def(
                                    rows,
                                    id_host,
                                    lookup_defs,
                                    lookup_spans,
                                    requested_name,
                                    def_filter,
                                    rel_path,
                                    file_id,
                                    source_text,
                                    DefKind::Variant,
                                    name.text().as_str(),
                                    variant.syntax().text_range(),
                                    None,
                                    None,
                                );
                            }
                        }
                    }
                }
                ast::Item::Trait(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::Trait,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                    if let Some(item_list) = it.assoc_item_list() {
                        for child in item_list.assoc_items() {
                            collect_named_assoc_defs(
                                sema,
                                child,
                                rows,
                                id_host,
                                lookup_defs,
                                lookup_spans,
                                requested_name,
                                def_filter,
                                rel_path,
                                file_id,
                                source_text,
                            );
                        }
                    }
                }
                ast::Item::Module(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::Mod,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                    if let Some(item_list) = it.item_list() {
                        for child in item_list.items() {
                            collect_named_defs_from_item(
                                sema,
                                child,
                                rows,
                                id_host,
                                lookup_defs,
                                lookup_spans,
                                requested_name,
                                def_filter,
                                rel_path,
                                file_id,
                                source_text,
                            );
                        }
                    }
                }
                ast::Item::Impl(it) => {
                    if let Some(item_list) = it.assoc_item_list() {
                        for child in item_list.assoc_items() {
                            collect_named_assoc_defs(
                                sema,
                                child,
                                rows,
                                id_host,
                                lookup_defs,
                                lookup_spans,
                                requested_name,
                                def_filter,
                                rel_path,
                                file_id,
                                source_text,
                            );
                        }
                    }
                }
                ast::Item::MacroRules(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::Macro,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                }
                ast::Item::MacroDef(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::Macro,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                }
                ast::Item::TypeAlias(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::TypeAlias,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                }
                ast::Item::Const(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::Const,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                }
                ast::Item::Static(it) => {
                    if let Some(name) = it.name() {
                        push_named_ast_def(
                            rows,
                            id_host,
                            lookup_defs,
                            lookup_spans,
                            requested_name,
                            def_filter,
                            rel_path,
                            file_id,
                            source_text,
                            DefKind::Static,
                            name.text().as_str(),
                            it.syntax().text_range(),
                            None,
                            None,
                        );
                    }
                }
                _ => {}
            }
        }

        fn collect_named_functions_from_module(
            db: &ide::RootDatabase,
            vfs: &vfs::Vfs,
            workspace_root: &Path,
            module: hir::Module,
            target_file: vfs::FileId,
            requested_name: &str,
            def_filter: Option<DefId>,
            rows: &mut BTreeSet<Vec<ExternLookupValue>>,
            id_host: &mut DeterministicRaHost,
            lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
            lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        ) {
            let query_name =
                ide_db::imports::import_assets::NameToImport::Exact(requested_name.to_string(), true);
            let _ = ide_db::items_locator::items_with_name_in_module(
                db,
                module,
                query_name,
                ide_db::items_locator::AssocSearchMode::Include,
                |item| {
                    let ModuleDef::Function(function) = item.into_module_def() else {
                        return std::ops::ControlFlow::<()>::Continue(());
                    };
                    let Some(source) = function.source(db) else {
                        return std::ops::ControlFlow::<()>::Continue(());
                    };
                    let editioned = source.file_id.original_file(db);
                    if editioned.file_id(db) != target_file {
                        return std::ops::ControlFlow::<()>::Continue(());
                    }
                    let Some(def_id) = WorkspaceService::ensure_lookup_function_def(
                        db,
                        vfs,
                        workspace_root,
                        lookup_defs,
                        lookup_spans,
                        id_host,
                        function,
                    ) else {
                        return std::ops::ControlFlow::<()>::Continue(());
                    };
                    if def_filter.is_some_and(|expected| expected != def_id) {
                        return std::ops::ControlFlow::<()>::Continue(());
                    }
                    rows.insert(vec![
                        ExternLookupValue::Host(ExternLookupHostValue::new(
                            ExternLookupHostValueKind::Def,
                            def_id.stable_id(),
                        )),
                        ExternLookupValue::String(requested_name.to_string().into_boxed_str()),
                    ]);
                    std::ops::ControlFlow::<()>::Continue(())
                },
            );
        }

        let workspace_root = self.workspace_root.clone();
        let vfs = &self.vfs;
        let lookup_defs = &mut self.lookup_defs;
        let lookup_spans = &mut self.lookup_spans;
        let db = self.analysis_host.raw_database();
        let symbol_lookup_started = Instant::now();
        let mut symbol_id_host = DeterministicRaHost::new();
        let mut symbol_rows = BTreeSet::<Vec<ExternLookupValue>>::new();
        let mut collect_symbol_rows = |mut query: Query, include_functions: bool| {
            query.exact();
            query.exclude_imports();
            for symbol in world_symbols(db, query) {
                if symbol.is_alias || symbol.is_import {
                    continue;
                }
                let def = symbol.def;
                if matches!(def, ModuleDef::Function(_)) != include_functions {
                    continue;
                }
                let Some(module) = def.module(db) else {
                    continue;
                };
                if !module.krate(db).origin(db).is_local() {
                    continue;
                }
                let original = symbol.loc.hir_file_id.original_file_respecting_includes(db);
                let Some(def_id) = WorkspaceService::ensure_lookup_symbol_module_def(
                    db,
                    vfs,
                    workspace_root.as_path(),
                    lookup_defs,
                    lookup_spans,
                    &mut symbol_id_host,
                    def,
                    original.editioned_file_id(db),
                    symbol.loc.ptr.text_range(),
                ) else {
                    continue;
                };
                if def_filter.is_some_and(|expected| expected != def_id) {
                    continue;
                }
                symbol_rows.insert(vec![
                    ExternLookupValue::Host(ExternLookupHostValue::new(
                        ExternLookupHostValueKind::Def,
                        def_id.stable_id(),
                    )),
                    ExternLookupValue::String(requested_name.to_string().into_boxed_str()),
                ]);
            }
        };
        collect_symbol_rows(Query::new(requested_name.to_string()), false);
        collect_symbol_rows(Query::new(format!("{requested_name}#")), true);
        trace_timing(
            "workspace_service.lookup_def_name_rows.resolve_function_symbols",
            symbol_lookup_started.elapsed(),
        );
        if !symbol_rows.is_empty() {
            return Ok(symbol_rows.into_iter().collect());
        }
        let scan_started = Instant::now();
        let mut candidate_files = Vec::<(PathBuf, String)>::new();
        for path in &self.tracked_files {
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let Ok(text) = fs::read_to_string(path) else {
                continue;
            };
            if text.contains(requested_name) {
                candidate_files.push((path.clone(), text));
            }
        }
        trace_timing(
            "workspace_service.lookup_def_name_rows.scan_candidates",
            scan_started.elapsed(),
        );
        let resolve_started = Instant::now();
        let mut ast_fallback_total = std::time::Duration::ZERO;
        let mut id_host = DeterministicRaHost::new();
        let mut rows = BTreeSet::<Vec<ExternLookupValue>>::new();
        hir::attach_db(db, || {
            let sema = hir::Semantics::new(db);
            for (path, text) in &candidate_files {
                let rel_path = path
                    .strip_prefix(workspace_root.as_path())
                    .unwrap_or(path.as_path())
                    .display()
                    .to_string();
                let Ok(utf8) = Utf8PathBuf::from_path_buf(path.clone()) else {
                    continue;
                };
                let abs = AbsPathBuf::assert(utf8);
                let vfs_path = VfsPath::from(abs);
                let Some((file_id, excluded)) = vfs.file_id(&vfs_path) else {
                    continue;
                };
                if matches!(excluded, vfs::FileExcluded::Yes) {
                    continue;
                }
                let ast_fallback_started = Instant::now();
                let editioned = base_db::EditionedFileId::current_edition_guess_origin(db, file_id);
                let source = sema.parse(editioned);
                for item in source.items() {
                    collect_named_defs_from_item(
                        &sema,
                        item,
                        &mut rows,
                        &mut id_host,
                        lookup_defs,
                        lookup_spans,
                        requested_name,
                        def_filter,
                        rel_path.as_str(),
                        editioned.editioned_file_id(db),
                        text.as_str(),
                    );
                }
                ast_fallback_total += ast_fallback_started.elapsed();
            }
        });
        if std::env::var_os("RAQL_TRACE_TIMINGS").is_some() {
            eprintln!(
                "raql-timing workspace_service.lookup_def_name_rows.resolve_candidates_parts ast_fallback_ms={}",
                ast_fallback_total.as_millis(),
            );
        }
        trace_timing(
            "workspace_service.lookup_def_name_rows.resolve_candidates",
            resolve_started.elapsed(),
        );
        if !rows.is_empty() {
            return Ok(rows.into_iter().collect());
        }

        let fallback_started = Instant::now();
        let mut fallback_hir_ms = std::time::Duration::ZERO;
        hir::attach_db(db, || {
            for (path, _) in &candidate_files {
                let Ok(utf8) = Utf8PathBuf::from_path_buf(path.clone()) else {
                    continue;
                };
                let abs = AbsPathBuf::assert(utf8);
                let vfs_path = VfsPath::from(abs);
                let Some((file_id, excluded)) = vfs.file_id(&vfs_path) else {
                    continue;
                };
                if matches!(excluded, vfs::FileExcluded::Yes) {
                    continue;
                }
                let started = Instant::now();
                for module in hir::Semantics::new(db).file_to_module_defs(file_id) {
                    collect_named_functions_from_module(
                        db,
                        vfs,
                        workspace_root.as_path(),
                        module,
                        file_id,
                        requested_name,
                        def_filter,
                        &mut rows,
                        &mut id_host,
                        lookup_defs,
                        lookup_spans,
                    );
                }
                fallback_hir_ms += started.elapsed();
                if !rows.is_empty() {
                    break;
                }
            }
        });
        if std::env::var_os("RAQL_TRACE_TIMINGS").is_some() {
            eprintln!(
                "raql-timing workspace_service.lookup_def_name_rows.hir_fallback hir_ms={}",
                fallback_hir_ms.as_millis(),
            );
        }
        trace_timing(
            "workspace_service.lookup_def_name_rows.hir_fallback.total",
            fallback_started.elapsed(),
        );
        if !rows.is_empty() {
            return Ok(rows.into_iter().collect());
        }

        Ok(Vec::new())
    }

    fn lookup_bound_def_name_from_core_host(&mut self, def: DefId) -> Option<Box<str>> {
        let rows = EngineHostView::extern_relation_rows(self.core_host.as_mut()?, "def_name")
            .ok()
            .flatten()?;
        rows.into_iter().find_map(|row| match row.as_slice() {
            [
                RuntimeValue::Host {
                    kind: HostValueKind::Def,
                    id,
                },
                RuntimeValue::String(name),
            ] if *id == def.stable_id().as_u64() => Some(name.clone().into_boxed_str()),
            _ => None,
        })
    }

    fn lookup_def_path_rows(
        &mut self,
        request: &ExternLookupRequest,
    ) -> Result<Option<Vec<Vec<ExternLookupValue>>>, RaHostInitError> {
        let lookup_started = Instant::now();
        let mut def = None::<DefId>;
        let mut path_filter = None::<&str>;
        for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
            match (*idx, value) {
                (0, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Def =>
                {
                    def = Some(DefId::new(host.stable_id()));
                }
                (1, ExternLookupValue::String(path)) => path_filter = Some(path.as_ref()),
                _ => return Ok(None),
            }
        }
        let Some(def) = def else {
            return Ok(None);
        };
        let Some(mut record) = self.lookup_defs.get(&def).cloned() else {
            return Ok(None);
        };
        if record.path.is_none() {
            let db = self.analysis_host.raw_database();
            hir::attach_db(db, || {
                if let Some(function) = record
                    .entity
                    .as_ref()
                    .and_then(RaEntity::as_function)
                {
                    record.path = Some(canonical_function_path(db, function).into_boxed_str());
                } else if let Some(path) = record.entity.as_ref().and_then(|entity| entity.canonical_path(db)) {
                    record.path = Some(path.into_boxed_str());
                }
            });
            if let Some(entry) = self.lookup_defs.get_mut(&def) {
                if entry.path.is_none() {
                    entry.path = record.path.clone();
                }
                if entry.entity.is_none() {
                    entry.entity = record.entity.clone();
                }
            }
        }
        let Some(path) = record.path.as_ref() else {
            return Ok(None);
        };
        if path_filter.is_some_and(|expected| expected != path.as_ref()) {
            trace_timing(
                "workspace_service.lookup_def_path_rows.total",
                lookup_started.elapsed(),
            );
            return Ok(Some(Vec::new()));
        }
        let rows = Some(vec![vec![
            ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                def.stable_id(),
            )),
            ExternLookupValue::String(path.clone()),
        ]]);
        trace_timing(
            "workspace_service.lookup_def_path_rows.total",
            lookup_started.elapsed(),
        );
        Ok(rows)
    }

    fn lookup_call_edge_rows(
        &mut self,
        request: &ExternLookupRequest,
    ) -> Result<Option<Vec<Vec<ExternLookupValue>>>, RaHostInitError> {
        let lookup_started = Instant::now();
        let mut caller_filter = None::<DefId>;
        let mut callee_filter = None::<DefId>;
        let mut site_filter = None::<SpanId>;
        let mut dispatch_filter = None::<crate::DispatchKind>;
        for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
            match (*idx, value) {
                (0, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Def =>
                {
                    caller_filter = Some(DefId::new(host.stable_id()));
                }
                (1, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Def =>
                {
                    callee_filter = Some(DefId::new(host.stable_id()));
                }
                (2, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Span =>
                {
                    site_filter = Some(SpanId::new(host.stable_id()));
                }
                (3, ExternLookupValue::Enum { name, variant })
                    if name.as_ref() == "DispatchKind" =>
                {
                    let Some(dispatch) = dispatch_kind_from_variant(variant.as_ref()) else {
                        return Ok(Some(Vec::new()));
                    };
                    dispatch_filter = Some(dispatch);
                }
                _ => return Ok(None),
            }
        }
        if caller_filter.is_none() && callee_filter.is_none() {
            return Ok(None);
        }

        let mut id_host = DeterministicRaHost::new();
        let mut rows = BTreeSet::<Vec<ExternLookupValue>>::new();
        let mut unsupported = false;
        let mut lookup_defs = self.lookup_defs.clone();
        let mut lookup_spans = self.lookup_spans.clone();
        let db = self.analysis_host.raw_database();
        let mut lookup_error = None::<Option<Vec<Vec<ExternLookupValue>>>>;
        hir::attach_db(db, || {
            let sema = hir::Semantics::new(db);
            if let Some(caller) = caller_filter {
                let Some(record) = lookup_defs.get(&caller).cloned() else {
                    lookup_error = Some(None);
                    return;
                };
                let Some(function) = record
                    .entity
                    .as_ref()
                    .and_then(RaEntity::as_function)
                else {
                    lookup_error = Some(Some(Vec::new()));
                    return;
                };
                if let Some(entry) = lookup_defs.get_mut(&caller) {
                    entry.entity = Some(RaEntity::ModuleDef(ModuleDef::Function(function)));
                }
                if !Self::collect_lookup_call_edges_for_function(
                    db,
                    &self.vfs,
                    self.workspace_root.as_path(),
                    &sema,
                    &mut lookup_defs,
                    &mut lookup_spans,
                    &mut id_host,
                    &mut rows,
                    caller,
                    function,
                    callee_filter,
                    site_filter,
                    dispatch_filter,
                ) {
                    unsupported = true;
                }
            } else if let Some(callee) = callee_filter {
                let Some(record) = lookup_defs.get(&callee).cloned() else {
                    lookup_error = Some(None);
                    return;
                };
                let collect_started = Instant::now();
                let resolve_function_started = Instant::now();
                let resolved = record
                    .entity
                    .as_ref()
                    .and_then(RaEntity::as_function);
                let resolve_function_elapsed = resolve_function_started.elapsed();
                if let Some(resolved) = resolved {
                    if let Some(entry) = lookup_defs.get_mut(&callee) {
                        entry.entity = Some(RaEntity::ModuleDef(ModuleDef::Function(resolved)));
                    }
                    let collect_callers_started = Instant::now();
                    if !Self::collect_lookup_callers_for_function(
                        db,
                        &self.vfs,
                        self.workspace_root.as_path(),
                        &sema,
                        &mut lookup_defs,
                        &mut lookup_spans,
                        &mut id_host,
                        &mut rows,
                        resolved,
                        caller_filter,
                        callee,
                        site_filter,
                        dispatch_filter,
                    ) {
                        unsupported = true;
                    }
                    if std::env::var_os("RAQL_TRACE_TIMINGS").is_some() {
                        eprintln!(
                            "raql-timing workspace_service.lookup_call_edge_rows.callee_parts resolve_function_ms={} collect_callers_ms={}",
                            resolve_function_elapsed.as_millis(),
                            collect_callers_started.elapsed().as_millis(),
                        );
                    }
                } else {
                    lookup_error = Some(Some(Vec::new()));
                    return;
                }
                trace_timing(
                    "workspace_service.lookup_call_edge_rows.callee_collect",
                    collect_started.elapsed(),
                );
            }
        });

        if let Some(result) = lookup_error {
            trace_timing(
                "workspace_service.lookup_call_edge_rows.total",
                lookup_started.elapsed(),
            );
            return Ok(result);
        }

        if unsupported {
            trace_timing(
                "workspace_service.lookup_call_edge_rows.total",
                lookup_started.elapsed(),
            );
            return Ok(None);
        }
        self.lookup_defs = lookup_defs;
        self.lookup_spans = lookup_spans;
        let rows = Some(rows.into_iter().collect());
        trace_timing(
            "workspace_service.lookup_call_edge_rows.total",
            lookup_started.elapsed(),
        );
        Ok(rows)
    }

    fn collect_lookup_call_edges_for_function(
        db: &ide::RootDatabase,
        vfs: &vfs::Vfs,
        workspace_root: &Path,
        sema: &hir::Semantics<'_, ide::RootDatabase>,
        lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        rows: &mut BTreeSet<Vec<ExternLookupValue>>,
        caller_def: DefId,
        function: hir::Function,
        callee_filter: Option<DefId>,
        site_filter: Option<SpanId>,
        dispatch_filter: Option<crate::DispatchKind>,
    ) -> bool {
        let Some(source) = function.source(db) else {
            return true;
        };
        let editioned = source.file_id.original_file(db);
        let Some(local) = Self::lookup_local_file(vfs, workspace_root, db, editioned.file_id(db)) else {
            return true;
        };
        let Some(body) = source.value.body() else {
            return true;
        };
        for callable in body.syntax().descendants().filter_map(ast::CallableExpr::cast) {
            if Self::lookup_callable_owner_def(
                db,
                sema,
                lookup_defs,
                lookup_spans,
                id_host,
                callable.syntax(),
                editioned.editioned_file_id(db),
                &local,
            ) != Some(caller_def)
            {
                continue;
            }
            match callable {
                ast::CallableExpr::Call(call) => {
                    let Some(callee_expr) = call.expr() else {
                        continue;
                    };
                    let Some(type_info) = sema.type_of_expr(&callee_expr) else {
                        continue;
                    };
                    let Some(callable) = type_info.original.as_callable(db) else {
                        continue;
                    };
                    let Some((callee_def, dispatch)) = Self::lookup_call_target_from_callable(
                        db,
                        vfs,
                        workspace_root,
                        sema,
                        lookup_defs,
                        lookup_spans,
                        id_host,
                        &callable,
                        &callee_expr,
                        editioned.editioned_file_id(db),
                        &local,
                    ) else {
                        return false;
                    };
                    Self::push_lookup_call_edge_row(
                        lookup_spans,
                        id_host,
                        rows,
                        caller_def,
                        callee_def,
                        editioned.editioned_file_id(db),
                        &local,
                        call.syntax().text_range(),
                        dispatch,
                        callee_filter,
                        site_filter,
                        dispatch_filter,
                    );
                }
                ast::CallableExpr::MethodCall(method_call) => {
                    let Some(callee_function) = sema.resolve_method_call(&method_call) else {
                        continue;
                    };
                    let Some(callee_def) = Self::ensure_lookup_function_def(
                        db,
                        vfs,
                        workspace_root,
                        lookup_defs,
                        lookup_spans,
                        id_host,
                        callee_function,
                    ) else {
                        return false;
                    };
                    let dispatch = method_dispatch_kind(sema, &method_call, callee_function, db);
                    Self::push_lookup_call_edge_row(
                        lookup_spans,
                        id_host,
                        rows,
                        caller_def,
                        callee_def,
                        editioned.editioned_file_id(db),
                        &local,
                        method_call.syntax().text_range(),
                        dispatch,
                        callee_filter,
                        site_filter,
                        dispatch_filter,
                    );
                }
            }
        }
        true
    }

    fn collect_lookup_callers_for_function(
        db: &ide::RootDatabase,
        vfs: &vfs::Vfs,
        workspace_root: &Path,
        sema: &hir::Semantics<'_, ide::RootDatabase>,
        lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        rows: &mut BTreeSet<Vec<ExternLookupValue>>,
        function: hir::Function,
        caller_filter: Option<DefId>,
        callee_def: DefId,
        site_filter: Option<SpanId>,
        dispatch_filter: Option<crate::DispatchKind>,
    ) -> bool {
        let function_krate = function.module(db).krate(db);
        let mut search_files = Vec::new();
        for rev_dep in function_krate.transitive_reverse_dependencies(db) {
            let root_file = rev_dep.root_file(db);
            let source_root_id = db.file_source_root(root_file).source_root_id(db);
            let source_root = db.source_root(source_root_id).source_root(db);
            if source_root.is_library {
                continue;
            }
            search_files.extend(
                source_root
                    .iter()
                    .map(|file_id| base_db::EditionedFileId::new(db, file_id, rev_dep.edition(db), rev_dep.into())),
            );
        }
        let mut usage_file_hits = 0usize;
        let mut usage_refs = 0usize;
        let mut resolved_methods = 0usize;
        let mut resolved_calls = 0usize;
        let mut pushed_rows = 0usize;
        let mut usage_total = std::time::Duration::ZERO;
        let mut resolve_total = std::time::Duration::ZERO;
        let mut caller_owner_total = std::time::Duration::ZERO;
        let mut local_total = std::time::Duration::ZERO;
        let mut alias_names = BTreeSet::<String>::new();
        let usage_started = Instant::now();
        let scope = ide_db::search::SearchScope::files(&search_files);
        let references = ide_db::defs::Definition::Function(function)
            .usages(sema)
            .in_scope(&scope)
            .all();
        usage_total += usage_started.elapsed();
        for (editioned, file_references) in references.into_iter() {
            usage_file_hits += 1;
            let file_id = editioned.file_id(db);
            let local_started = Instant::now();
            let Some(local) = Self::lookup_local_file(vfs, workspace_root, db, file_id) else {
                local_total += local_started.elapsed();
                continue;
            };
            local_total += local_started.elapsed();
            for reference in file_references {
                usage_refs += 1;
                if reference
                    .category
                    .contains(ide_db::search::ReferenceCategory::IMPORT)
                    && let Some(alias_name) = {
                        let use_tree: Option<ast::UseTree> = reference
                            .name
                            .syntax()
                            .ancestors()
                            .find_map(|node| ast::UseTree::cast(node));
                        use_tree
                            .and_then(|use_tree: ast::UseTree| use_tree.rename())
                            .and_then(|rename: ast::Rename| rename.name())
                            .map(|name: ast::Name| name.text().to_string())
                    }
                {
                    alias_names.insert(alias_name);
                }
                let Some(name_ref) = reference.name.as_name_ref().cloned() else {
                    continue;
                };
                if let Some(method_call) = name_ref
                    .syntax()
                    .ancestors()
                    .find_map(|node| ast::MethodCallExpr::cast(node))
                {
                    let resolve_started = Instant::now();
                    let Some(resolved) = sema.resolve_method_call(&method_call) else {
                        resolve_total += resolve_started.elapsed();
                        continue;
                    };
                    resolve_total += resolve_started.elapsed();
                    if resolved != function {
                        continue;
                    }
                    resolved_methods += 1;
                    let caller_owner_started = Instant::now();
                    let Some(caller_def) = Self::lookup_callable_owner_def(
                        db,
                        sema,
                        lookup_defs,
                        lookup_spans,
                        id_host,
                        method_call.syntax(),
                        editioned.editioned_file_id(db),
                        &local,
                    ) else {
                        caller_owner_total += caller_owner_started.elapsed();
                        continue;
                    };
                    caller_owner_total += caller_owner_started.elapsed();
                    Self::push_lookup_call_edge_row(
                        lookup_spans,
                        id_host,
                        rows,
                        caller_def,
                        callee_def,
                        editioned.editioned_file_id(db),
                        &local,
                        method_call.syntax().text_range(),
                        method_dispatch_kind(sema, &method_call, resolved, db),
                        caller_filter,
                        site_filter,
                        dispatch_filter,
                    );
                    pushed_rows += 1;
                    continue;
                }
                let path_segment: Option<ast::PathSegment> = name_ref
                    .syntax()
                    .ancestors()
                    .find_map(|node| ast::PathSegment::cast(node));
                let Some(path) = path_segment
                    .map(|segment: ast::PathSegment| segment.parent_path())
                else {
                    continue;
                };
                let Some(path_parent) = path.syntax().parent() else {
                    continue;
                };
                let Some(path_expr) = ast::PathExpr::cast(path_parent) else {
                    continue;
                };
                let Some(call_parent) = path_expr.syntax().parent() else {
                    continue;
                };
                let Some(call) = ast::CallExpr::cast(call_parent) else {
                    continue;
                };
                let resolve_started = Instant::now();
                let Some(resolved) = sema.resolve_path(&path).and_then(|resolved| match resolved {
                    hir::PathResolution::Def(ModuleDef::Function(resolved)) => Some(resolved),
                    _ => None,
                }) else {
                    resolve_total += resolve_started.elapsed();
                    continue;
                };
                resolve_total += resolve_started.elapsed();
                if resolved != function {
                    continue;
                }
                resolved_calls += 1;
                let caller_owner_started = Instant::now();
                let Some(caller_def) = Self::lookup_callable_owner_def(
                    db,
                    sema,
                    lookup_defs,
                    lookup_spans,
                    id_host,
                    call.syntax(),
                    editioned.editioned_file_id(db),
                    &local,
                ) else {
                    caller_owner_total += caller_owner_started.elapsed();
                    continue;
                };
                caller_owner_total += caller_owner_started.elapsed();
                Self::push_lookup_call_edge_row(
                    lookup_spans,
                    id_host,
                    rows,
                    caller_def,
                    callee_def,
                    editioned.editioned_file_id(db),
                    &local,
                    call.syntax().text_range(),
                    crate::DispatchKind::Direct,
                    caller_filter,
                    site_filter,
                    dispatch_filter,
                );
                pushed_rows += 1;
            }
        }
        if !alias_names.is_empty()
            && !Self::scan_lookup_callers_for_names(
                db,
                vfs,
                workspace_root,
                sema,
                lookup_defs,
                lookup_spans,
                id_host,
                rows,
                &search_files,
                alias_names,
                function,
                caller_filter,
                callee_def,
                site_filter,
                dispatch_filter,
            )
        {
            return false;
        }
        if std::env::var_os("RAQL_TRACE_TIMINGS").is_some() {
            eprintln!(
                "raql-timing workspace_service.collect_lookup_callers_for_function.usages usage_file_hits={} usage_refs={} resolved_methods={} resolved_calls={} pushed_rows={} usage_ms={} local_ms={} resolve_ms={} caller_owner_ms={}",
                usage_file_hits,
                usage_refs,
                resolved_methods,
                resolved_calls,
                pushed_rows,
                usage_total.as_millis(),
                local_total.as_millis(),
                resolve_total.as_millis(),
                caller_owner_total.as_millis(),
            );
        }
        true
    }

    fn scan_lookup_callers_for_names(
        db: &ide::RootDatabase,
        vfs: &vfs::Vfs,
        workspace_root: &Path,
        sema: &hir::Semantics<'_, ide::RootDatabase>,
        lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        rows: &mut BTreeSet<Vec<ExternLookupValue>>,
        search_files: &[base_db::EditionedFileId],
        mut candidate_names: BTreeSet<String>,
        function: hir::Function,
        caller_filter: Option<DefId>,
        callee_def: DefId,
        site_filter: Option<SpanId>,
        dispatch_filter: Option<crate::DispatchKind>,
    ) -> bool {
        let mut alias_names_added = 0usize;
        let mut candidate_file_hits = 0usize;
        let mut iterations = 0usize;
        let mut matching_method_names = 0usize;
        let mut matching_call_names = 0usize;
        let mut resolved_methods = 0usize;
        let mut resolved_calls = 0usize;
        let mut pushed_rows = 0usize;
        let mut parse_total = std::time::Duration::ZERO;
        let mut resolve_total = std::time::Duration::ZERO;
        let mut caller_owner_total = std::time::Duration::ZERO;
        let mut local_total = std::time::Duration::ZERO;
        loop {
            iterations += 1;
            let mut discovered_alias = false;
            for editioned in search_files {
                let file_id = editioned.file_id(db);
                let text = db.file_text(file_id).text(db);
                if !candidate_names
                    .iter()
                    .any(|name| text.contains(name.as_str()))
                {
                    continue;
                }
                let local_started = Instant::now();
                let Some(local) = Self::lookup_local_file(vfs, workspace_root, db, file_id) else {
                    local_total += local_started.elapsed();
                    continue;
                };
                local_total += local_started.elapsed();
                candidate_file_hits += 1;
                let parse_started = Instant::now();
                let source = sema.parse(*editioned);
                parse_total += parse_started.elapsed();

                for use_tree in source.syntax().descendants().filter_map(ast::UseTree::cast) {
                    let Some(path) = use_tree.path() else {
                        continue;
                    };
                    let Some(resolved) = sema.resolve_path(&path).and_then(|resolved| match resolved {
                        hir::PathResolution::Def(ModuleDef::Function(resolved)) => Some(resolved),
                        _ => None,
                    }) else {
                        continue;
                    };
                    if resolved != function {
                        continue;
                    }
                    let alias_name = use_tree
                        .rename()
                        .and_then(|rename| rename.name())
                        .map(|name| name.text().to_string())
                        .or_else(|| {
                            path.segment()
                                .and_then(|segment| segment.name_ref())
                                .map(|name_ref| name_ref.text().to_string())
                        });
                    let Some(alias_name) = alias_name else {
                        continue;
                    };
                    if candidate_names.insert(alias_name) {
                        alias_names_added += 1;
                        discovered_alias = true;
                    }
                }

                for method_call in source.syntax().descendants().filter_map(ast::MethodCallExpr::cast)
                {
                    let Some(name_ref) = method_call.name_ref() else {
                        continue;
                    };
                    if !candidate_names.contains(name_ref.text().as_str()) {
                        continue;
                    }
                    matching_method_names += 1;
                    let resolve_started = Instant::now();
                    let Some(resolved) = sema.resolve_method_call(&method_call) else {
                        resolve_total += resolve_started.elapsed();
                        continue;
                    };
                    resolve_total += resolve_started.elapsed();
                    if resolved != function {
                        continue;
                    }
                    resolved_methods += 1;
                    let caller_owner_started = Instant::now();
                    let Some(caller_def) = Self::lookup_callable_owner_def(
                        db,
                        sema,
                        lookup_defs,
                        lookup_spans,
                        id_host,
                        method_call.syntax(),
                        editioned.editioned_file_id(db),
                        &local,
                    ) else {
                        caller_owner_total += caller_owner_started.elapsed();
                        continue;
                    };
                    caller_owner_total += caller_owner_started.elapsed();
                    Self::push_lookup_call_edge_row(
                        lookup_spans,
                        id_host,
                        rows,
                        caller_def,
                        callee_def,
                        editioned.editioned_file_id(db),
                        &local,
                        method_call.syntax().text_range(),
                        method_dispatch_kind(sema, &method_call, resolved, db),
                        caller_filter,
                        site_filter,
                        dispatch_filter,
                    );
                    pushed_rows += 1;
                }

                for call in source.syntax().descendants().filter_map(ast::CallExpr::cast) {
                    let Some(ast::Expr::PathExpr(path_expr)) = call.expr() else {
                        continue;
                    };
                    let Some(path) = path_expr.path() else {
                        continue;
                    };
                    let Some(segment) = path.segment() else {
                        continue;
                    };
                    let Some(name_ref) = segment.name_ref() else {
                        continue;
                    };
                    if !candidate_names.contains(name_ref.text().as_str()) {
                        continue;
                    }
                    matching_call_names += 1;
                    let resolve_started = Instant::now();
                    let Some(resolved) = sema.resolve_path(&path).and_then(|resolved| match resolved {
                        hir::PathResolution::Def(ModuleDef::Function(resolved)) => Some(resolved),
                        _ => None,
                    }) else {
                        resolve_total += resolve_started.elapsed();
                        continue;
                    };
                    resolve_total += resolve_started.elapsed();
                    if resolved != function {
                        continue;
                    }
                    resolved_calls += 1;
                    let caller_owner_started = Instant::now();
                    let Some(caller_def) = Self::lookup_callable_owner_def(
                        db,
                        sema,
                        lookup_defs,
                        lookup_spans,
                        id_host,
                        call.syntax(),
                        editioned.editioned_file_id(db),
                        &local,
                    ) else {
                        caller_owner_total += caller_owner_started.elapsed();
                        continue;
                    };
                    caller_owner_total += caller_owner_started.elapsed();
                    Self::push_lookup_call_edge_row(
                        lookup_spans,
                        id_host,
                        rows,
                        caller_def,
                        callee_def,
                        editioned.editioned_file_id(db),
                        &local,
                        call.syntax().text_range(),
                        crate::DispatchKind::Direct,
                        caller_filter,
                        site_filter,
                        dispatch_filter,
                    );
                    pushed_rows += 1;
                }
            }
            if !discovered_alias {
                break;
            }
        }
        if std::env::var_os("RAQL_TRACE_TIMINGS").is_some() {
            eprintln!(
                "raql-timing workspace_service.scan_lookup_callers_for_names alias_names_added={} candidate_file_hits={} iterations={} method_names={} call_names={} resolved_methods={} resolved_calls={} pushed_rows={} local_ms={} parse_ms={} resolve_ms={} caller_owner_ms={}",
                alias_names_added,
                candidate_file_hits,
                iterations,
                matching_method_names,
                matching_call_names,
                resolved_methods,
                resolved_calls,
                pushed_rows,
                local_total.as_millis(),
                parse_total.as_millis(),
                resolve_total.as_millis(),
                caller_owner_total.as_millis(),
            );
        }
        true
    }

    fn lookup_callable_owner_def(
        db: &ide::RootDatabase,
        sema: &hir::Semantics<'_, ide::RootDatabase>,
        lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        syntax: &syntax::SyntaxNode,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) -> Option<DefId> {
        for ancestor in syntax.ancestors().skip(1) {
            if let Some(closure) = ast::ClosureExpr::cast(ancestor.clone()) {
                return Self::ensure_lookup_synthetic_callable_def(
                    lookup_defs,
                    lookup_spans,
                    id_host,
                    "closure",
                    closure.syntax(),
                    file_id,
                    local,
                );
            }
            if let Some(ast_fn) = ast::Fn::cast(ancestor) {
                let Some(name) = ast_fn.name() else {
                    continue;
                };
                let kind = if ast_fn
                    .param_list()
                    .is_some_and(|params| params.self_param().is_some())
                {
                    DefKind::Method
                } else {
                    DefKind::Fn
                };
                let Some(function) = sema.to_def(&ast_fn) else {
                    return None;
                };
                return Self::ensure_lookup_syntax_function_def(
                    db,
                    lookup_defs,
                    lookup_spans,
                    id_host,
                    name.text().as_str(),
                    kind,
                    ast_fn.syntax(),
                    file_id,
                    local,
                    function,
                );
            }
        }
        None
    }

    fn lookup_call_target_from_callable(
        db: &ide::RootDatabase,
        _vfs: &vfs::Vfs,
        _workspace_root: &Path,
        _sema: &hir::Semantics<'_, ide::RootDatabase>,
        lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        callable: &hir::Callable<'_>,
        callee_expr: &ast::Expr,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) -> Option<(DefId, crate::DispatchKind)> {
        match callable.kind() {
            hir::CallableKind::Function(function) => Self::ensure_lookup_function_def(
                db,
                _vfs,
                _workspace_root,
                lookup_defs,
                lookup_spans,
                id_host,
                function,
            )
                .map(|def| (def, crate::DispatchKind::Direct)),
            hir::CallableKind::Closure(_) => Self::ensure_lookup_synthetic_callable_def(
                    lookup_defs,
                    lookup_spans,
                    id_host,
                    "closure",
                    callee_expr.syntax(),
                    file_id,
                    local,
                )
                .map(|def| (def, crate::DispatchKind::Closure)),
            hir::CallableKind::FnPtr | hir::CallableKind::FnImpl(_) => Self::ensure_lookup_synthetic_callable_def(
                    lookup_defs,
                    lookup_spans,
                    id_host,
                    "fn_pointer",
                    callee_expr.syntax(),
                    file_id,
                    local,
                )
                .map(|def| (def, crate::DispatchKind::FnPointer)),
            hir::CallableKind::TupleStruct(_) | hir::CallableKind::TupleEnumVariant(_) => None,
        }
    }

    fn ensure_lookup_function_def(
        db: &ide::RootDatabase,
        vfs: &vfs::Vfs,
        workspace_root: &Path,
        lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        function: hir::Function,
    ) -> Option<DefId> {
        let source = function.source(db)?;
        let editioned = source.file_id.original_file(db);
        let local = Self::lookup_local_file(vfs, workspace_root, db, editioned.file_id(db))?;
        let range = source.value.syntax().text_range();
        let span_key =
            lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range)?;
        let span = id_host
            .intern_span_from_text(
                editioned.editioned_file_id(db),
                local.rel_path.clone(),
                local.text.as_str(),
                range,
            )
            .ok()?;
        let kind = if function.has_self_param(db) {
            DefKind::Method
        } else {
            DefKind::Fn
        };
        let path = canonical_function_path(db, function);
        let token = format!("def:function:{path}");
        let def_id = id_host.intern_def_from_token(token.as_str());
        let name = function.name(db).display(db, Edition::CURRENT).to_string();
        lookup_defs.insert(
            def_id,
            LookupDefRecord {
                name: name.into_boxed_str(),
                kind,
                span,
                path: Some(path.into_boxed_str()),
                entity: Some(RaEntity::ModuleDef(ModuleDef::Function(function))),
                ra_span: Some(RaSpan {
                    file_id: editioned.editioned_file_id(db),
                    range,
                }),
            },
        );
        lookup_spans.insert(span, span_key);
        Some(def_id)
    }

    fn def_kind_for_module_def(db: &ide::RootDatabase, def: ModuleDef) -> Option<DefKind> {
        Some(match def {
            ModuleDef::Function(function) => {
                if function.has_self_param(db) {
                    DefKind::Method
                } else {
                    DefKind::Fn
                }
            }
            ModuleDef::Module(_) => DefKind::Mod,
            ModuleDef::Adt(Adt::Struct(_)) => DefKind::Struct,
            ModuleDef::Adt(Adt::Enum(_)) => DefKind::Enum,
            ModuleDef::Adt(Adt::Union(_)) => DefKind::Union,
            ModuleDef::Variant(_) => DefKind::Variant,
            ModuleDef::Const(_) => DefKind::Const,
            ModuleDef::Static(_) => DefKind::Static,
            ModuleDef::Trait(_) => DefKind::Trait,
            ModuleDef::TypeAlias(_) => DefKind::TypeAlias,
            ModuleDef::Macro(_) => DefKind::Macro,
            _ => return None,
        })
    }

    fn ensure_lookup_symbol_module_def(
        db: &ide::RootDatabase,
        vfs: &vfs::Vfs,
        workspace_root: &Path,
        lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        def: ModuleDef,
        file_id: span::EditionedFileId,
        range: syntax::TextRange,
    ) -> Option<DefId> {
        let kind = Self::def_kind_for_module_def(db, def)?;
        let local = Self::lookup_local_file(vfs, workspace_root, db, file_id.file_id())?;
        let span_key =
            lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range)?;
        let span = id_host
            .intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range)
            .ok()?;
        let entity = RaEntity::ModuleDef(def);
        let path = entity.canonical_path(db);
        let token = path
            .as_ref()
            .map(|path| match kind {
                DefKind::Fn | DefKind::Method => format!("def:function:{path}"),
                _ => format!("def:{kind:?}:{path}"),
            })
            .unwrap_or_else(|| {
                format!(
                    "lookup_def:{kind:?}:{}:{}..{}",
                    local.rel_path,
                    u32::from(range.start()),
                    u32::from(range.end())
                )
            });
        let def_id = id_host.intern_def_from_token(token.as_str());
        let name = def.name(db)?.display(db, Edition::CURRENT).to_string();
        lookup_defs.insert(
            def_id,
            LookupDefRecord {
                name: name.into_boxed_str(),
                kind,
                span,
                path: path.map(Into::into),
                entity: Some(entity),
                ra_span: Some(RaSpan { file_id, range }),
            },
        );
        lookup_spans.insert(span, span_key);
        Some(def_id)
    }

    fn ensure_lookup_syntax_function_def(
        _db: &ide::RootDatabase,
        lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        name: &str,
        kind: DefKind,
        syntax: &syntax::SyntaxNode,
        file_id: span::EditionedFileId,
        local: &LocalFile,
        function: hir::Function,
    ) -> Option<DefId> {
        let range = syntax.text_range();
        let span_key =
            lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range)?;
        let span = id_host
            .intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range)
            .ok()?;
        let path = canonical_function_path(_db, function);
        let token = format!("def:function:{path}");
        let def_id = id_host.intern_def_from_token(token.as_str());
        lookup_defs.insert(
            def_id,
            LookupDefRecord {
                name: name.to_string().into_boxed_str(),
                kind,
                span,
                path: Some(path.into_boxed_str()),
                entity: Some(RaEntity::ModuleDef(ModuleDef::Function(function))),
                ra_span: Some(RaSpan { file_id, range }),
            },
        );
        lookup_spans.insert(span, span_key);
        Some(def_id)
    }

    fn ensure_lookup_synthetic_callable_def(
        lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        prefix: &str,
        syntax: &syntax::SyntaxNode,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) -> Option<DefId> {
        let range = syntax.text_range();
        let span_key =
            lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range)?;
        let span = id_host
            .intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range)
            .ok()?;
        let label = syntax.text().to_string();
        let path = format!(
            "{prefix}::{}:{}..{}:{}",
            local.rel_path,
            u32::from(range.start()),
            u32::from(range.end()),
            label
        );
        let def_id = id_host.intern_def_from_token(format!("def:{prefix}:{path}").as_str());
        lookup_defs.insert(
            def_id,
            LookupDefRecord {
                name: label.into_boxed_str(),
                kind: DefKind::Other,
                span,
                path: Some(path.into_boxed_str()),
                entity: None,
                ra_span: Some(RaSpan { file_id, range }),
            },
        );
        lookup_spans.insert(span, span_key);
        Some(def_id)
    }

    fn lookup_local_file(
        vfs: &vfs::Vfs,
        workspace_root: &Path,
        db: &ide::RootDatabase,
        file_id: vfs::FileId,
    ) -> Option<LocalFile> {
        let abs_path = vfs.file_path(file_id).as_path()?;
        let path: &Path = abs_path.as_ref();
        let rel_path = if path.starts_with(workspace_root) {
            path.strip_prefix(workspace_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            path.to_string_lossy().replace('\\', "/")
        };
        let text = db.file_text(file_id).text(db).to_string();
        Some(LocalFile { rel_path, text })
    }

    fn push_lookup_call_edge_row(
        lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
        id_host: &mut DeterministicRaHost,
        rows: &mut BTreeSet<Vec<ExternLookupValue>>,
        caller_def: DefId,
        callee_def: DefId,
        file_id: span::EditionedFileId,
        local: &LocalFile,
        range: syntax::TextRange,
        dispatch: crate::DispatchKind,
        peer_filter: Option<DefId>,
        site_filter: Option<SpanId>,
        dispatch_filter: Option<crate::DispatchKind>,
    ) {
        if peer_filter.is_some_and(|expected| expected != callee_def && expected != caller_def) {
            return;
        }
        if dispatch_filter.is_some_and(|expected| expected != dispatch) {
            return;
        }
        let Some(span_key) =
            lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range)
        else {
            return;
        };
        let Ok(site) =
            id_host.intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range)
        else {
            return;
        };
        if site_filter.is_some_and(|expected| expected != site) {
            return;
        }
        lookup_spans.insert(site, span_key);
        rows.insert(vec![
            ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                caller_def.stable_id(),
            )),
            ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                callee_def.stable_id(),
            )),
            ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Span,
                site.stable_id(),
            )),
            dispatch_lookup_value(dispatch),
        ]);
    }

    fn lookup_def_rows(&self, request: &ExternLookupRequest) -> Vec<Vec<ExternLookupValue>> {
        let mut def = None::<DefId>;
        for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
            match (*idx, value) {
                (0, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Def =>
                {
                    def = Some(DefId::new(host.stable_id()));
                }
                _ => return Vec::new(),
            }
        }
        let Some(def) = def else {
            return Vec::new();
        };
        if self.lookup_defs.contains_key(&def) {
            vec![vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                def.stable_id(),
            ))]]
        } else {
            Vec::new()
        }
    }

    fn lookup_def_kind_rows(&self, request: &ExternLookupRequest) -> Vec<Vec<ExternLookupValue>> {
        let mut def = None::<DefId>;
        let mut kind_filter = None::<DefKind>;
        for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
            match (*idx, value) {
                (0, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Def =>
                {
                    def = Some(DefId::new(host.stable_id()));
                }
                (1, ExternLookupValue::Enum { name, variant }) if name.as_ref() == "DefKind" => {
                    kind_filter = def_kind_from_variant(variant.as_ref());
                }
                _ => return Vec::new(),
            }
        }
        let Some(def) = def else {
            return Vec::new();
        };
        let Some(kind) = self.lookup_defs.get(&def).map(|record| record.kind) else {
            return Vec::new();
        };
        if kind_filter.is_some_and(|expected| expected != kind) {
            return Vec::new();
        }
        vec![vec![
            ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                def.stable_id(),
            )),
            def_kind_lookup_value(kind),
        ]]
    }

    fn lookup_def_span_rows(&self, request: &ExternLookupRequest) -> Vec<Vec<ExternLookupValue>> {
        let mut def = None::<DefId>;
        let mut span_filter = None::<SpanId>;
        for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
            match (*idx, value) {
                (0, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Def =>
                {
                    def = Some(DefId::new(host.stable_id()));
                }
                (1, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Span =>
                {
                    span_filter = Some(SpanId::new(host.stable_id()));
                }
                _ => return Vec::new(),
            }
        }
        let Some(def) = def else {
            return Vec::new();
        };
        let Some(record) = self.lookup_defs.get(&def) else {
            return Vec::new();
        };
        if span_filter.is_some_and(|expected| expected != record.span) {
            return Vec::new();
        }
        vec![vec![
            ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                def.stable_id(),
            )),
            ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Span,
                record.span.stable_id(),
            )),
        ]]
    }

    fn lookup_span_allowed_rows(&self, request: &ExternLookupRequest) -> Vec<Vec<ExternLookupValue>> {
        let mut span = None::<SpanId>;
        for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
            match (*idx, value) {
                (0, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Span =>
                {
                    span = Some(SpanId::new(host.stable_id()));
                }
                _ => return Vec::new(),
            }
        }
        let Some(span) = span else {
            return Vec::new();
        };
        if self.lookup_spans.contains_key(&span) {
            vec![vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Span,
                span.stable_id(),
            ))]]
        } else {
            Vec::new()
        }
    }

    fn lookup_span_key_rows(&self, request: &ExternLookupRequest) -> Vec<Vec<ExternLookupValue>> {
        let mut span = None::<SpanId>;
        let mut rel_path_filter = None::<&str>;
        let mut l0_filter = None::<i64>;
        let mut c0_filter = None::<i64>;
        let mut l1_filter = None::<i64>;
        let mut c1_filter = None::<i64>;
        for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
            match (*idx, value) {
                (0, ExternLookupValue::Host(host))
                    if host.kind() == ExternLookupHostValueKind::Span =>
                {
                    span = Some(SpanId::new(host.stable_id()));
                }
                (1, ExternLookupValue::String(path)) => rel_path_filter = Some(path.as_ref()),
                (2, ExternLookupValue::Int(v)) => l0_filter = Some(*v),
                (3, ExternLookupValue::Int(v)) => c0_filter = Some(*v),
                (4, ExternLookupValue::Int(v)) => l1_filter = Some(*v),
                (5, ExternLookupValue::Int(v)) => c1_filter = Some(*v),
                _ => return Vec::new(),
            }
        }
        let Some(span) = span else {
            return Vec::new();
        };
        let Some(key) = self.lookup_spans.get(&span) else {
            return Vec::new();
        };
        if rel_path_filter.is_some_and(|expected| expected != key.rel_path())
            || l0_filter.is_some_and(|expected| expected != key.start().line() as i64)
            || c0_filter.is_some_and(|expected| expected != key.start().column() as i64)
            || l1_filter.is_some_and(|expected| expected != key.end().line() as i64)
            || c1_filter.is_some_and(|expected| expected != key.end().column() as i64)
        {
            return Vec::new();
        }
        vec![vec![
            ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Span,
                span.stable_id(),
            )),
            ExternLookupValue::String(key.rel_path().to_string().into_boxed_str()),
            ExternLookupValue::Int(key.start().line() as i64),
            ExternLookupValue::Int(key.start().column() as i64),
            ExternLookupValue::Int(key.end().line() as i64),
            ExternLookupValue::Int(key.end().column() as i64),
        ]]
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
            self.core_host = Some(build_core_host(
                &self.analysis_host,
                &self.vfs,
                &self.workspace_root,
                &self.tracked_files,
                self.workspace_epoch,
                self.content_revision,
                &build_spec,
            )?);
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

#[derive(Debug)]
#[derive(Clone)]
struct LocalFile {
    rel_path: String,
    text: String,
}

struct CoreFactsBuilder<'db> {
    db: &'db ide::RootDatabase,
    build_spec: CoreHostBuildSpec,
    files: BTreeMap<vfs::FileId, LocalFile>,
    host: DeterministicRaHost,
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
                self.extract_syntax_nodes();
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
                if self.build_spec.def_publicity {
                    self.host.mark_public(def_id, module_def_is_public(symbol.def, self.db));
                }
                if self.build_spec.def_test_flags {
                    self.host
                        .mark_in_test(def_id, module_def_in_test(symbol.def, self.db));
                }
                self.def_path_by_id.insert(def_id, path);
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

    fn extract_syntax_nodes(&mut self) {
        // TODO(ra-native-audit): this still walks our own host-side node graph, but the parsed
        // syntax tree itself now comes directly from RA instead of a parallel raw parse pass.
        let sema = hir::Semantics::new(self.db);
        let files = self.files.clone();
        for (file_id, local) in files {
            let editioned_file =
                base_db::EditionedFileId::current_edition_guess_origin(self.db, file_id);
            let root = sema.parse(editioned_file).syntax().clone();
            self.record_syntax_node(&root, editioned_file.editioned_file_id(self.db), &local, None);
        }
    }

    fn record_syntax_node(
        &mut self,
        node: &syntax::SyntaxNode,
        file_id: span::EditionedFileId,
        local: &LocalFile,
        parent: Option<NodeId>,
    ) {
        let range = node.text_range();
        let Ok(span) = self.host.intern_span_from_text(
            file_id,
            local.rel_path.clone(),
            local.text.as_str(),
            range,
        ) else {
            for child in node.children() {
                self.record_syntax_node(&child, file_id, local, parent);
            }
            return;
        };
        let token = format!(
            "{}:{}..{}:{:?}",
            local.rel_path,
            u32::from(range.start()),
            u32::from(range.end()),
            node.kind()
        );
        let node_id = NodeId::new(crate::deterministic_stable_id("node", token.as_str()));
        self.host.insert_node_id(node_id, format!("node:{token}"));
        self.host
            .insert_node(node_id, syntax_node_kind(node), span, parent);
        for child in node.children() {
            self.record_syntax_node(&child, file_id, local, Some(node_id));
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
            self.def_path_by_id.insert(def_id, path);
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
                self.def_path_by_id.insert(def_id, path);
            } else {
                self.host
                    .insert_synthetic_def(def_id, label.as_str(), DefKind::Other, path.as_str());
                self.host.mark_public(def_id, false);
                self.host.mark_in_test(def_id, false);
                self.def_path_by_id.insert(def_id, path);
            }
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
            self.def_path_by_id.insert(def_id, path);
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
        self.def_path_by_id.insert(def_id, path);
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
        self.def_path_by_id.insert(def_id, path.clone());

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
        }
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
    build_spec: &CoreHostBuildSpec,
) -> Result<DeterministicRaHost, RaHostInitError> {
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
        def_path_by_id: BTreeMap::new(),
        local_adts: Vec::new(),
        local_traits: Vec::new(),
        local_functions: Vec::new(),
    };
    builder.populate(build_spec);
    trace_timing("workspace_service.build_core_host.total", build_started.elapsed());
    Ok(builder.host)
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

fn canonical_function_path(db: &ide::RootDatabase, function: hir::Function) -> String {
    let name = function.name(db).display(db, Edition::CURRENT).to_string();
    let raw_path = ModuleDef::Function(function)
        .canonical_path(db, Edition::CURRENT)
        .unwrap_or_else(|| name.clone());
    function
        .module(db)
        .krate(db)
        .display_name(db)
        .map(|crate_name| crate_name.to_string())
        .filter(|crate_name| {
            raw_path != *crate_name && !raw_path.starts_with(format!("{crate_name}::").as_str())
        })
        .map(|crate_name| format!("{crate_name}::{raw_path}"))
        .unwrap_or(raw_path)
}

fn dispatch_kind_from_variant(variant: &str) -> Option<crate::DispatchKind> {
    match variant {
        "DIRECT" => Some(crate::DispatchKind::Direct),
        "THROUGH_TRAIT" => Some(crate::DispatchKind::ThroughTrait),
        "DYN" => Some(crate::DispatchKind::Dyn),
        "CLOSURE" => Some(crate::DispatchKind::Closure),
        "FN_POINTER" => Some(crate::DispatchKind::FnPointer),
        _ => None,
    }
}

fn dispatch_lookup_value(dispatch: crate::DispatchKind) -> ExternLookupValue {
    let variant = match dispatch {
        crate::DispatchKind::Direct => "DIRECT",
        crate::DispatchKind::ThroughTrait => "THROUGH_TRAIT",
        crate::DispatchKind::Dyn => "DYN",
        crate::DispatchKind::Closure => "CLOSURE",
        crate::DispatchKind::FnPointer => "FN_POINTER",
    };
    ExternLookupValue::Enum {
        name: "DispatchKind".into(),
        variant: variant.into(),
    }
}

fn belongs_to_item(node: &syntax::SyntaxNode, owner_item: &syntax::SyntaxNode) -> bool {
    node.ancestors()
        .find_map(ast::Item::cast)
        .is_some_and(|item| item.syntax() == owner_item)
}

fn method_dispatch_kind(
    sema: &hir::Semantics<'_, ide::RootDatabase>,
    method_call: &ast::MethodCallExpr,
    function: hir::Function,
    db: &dyn hir::db::HirDatabase,
) -> crate::DispatchKind {
    let receiver_is_dyn = method_call
        .receiver()
        .and_then(|receiver| sema.type_of_expr(&receiver))
        .map(|info| {
            let receiver_ty = info.original;
            receiver_ty.as_dyn_trait().is_some()
                || receiver_ty
                    .autoderef(db)
                    .any(|candidate| candidate.as_dyn_trait().is_some())
        })
        .unwrap_or(false);
    if receiver_is_dyn {
        return crate::DispatchKind::Dyn;
    }
    if hir::AssocItem::Function(function)
        .container_or_implemented_trait(db)
        .is_some()
    {
        return crate::DispatchKind::ThroughTrait;
    }
    crate::DispatchKind::Direct
}

fn module_def_kind(def: ModuleDef) -> Option<DefKind> {
    Some(match def {
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
    })
}

fn def_kind_lookup_value(kind: DefKind) -> ExternLookupValue {
    let variant = match kind {
        DefKind::Fn => "FN",
        DefKind::Method => "METHOD",
        DefKind::Struct => "STRUCT",
        DefKind::Enum => "ENUM",
        DefKind::Union => "UNION",
        DefKind::Trait => "TRAIT",
        DefKind::Mod => "MOD",
        DefKind::Impl => "IMPL",
        DefKind::TypeAlias => "TYPE_ALIAS",
        DefKind::Const => "CONST",
        DefKind::Static => "STATIC",
        DefKind::Field => "FIELD",
        DefKind::Variant => "VARIANT",
        DefKind::AssocType => "ASSOC_TYPE",
        DefKind::AssocConst => "ASSOC_CONST",
        DefKind::Macro => "MACRO",
        DefKind::Other => "OTHER",
    };
    ExternLookupValue::Enum {
        name: "DefKind".to_string().into_boxed_str(),
        variant: variant.to_string().into_boxed_str(),
    }
}

fn def_kind_from_variant(variant: &str) -> Option<DefKind> {
    match variant {
        "FN" => Some(DefKind::Fn),
        "METHOD" => Some(DefKind::Method),
        "STRUCT" => Some(DefKind::Struct),
        "ENUM" => Some(DefKind::Enum),
        "UNION" => Some(DefKind::Union),
        "TRAIT" => Some(DefKind::Trait),
        "MOD" => Some(DefKind::Mod),
        "IMPL" => Some(DefKind::Impl),
        "TYPE_ALIAS" => Some(DefKind::TypeAlias),
        "CONST" => Some(DefKind::Const),
        "STATIC" => Some(DefKind::Static),
        "FIELD" => Some(DefKind::Field),
        "VARIANT" => Some(DefKind::Variant),
        "ASSOC_TYPE" => Some(DefKind::AssocType),
        "ASSOC_CONST" => Some(DefKind::AssocConst),
        "MACRO" => Some(DefKind::Macro),
        "OTHER" => Some(DefKind::Other),
        _ => None,
    }
}

fn lookup_span_key_from_text(
    rel_path: &str,
    source_text: &str,
    range: syntax::TextRange,
) -> Option<SpanKey> {
    let line_index = LineIndex::new(source_text);
    let start = line_index.try_line_col(range.start())?;
    let end = line_index.try_line_col(range.end())?;
    Some(SpanKey::new(
        rel_path.to_string(),
        SpanCoord::new(start.line, start.col),
        SpanCoord::new(end.line, end.col),
    ))
}

fn module_def_is_public(def: ModuleDef, db: &dyn hir::db::HirDatabase) -> bool {
    match def {
        ModuleDef::Module(module) => module.visibility(db) == hir::Visibility::Public,
        ModuleDef::Function(function) => function.visibility(db) == hir::Visibility::Public,
        ModuleDef::Adt(adt) => adt.visibility(db) == hir::Visibility::Public,
        ModuleDef::Variant(variant) => variant.visibility(db) == hir::Visibility::Public,
        ModuleDef::Const(const_) => const_.visibility(db) == hir::Visibility::Public,
        ModuleDef::Static(static_) => static_.visibility(db) == hir::Visibility::Public,
        ModuleDef::Trait(trait_) => trait_.visibility(db) == hir::Visibility::Public,
        ModuleDef::TypeAlias(alias) => alias.visibility(db) == hir::Visibility::Public,
        ModuleDef::Macro(mac) => mac.visibility(db) == hir::Visibility::Public,
        ModuleDef::BuiltinType(_) => false,
    }
}

fn module_def_in_test(def: ModuleDef, db: &dyn hir::db::HirDatabase) -> bool {
    let module = match def {
        ModuleDef::Module(module) => module,
        ModuleDef::Function(function) => return function.is_test(db) || module_is_test_scope(function.module(db), db),
        ModuleDef::Adt(adt) => adt.module(db),
        ModuleDef::Variant(variant) => variant.module(db),
        ModuleDef::Const(const_) => const_.module(db),
        ModuleDef::Static(static_) => static_.module(db),
        ModuleDef::Trait(trait_) => trait_.module(db),
        ModuleDef::TypeAlias(alias) => alias.module(db),
        ModuleDef::Macro(mac) => mac.module(db),
        ModuleDef::BuiltinType(_) => return false,
    };
    module_is_test_scope(module, db)
}

fn module_is_test_scope(module: Module, db: &dyn hir::db::HirDatabase) -> bool {
    module.path_to_root(db).into_iter().any(|m| {
        m.name(db)
            .is_some_and(|name| name.display(db, Edition::CURRENT).to_string() == "tests")
    })
}

fn syntax_node_kind(node: &syntax::SyntaxNode) -> NodeKind {
    if ast::IfExpr::cast(node.clone()).is_some() {
        NodeKind::If
    } else if ast::MatchExpr::cast(node.clone()).is_some() {
        NodeKind::Match
    } else if ast::WhileExpr::cast(node.clone()).is_some() {
        NodeKind::While
    } else if ast::ForExpr::cast(node.clone()).is_some() {
        NodeKind::For
    } else if ast::LoopExpr::cast(node.clone()).is_some() {
        NodeKind::Loop
    } else if ast::BlockExpr::cast(node.clone()).is_some() {
        NodeKind::Block
    } else if ast::TryExpr::cast(node.clone()).is_some() {
        NodeKind::Try
    } else if ast::MatchArm::cast(node.clone()).is_some() {
        NodeKind::Arm
    } else if ast::Item::cast(node.clone()).is_some() {
        NodeKind::Item
    } else if ast::Expr::cast(node.clone()).is_some() {
        NodeKind::Expr
    } else {
        NodeKind::Other
    }
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
