//! The host-side output boundary (SPEC §13.1).
//!
//! Engine values are snapshot-scoped: `raql_ra::Value` holds live RA
//! handles and `FileId`s that are meaningless outside the snapshot that
//! produced them. Everything leaving [`crate::workspace_service`] passes
//! through here first, so no RA identity ever reaches the daemon, the
//! protocol, or a cache.
//!
//! Rendering is delegated to `raql_ra::projection`, which is the only
//! place RA values become text. A value that cannot be projected renders
//! as `<unprojectable:reason>` (SPEC §4.4) — never a fabricated path or
//! id.

use std::collections::BTreeMap;
use std::path::Path;

use ide_db::RootDatabase;
use raql_engine::{EvalNote, EvalResult, EvalStatus};
use raql_ra::Value;

/// One projected value: plain, serializable data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectedValue {
    Int(i64),
    String(String),
    Bool(bool),
    Enum { name: String, variant: String },
    None,
    Some(Box<ProjectedValue>),
    List(Vec<ProjectedValue>),
}

/// One projected evaluation result: the §13.1 projection of every demand
/// root's extent, plus the run's status envelope.
#[derive(Clone, Debug)]
pub struct ProjectedRunResult {
    pub status: EvalStatus,
    pub notes: Vec<EvalNote>,
    pub iterations: usize,
    pub relations: BTreeMap<String, Vec<Vec<ProjectedValue>>>,
}

/// Project a finished evaluation against the snapshot that produced it.
pub(crate) fn project_result(
    db: &RootDatabase,
    workspace_root: &Path,
    result: EvalResult<Value>,
) -> ProjectedRunResult {
    let relations = result
        .relations
        .into_iter()
        .map(|(name, rows)| {
            let rows = rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|value| project_value(db, workspace_root, value))
                        .collect()
                })
                .collect();
            (name, rows)
        })
        .collect();
    ProjectedRunResult {
        status: result.status,
        notes: result.notes,
        iterations: result.iterations,
        relations,
    }
}

fn project_value(db: &RootDatabase, workspace_root: &Path, value: Value) -> ProjectedValue {
    match value {
        Value::Def(def) => {
            let projected = raql_ra::project_def(db, workspace_root, def);
            ProjectedValue::String(format!(
                "{} {} @ {}",
                projected.kind.tag(),
                projected.path,
                projected.location
            ))
        }
        Value::FileRange(range) => ProjectedValue::String(
            raql_ra::project_file_range(db, workspace_root, range)
                .unwrap_or_else(|| "<unprojectable:span-outside-workspace>".to_string()),
        ),
        // A bare `File` has no honest rendering of its own: the file id is
        // snapshot-scoped identity, and a path would be a different value
        // than the one the engine held.
        Value::File(_) => ProjectedValue::String("<unprojectable:raw-file-id>".to_string()),
        // Engine positions are 0-based (`LineIndex` convention); output is
        // 1-based (editor convention), matching span rendering.
        Value::Position(position) => {
            ProjectedValue::String(format!("{}:{}", position.line + 1, position.col + 1))
        }
        Value::String(text) => ProjectedValue::String(text.to_string()),
        Value::Int(value) => ProjectedValue::Int(value),
        Value::Bool(value) => ProjectedValue::Bool(value),
        Value::Enum(tag) => ProjectedValue::Enum {
            name: tag.ty.to_string(),
            variant: tag.variant.to_string(),
        },
        Value::Option(None) => ProjectedValue::None,
        Value::Option(Some(inner)) => {
            ProjectedValue::Some(Box::new(project_value(db, workspace_root, *inner)))
        }
        Value::List(items) => ProjectedValue::List(
            items
                .into_iter()
                .map(|item| project_value(db, workspace_root, item))
                .collect(),
        ),
    }
}
