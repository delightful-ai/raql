//! Physical plan types and explain rendering (SPEC §10.1, §10.4).
//!
//! A plan is data, not behavior: per-rule ordered goal sequences with the
//! chosen access path for each goal, the demand-specialization table for
//! derived predicates, and plan-level metadata (scans, max cost). The
//! `explain` rendering is part of the stable interface — agents read it —
//! so keep its format deliberate.

use std::fmt::Write as _;

use crate::logic::{BuiltinId, DerivedId, Goal, InputId, Program, Root, Rule};
use crate::mode::{AccessKind, CostClass, ModeDef, Pattern};

/// The chosen access path of one planned goal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    Extern {
        /// Catalog predicate name.
        predicate: &'static str,
        /// The catalog mode this goal runs under.
        mode: &'static ModeDef,
    },
    /// A demanded derived call: evaluated through the specialization for
    /// `(id, pattern)`.
    Derived { id: DerivedId, pattern: Pattern },
    Input { id: InputId },
    Builtin { id: BuiltinId },
}

/// One goal in execution order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedGoal {
    /// Index of this goal in the rule's *source* body (for tracing back to
    /// the program).
    pub source_index: usize,
    pub access: Access,
    pub negated: bool,
}

/// One rule body, reordered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedRule {
    /// Index into the owning predicate's rule list.
    pub rule_index: usize,
    pub goals: Vec<PlannedGoal>,
}

/// The compiled form of one demanded `(derived predicate, binding pattern)`
/// pair (SPEC §9.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Specialization {
    pub predicate: DerivedId,
    /// Which head arguments arrive bound.
    pub pattern: Pattern,
    pub rules: Vec<PlannedRule>,
    /// A call under the empty pattern is a scan of a derived predicate
    /// (SPEC §9.2) and is reported with the scans.
    pub is_scan: bool,
    /// Max cost class over every goal in every rule of this
    /// specialization, derived goals resolved transitively.
    pub max_cost: CostClass,
}

/// One scan used by the plan (SPEC §8.5: visible always).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanUse {
    /// Predicate name (extern or derived).
    pub predicate: String,
    pub cost: CostClass,
}

/// The planner's output (SPEC §10.1). Deterministic: same program + same
/// catalog version ⇒ identical plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalPlan {
    /// The demand roots, in request order. Each root's plan is its
    /// specialization in [`PhysicalPlan::specializations`].
    pub roots: Vec<Root>,
    /// Demanded specializations, in deterministic (predicate, pattern)
    /// order.
    pub specializations: Vec<Specialization>,
    /// Every scan the plan contains, in first-use order, deduplicated.
    pub scans: Vec<ScanUse>,
    /// Max cost class over the whole plan.
    pub max_cost: CostClass,
}

impl PhysicalPlan {
    /// The specialization a root is planned as.
    pub fn root_specialization(&self, root: &Root) -> &Specialization {
        self.specializations
            .iter()
            .find(|spec| spec.predicate == root.predicate && spec.pattern == root.pattern)
            .expect("every root is planned")
    }

    /// The §10.4 explain rendering: per-goal operator, mode, cost class,
    /// demand specializations, scans, and the RA primitives from the
    /// catalog. Roots render first in request order, then the remaining
    /// demanded specializations in deterministic order.
    pub fn explain(&self, program: &Program) -> String {
        let mut out = String::new();
        for root in &self.roots {
            self.render_spec(&mut out, program, self.root_specialization(root), "root");
        }
        for spec in &self.specializations {
            let is_root = self
                .roots
                .iter()
                .any(|root| root.predicate == spec.predicate && root.pattern == spec.pattern);
            if !is_root {
                self.render_spec(&mut out, program, spec, "specialization");
            }
        }
        if self.scans.is_empty() {
            writeln!(out, "scans: (none)").unwrap();
        } else {
            let scans = self
                .scans
                .iter()
                .map(|scan| format!("{}/{}", scan.predicate, scan.cost.name()))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(out, "scans: [{scans}]").unwrap();
        }
        writeln!(out, "max cost: {}", self.max_cost.name()).unwrap();
        out
    }

    fn render_spec(&self, out: &mut String, program: &Program, spec: &Specialization, label: &str) {
        let derived = &program.derived[spec.predicate.0];
        let scan = if spec.is_scan { "  scan" } else { "" };
        writeln!(
            out,
            "{label} {}{}{scan} [{}]:",
            derived.name,
            spec.pattern.render(),
            spec.max_cost.name(),
        )
        .unwrap();
        for planned in &spec.rules {
            if spec.rules.len() > 1 {
                writeln!(out, "  rule {}:", planned.rule_index + 1).unwrap();
                render_rule_goals(out, program, &derived.rules[planned.rule_index], planned, "    ");
            } else {
                render_rule_goals(out, program, &derived.rules[planned.rule_index], planned, "  ");
            }
        }
    }
}

fn render_rule_goals(
    out: &mut String,
    program: &Program,
    rule: &Rule,
    planned: &PlannedRule,
    indent: &str,
) {
    for (position, goal) in planned.goals.iter().enumerate() {
        let source: &Goal = &rule.body[goal.source_index];
        let rendered = program.render_goal(rule, source);
        let access = match &goal.access {
            Access::Extern { mode, .. } => {
                let kind = match mode.access {
                    AccessKind::Scan => "scan",
                    AccessKind::Keyed => "keyed",
                };
                format!(
                    "{} {} {kind} [{}] via [{}]",
                    mode.operator.name(),
                    Pattern::from_bindings(mode.pattern).render(),
                    mode.cost.name(),
                    mode.ra_primitives.join(", "),
                )
            }
            Access::Derived { id, pattern } => {
                format!("derived {}{}", program.derived[id.0].name, pattern.render())
            }
            Access::Input { id } => format!("input {}", program.inputs[id.0].name),
            Access::Builtin { id } => format!("builtin {}", program.builtins[id.0].name),
        };
        writeln!(out, "{indent}{}. {rendered}  ->  {access}", position + 1).unwrap();
    }
}
