//! Rule-body expansion: disjunction flattening and helper-rule inlining.

use std::collections::{BTreeMap, BTreeSet};

use raql_syntax::{Atom, Constraint, Goal, Spanned, Term};

use crate::plan::term_vars;
use crate::program::TypedRule;

pub(crate) fn expand_rules(rules: &[TypedRule]) -> Vec<TypedRule> {
    let inlineable = inlineable_rule_bodies(rules);
    let mut out = Vec::new();
    for rule in rules {
        let bodies = expand_goals_with_inline(&rule.rule.value.body, &inlineable);
        for body in bodies {
            let mut r = rule.clone();
            r.rule.value.body = body;
            out.push(r);
        }
    }
    out
}

fn expand_goals_with_inline(
    goals: &[Spanned<Goal>],
    inlineable: &BTreeMap<String, Vec<InlineRule>>,
) -> Vec<Vec<Spanned<Goal>>> {
    let mut bodies: Vec<Vec<Spanned<Goal>>> = vec![Vec::new()];
    for goal in goals {
        let expanded_segments = expand_goal_with_inline(goal, inlineable);
        let mut next = Vec::new();
        for base in &bodies {
            for segment in &expanded_segments {
                let mut body = base.clone();
                body.extend(segment.clone());
                next.push(body);
            }
        }
        bodies = next;
    }
    bodies
}

fn expand_goal_with_inline(
    goal: &Spanned<Goal>,
    inlineable: &BTreeMap<String, Vec<InlineRule>>,
) -> Vec<Vec<Spanned<Goal>>> {
    match &goal.value {
        Goal::Atom(atom) => {
            if let Some(callees) = inlineable.get(atom.name.value.as_str()) {
                let mut expanded = Vec::new();
                for callee in callees {
                    let body = inline_goal_body(atom, callee);
                    expanded.extend(expand_goals_with_inline(&body, inlineable));
                }
                expanded
            } else {
                vec![vec![goal.clone()]]
            }
        }
        Goal::Disjunction(d) => d
            .branches
            .iter()
            .flat_map(|branch| expand_goals_with_inline(branch, inlineable))
            .collect(),
        Goal::Aggregate(a) => expand_goals_with_inline(&a.goals, inlineable)
            .into_iter()
            .map(|goals| {
                let mut aggregate = a.clone();
                aggregate.goals = goals;
                vec![Spanned::new(goal.span, Goal::Aggregate(aggregate))]
            })
            .collect(),
        Goal::ChooseTopK(c) => expand_goals_with_inline(&c.goals, inlineable)
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

#[derive(Clone)]
struct InlineRule {
    head_vars: Vec<String>,
    body: Vec<Spanned<Goal>>,
}

fn inlineable_rule_bodies(rules: &[TypedRule]) -> BTreeMap<String, Vec<InlineRule>> {
    let mut grouped = BTreeMap::<String, Vec<&TypedRule>>::new();
    for rule in rules {
        grouped
            .entry(rule.rule.value.head.value.name.value.to_string())
            .or_default()
            .push(rule);
    }

    let mut inlineable = BTreeMap::<String, Vec<InlineRule>>::new();
    for (predicate, predicate_rules) in grouped {
        let mut bodies = Vec::new();
        let mut ok = true;
        for rule in predicate_rules {
            let Some(head_vars) = inline_rule_head_vars(rule, predicate.as_str()) else {
                ok = false;
                break;
            };
            bodies.push(InlineRule {
                head_vars,
                body: rule.rule.value.body.clone(),
            });
        }
        if ok {
            inlineable.insert(predicate, bodies);
        }
    }
    inlineable
}

fn inline_rule_head_vars(rule: &TypedRule, predicate: &str) -> Option<Vec<String>> {
    let mut head_vars = Vec::<String>::new();
    let mut seen_head_vars = BTreeSet::<String>::new();
    for term in &rule.rule.value.head.value.terms {
        let Term::Var(name) = &term.value else {
            return None;
        };
        if !seen_head_vars.insert(name.to_string()) {
            return None;
        }
        head_vars.push(name.to_string());
    }
    let head_var_set = head_vars.iter().cloned().collect::<BTreeSet<_>>();
    if rule
        .rule
        .value
        .body
        .iter()
        .any(|goal| goal_references_predicate(goal, predicate))
    {
        return None;
    }
    let mut body_vars = BTreeSet::new();
    for goal in &rule.rule.value.body {
        if !goal_is_inlineable(goal) {
            return None;
        }
        collect_goal_vars(goal, &mut body_vars);
    }
    body_vars.is_subset(&head_var_set).then_some(head_vars)
}

fn goal_is_inlineable(goal: &Spanned<Goal>) -> bool {
    matches!(
        &goal.value,
        Goal::Atom(_) | Goal::Not(_) | Goal::Constraint(Constraint::Relational(_))
    )
}

fn goal_references_predicate(goal: &Spanned<Goal>, predicate: &str) -> bool {
    match &goal.value {
        Goal::Atom(atom) => atom.name.value.as_str() == predicate,
        Goal::Not(not_goal) => not_goal.atom.value.name.value.as_str() == predicate,
        Goal::Aggregate(aggregate) => aggregate
            .goals
            .iter()
            .any(|goal| goal_references_predicate(goal, predicate)),
        Goal::ChooseTopK(choose) => choose
            .goals
            .iter()
            .any(|goal| goal_references_predicate(goal, predicate)),
        Goal::Disjunction(group) => group
            .branches
            .iter()
            .flat_map(|branch| branch.iter())
            .any(|goal| goal_references_predicate(goal, predicate)),
        Goal::Constraint(_) => false,
    }
}

fn collect_goal_vars(goal: &Spanned<Goal>, out: &mut BTreeSet<String>) {
    match &goal.value {
        Goal::Atom(atom) => {
            for term in &atom.terms {
                out.extend(term_vars(term));
            }
        }
        Goal::Not(not_goal) => {
            for term in &not_goal.atom.value.terms {
                out.extend(term_vars(term));
            }
        }
        Goal::Constraint(Constraint::Relational(rel)) => {
            out.extend(term_vars(&rel.lhs));
            out.extend(term_vars(&rel.rhs));
        }
        Goal::Constraint(Constraint::ArithmeticBind(_))
        | Goal::Aggregate(_)
        | Goal::ChooseTopK(_)
        | Goal::Disjunction(_) => {}
    }
}

fn inline_goal_body(atom: &Atom, callee: &InlineRule) -> Vec<Spanned<Goal>> {
    let substitution = atom
        .terms
        .iter()
        .cloned()
        .zip(callee.head_vars.iter().cloned())
        .map(|(term, name)| (name, term))
        .collect::<BTreeMap<_, _>>();
    callee
        .body
        .iter()
        .cloned()
        .map(|goal| substitute_goal(goal, &substitution))
        .collect()
}

fn substitute_goal(
    mut goal: Spanned<Goal>,
    substitution: &BTreeMap<String, Spanned<Term>>,
) -> Spanned<Goal> {
    match &mut goal.value {
        Goal::Atom(atom) => {
            for term in &mut atom.terms {
                substitute_term(term, substitution);
            }
        }
        Goal::Not(not_goal) => {
            for term in &mut not_goal.atom.value.terms {
                substitute_term(term, substitution);
            }
        }
        Goal::Constraint(Constraint::Relational(rel)) => {
            substitute_term(&mut rel.lhs, substitution);
            substitute_term(&mut rel.rhs, substitution);
        }
        Goal::Constraint(Constraint::ArithmeticBind(_))
        | Goal::Aggregate(_)
        | Goal::ChooseTopK(_)
        | Goal::Disjunction(_) => {}
    }
    goal
}

fn substitute_term(term: &mut Spanned<Term>, substitution: &BTreeMap<String, Spanned<Term>>) {
    match &mut term.value {
        Term::Var(name) => {
            if let Some(replacement) = substitution.get(name.as_str()) {
                *term = replacement.clone();
            }
        }
        Term::Some(inner) => substitute_term(inner, substitution),
        Term::List { items, .. } => {
            for item in items {
                substitute_term(item, substitution);
            }
        }
        _ => {}
    }
}
