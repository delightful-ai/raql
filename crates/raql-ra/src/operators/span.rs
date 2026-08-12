//! Span-family operators (SPEC §8.6 `span_allowed`, `span_key`).

use ide_db::base_db::SourceDatabase as _;

use crate::operators::OperatorError;
use crate::projection::workspace_relative_path;
use crate::snapshot::Attached;
use crate::value::Value;

/// `span_allowed(+S)`: request-scope filter, not a semantic fact. v0
/// scope: the span's file lies in a workspace-local source root (library
/// roots — dependencies, sysroot — are out of scope). Request scope
/// options compile onto this filter when they land (SPEC §14).
pub(super) fn span_allowed(
    att: &Attached<'_>,
    range: ide_db::FileRange,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let db = att.db();
    let source_root_id = db.file_source_root(range.file_id).source_root_id(db);
    let source_root = db.source_root(source_root_id).source_root(db);
    if source_root.is_library {
        return Ok(Vec::new());
    }
    Ok(vec![vec![Value::FileRange(range)]])
}

/// `span_key(+S, -Path, -L0, -C0, -L1, -C1)`: workspace-relative location
/// projection, zero-based `LineIndex` lines/columns (the machine ordering
/// key; the output boundary renders 1-based). Spans whose file has no
/// source-root path or whose range is out of bounds have no row.
pub(super) fn span_key(
    att: &Attached<'_>,
    workspace_root: &std::path::Path,
    range: ide_db::FileRange,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let db = att.db();
    let Some(rel_path) = workspace_relative_path(db, workspace_root, range.file_id) else {
        return Ok(Vec::new());
    };
    let line_index = ide_db::line_index(db, range.file_id);
    let (Some(start), Some(end)) = (
        line_index.try_line_col(range.range.start()),
        line_index.try_line_col(range.range.end()),
    ) else {
        return Ok(Vec::new());
    };
    Ok(vec![vec![
        Value::FileRange(range),
        Value::string(rel_path),
        Value::Int(i64::from(start.line)),
        Value::Int(i64::from(start.col)),
        Value::Int(i64::from(end.line)),
        Value::Int(i64::from(end.col)),
    ]])
}
