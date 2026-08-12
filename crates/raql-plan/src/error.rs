//! Plan errors and their message contracts (SPEC §10.3, §4.4).
//!
//! `RAQL0301` (unsatisfiable modes) and `RAQL0310` (scan denied) are
//! SPEC-normative. The remaining planning-family codes are assigned here:
//! `RAQL0302` disabled predicate (the §4.4 capability error), `RAQL0303`
//! declared mode not inferable (§9.1), `RAQL0304` malformed planner input
//! (defensive; the lang layer validates first).
//!
//! Errors are structured data with a `Display` that renders the full
//! contract: agents read these messages, so every field the SPEC promises
//! is present — the goal with argument names, every declared mode with its
//! cost, the bindings at the failure point, and the minimal unlock sets.

use std::fmt;

use crate::logic::DerivedId;
use crate::mode::CostClass;

/// Where an unsatisfiable goal lives in the planner input, so the lang
/// layer can map the error back to a source span: the owning derived
/// predicate, the rule index within it, and the goal's index in that
/// rule's body. Not part of the rendered message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GoalLocation {
    pub predicate: DerivedId,
    pub rule_index: usize,
    pub source_index: usize,
}

/// One access-path alternative in a `RAQL0301` listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeAlternative {
    /// `call_edge(+Caller, -, -, -)` — the mode over the goal's own
    /// argument names.
    pub signature: String,
    /// Short operator label (`by-caller`) or `derived`/`input`/`builtin`.
    pub via: String,
    pub cost: Option<CostClass>,
    pub is_scan: bool,
    /// The mode exists but `deny_scans` excluded it.
    pub denied: bool,
}

/// The `RAQL0301` payload (SPEC §10.3 message contract).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsatisfiableGoal {
    /// The offending goal with argument names: `call_edge(Caller, ...)`.
    pub goal: String,
    /// Argument names bound at the failure point, in argument order.
    pub bound: Vec<String>,
    /// Every declared mode / supported pattern of the goal's predicate.
    pub alternatives: Vec<ModeAlternative>,
    /// Minimal sets of additional argument bindings that would unblock the
    /// goal, cheapest-mode first.
    pub unlock_sets: Vec<Vec<String>>,
    /// Seed predicates from the catalog that can bind a `Def` from scratch
    /// (rendered as the `e.g. via ...` hint).
    pub seed_hint: Vec<&'static str>,
    /// True when a scan mode would have satisfied the goal but scans are
    /// denied for this request.
    pub scans_denied: bool,
    /// Where the goal lives in the planner input (`None` for a root
    /// demand, which has no call site).
    pub location: Option<GoalLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// `RAQL0301`: no satisfiable access path for a goal, under any body
    /// ordering.
    UnsatisfiableModes(Box<UnsatisfiableGoal>),
    /// `RAQL0310`: the plan requires a scan and the request denies scans.
    ScanDenied { predicate: String, cost: CostClass },
    /// `RAQL0311`: a goal under `not` reaches a scan after demand
    /// propagation (forbidden in v0, SPEC §8.5).
    ScanUnderNegation { goal: String, cost: CostClass },
    /// `RAQL0302`: the predicate exists but is `disabled` (SPEC §4.3).
    PredicateDisabled { predicate: String },
    /// `RAQL0303`: a `.mode` assertion on a derived predicate is not
    /// inferable from its rules (SPEC §9.1).
    DeclaredModeNotInferable { predicate: String, mode: String },
    /// `RAQL0304`: malformed planner input (unknown predicate name or
    /// arity mismatch) — a lang-layer bug, reported defensively.
    MalformedInput { detail: String },
}

impl PlanError {
    pub fn code(&self) -> &'static str {
        match self {
            PlanError::UnsatisfiableModes(_) => "RAQL0301",
            PlanError::ScanDenied { .. } => "RAQL0310",
            PlanError::ScanUnderNegation { .. } => "RAQL0311",
            PlanError::PredicateDisabled { .. } => "RAQL0302",
            PlanError::DeclaredModeNotInferable { .. } => "RAQL0303",
            PlanError::MalformedInput { .. } => "RAQL0304",
        }
    }
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::UnsatisfiableModes(goal) => render_unsatisfiable(f, goal),
            PlanError::ScanDenied { predicate, cost } => write!(
                f,
                "error[RAQL0310]: plan requires the scan `{predicate}` [{}], denied by \
                 --no-scan",
                cost.name(),
            ),
            PlanError::ScanUnderNegation { goal, cost } => write!(
                f,
                "error[RAQL0311]: `{goal}` is demanded under `not` but reaches a scan [{}] — \
                 negated goals must be scan-free (SPEC §8.5)",
                cost.name(),
            ),
            PlanError::PredicateDisabled { predicate } => write!(
                f,
                "error[RAQL0302]: predicate `{predicate}` is disabled — no honest RA-native \
                 operator exists yet (SPEC §4.3); see `raql capabilities`",
            ),
            PlanError::DeclaredModeNotInferable { predicate, mode } => write!(
                f,
                "error[RAQL0303]: declared mode `{predicate}{mode}` is not inferable from its \
                 rules (SPEC §9.1)",
            ),
            PlanError::MalformedInput { detail } => {
                write!(f, "error[RAQL0304]: malformed planner input: {detail}")
            }
        }
    }
}

impl std::error::Error for PlanError {}

fn render_unsatisfiable(f: &mut fmt::Formatter<'_>, goal: &UnsatisfiableGoal) -> fmt::Result {
    writeln!(f, "error[RAQL0301]: no satisfiable access path for `{}`", goal.goal)?;
    if goal.bound.is_empty() {
        writeln!(f, "  bound here: (none)")?;
    } else {
        writeln!(f, "  bound here: {}", goal.bound.join(", "))?;
    }
    let width = goal
        .alternatives
        .iter()
        .map(|alt| alt.signature.len())
        .max()
        .unwrap_or(0);
    for (index, alt) in goal.alternatives.iter().enumerate() {
        let label = if index == 0 { "  supported: " } else { "             " };
        let cost = match alt.cost {
            Some(cost) => format!("  [{}]", cost.name()),
            None => String::new(),
        };
        let denied = if alt.denied { "  (denied: --no-scan)" } else { "" };
        writeln!(f, "{label}{:width$}   {}{cost}{denied}", alt.signature, alt.via)?;
    }
    write!(f, "  fix: ")?;
    if goal.unlock_sets.is_empty() {
        write!(f, "no additional bindings can unblock this goal")?;
    } else {
        let sets = goal
            .unlock_sets
            .iter()
            .map(|set| set.join("+"))
            .collect::<Vec<_>>()
            .join(" or ");
        write!(f, "bind {sets} first")?;
        if !goal.seed_hint.is_empty() {
            write!(f, " (e.g. via {})", goal.seed_hint.join("/"))?;
        }
    }
    if goal.scans_denied {
        write!(f, ", or allow scans")?;
    }
    Ok(())
}
