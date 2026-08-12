//! RAQL evaluation engine (SPEC §11): demand-driven execution of the
//! physical plan over the catalog operator boundary.
//!
//! The engine owns row semantics — joins, stratified negation,
//! aggregation, `choose_topk`, bounded reachability (`witness_path`/
//! `path_hop`), and per-request demand memoization (SPEC §9.2). It owns no
//! Rust semantic discovery: every extern goal is an
//! [`raql_plan::OperatorSet`] invocation, generic over the host's value
//! type through [`raql_plan::EngineValue`]. There is no bulk relation
//! materialization and no fallback access path: operator errors are
//! errors, never an empty relation (SPEC §8).
//!
//! Evaluation is unwind-safe by construction (SPEC §11.2): all state is
//! request-local, so a Salsa cancellation panic from inside an operator
//! unwinds cleanly through `execute` for the caller's `catch_unwind`.
#![forbid(unsafe_code)]

mod eval;
mod terms;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use indexmap::IndexSet;
use raql_compiler::PlannedProgram;
use raql_plan::{EngineValue, OperatorSet};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalStatus {
    Ok,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalNote {
    pub section: String,
    pub message: String,
}

/// The result of one request-scoped evaluation: the extents of the demand
/// roots (keyed by predicate name), plus `out_status`.
#[derive(Debug, Clone)]
pub struct EvalResult<V> {
    pub status: EvalStatus,
    pub notes: Vec<EvalNote>,
    pub relations: BTreeMap<String, IndexSet<Vec<V>>>,
    pub iterations: usize,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeError {
    #[error("division by zero")]
    DivisionByZero,
    #[error("integer overflow")]
    Overflow,
    #[error("unbound variable `{0}`")]
    UnboundVar(String),
    #[error("{context}")]
    TypeMismatchContext { context: String },
    #[error("operator `{operator}` failed: {message}")]
    OperatorFailed { operator: &'static str, message: String },
    #[error("input `{predicate}` cardinality violation: {context}")]
    ScalarCardinality { predicate: String, context: String },
    #[error("fixpoint iteration limit exceeded: max_iters={max_iters}")]
    IterationLimit { max_iters: usize },
    #[error("internal plan/provenance mismatch: {detail}")]
    Internal { detail: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RuntimeErrorCode {
    DivisionByZero,
    Overflow,
    UnboundVar,
    TypeMismatch,
    OperatorFailed,
    ScalarCardinality,
    IterationLimit,
    Internal,
}

impl RuntimeErrorCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DivisionByZero => "RAQL0901",
            Self::Overflow => "RAQL0902",
            Self::UnboundVar => "RAQL0904",
            Self::TypeMismatch => "RAQL0905",
            Self::OperatorFailed => "RAQL0909",
            Self::ScalarCardinality => "RAQL0907",
            Self::IterationLimit => "RAQL0910",
            Self::Internal => "RAQL0903",
        }
    }
}

impl RuntimeError {
    pub(crate) const fn code(&self) -> RuntimeErrorCode {
        match self {
            Self::DivisionByZero => RuntimeErrorCode::DivisionByZero,
            Self::Overflow => RuntimeErrorCode::Overflow,
            Self::UnboundVar(_) => RuntimeErrorCode::UnboundVar,
            Self::TypeMismatchContext { .. } => RuntimeErrorCode::TypeMismatch,
            Self::OperatorFailed { .. } => RuntimeErrorCode::OperatorFailed,
            Self::ScalarCardinality { .. } => RuntimeErrorCode::ScalarCardinality,
            Self::IterationLimit { .. } => RuntimeErrorCode::IterationLimit,
            Self::Internal { .. } => RuntimeErrorCode::Internal,
        }
    }
}

/// Execute the planned program: evaluate every demand root against the
/// operator set, with request-scoped memoization. `inputs` carries the
/// pre-bound input relations (selector seeds like `target_def`); rows for
/// input relations also come from program facts.
///
/// Runtime failures do not tear down the evaluation result shape: the
/// status degrades to `Partial`, the error lands in `notes` (coded), and
/// `out_status` says `"partial"` — partial results are labeled, never
/// silently returned (SPEC §4.4).
pub fn execute<Ops>(
    program: &PlannedProgram,
    inputs: &BTreeMap<String, Vec<Vec<Ops::Value>>>,
    ops: &mut Ops,
) -> EvalResult<Ops::Value>
where
    Ops: OperatorSet,
    Ops::Value: EngineValue,
    Ops::Error: std::fmt::Display,
{
    let mut evaluation = eval::Evaluation::new(program, inputs, ops);
    let mut relations = BTreeMap::<String, IndexSet<Vec<Ops::Value>>>::new();
    let mut notes = Vec::new();
    let mut status = EvalStatus::Ok;

    for root in &program.physical().roots {
        let name = program.lowered().logic.derived[root.predicate.0].name.clone();
        match evaluation.eval_root(root) {
            Ok(rows) => {
                relations.insert(name, rows);
            }
            Err(error) => {
                status = EvalStatus::Partial;
                notes.push(EvalNote {
                    section: "Errors".to_string(),
                    message: format!("runtime error [{}]: {error}", error.code().as_str()),
                });
                relations.entry(name).or_default();
            }
        }
    }
    if evaluation.hit_iteration_cap() {
        status = EvalStatus::Partial;
    }
    notes.extend(evaluation.take_notes());

    let iterations = evaluation.iterations();
    let mut out_status = IndexSet::new();
    out_status.insert(vec![Ops::Value::string(match status {
        EvalStatus::Ok => "ok",
        EvalStatus::Partial => "partial",
    })]);
    relations.insert("out_status".to_string(), out_status);

    EvalResult { status, notes, relations, iterations }
}
