//! RAQL compiler pipeline: resolve -> typecheck -> plan.
#![forbid(unsafe_code)]

mod diagnostics;
mod expand;
mod plan;
mod program;
mod resolve;
mod strata;
mod typecheck;

pub use diagnostics::{CompilerDiagnostic, DiagBundle};
pub use plan::{
    ExternLookupPlan, GoalPlan, PlannedProgram, PlannedRule, RulePlan, plan, reachable_predicates,
    required_extern_capabilities,
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
