//! The RA-backed session: one loaded workspace, kept in sync, answering
//! planned programs.
//!
//! Everything here is lifecycle — load, watch, apply changes, warm, run.
//! Semantic truth belongs to `raql-ra` (SPEC §6/§8), row semantics to
//! `raql-engine` (SPEC §11), and rendering to [`crate::projection`]
//! (SPEC §13.1). The tracked-file/mtime machinery below is the pre-§12
//! pipeline, kept as-is until the workspace actor replaces it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use ide::AnalysisHost;
use project_model::ProjectManifest;
use raql_compiler::PlannedProgram;
use raql_host::CapabilitySet;
use vfs::{AbsPathBuf, VfsPath};

use crate::RaHostInitError;
use crate::capability::supported_capabilities;
use crate::projection::{ProjectedRunResult, project_result};
use crate::workspace_loader;

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
    tracked_files: BTreeSet<PathBuf>,
    tracked_file_states: BTreeMap<PathBuf, WatchedFileState>,
    tracked_dirs: BTreeSet<PathBuf>,
    tracked_dir_states: BTreeMap<PathBuf, WatchedFileState>,
    watcher: WorkspaceWatcher,
    watched_entries: Vec<vfs::loader::Entry>,
    build_script_rerun_paths: BTreeSet<PathBuf>,
    auxiliary_build_inputs: BTreeMap<PathBuf, Option<WatchedFileState>>,
    workspace_epoch: u64,
    content_revision: u64,
}

pub struct WarmupSnapshot(ide::Analysis, std::sync::Arc<[ide::Crate]>);

impl WarmupSnapshot {
    pub fn parallel_prime_caches(&self, worker_threads: usize) -> Result<(), String> {
        self.0
            .parallel_prime_caches(&self.1, worker_threads, |_| {})
            .map_err(|err| format!("rust-analyzer cache priming cancelled: {err:?}"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WatchedFileState {
    len: u64,
    modified_unix_nanos: Option<u128>,
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
            tracked_files,
            tracked_file_states,
            tracked_dirs,
            tracked_dir_states,
            watcher,
            watched_entries: loaded.watched_entries,
            build_script_rerun_paths,
            auxiliary_build_inputs,
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
        // TODO(ra-bump b2d445b22a): `parallel_prime_caches` now takes an explicit crate scope; `all_crates` keeps the previous prime-everything behavior.
        let prime_scope = base_db::all_crates(self.analysis_host.raw_database());
        WarmupSnapshot(self.analysis_host.analysis(), prime_scope)
    }

    pub fn content_revision(&self) -> u64 {
        self.content_revision
    }

    pub fn supported_capabilities(&self) -> CapabilitySet {
        supported_capabilities()
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
        // TODO(ra-bump b2d445b22a): `parallel_prime_caches` now takes an explicit crate scope; `all_crates` keeps the previous prime-everything behavior.
        let prime_scope = base_db::all_crates(self.analysis_host.raw_database());
        self.analysis_host
            .analysis()
            .parallel_prime_caches(&prime_scope, worker_threads, |_| {})
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

    /// Run a planned program against the current snapshot.
    ///
    /// The whole execution is one snapshot: sync first, then evaluate
    /// demand-driven through `raql-ra`'s catalog operators, then project
    /// (SPEC §13.1) before anything leaves. `raql_ra::Value`s hold live RA
    /// handles, so they must not outlive the borrow of the database that
    /// produced them.
    pub fn run_planned(
        &mut self,
        planned: &PlannedProgram,
    ) -> Result<ProjectedRunResult, RaHostInitError> {
        let run_started = Instant::now();
        let sync_started = Instant::now();
        self.sync_workspace()?;
        trace_timing("workspace_service.sync_workspace", sync_started.elapsed());

        let execute_started = Instant::now();
        let db = self.analysis_host.raw_database();
        let mut ops = raql_ra::SnapshotOperators::new(db, &self.workspace_root);
        let result = raql_engine::execute(planned, &BTreeMap::new(), &mut ops);
        trace_timing("workspace_service.execute", execute_started.elapsed());

        let project_started = Instant::now();
        let projected = project_result(db, &self.workspace_root, result);
        trace_timing("workspace_service.project_result", project_started.elapsed());
        trace_timing("workspace_service.run_planned.total", run_started.elapsed());
        Ok(projected)
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
            saw_change = true;
        }
        let _ = self.vfs.take_changes();
        if saw_change {
            self.analysis_host.apply_change(change);
            self.content_revision = self.content_revision.saturating_add(1);
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
        self.tracked_files = tracked_files;
        self.tracked_file_states = tracked_file_states;
        self.tracked_dirs = tracked_dirs;
        self.tracked_dir_states = tracked_dir_states;
        self.watcher = WorkspaceWatcher::new(&watched_entries);
        self.watched_entries = watched_entries;
        self.build_script_rerun_paths = build_script_rerun_paths;
        self.auxiliary_build_inputs = auxiliary_build_inputs;
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

fn trace_timing(label: &str, elapsed: std::time::Duration) {
    if std::env::var_os("RAQL_TRACE_TIMINGS").is_none() {
        return;
    }
    eprintln!("raql-timing {label} {}ms", elapsed.as_millis());
}
