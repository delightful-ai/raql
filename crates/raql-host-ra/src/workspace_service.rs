use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use base_db::SourceDatabase;
use camino::Utf8PathBuf;
use hir::{Adt, AssocItem, Module, ModuleDef};
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
use syntax::Edition;
use vfs::{AbsPathBuf, VfsPath};

use crate::capability::{day_one_supported_capabilities, supports_day_one_capability};
use crate::lazy_runtime::LazyRaRuntime;
use crate::provider::calls::{
    CallGraphProvider, extract_call_edges as provider_extract_call_edges,
    lookup_call_edge_rows as provider_lookup_call_edge_rows,
};
use crate::provider::core_index::CoreLookupIndex;
use crate::provider::defs::{
    LocalFile, LookupDefRecord, lookup_def_flag_rows, lookup_def_kind_rows, lookup_def_name_rows,
    lookup_def_handle_rows, lookup_def_path_rows, lookup_def_rows, lookup_def_span_rows, module_def_in_test,
    module_def_is_public, module_def_kind,
};
use crate::provider::syntax::{
    LookupNodeRecord, extract_syntax_nodes, lookup_enclosing_control_rows, lookup_node_at_rows,
    lookup_node_id_rows, lookup_node_kind_rows, lookup_node_parent_rows, lookup_node_span_rows,
    lookup_span_allowed_rows, lookup_span_key_rows,
};
use crate::workspace_loader;
use crate::{DefId, DeterministicRaHost, NodeId, RaHostInitError, SpanId, StableHandle, WorldStamp};

#[path = "workspace_service/build.rs"]
mod build;
#[path = "workspace_service/tracking.rs"]
mod tracking;
#[path = "workspace_service/watch.rs"]
mod watch;

use self::tracking::{
    auxiliary_build_input_state, build_script_rerun_paths, path_requires_reload,
    tracked_directory_watch_set, tracked_file_state_map, tracked_path_state,
    tracked_rust_file_state_map,
    tracked_workspace_state,
};
use self::watch::WorkspaceWatcher;

const WATCHER_READY_TIMEOUT: Duration = Duration::from_millis(20);
const WATCHER_SETTLE_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct WorkspaceService {
    manifest_path: PathBuf,
    workspace_root: PathBuf,
    analysis_host: AnalysisHost,
    vfs: vfs::Vfs,
    _proc_macro_client: Option<Box<dyn workspace_loader::ProcMacroClientHandle>>,
    core_host: Option<DeterministicRaHost>,
    core_index: Option<CoreLookupIndex>,
    core_index_spec: Option<CoreHostBuildSpec>,
    core_index_complete: bool,
    core_host_spec: Option<CoreHostBuildSpec>,
    tracked_files: BTreeSet<PathBuf>,
    tracked_file_states: BTreeMap<PathBuf, WatchedFileState>,
    tracked_dirs: BTreeSet<PathBuf>,
    tracked_dir_states: BTreeMap<PathBuf, WatchedFileState>,
    watcher: WorkspaceWatcher,
    watched_entries: Vec<vfs::loader::Entry>,
    build_script_rerun_paths: BTreeSet<PathBuf>,
    auxiliary_build_inputs: BTreeMap<PathBuf, Option<WatchedFileState>>,
    lookup_defs: BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: BTreeMap<SpanId, SpanKey>,
    lookup_nodes: BTreeMap<NodeId, LookupNodeRecord>,
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
            && !self.type_facts
            && !self.adt_structure
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
        )?;
        trace_timing("workspace_service.from_loaded.tracked_workspace_state", tracked_files_started.elapsed());
        let tracked_file_states_started = Instant::now();
        let tracked_file_states = tracked_file_state_map(&tracked_files)?;
        trace_timing("workspace_service.from_loaded.tracked_file_state_map", tracked_file_states_started.elapsed());
        let tracked_dirs_started = Instant::now();
        let tracked_dirs = tracked_directory_watch_set(&tracked_files, &loaded.watched_entries);
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
        let watcher_started = Instant::now();
        let watcher = WorkspaceWatcher::new(&loaded.watched_entries);
        trace_timing("workspace_service.from_loaded.watcher_init", watcher_started.elapsed());
        Ok(Self {
            manifest_path: loaded.manifest_path,
            workspace_root: loaded.workspace_root,
            analysis_host: AnalysisHost::with_database(loaded.db),
            vfs: loaded.vfs,
            _proc_macro_client: loaded.proc_macro_client,
            core_host: None,
            core_index: None,
            core_index_spec: None,
            core_index_complete: false,
            core_host_spec: None,
            tracked_files,
            tracked_file_states,
            tracked_dirs,
            tracked_dir_states,
            watcher,
            watched_entries: loaded.watched_entries,
            build_script_rerun_paths,
            auxiliary_build_inputs,
            lookup_defs: BTreeMap::new(),
            lookup_spans: BTreeMap::new(),
            lookup_nodes: BTreeMap::new(),
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

    #[cfg(test)]
    pub(crate) fn has_core_host(&self) -> bool {
        self.core_host.is_some()
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
                if request.bound_positions().is_empty() && self.core_index.is_none() {
                    let _ = self.ensure_core_index(&CoreHostBuildSpec::default())?;
                }
                Ok(Some(lookup_def_rows(
                    request,
                    self.core_index.as_ref(),
                    &self.lookup_defs,
                )))
            }
            ("def_kind", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_def_kind_rows(
                    request,
                    self.core_index.as_ref(),
                    &self.lookup_defs,
                )))
            }
            ("def_span", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_def_span_rows(
                    request,
                    self.core_index.as_ref(),
                    &self.lookup_defs,
                    &mut self.lookup_spans,
                )))
            }
            ("def_path", ExternLookupShape::FunctionExactBindings) => lookup_def_path_rows(
                request,
                self.analysis_host.raw_database(),
                self.core_index.as_ref(),
                &mut self.lookup_defs,
            ),
            ("handle", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_def_handle_rows(
                    request,
                    self.core_index.as_ref(),
                    &self.lookup_defs,
                )))
            }
            ("is_public", ExternLookupShape::RelationExactBindings) => {
                let _ = self.ensure_core_index(&CoreHostBuildSpec {
                    def_publicity: true,
                    ..CoreHostBuildSpec::default()
                })?;
                Ok(lookup_def_flag_rows(request, self.core_index.as_ref(), "is_public"))
            }
            ("in_test", ExternLookupShape::RelationExactBindings) => {
                let _ = self.ensure_core_index(&CoreHostBuildSpec {
                    def_test_flags: true,
                    ..CoreHostBuildSpec::default()
                })?;
                Ok(lookup_def_flag_rows(request, self.core_index.as_ref(), "in_test"))
            }
            ("call_edge", ExternLookupShape::RelationExactBindings) => {
                self.lookup_call_edge_rows(request)
            }
            ("span_allowed", ExternLookupShape::RelationExactBindings) => {
                Ok(Some(lookup_span_allowed_rows(
                    request,
                    self.core_index.as_ref(),
                    &self.lookup_spans,
                )))
            }
            ("span_key", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_span_key_rows(
                    request,
                    self.core_index.as_ref(),
                    &mut self.lookup_spans,
                )))
            }
            ("node_at", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_node_at_rows(
                    request,
                    self.analysis_host.raw_database(),
                    &self.vfs,
                    self.workspace_root.as_path(),
                    self.core_index.as_ref(),
                    &mut self.lookup_spans,
                    &mut self.lookup_nodes,
                )))
            }
            ("node_kind", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_node_kind_rows(request, &self.lookup_nodes)))
            }
            ("node_span", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_node_span_rows(request, &self.lookup_nodes)))
            }
            ("node_parent", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_node_parent_rows(request, &self.lookup_nodes)))
            }
            ("node_id", ExternLookupShape::FunctionExactBindings) => {
                Ok(Some(lookup_node_id_rows(request, &self.lookup_nodes)))
            }
            ("enclosing_control", ExternLookupShape::RelationExactBindings) => {
                Ok(Some(lookup_enclosing_control_rows(
                    request,
                    self.analysis_host.raw_database(),
                    &self.vfs,
                    self.workspace_root.as_path(),
                    self.core_index.as_ref(),
                    &mut self.lookup_spans,
                    &mut self.lookup_nodes,
                )))
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

    pub(crate) fn lookup_span_key_if_known(&mut self, span: SpanId) -> Option<SpanKey> {
        if let Some(key) = self.lookup_spans.get(&span) {
            return Some(key.clone());
        }
        let key = self
            .core_index
            .as_ref()
            .and_then(|index| index.span_key(span))
            .cloned()?;
        self.lookup_spans.insert(span, key.clone());
        Some(key)
    }

    pub(crate) fn lookup_handle_if_known(&self, def: DefId) -> Option<StableHandle> {
        self.lookup_defs
            .get(&def)
            .and_then(|record| record.path.as_deref())
            .map(|path| StableHandle::new(format!("def://{path}")))
            .or_else(|| self.core_index.as_ref().and_then(|index| index.def_handle(def)))
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
            self.core_index.as_ref(),
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
            self.core_index_spec = Some(build_spec.clone());
            self.core_index_complete = true;
            trace_timing("workspace_service.ensure_core_host.build_core_host", build_started.elapsed());
            self.core_host_spec = Some(build_spec);
        }
        trace_timing("workspace_service.ensure_core_host.total", ensure_started.elapsed());
        Ok(self.core_host.as_mut().expect("core host initialized"))
    }

    fn ensure_core_index(
        &mut self,
        required_spec: &CoreHostBuildSpec,
    ) -> Result<&CoreLookupIndex, RaHostInitError> {
        let ensure_started = Instant::now();
        let build_spec = self
            .core_index_spec
            .as_ref()
            .map(|current| current.union(required_spec))
            .unwrap_or_else(|| required_spec.clone());
        if self.core_index.is_none()
            || !self.core_index_complete
            || self
                .core_index_spec
                .as_ref()
                .is_none_or(|current| !current.covers(required_spec))
        {
            let build_started = Instant::now();
            let index = build_core_index(
                &self.analysis_host,
                &self.vfs,
                &self.workspace_root,
                &self.tracked_files,
                self.workspace_epoch,
                self.content_revision,
                &build_spec,
            )?;
            self.core_index = Some(index);
            self.core_index_spec = Some(build_spec);
            self.core_index_complete = true;
            trace_timing("workspace_service.ensure_core_index.build_core_index", build_started.elapsed());
        }
        trace_timing("workspace_service.ensure_core_index.total", ensure_started.elapsed());
        Ok(self.core_index.as_ref().expect("core index initialized"))
    }

    fn sync_workspace(&mut self) -> Result<(), RaHostInitError> {
        // TODO(ra-native-audit): replace raw filesystem rescans with RA/Cargo/VFS-backed truth.
        // This warm-path sweep is both a correctness risk and the main latency offender.
        let auxiliary_build_inputs =
            auxiliary_build_input_state(&self.build_script_rerun_paths, &self.tracked_files)?;
        if auxiliary_build_inputs != self.auxiliary_build_inputs {
            return self.reload_full();
        }

        let watch_batch = self.watcher.drain(WATCHER_READY_TIMEOUT);
        let watcher_ready = self.watcher.is_ready();
        let watcher_settled = self.watcher.is_settled(WATCHER_SETTLE_GRACE);
        if !watch_batch.changed_files.is_empty() {
            let mut change = hir::ChangeWithProcMacros::default();
            let mut changed_tracked_paths = BTreeSet::new();
            let mut saw_change = false;
            for (abs_path, contents) in watch_batch.changed_files {
                let path: &Path = abs_path.as_ref();
                let path_buf = path.to_path_buf();
                let requires_reload = path_requires_reload(path, &self.build_script_rerun_paths);
                let is_rust_file = path.extension().is_some_and(|ext| ext == "rs");
                if requires_reload {
                    let _ = self.vfs.take_changes();
                    return self.reload_full();
                }
                let vfs_path = VfsPath::from(abs_path);
                let changed = self.vfs.set_file_contents(vfs_path.clone(), contents.clone());
                if !changed {
                    continue;
                }
                let Some((file_id, excluded)) = self.vfs.file_id(&vfs_path) else {
                    let _ = self.vfs.take_changes();
                    return self.reload_full();
                };
                if matches!(excluded, vfs::FileExcluded::Yes) {
                    let _ = self.vfs.take_changes();
                    return self.reload_full();
                }
                let Some(bytes) = contents else {
                    let _ = self.vfs.take_changes();
                    return self.reload_full();
                };
                let text = String::from_utf8(bytes).map_err(|err| RaHostInitError::WorkspaceLoad {
                    manifest: self.manifest_path.display().to_string(),
                    details: err.to_string(),
                })?;
                if self.tracked_files.contains(&path_buf) || is_rust_file {
                    changed_tracked_paths.insert(path_buf.clone());
                    self.tracked_files.insert(path_buf);
                }
                change.change_file(file_id, Some(text));
                saw_change = true;
            }
            let _ = self.vfs.take_changes();
            if saw_change {
                self.analysis_host.apply_change(change);
                self.content_revision = self.content_revision.saturating_add(1);
                if !self.try_overlay_core_host_for_paths(&changed_tracked_paths)? {
                    self.core_host = None;
                    self.invalidate_core_index_for_paths(&changed_tracked_paths);
                }
                self.invalidate_lookup_state_for_paths(&changed_tracked_paths);
                self.tracked_dirs =
                    tracked_directory_watch_set(&self.tracked_files, &self.watched_entries);
                for path in changed_tracked_paths {
                    match tracked_path_state(path.as_path())? {
                        Some(state) => {
                            self.tracked_file_states.insert(path, state);
                        }
                        None => {
                            self.tracked_file_states.remove(&path);
                        }
                    }
                }
                self.tracked_dir_states = tracked_file_state_map(&self.tracked_dirs)?;
            }
            return Ok(());
        }

        if self.reload_sensitive_files_changed()? {
            return self.reload_full();
        }
        if watcher_ready && watcher_settled {
            return Ok(());
        }
        let tracked_dir_states = tracked_file_state_map(&self.tracked_dirs)?;
        if tracked_dir_states != self.tracked_dir_states {
            return self.reload_full();
        }
        let tracked_rust_file_states = tracked_rust_file_state_map(&self.tracked_files)?;
        if tracked_rust_file_states.iter().all(|(path, state)| {
            self.tracked_file_states.get(path) == Some(state)
        }) {
            self.tracked_dir_states = tracked_dir_states;
            return Ok(());
        }

        let mut change = hir::ChangeWithProcMacros::default();
        let mut changed_tracked_paths = BTreeSet::new();
        let mut saw_change = false;
        for path in &self.tracked_files {
            if !path.extension().is_some_and(|ext| ext == "rs") {
                continue;
            }
            let changed_on_disk = tracked_rust_file_states
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
            changed_tracked_paths.insert(path.clone());
            saw_change = true;
        }
        let _ = self.vfs.take_changes();
        if saw_change {
            self.analysis_host.apply_change(change);
            self.content_revision = self.content_revision.saturating_add(1);
            if !self.try_overlay_core_host_for_paths(&changed_tracked_paths)? {
                self.core_host = None;
                self.invalidate_core_index_for_paths(&changed_tracked_paths);
            }
            self.invalidate_lookup_state_for_paths(&changed_tracked_paths);
        }
        for (path, state) in tracked_rust_file_states {
            self.tracked_file_states.insert(path, state);
        }
        self.tracked_dir_states = tracked_dir_states;
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
            )?;
        let tracked_file_states = tracked_file_state_map(&tracked_files)?;
        let tracked_dirs = tracked_directory_watch_set(&tracked_files, &watched_entries);
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
        self.core_index_spec = None;
        self.core_index_complete = false;
        self.core_host_spec = None;
        self.tracked_files = tracked_files;
        self.tracked_file_states = tracked_file_states;
        self.tracked_dirs = tracked_dirs;
        self.tracked_dir_states = tracked_dir_states;
        self.watcher = WorkspaceWatcher::new(&watched_entries);
        self.watched_entries = watched_entries;
        self.build_script_rerun_paths = build_script_rerun_paths;
        self.auxiliary_build_inputs = auxiliary_build_inputs;
        self.lookup_defs.clear();
        self.lookup_spans.clear();
        self.lookup_nodes.clear();
        self.workspace_epoch = self.workspace_epoch.saturating_add(1);
        self.content_revision = self.content_revision.saturating_add(1);
        Ok(())
    }

    fn reload_sensitive_files_changed(&self) -> Result<bool, RaHostInitError> {
        for path in &self.tracked_files {
            if !path_requires_reload(path.as_path(), &self.build_script_rerun_paths) {
                continue;
            }
            let current = tracked_path_state(path.as_path())?;
            match (self.tracked_file_states.get(path), current) {
                (Some(previous), Some(current)) if previous == &current => {}
                _ => return Ok(true),
            }
        }
        Ok(false)
    }

    fn invalidate_lookup_state_for_paths(&mut self, changed_paths: &BTreeSet<PathBuf>) {
        if changed_paths.is_empty() {
            return;
        }
        let changed_path_strings = changed_paths
            .iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect::<BTreeSet<_>>();
        let changed_rel_paths = changed_paths
            .iter()
            .filter_map(|path| {
                path.strip_prefix(&self.workspace_root)
                    .ok()
                    .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            })
            .collect::<BTreeSet<_>>();
        let invalid_spans = self
            .lookup_spans
            .iter()
            .filter_map(|(span_id, span_key)| {
                (changed_rel_paths.contains(span_key.rel_path())
                    || changed_path_strings.contains(span_key.rel_path()))
                .then_some(*span_id)
            })
            .collect::<BTreeSet<_>>();
        self.lookup_spans
            .retain(|span_id, _| !invalid_spans.contains(span_id));
        let vfs = &self.vfs;
        self.lookup_defs.retain(|_, record| {
            if invalid_spans.contains(&record.span) {
                return false;
            }
            let Some(ra_span) = &record.ra_span else {
                return true;
            };
            let path = vfs.file_path(ra_span.file_id.file_id());
            let Some(abs_path) = path.as_path() else {
                return true;
            };
            let path: &Path = abs_path.as_ref();
            !changed_paths.contains(path)
        });
        self.lookup_nodes
            .retain(|_, record| !invalid_spans.contains(&record.span));
    }

    fn invalidate_core_index_for_paths(&mut self, changed_paths: &BTreeSet<PathBuf>) {
        let changed_rel_paths = self.changed_rel_paths(changed_paths);
        let Some(core_index) = self.core_index.as_mut() else {
            return;
        };
        core_index.invalidate_paths(&changed_rel_paths);
        if !changed_rel_paths.is_empty() {
            self.core_index_complete = false;
        }
    }

    fn try_overlay_core_host_for_paths(
        &mut self,
        changed_paths: &BTreeSet<PathBuf>,
    ) -> Result<bool, RaHostInitError> {
        let Some(build_spec) = self.core_host_spec.clone() else {
            return Ok(false);
        };
        let changed_rel_paths = self.changed_rel_paths(changed_paths);
        let Some(core_host) = self.core_host.as_mut() else {
            return Ok(false);
        };
        let changed_rust_paths = changed_paths
            .iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .cloned()
            .collect::<BTreeSet<_>>();
        if changed_rust_paths.is_empty() {
            return Ok(false);
        }
        let overlay = build_core_host(
            &self.analysis_host,
            &self.vfs,
            &self.workspace_root,
            &changed_rust_paths,
            self.workspace_epoch,
            self.content_revision,
            &build_spec,
        )?;
        if build_spec.impls {
            let Some(core_index) = self.core_index.as_ref() else {
                return Ok(false);
            };
            let previous = core_index.def_fingerprints_for_paths(&changed_rel_paths);
            let next = overlay.index.def_fingerprints_for_paths(&changed_rel_paths);
            if previous != next {
                return Ok(false);
            }
        }
        core_host.invalidate_paths(&changed_rel_paths);
        core_host.merge_from(overlay.host);
        if let Some(core_index) = self.core_index.as_mut() {
            core_index.invalidate_paths(&changed_rel_paths);
            core_index.merge_from(overlay.index);
        } else {
            self.core_index = Some(overlay.index);
        }
        self.core_index_complete = true;
        Ok(true)
    }

    fn changed_rel_paths(&self, changed_paths: &BTreeSet<PathBuf>) -> BTreeSet<String> {
        changed_paths
            .iter()
            .filter_map(|path| {
                path.strip_prefix(&self.workspace_root)
                    .ok()
                    .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            })
            .collect()
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
                let local_functions = self.local_functions.clone();
                provider_extract_call_edges(self, self.db, &local_functions);
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
                let local_rel_path = local.rel_path.clone();
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
                            local_rel_path.clone(),
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
                self.record_core_def(def_id, name, kind, path.as_str(), Some(local_rel_path.as_str()));
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

fn build_core_index(
    analysis_host: &AnalysisHost,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    tracked_files: &BTreeSet<PathBuf>,
    workspace_epoch: u64,
    content_revision: u64,
    build_spec: &CoreHostBuildSpec,
) -> Result<CoreLookupIndex, RaHostInitError> {
    let _ = (workspace_epoch, content_revision);
    build::build_core_index_only(
        analysis_host,
        vfs,
        workspace_root,
        tracked_files,
        build_spec,
    )
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

impl<'db> CallGraphProvider for CoreFactsBuilder<'db> {
    fn local_file(&self, file_id: vfs::FileId) -> Option<LocalFile> {
        self.files.get(&file_id).cloned()
    }

    fn register_function_def_for_call_graph(&mut self, function: hir::Function) -> Option<DefId> {
        CoreFactsBuilder::register_function_def(self, function)
    }

    fn register_adt_def_for_call_graph(&mut self, adt: Adt) -> DefId {
        CoreFactsBuilder::register_adt_def(self, adt)
    }

    fn register_variant_def_for_call_graph(&mut self, variant: hir::Variant) -> Option<DefId> {
        CoreFactsBuilder::register_module_def(self, ModuleDef::Variant(variant), false)
    }

    fn register_synthetic_callable_for_call_graph(
        &mut self,
        prefix: &str,
        syntax: &syntax::SyntaxNode,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) -> DefId {
        CoreFactsBuilder::register_synthetic_callable_def(self, prefix, syntax, file_id, local)
    }

    fn intern_call_site_for_call_graph(
        &mut self,
        file_id: span::EditionedFileId,
        rel_path: String,
        source_text: &str,
        range: syntax::TextRange,
    ) -> Option<SpanId> {
        self.host
            .intern_span_from_text(file_id, rel_path, source_text, range)
            .ok()
    }

    fn insert_call_id_for_call_graph(&mut self, call_id: crate::CallId, value: String) {
        self.host.insert_call_id(call_id, value);
    }

    fn insert_call_edge_for_call_graph(
        &mut self,
        caller: DefId,
        callee: DefId,
        site: SpanId,
        dispatch: crate::DispatchKind,
    ) {
        self.host.insert_call_edge(caller, callee, site, dispatch);
    }
}
