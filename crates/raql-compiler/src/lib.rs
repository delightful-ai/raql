//! RAQL compiler pipeline: resolve -> typecheck -> plan.
#![forbid(unsafe_code)]

mod diagnostics;
mod expand;
mod externs;
mod lower;
mod plan;
mod program;
mod resolve;
mod strata;
mod typecheck;

pub use diagnostics::{CompilerDiagnostic, DiagBundle};
pub use lower::{
    DerivedProvenance, GoalPath, InputKind, InputProvenance, LoweredProgram, RuleProvenance,
};
pub use plan::{
    PlannedProgram, plan, plan_with_options, reachable_predicates, required_extern_capabilities,
};
pub use program::{
    CompilerType, EnumDecl, ModeDir, ModeSig, PredicateDecl, ResolvedProgram, TypedProgram,
    TypedRule,
};
pub use resolve::resolve;
pub use strata::{DepKind, SccPlan};
pub use typecheck::typecheck;

#[cfg(test)]
mod tests;
