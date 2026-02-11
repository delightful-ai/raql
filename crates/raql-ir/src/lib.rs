//! Foundational intermediate representation for RAQL.
//!
//! The crate models query programs as a phase-typed internal `Program` to
//! keep AST, typed, and planned IR states distinct at compile time.
#![forbid(unsafe_code)]
#![allow(dead_code)]

// The module/API structure pass intentionally narrowed crate exports, but we
// still keep the internal IR layers in-tree for phased wiring across crates.
// Suppress dead-code noise at the module boundary so workspace test output
// remains signal-heavy while those layers are still crate-private.
mod ast;
mod diag;
mod ids;
mod phase;
mod plan;
mod program;
mod ty;
mod typed;
mod value;

pub use ids::StableId;
pub use value::ScalarValue;
