use std::path::{Path, PathBuf};
use std::time::Instant;

use ide::RootDatabase;
use load_cargo::{LoadCargoConfig, ProcMacroServerChoice, ProjectFolders, load_workspace_into_db};
use project_model::{CargoConfig, ProjectManifest, ProjectWorkspace, RustLibSource};
use toml::Value;
use vfs::AbsPathBuf;

use crate::RaHostInitError;

pub(crate) trait ProcMacroClientHandle: std::any::Any + std::fmt::Debug {}
impl<T> ProcMacroClientHandle for T where T: std::any::Any + std::fmt::Debug {}

#[derive(Debug)]
pub(crate) struct LoadedWorkspace {
    pub(crate) manifest_path: PathBuf,
    pub(crate) workspace_root: PathBuf,
    pub(crate) db: RootDatabase,
    pub(crate) vfs: vfs::Vfs,
    pub(crate) watched_entries: Vec<vfs::loader::Entry>,
    pub(crate) proc_macro_client: Option<Box<dyn ProcMacroClientHandle>>,
}

pub(crate) fn load_from_workspace_root(root: &Path) -> Result<LoadedWorkspace, RaHostInitError> {
    let load_started = Instant::now();
    let input_path = root.to_string_lossy().to_string();
    let canonical_started = Instant::now();
    let abs_root = canonical_abs(root).map_err(|details| RaHostInitError::WorkspaceNotFound {
        input_path: input_path.clone(),
        details,
    })?;
    trace_loader_timing("workspace_loader.canonical_abs_root", canonical_started.elapsed());

    let discover_started = Instant::now();
    let manifest = ProjectManifest::discover_single(abs_root.as_ref()).map_err(|err| {
        RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
            details: err.to_string(),
        }
    })?;
    trace_loader_timing("workspace_loader.discover_single", discover_started.elapsed());

    let loaded = load_from_manifest(manifest)?;
    trace_loader_timing("workspace_loader.from_workspace_root.total", load_started.elapsed());
    Ok(loaded)
}

pub(crate) fn load_from_manifest_path(
    manifest_path: &Path,
) -> Result<LoadedWorkspace, RaHostInitError> {
    let load_started = Instant::now();
    let input_path = manifest_path.to_string_lossy().to_string();
    let canonical_started = Instant::now();
    let abs_manifest =
        canonical_abs(manifest_path).map_err(|details| RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
            details,
        })?;
    trace_loader_timing("workspace_loader.canonical_abs_manifest", canonical_started.elapsed());

    let manifest_started = Instant::now();
    let manifest = ProjectManifest::from_manifest_file(abs_manifest).map_err(|err| {
        RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
            details: err.to_string(),
        }
    })?;
    trace_loader_timing("workspace_loader.from_manifest_file", manifest_started.elapsed());
    let loaded = load_from_manifest(manifest)?;
    trace_loader_timing("workspace_loader.from_manifest_path.total", load_started.elapsed());
    Ok(loaded)
}

fn load_from_manifest(manifest: ProjectManifest) -> Result<LoadedWorkspace, RaHostInitError> {
    let load_started = Instant::now();
    let manifest_path = PathBuf::from(manifest.manifest_path().to_string());
    let cargo_config = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        all_targets: true,
        set_test: true,
        no_deps: false,
        ..CargoConfig::default()
    };
    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: true,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
    };

    let workspace_load_started = Instant::now();
    let mut workspace =
        ProjectWorkspace::load(manifest, &cargo_config, &|_| {}).map_err(|err| {
            RaHostInitError::WorkspaceLoad {
                manifest: manifest_path.display().to_string(),
                details: err.to_string(),
            }
        })?;
    trace_loader_timing("workspace_loader.project_workspace_load", workspace_load_started.elapsed());

    if load_config.load_out_dirs_from_check {
        let build_scripts_started = Instant::now();
        let build_scripts = workspace
            .run_build_scripts(&cargo_config, &|_| {})
            .map_err(|err| RaHostInitError::WorkspaceLoad {
                manifest: manifest_path.display().to_string(),
                details: err.to_string(),
            })?;
        workspace.set_build_scripts(build_scripts);
        trace_loader_timing("workspace_loader.run_build_scripts", build_scripts_started.elapsed());
    }

    let workspace_root = PathBuf::from(workspace.workspace_root().to_string());
    let project_folders_started = Instant::now();
    let project_folders = ProjectFolders::new(std::slice::from_ref(&workspace), &[], None);
    let watched_entries = project_folders
        .watch
        .iter()
        .filter_map(|&idx| project_folders.load.get(idx).cloned())
        .collect::<Vec<_>>();
    trace_loader_timing("workspace_loader.project_folders", project_folders_started.elapsed());

    let lru_cap = std::env::var("RA_LRU_CAP")
        .ok()
        .and_then(|value| value.parse::<u16>().ok());
    let mut db = RootDatabase::new(lru_cap);
    let load_db_started = Instant::now();
    let (vfs, proc_macro_client) = load_workspace_into_db(
        workspace,
        &cargo_config.extra_env,
        &load_config,
        &mut db,
    )
    .map_err(|err| RaHostInitError::WorkspaceLoad {
        manifest: manifest_path.display().to_string(),
        details: err.to_string(),
    })?;
    trace_loader_timing("workspace_loader.load_workspace_into_db", load_db_started.elapsed());
    trace_loader_timing("workspace_loader.total", load_started.elapsed());
    Ok(LoadedWorkspace {
        manifest_path,
        workspace_root,
        db,
        vfs,
        watched_entries,
        proc_macro_client: proc_macro_client
            .map(|client| Box::new(client) as Box<dyn ProcMacroClientHandle>),
    })
}

fn canonical_abs(path: &Path) -> Result<AbsPathBuf, String> {
    std::fs::canonicalize(path)
        .map(AbsPathBuf::assert_utf8)
        .map_err(|err| err.to_string())
}

pub(crate) fn true_workspace_root(manifest_path: &Path) -> Result<PathBuf, RaHostInitError> {
    let canonical_manifest = std::fs::canonicalize(manifest_path).map_err(|err| {
        RaHostInitError::WorkspaceNotFound {
            input_path: manifest_path.display().to_string(),
            details: err.to_string(),
        }
    })?;
    if let Some(workspace_path) = package_workspace_override(canonical_manifest.as_path())? {
        return std::fs::canonicalize(workspace_path).map_err(|err| RaHostInitError::WorkspaceLoad {
            manifest: canonical_manifest.display().to_string(),
            details: err.to_string(),
        });
    }

    let manifest_dir = canonical_manifest.parent().ok_or_else(|| RaHostInitError::WorkspaceLoad {
        manifest: canonical_manifest.display().to_string(),
        details: "manifest path has no parent directory".to_string(),
    })?;
    for dir in manifest_dir.ancestors() {
        let candidate = dir.join("Cargo.toml");
        if !candidate.exists() {
            continue;
        }
        if manifest_declares_workspace(candidate.as_path())? {
            return std::fs::canonicalize(dir).map_err(|err| RaHostInitError::WorkspaceLoad {
                manifest: candidate.display().to_string(),
                details: err.to_string(),
            });
        }
    }
    Ok(manifest_dir.to_path_buf())
}

fn manifest_declares_workspace(path: &Path) -> Result<bool, RaHostInitError> {
    let value = manifest_value(path)?;
    Ok(value
        .as_table()
        .and_then(|table| table.get("workspace"))
        .is_some())
}

fn package_workspace_override(path: &Path) -> Result<Option<PathBuf>, RaHostInitError> {
    let value = manifest_value(path)?;
    let override_path = value
        .as_table()
        .and_then(|table| table.get("package"))
        .and_then(Value::as_table)
        .and_then(|package| package.get("workspace"))
        .and_then(Value::as_str);
    Ok(override_path.map(|workspace| {
        path.parent()
            .expect("manifest path has parent")
            .join(workspace)
    }))
}

fn manifest_value(path: &Path) -> Result<Value, RaHostInitError> {
    let text = std::fs::read_to_string(path).map_err(|err| RaHostInitError::WorkspaceLoad {
        manifest: path.display().to_string(),
        details: err.to_string(),
    })?;
    toml::from_str(&text).map_err(|err| RaHostInitError::WorkspaceLoad {
        manifest: path.display().to_string(),
        details: err.to_string(),
    })
}

fn trace_loader_timing(label: &str, elapsed: std::time::Duration) {
    if std::env::var_os("RAQL_TRACE_TIMINGS").is_none() {
        return;
    }
    eprintln!("raql-timing {label} {}ms", elapsed.as_millis());
}
