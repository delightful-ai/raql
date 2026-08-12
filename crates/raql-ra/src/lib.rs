//! RA-native semantic operators for RAQL (`docs/SPEC.md` §6).
//!
//! This crate extends rust-analyzer's Salsa database with RAQL's derived
//! queries and implements the `raql-plan` catalog's physical operators over
//! live RA snapshots. It is the only place that computes Rust semantic truth,
//! and it computes it exclusively through rust-analyzer.
//!
//! Module map:
//! - [`snapshot`] — the typed attachment discipline (`Snapshot`/`Attached`).
//! - [`calls`] — the call family: Salsa-tracked `raql_callees`, untracked
//!   `callers_of`, dispatch classification.
//! - [`def`] — the `Def` union of RA definition handles and its projections.
//! - [`value`] — the engine value model (SPEC §7), snapshot-scoped.
//! - [`operators`] — catalog operator dispatch over one snapshot.
//! - [`projection`] — the output boundary (SPEC §13): the only place RA
//!   values become text.

mod calls;
mod def;
mod operators;
mod projection;
mod snapshot;
mod value;

pub use calls::{CallSite, DispatchKind, callers_of, raql_callees, raql_callees_execution_count};
pub use def::{Def, DefKind};
pub use operators::{OperatorError, SnapshotOperators};
pub use projection::{Projected, ProjectedDef, project_def, project_file_range};
pub use snapshot::{Attached, Snapshot};
pub use value::{EnumTag, Position, Value};
