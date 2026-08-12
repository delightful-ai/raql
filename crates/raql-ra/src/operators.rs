//! Catalog operator dispatch over one RA snapshot (SPEC §8.1).
//!
//! `SnapshotOperators` implements `raql_plan::OperatorSet` for the SPEC §7
//! value model. `OperatorId` is a closed enum owned by the catalog, so the
//! dispatch below is exhaustive: a new catalog operator fails compilation
//! here until it is implemented — the registry cannot drift from the host.
//!
//! Database TLS attachment is typed, not disciplined: attached-context
//! operators take [`Attached`], the symbol-index seed takes
//! `&mut Snapshot`, and the borrow checker rejects mixing them (see
//! `snapshot.rs`).

mod calls;
mod def_at;
mod def_meta;
mod def_name;
mod scans;
mod span;

use std::path::PathBuf;

use ide_db::RootDatabase;
use raql_plan::{OperatorId, OperatorSet};

use crate::def::Def;
use crate::snapshot::Snapshot;
use crate::value::{Position, Value};

/// Operator invocation failure. Errors are errors (SPEC §8): callers must
/// never degrade to another access path or treat this as an empty relation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperatorError {
    /// The inputs violated the mode contract (wrong arity or value type) —
    /// a planner bug, not a user error.
    InvalidInput { operator: OperatorId, expected: &'static str },
}

impl std::fmt::Display for OperatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperatorError::InvalidInput { operator, expected } => {
                write!(f, "operator `{}` expected inputs: {expected}", operator.name())
            }
        }
    }
}

impl std::error::Error for OperatorError {}

/// The catalog operators evaluated against one snapshot. Holds the
/// snapshot plus request-level normalization config (the workspace root,
/// for workspace-relative projections) — no semantic state, no caches
/// (Salsa is the only memoization).
pub struct SnapshotOperators<'db> {
    snapshot: Snapshot<'db>,
    workspace_root: PathBuf,
}

impl<'db> SnapshotOperators<'db> {
    pub fn new(db: &'db RootDatabase, workspace_root: impl Into<PathBuf>) -> Self {
        Self { snapshot: Snapshot::new(db), workspace_root: workspace_root.into() }
    }
}

impl OperatorSet for SnapshotOperators<'_> {
    type Value = Value;
    type Error = OperatorError;

    fn invoke(
        &mut self,
        operator: OperatorId,
        inputs: &[Value],
    ) -> Result<Vec<Vec<Value>>, OperatorError> {
        match operator {
            OperatorId::NameOfDef => {
                let def = one_def(operator, inputs)?;
                self.snapshot.attached(|att| def_meta::name_of_def(att, def))
            }
            OperatorId::DefsByExactName => {
                def_name::defs_by_exact_name(&mut self.snapshot, one_string(operator, inputs)?)
            }
            OperatorId::KindOfDef => {
                let def = one_def(operator, inputs)?;
                self.snapshot.attached(|att| def_meta::kind_of_def(att, def))
            }
            OperatorId::CanonicalPathOfDef => {
                let def = one_def(operator, inputs)?;
                self.snapshot.attached(|att| def_meta::canonical_path_of_def(att, def))
            }
            OperatorId::SpanOfDef => {
                let def = one_def(operator, inputs)?;
                self.snapshot.attached(|att| def_meta::span_of_def(att, def))
            }
            OperatorId::DefAtPosition => {
                let (file, pos) = file_position(operator, inputs)?;
                self.snapshot.attached(|att| def_at::classify_at_position(att, file, pos))
            }
            OperatorId::CalleesOfFn | OperatorId::CallEdgesByCaller => {
                let def = one_def(operator, inputs)?;
                self.snapshot.attached(|att| calls::callees_of_fn(att, def))
            }
            OperatorId::CallersOfFn => {
                let def = one_def(operator, inputs)?;
                self.snapshot.attached(|att| calls::callers_of_fn(att, def))
            }
            OperatorId::CallEdgesByCallee => {
                let def = one_def(operator, inputs)?;
                self.snapshot.attached(|att| calls::call_edges_by_callee(att, def))
            }
            OperatorId::DefsScan => {
                no_inputs(operator, inputs)?;
                self.snapshot.attached(scans::defs_scan)
            }
            OperatorId::FnDefsScan => {
                no_inputs(operator, inputs)?;
                self.snapshot.attached(scans::fn_defs_scan)
            }
            OperatorId::DefNamesScan => {
                no_inputs(operator, inputs)?;
                self.snapshot.attached(scans::def_names_scan)
            }
            OperatorId::CallEdgesScan => {
                no_inputs(operator, inputs)?;
                self.snapshot.attached(scans::call_edges_scan)
            }
            OperatorId::IsPublicFilter => {
                let def = one_def(operator, inputs)?;
                self.snapshot.attached(|att| def_meta::is_public_filter(att, def))
            }
            OperatorId::InTestFilter => {
                let def = one_def(operator, inputs)?;
                self.snapshot.attached(|att| def_meta::in_test_filter(att, def))
            }
            OperatorId::SpanAllowedFilter => {
                let range = one_span(operator, inputs)?;
                self.snapshot.attached(|att| span::span_allowed(att, range))
            }
            OperatorId::HandleOfDef => {
                let def = one_def(operator, inputs)?;
                let workspace_root = &self.workspace_root;
                self.snapshot
                    .attached(|att| def_meta::handle_of_def(att, workspace_root, def))
            }
            OperatorId::SpanKeyOfSpan => {
                let range = one_span(operator, inputs)?;
                let workspace_root = &self.workspace_root;
                self.snapshot.attached(|att| span::span_key(att, workspace_root, range))
            }
        }
    }
}

fn one_def(operator: OperatorId, inputs: &[Value]) -> Result<Def, OperatorError> {
    match inputs {
        [Value::Def(def)] => Ok(*def),
        _ => Err(OperatorError::InvalidInput { operator, expected: "(Def)" }),
    }
}

fn one_span(operator: OperatorId, inputs: &[Value]) -> Result<ide_db::FileRange, OperatorError> {
    match inputs {
        [Value::FileRange(range)] => Ok(*range),
        _ => Err(OperatorError::InvalidInput { operator, expected: "(Span)" }),
    }
}

fn no_inputs(operator: OperatorId, inputs: &[Value]) -> Result<(), OperatorError> {
    if inputs.is_empty() {
        Ok(())
    } else {
        Err(OperatorError::InvalidInput { operator, expected: "()" })
    }
}

fn one_string<'i>(operator: OperatorId, inputs: &'i [Value]) -> Result<&'i str, OperatorError> {
    match inputs {
        [Value::String(s)] => Ok(s),
        _ => Err(OperatorError::InvalidInput { operator, expected: "(String)" }),
    }
}

fn file_position(
    operator: OperatorId,
    inputs: &[Value],
) -> Result<(ide_db::FileId, Position), OperatorError> {
    match inputs {
        [Value::File(file), Value::Position(pos)] => Ok((*file, *pos)),
        _ => Err(OperatorError::InvalidInput { operator, expected: "(File, Position)" }),
    }
}
