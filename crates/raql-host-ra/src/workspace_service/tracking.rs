use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::WatchedFileState;
use crate::RaHostInitError;

pub(super) fn tracked_workspace_state(
    _db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    watched_entries: &[vfs::loader::Entry],
    manifest_path: &Path,
    workspace_root: &Path,
) -> Result<BTreeSet<PathBuf>, RaHostInitError> {
    let mut tracked_files = vfs
        .iter()
        .filter_map(|(_, path)| {
            let abs = path.as_path()?;
            if !watched_entries.iter().any(|entry| entry.contains_file(abs)) {
                return None;
            }
            let path: &Path = abs.as_ref();
            is_relevant_workspace_file(path).then(|| path.to_path_buf())
        })
        .collect::<BTreeSet<_>>();
    tracked_files.extend(explicit_watched_files(watched_entries));
    tracked_files.extend(workspace_watch_files(manifest_path, workspace_root));
    Ok(tracked_files)
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

pub(super) fn tracked_directory_watch_set(
    tracked_files: &BTreeSet<PathBuf>,
    watched_entries: &[vfs::loader::Entry],
) -> BTreeSet<PathBuf> {
    let mut dirs = BTreeSet::new();
    for path in tracked_files {
        if let Some(parent) = path.parent() {
            dirs.insert(parent.to_path_buf());
        }
    }
    for entry in watched_entries {
        match entry {
            vfs::loader::Entry::Files(paths) => {
                for path in paths {
                    let path: &Path = path.as_ref();
                    if let Some(parent) = path.parent() {
                        dirs.insert(parent.to_path_buf());
                    }
                }
            }
            vfs::loader::Entry::Directories(directories) => {
                for include in &directories.include {
                    let path: &Path = include.as_ref();
                    dirs.insert(path.to_path_buf());
                }
            }
        }
    }
    dirs
}

pub(super) fn build_script_rerun_paths(
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, RaHostInitError> {
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

pub(super) fn auxiliary_build_input_state(
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

pub(super) fn tracked_file_state_map(
    tracked_files: &BTreeSet<PathBuf>,
) -> Result<BTreeMap<PathBuf, WatchedFileState>, RaHostInitError> {
    let mut states = BTreeMap::new();
    for path in tracked_files {
        states.insert(path.clone(), watched_file_state(path)?);
    }
    Ok(states)
}

pub(super) fn tracked_path_state(
    path: &Path,
) -> Result<Option<WatchedFileState>, RaHostInitError> {
    watched_path_state(path)
}

fn scan_all_files(dir: &Path, out: &mut BTreeSet<PathBuf>) -> Result<(), RaHostInitError> {
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

pub(super) fn path_requires_reload(
    path: &Path,
    build_script_rerun_paths: &BTreeSet<PathBuf>,
) -> bool {
    if path.file_name().and_then(|value| value.to_str()) == Some("build.rs") {
        return true;
    }
    if build_script_rerun_paths.contains(path) {
        return true;
    }
    !path.extension().is_some_and(|ext| ext == "rs")
}
