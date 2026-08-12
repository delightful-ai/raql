//! The output boundary (SPEC §13): the only place RA values become text.
//!
//! Projections are exactly that — projections. They are never semantic
//! identity, and a value that cannot be projected is typed as
//! [`Projected::Unprojectable`] with its reason; rendering it produces the
//! SPEC §4.4 `<unprojectable:reason>` marker. Nothing here fabricates a
//! path or ID.

use std::fmt;
use std::path::Path;

use ide_db::RootDatabase;

use crate::def::{Def, DefKind};

/// A projection result: text, or an explicit refusal with its reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Projected {
    Text(String),
    Unprojectable(&'static str),
}

impl Projected {
    fn from_option(value: Option<String>, reason: &'static str) -> Projected {
        match value {
            Some(text) => Projected::Text(text),
            None => Projected::Unprojectable(reason),
        }
    }
}

impl fmt::Display for Projected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Projected::Text(text) => f.write_str(text),
            Projected::Unprojectable(reason) => write!(f, "<unprojectable:{reason}>"),
        }
    }
}

/// The §13.1 projection of one definition: kind + canonical path + location
/// + handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedDef {
    pub kind: DefKind,
    pub path: Projected,
    /// `workspace-relative-path:line:col-line:col` (1-based, primary source
    /// location).
    pub location: Projected,
    /// SPEC §13.2 handle.
    pub handle: Projected,
}

/// Project a definition for output (SPEC §13.1).
pub fn project_def(db: &RootDatabase, workspace_root: &Path, def: Def) -> ProjectedDef {
    crate::snapshot::with_attached(db, |att| project_def_attached(att.db(), workspace_root, def))
}

fn project_def_attached(db: &RootDatabase, workspace_root: &Path, def: Def) -> ProjectedDef {
    let kind = def.kind(db);
    let canonical_path = def.canonical_path(db);
    let location = def
        .original_span(db)
        .and_then(|range| project_file_range(db, workspace_root, range));
    let handle = match handle_text(db, workspace_root, def) {
        Ok(text) => Projected::Text(text),
        Err(reason) => Projected::Unprojectable(reason),
    };
    ProjectedDef {
        kind,
        path: Projected::from_option(canonical_path, "no-canonical-path"),
        location: Projected::from_option(location, "no-primary-location"),
        handle,
    }
}

/// The SPEC §13.2 handle text of a definition, or the typed reason there is
/// none. Callers must hold an attached scope. Shared by [`project_def`] and
/// the `handle` operator, so the predicate and the output boundary cannot
/// drift.
pub(crate) fn handle_text(
    db: &RootDatabase,
    workspace_root: &Path,
    def: Def,
) -> Result<String, &'static str> {
    if let Def::Impl(impl_) = def {
        return impl_handle_text(db, workspace_root, impl_);
    }
    let path = def.canonical_path(db).ok_or("no-canonical-path")?;
    Ok(format!("@H:{}:{path}", def.kind(db).handle_kind()))
}

/// Impl handles (SPEC §13.2): the self-type's canonical path, with a
/// `#ordinal` suffix only when several impls share it, numbered in stable
/// source order (workspace-relative path, then range start). Impls on
/// non-ADT self types (references, tuples, type params) have no honest
/// path and no handle.
fn impl_handle_text(
    db: &RootDatabase,
    workspace_root: &Path,
    impl_: hir::Impl,
) -> Result<String, &'static str> {
    let no_path = "impl-self-type-has-no-canonical-path";
    let self_adt = impl_.self_ty(db).as_adt().ok_or(no_path)?;
    let path = Def::Adt(self_adt).canonical_path(db).ok_or(no_path)?;

    // Peers = impls whose self type is the same ADT constructor (RA's own
    // "impls for type" notion, filtered to exact ADT identity — blanket
    // impls and other constructors drop out).
    let mut peers: Vec<hir::Impl> = hir::Impl::all_for_type(db, impl_.self_ty(db))
        .into_iter()
        .filter(|peer| peer.self_ty(db).as_adt() == Some(self_adt))
        .collect();
    if peers.len() <= 1 {
        return Ok(format!("@H:impl:{path}"));
    }
    peers.sort_by_cached_key(|peer| impl_source_key(db, workspace_root, *peer));
    let ordinal = peers
        .iter()
        .position(|peer| *peer == impl_)
        .ok_or("impl-outside-peer-enumeration")?;
    Ok(format!("@H:impl:{path}#{ordinal}"))
}

/// Deterministic source-order key for impl ordinals: primary-source
/// location projected to workspace-relative path + range start (stable
/// across sessions, unlike raw `FileId`s). Impls without a primary
/// location sort last.
fn impl_source_key(
    db: &RootDatabase,
    workspace_root: &Path,
    impl_: hir::Impl,
) -> (u8, String, u32) {
    let Some(range) = Def::Impl(impl_).original_span(db) else {
        return (1, String::new(), 0);
    };
    let path = workspace_relative_path(db, workspace_root, range.file_id).unwrap_or_default();
    (0, path, u32::from(range.range.start()))
}

/// Project a file range as `workspace-relative-path:line:col-line:col`
/// (1-based lines and columns, editor convention; engine-level span values
/// stay 0-based per `LineIndex`).
pub fn project_file_range(
    db: &RootDatabase,
    workspace_root: &Path,
    range: ide_db::FileRange,
) -> Option<String> {
    let rel_path = workspace_relative_path(db, workspace_root, range.file_id)?;
    let line_index = ide_db::line_index(db, range.file_id);
    let start = line_index.try_line_col(range.range.start())?;
    let end = line_index.try_line_col(range.range.end())?;
    Some(format!(
        "{rel_path}:{}:{}-{}:{}",
        start.line + 1,
        start.col + 1,
        end.line + 1,
        end.col + 1,
    ))
}

/// The workspace-relative path of a file, from RA's own source roots (no
/// filesystem access, no VFS handle needed).
pub(crate) fn workspace_relative_path(
    db: &RootDatabase,
    workspace_root: &Path,
    file_id: ide_db::FileId,
) -> Option<String> {
    use ide_db::base_db::SourceDatabase as _;

    let source_root_id = db.file_source_root(file_id).source_root_id(db);
    let source_root = db.source_root(source_root_id).source_root(db);
    let vfs_path = source_root.path_for_file(&file_id)?;
    let abs_path = vfs_path.as_path()?;
    let path: &Path = abs_path.as_ref();
    let rel = path.strip_prefix(workspace_root).unwrap_or(path);
    Some(rel.to_string_lossy().replace('\\', "/"))
}
