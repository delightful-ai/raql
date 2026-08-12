//! Disjunction flattening: `(a ; b)` is a union, so a rule body containing
//! one expands into one rule per branch combination — inside aggregate and
//! choose sub-bodies too. After flattening no `Goal::Disjunction` remains
//! anywhere, which is what the lowering relies on.
//!
//! The old helper-rule *inlining* that used to live here is gone: demand
//! specialization (SPEC §9.2) plans helper predicates per call pattern,
//! and inlining bypassed their declared `.mode` contracts (§9.1).

use raql_syntax::{Goal, Spanned};

use crate::program::TypedRule;

pub(crate) fn flatten_rules(rules: &[TypedRule]) -> Vec<TypedRule> {
    let mut out = Vec::new();
    for rule in rules {
        for body in flatten_goals(&rule.rule.value.body) {
            let mut flattened = rule.clone();
            flattened.rule.value.body = body;
            out.push(flattened);
        }
    }
    out
}

fn flatten_goals(goals: &[Spanned<Goal>]) -> Vec<Vec<Spanned<Goal>>> {
    let mut bodies: Vec<Vec<Spanned<Goal>>> = vec![Vec::new()];
    for goal in goals {
        let segments = flatten_goal(goal);
        let mut next = Vec::new();
        for base in &bodies {
            for segment in &segments {
                let mut body = base.clone();
                body.extend(segment.clone());
                next.push(body);
            }
        }
        bodies = next;
    }
    bodies
}

fn flatten_goal(goal: &Spanned<Goal>) -> Vec<Vec<Spanned<Goal>>> {
    match &goal.value {
        Goal::Disjunction(d) => d.branches.iter().flat_map(|branch| flatten_goals(branch)).collect(),
        Goal::Aggregate(a) => flatten_goals(&a.goals)
            .into_iter()
            .map(|goals| {
                let mut aggregate = a.clone();
                aggregate.goals = goals;
                vec![Spanned::new(goal.span, Goal::Aggregate(aggregate))]
            })
            .collect(),
        Goal::ChooseTopK(c) => flatten_goals(&c.goals)
            .into_iter()
            .map(|goals| {
                let mut choose = c.clone();
                choose.goals = goals;
                vec![Spanned::new(goal.span, Goal::ChooseTopK(choose))]
            })
            .collect(),
        _ => vec![vec![goal.clone()]],
    }
}
