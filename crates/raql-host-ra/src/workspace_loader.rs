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
    pub(crate) manifest_path: PathBuf,
    pub(crate) workspace_root: PathBuf,
    pub(crate) db: RootDatabase,
    pub(crate) vfs: vfs::Vfs,
    pub(crate) proc_macro_client: Option<Box<dyn ProcMacroClientHandle>>,
}

pub(crate) fn load_from_workspace_root(root: &Path) -> Result<LoadedWorkspace, RaHostInitError> {
    let input_path = root.to_string_lossy().to_string();
    let abs_root = canonical_abs(root).map_err(|details| RaHostInitError::WorkspaceNotFound {
        input_path: input_path.clone(),
        details,
    })?;

    let manifest = ProjectManifest::discover_single(abs_root.as_ref()).map_err(|err| {
        RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
            details: err.to_string(),
        }
    })?;

    load_from_manifest(manifest)
}

pub(crate) fn load_from_manifest_path(
    manifest_path: &Path,
) -> Result<LoadedWorkspace, RaHostInitError> {
    let input_path = manifest_path.to_string_lossy().to_string();
    let abs_manifest =
        canonical_abs(manifest_path).map_err(|details| RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
            details,
        })?;

    let manifest = ProjectManifest::from_manifest_file(abs_manifest).map_err(|err| {
        RaHostInitError::WorkspaceNotFound {
            input_path: input_path.clone(),
            details: err.to_string(),
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
        no_deps: false,
        ..CargoConfig::default()
    };
    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: true,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
    };

    let (db, vfs, proc_macro_client) = load_workspace_with_config(
        workspace_root.as_path(),
        manifest_path.as_path(),
        &cargo_config,
        &load_config,
    )?;
    Ok(LoadedWorkspace {
        manifest_path,
        workspace_root,
        db,
        vfs,
        proc_macro_client,
    })
}

fn load_workspace_with_config(
    workspace_root: &Path,
    manifest_path: &Path,
    cargo_config: &CargoConfig,
    load_config: &LoadCargoConfig,
) -> Result<
    (
        RootDatabase,
        vfs::Vfs,
        Option<Box<dyn ProcMacroClientHandle>>,
    ),
    RaHostInitError,
> {
    let (db, vfs, proc_macro_client) =
        load_workspace_at(workspace_root, cargo_config, load_config, &|_| {}).map_err(|err| {
            RaHostInitError::WorkspaceLoad {
                manifest: manifest_path.to_string_lossy().to_string(),
                details: err.to_string(),
            }
        })?;
    Ok((
        db,
        vfs,
        proc_macro_client.map(|client| Box::new(client) as Box<dyn ProcMacroClientHandle>),
    ))
}

fn canonical_abs(path: &Path) -> Result<AbsPathBuf, String> {
    std::fs::canonicalize(path)
        .map(AbsPathBuf::assert_utf8)
        .map_err(|err| err.to_string())
}
