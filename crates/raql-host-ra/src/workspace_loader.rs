use std::path::{Path, PathBuf};

use ide::RootDatabase;
use load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use project_model::{CargoConfig, ProjectManifest, RustLibSource};
use vfs::AbsPathBuf;

use crate::RaHostInitError;

const PROC_MACRO_UNAVAILABLE_NOTE: &str = "proc-macro server unavailable during workspace load; continuing with degraded macro expansion fidelity";

pub(crate) trait ProcMacroClientHandle: std::any::Any + std::fmt::Debug {}
impl<T> ProcMacroClientHandle for T where T: std::any::Any + std::fmt::Debug {}

#[derive(Debug)]
pub(crate) struct LoadedWorkspace {
    pub(crate) manifest_path: PathBuf,
    pub(crate) workspace_root: PathBuf,
    pub(crate) fast_mode: bool,
    pub(crate) db: RootDatabase,
    pub(crate) vfs: vfs::Vfs,
    pub(crate) init_notes: Vec<String>,
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

    load_from_manifest_with_options(manifest, false)
}

pub(crate) fn load_from_workspace_root_no_deps(
    root: &Path,
) -> Result<LoadedWorkspace, RaHostInitError> {
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

    load_from_manifest_with_options(manifest, true)
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

    load_from_manifest_with_options(manifest, false)
}

pub(crate) fn load_from_manifest_path_no_deps(
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

    load_from_manifest_with_options(manifest, true)
}

fn load_from_manifest_with_options(
    manifest: ProjectManifest,
    no_deps: bool,
) -> Result<LoadedWorkspace, RaHostInitError> {
    let manifest_path = PathBuf::from(manifest.manifest_path().to_string());
    let workspace_root = PathBuf::from(manifest.manifest_path().parent().to_string());
    let cargo_config = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        all_targets: !no_deps,
        set_test: !no_deps,
        no_deps,
        ..CargoConfig::default()
    };
    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: !no_deps,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
    };

    let (db, vfs, proc_macro_client) = load_workspace_with_config(
        workspace_root.as_path(),
        manifest_path.as_path(),
        &cargo_config,
        &load_config,
    )?;
    let init_notes = init_notes_for_proc_macro_client(&proc_macro_client);
    Ok(LoadedWorkspace {
        manifest_path,
        workspace_root,
        fast_mode: no_deps,
        db,
        vfs,
        init_notes,
        proc_macro_client,
    })
}

fn init_notes_for_proc_macro_client(
    proc_macro_client: &Option<Box<dyn ProcMacroClientHandle>>,
) -> Vec<String> {
    if proc_macro_client.is_none() {
        return vec![PROC_MACRO_UNAVAILABLE_NOTE.to_string()];
    }
    Vec::new()
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

#[cfg(test)]
mod tests {
    use super::{
        PROC_MACRO_UNAVAILABLE_NOTE, ProcMacroClientHandle, init_notes_for_proc_macro_client,
    };

    #[test]
    fn missing_proc_macro_client_emits_degraded_mode_note() {
        let notes = init_notes_for_proc_macro_client(&None);
        assert_eq!(notes, vec![PROC_MACRO_UNAVAILABLE_NOTE.to_string()]);
    }

    #[test]
    fn present_proc_macro_client_emits_no_degraded_mode_note() {
        let client = Some(Box::new("present".to_string()) as Box<dyn ProcMacroClientHandle>);
        let notes = init_notes_for_proc_macro_client(&client);
        assert!(notes.is_empty());
    }
}
