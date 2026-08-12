//! The RAQL predicate catalog and (eventually) the binding-aware planner
//! (`docs/SPEC.md` §8–§10).
//!
//! The catalog is the single source of truth for extern predicates: name,
//! typed argument schema, documentation, completeness class, and the full set
//! of supported binding modes, each with a cost class and operator binding
//! (SPEC §8.1). Everything else — compiler signatures, operator dispatch,
//! `raql capabilities` output, reference docs, conformance skeletons — is
//! generated from this registry. A predicate not in the registry does not
//! exist; a mode not in the registry is a plan error.
//!
//! This crate owns no execution and no RA types. Operator implementations
//! live in `raql-ra` and are reached through the [`OperatorSet`] trait, which
//! is generic over the host's value type.

mod capabilities;
mod catalog;
mod mode;
mod operator;
mod predicate;
mod schema;

pub use catalog::{Catalog, v0_catalog};
pub use mode::{AccessKind, Binding, CostClass, ModeDef};
pub use operator::{OperatorId, OperatorSet};
pub use predicate::{Completeness, PredicateDef};
pub use schema::{ArgDef, ArgType};
