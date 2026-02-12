use std::path::{Path, PathBuf};

use ide::RootDatabase;
use load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use project_model::{CargoConfig, ProjectManifest, RustLibSource};
use vfs::AbsPathBuf;

use crate::RaHostInitError;

pub(crate) trait ProcMacroClientHandle: std::any::Any + std::fmt::Debug {}
impl<T> ProcMacroClientHandle for T where T: std::any::Any + std::fmt::Debug {}

#[derive(Debug)]
pub(crate) struct LoadedWorkspace {
    pub(crate) workspace_root: PathBuf,
    pub(crate) db: RootDatabase,
    pub(crate) vfs: vfs::Vfs,
    pub(crate) proc_macro_client: Box<dyn ProcMacroClientHandle>,
}

pub(crate) fn load_from_workspace_root(root: &Path) -> Result<LoadedWorkspace, RaHostInitError> {
    let input_path = root.to_string_lossy().to_string();
    let abs_root = canonical_abs(root).ok_or_else(|| RaHostInitError::WorkspaceNotFound {
        input_path: input_path.clone(),
    })?;

    let manifest = ProjectManifest::discover_single(abs_root.as_ref()).map_err(|_| {
        RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
        }
    })?;

    load_from_manifest(manifest)
}

pub(crate) fn load_from_manifest_path(
    manifest_path: &Path,
) -> Result<LoadedWorkspace, RaHostInitError> {
    let input_path = manifest_path.to_string_lossy().to_string();
    let abs_manifest =
        canonical_abs(manifest_path).ok_or_else(|| RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
        })?;

    let manifest = ProjectManifest::from_manifest_file(abs_manifest).map_err(|_| {
        RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
        }
    })?;

    load_from_manifest(manifest)
}

fn load_from_manifest(manifest: ProjectManifest) -> Result<LoadedWorkspace, RaHostInitError> {
    let manifest_path = PathBuf::from(manifest.manifest_path().to_string());
    let workspace_root = PathBuf::from(manifest.manifest_path().parent().to_string());
    let cargo_config = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        all_targets: true,
        set_test: true,
        ..CargoConfig::default()
    };
    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: true,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
    };

    let (db, vfs, proc_macro_client) = load_workspace_at(
        workspace_root.as_path(),
        &cargo_config,
        &load_config,
        &|_| {},
    )
    .map_err(|err| RaHostInitError::WorkspaceLoad {
        manifest: manifest_path.to_string_lossy().to_string(),
        details: err.to_string(),
    })?;

    let proc_macro_client = proc_macro_client.ok_or(RaHostInitError::ProcMacroUnavailable)?;

    Ok(LoadedWorkspace {
        workspace_root,
        db,
        vfs,
        proc_macro_client: Box::new(proc_macro_client),
    })
}

fn canonical_abs(path: &Path) -> Option<AbsPathBuf> {
    std::fs::canonicalize(path)
        .ok()
        .map(AbsPathBuf::assert_utf8)
}
