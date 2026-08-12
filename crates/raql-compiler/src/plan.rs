//! Planning: lower the typechecked program to the logical IR and route it
//! through the binding-aware planner (SPEC §9–§10, §17.1).
//!
//! The compiler owns no goal ordering and no access-path selection any
//! more: `raql_plan::plan` does both, against the catalog. What remains
//! here is the lang layer's own validation (range restriction,
//! stratification, reserved heads), the lowering, and mapping plan errors
//! back to source spans.

use std::collections::{BTreeMap, BTreeSet};

use raql_plan::{GoalRef, PhysicalPlan, PlanError, PlanOptions};
use raql_syntax::{Constraint, Expr, Goal, RelOp, Spanned, Term};

use crate::diagnostics::{CompilerDiagnostic, DiagBundle, enrich_include_stack_context};
use crate::expand::flatten_rules;
use crate::lower::{GoalPath, LoweredProgram, lower};
use crate::program::{EnumDecl, ModeSig, PredicateDecl, TypedProgram, TypedRule};
use crate::strata::{SccPlan, compute_strata};

/// The compiled, planned program: the typed artifacts (the engine's source
/// AST), the lowered logical program with provenance, and the physical
/// plan.
#[derive(Debug, Clone)]
pub struct PlannedProgram {
    typed: TypedProgram,
    /// Disjunction-flattened rules — what the lowering indexes and the
    /// engine evaluates.
    rules: Vec<TypedRule>,
    pub(crate) strata: BTreeMap<String, usize>,
    sccs: Vec<SccPlan>,
    lowered: LoweredProgram,
    physical: PhysicalPlan,
}

impl PlannedProgram {
    pub fn predicates(&self) -> &BTreeMap<String, PredicateDecl> {
        self.typed.predicates()
    }

    pub fn predicate_decl(&self, name: &str) -> Option<&PredicateDecl> {
        self.typed.predicates().get(name)
    }

    pub fn facts(&self) -> &[Spanned<raql_syntax::Fact>] {
        self.typed.facts()
    }

    pub fn source_map(&self) -> &raql_syntax::SourceMap {
        self.typed.sources()
    }

    pub fn pragma_i64(&self, name: &str) -> Option<i64> {
        self.typed.pragma_i64(name)
    }

    pub fn modes(&self, predicate: &str) -> Option<&[ModeSig]> {
        self.typed.modes(predicate)
    }

    pub fn enum_decl(&self, name: &str) -> Option<&EnumDecl> {
        self.typed.enum_decl(name)
    }

    /// The disjunction-flattened rules; every `GoalPath` in the lowered
    /// provenance indexes into these.
    pub fn rules(&self) -> &[TypedRule] {
        &self.rules
    }

    pub fn lowered(&self) -> &LoweredProgram {
        &self.lowered
    }

    pub fn physical(&self) -> &PhysicalPlan {
        &self.physical
    }

    /// The SPEC §10.4 explain rendering of the physical plan.
    pub fn explain(&self) -> String {
        self.physical.explain(&self.lowered.logic)
    }

    pub fn goal_at(&self, path: &GoalPath) -> Option<&Spanned<Goal>> {
        path.resolve(&self.rules)
    }

    pub fn strata(&self) -> &BTreeMap<String, usize> {
        &self.strata
    }

    pub fn stratum_of(&self, predicate: &str) -> Option<usize> {
        self.strata.get(predicate).copied()
    }

    pub fn sccs(&self) -> &[SccPlan] {
        &self.sccs
    }
}

/// Every predicate name the plan actually touches: the lowered derived
/// predicates (user ones — synthesized binder bodies are internal), the
/// input relations, and the extern predicates referenced by lowered rules.
pub fn reachable_predicates(program: &PlannedProgram) -> BTreeSet<String> {
    let logic = &program.lowered().logic;
    let mut out = BTreeSet::new();
    for (derived, provenance) in logic.derived.iter().zip(&program.lowered().derived) {
        if provenance.binder.is_none() {
            out.insert(derived.name.clone());
        }
    }
    for input in &logic.inputs {
        out.insert(input.name.clone());
    }
    out.extend(required_extern_capabilities(program));
    out
}

/// The catalog extern predicates the plan invokes (capability gating for
/// hosts). Engine builtins and input relations are not capabilities.
pub fn required_extern_capabilities(program: &PlannedProgram) -> BTreeSet<String> {
    let logic = &program.lowered().logic;
    let mut out = BTreeSet::new();
    for rule in logic.derived.iter().flat_map(|derived| &derived.rules) {
        for goal in &rule.body {
            if let GoalRef::Extern(name) = &goal.target {
                out.insert(name.clone());
            }
        }
    }
    out
}

/// Compile with default options (scans allowed).
pub fn plan(typed: TypedProgram) -> Result<PlannedProgram, DiagBundle> {
    plan_with_options(typed, PlanOptions::default())
}

/// Compile with request-level plan options (`deny_scans` etc.).
pub fn plan_with_options(
    typed: TypedProgram,
    options: PlanOptions,
) -> Result<PlannedProgram, DiagBundle> {
    let mut diagnostics = Vec::new();
    let rules = flatten_rules(typed.rules());

    for rule in &rules {
        check_range_restriction(rule, &mut diagnostics);
        if rule.head_predicate() == "out_status" {
            diagnostics.push(CompilerDiagnostic::error(
                "RAQL0404",
                "`out_status/1` is reserved for engine output only",
                Some(rule.rule().value.head.span),
            ));
        }
    }
    if !diagnostics.is_empty() {
        enrich_include_stack_context(&mut diagnostics, typed.sources());
        return Err(diagnostics);
    }

    // Stratification stays a lang-layer semantic check (negation and
    // aggregation must not cycle), computed over the flattened rules.
    let mut strata_input = typed.clone();
    strata_input.set_rules(rules.clone());
    let stratification = compute_strata(&strata_input, &mut diagnostics);
    if !diagnostics.is_empty() {
        enrich_include_stack_context(&mut diagnostics, typed.sources());
        return Err(diagnostics);
    }

    let Some(lowered) = lower(&typed, &rules, &mut diagnostics) else {
        enrich_include_stack_context(&mut diagnostics, typed.sources());
        return Err(diagnostics);
    };

    let physical = match raql_plan::plan(&lowered.logic, &raql_plan::v0_catalog(), options) {
        Ok(physical) => physical,
        Err(error) => {
            diagnostics.push(plan_error_diagnostic(&error, &lowered, &rules));
            enrich_include_stack_context(&mut diagnostics, typed.sources());
            return Err(diagnostics);
        }
    };

    Ok(PlannedProgram {
        typed,
        rules,
        strata: stratification.strata,
        sccs: stratification.sccs,
        lowered,
        physical,
    })
}

/// Map a plan error to a compiler diagnostic. The message is raql-plan's
/// rendering verbatim (the RAQL0301 format is the SPEC §10.3 contract);
/// the span comes from the goal location when the planner reports one.
fn plan_error_diagnostic(
    error: &PlanError,
    lowered: &LoweredProgram,
    rules: &[TypedRule],
) -> CompilerDiagnostic {
    let span = match error {
        PlanError::UnsatisfiableModes(goal) => goal.location.and_then(|location| {
            lowered
                .derived
                .get(location.predicate.0)?
                .rules
                .get(location.rule_index)?
                .goals
                .get(location.source_index)?
                .resolve(rules)
                .map(|goal| goal.span)
        }),
        _ => None,
    };
    CompilerDiagnostic::error(error.code(), error.to_string(), span)
}

fn check_range_restriction(rule: &TypedRule, diagnostics: &mut DiagBundle) {
    let mut restricted = BTreeSet::<String>::new();

    let mut changed = true;
    while changed {
        changed = false;
        for goal in &rule.rule().value.body {
            match &goal.value {
                Goal::Atom(atom) => {
                    for term in &atom.terms {
                        changed |= add_term_vars(term, &mut restricted);
                    }
                }
                Goal::Not(_) => {}
                Goal::Constraint(c) => match c {
                    Constraint::Relational(r) => {
                        if r.op.value == RelOp::Eq {
                            let lhs_ground = term_range_ground(&r.lhs, &restricted);
                            let rhs_ground = term_range_ground(&r.rhs, &restricted);
                            if lhs_ground {
                                changed |= add_term_vars(&r.rhs, &mut restricted);
                            }
                            if rhs_ground {
                                changed |= add_term_vars(&r.lhs, &mut restricted);
                            }
                        }
                    }
                    Constraint::ArithmeticBind(b) => {
                        if expr_range_ground(&b.expr, &restricted) {
                            changed |= restricted.insert(b.target.value.to_string());
                        }
                    }
                },
                Goal::Aggregate(a) => {
                    changed |= restricted.insert(a.out.value.to_string());
                }
                Goal::ChooseTopK(c) => {
                    changed |= restricted.insert(c.score_var.value.to_string());
                    changed |= restricted.insert(c.item_var.value.to_string());
                }
                Goal::Disjunction(_) => {}
            }
        }
    }

    for term in &rule.rule().value.head.value.terms {
        for var in term_vars(term) {
            if !restricted.contains(var.as_str()) {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0406",
                    format!("variable `{var}` in rule head is not range-restricted"),
                    Some(rule.rule().value.head.span),
                ));
            }
        }
    }

    for goal in &rule.rule().value.body {
        match &goal.value {
            Goal::Not(n) => {
                for term in &n.atom.value.terms {
                    for var in term_vars(term) {
                        if !restricted.contains(var.as_str()) {
                            diagnostics.push(CompilerDiagnostic::error(
                                "RAQL0406",
                                format!("variable `{var}` in negated goal is not range-restricted"),
                                Some(n.atom.span),
                            ));
                        }
                    }
                }
            }
            Goal::Constraint(Constraint::Relational(r))
                if matches!(
                    r.op.value,
                    RelOp::NotEq | RelOp::Lt | RelOp::LtEq | RelOp::Gt | RelOp::GtEq
                ) =>
            {
                for var in term_vars(&r.lhs).into_iter().chain(term_vars(&r.rhs)) {
                    if !restricted.contains(var.as_str()) {
                        diagnostics.push(CompilerDiagnostic::error(
                            "RAQL0406",
                            format!(
                                "variable `{var}` in non-binding constraint is not range-restricted"
                            ),
                            Some(r.op.span),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}

fn add_term_vars(term: &Spanned<Term>, restricted: &mut BTreeSet<String>) -> bool {
    let mut changed = false;
    for var in term_vars(term) {
        changed |= restricted.insert(var);
    }
    changed
}

pub(crate) fn term_vars(term: &Spanned<Term>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_term_vars(term, &mut out);
    out
}

fn collect_term_vars(term: &Spanned<Term>, out: &mut BTreeSet<String>) {
    match &term.value {
        Term::Var(v) => {
            out.insert(v.to_string());
        }
        Term::Some(inner) => collect_term_vars(inner, out),
        Term::List { items, .. } => {
            for item in items {
                collect_term_vars(item, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn goals_mention_var(goals: &[Spanned<Goal>], var: &str) -> bool {
    goals.iter().any(|g| goal_mentions_var(&g.value, var))
}

fn goal_mentions_var(goal: &Goal, var: &str) -> bool {
    match goal {
        Goal::Atom(a) => a.terms.iter().any(|t| term_mentions_var(t, var)),
        Goal::Not(n) => n.atom.value.terms.iter().any(|t| term_mentions_var(t, var)),
        Goal::Constraint(c) => match c {
            Constraint::Relational(r) => {
                term_mentions_var(&r.lhs, var) || term_mentions_var(&r.rhs, var)
            }
            Constraint::ArithmeticBind(b) => {
                b.target.value == var || expr_mentions_var(&b.expr, var)
            }
        },
        Goal::Aggregate(a) => {
            a.projection_var.as_ref().is_some_and(|v| v.value == var)
                || a.out.value == var
                || goals_mention_var(&a.goals, var)
        }
        Goal::ChooseTopK(c) => {
            c.score_var.value == var || c.item_var.value == var || goals_mention_var(&c.goals, var)
        }
        Goal::Disjunction(d) => d
            .branches
            .iter()
            .any(|branch| goals_mention_var(branch, var)),
    }
}

fn term_mentions_var(term: &Spanned<Term>, var: &str) -> bool {
    match &term.value {
        Term::Var(v) => v.as_str() == var,
        Term::Some(inner) => term_mentions_var(inner, var),
        Term::List { items, .. } => items.iter().any(|item| term_mentions_var(item, var)),
        _ => false,
    }
}

fn expr_mentions_var(expr: &Spanned<Expr>, var: &str) -> bool {
    match &expr.value {
        Expr::Term(term) => term_mentions_var(term, var),
        Expr::UnaryNeg(inner) => expr_mentions_var(inner, var),
        Expr::Binary { lhs, rhs, .. } => expr_mentions_var(lhs, var) || expr_mentions_var(rhs, var),
    }
}

fn term_range_ground(term: &Spanned<Term>, restricted: &BTreeSet<String>) -> bool {
    match &term.value {
        Term::Var(v) => restricted.contains(v.as_str()),
        Term::Wildcard => false,
        Term::Int(_) | Term::String(_) | Term::Bool(_) | Term::EnumAtom { .. } => true,
        Term::None { .. } => true,
        Term::Some(inner) => term_range_ground(inner, restricted),
        Term::List { items, .. } => items.iter().all(|item| term_range_ground(item, restricted)),
    }
}

fn expr_range_ground(expr: &Spanned<Expr>, restricted: &BTreeSet<String>) -> bool {
    match &expr.value {
        Expr::Term(term) => term_range_ground(term, restricted),
        Expr::UnaryNeg(inner) => expr_range_ground(inner, restricted),
        Expr::Binary { lhs, rhs, .. } => {
            expr_range_ground(lhs, restricted) && expr_range_ground(rhs, restricted)
        }
    }
}
