//! The binding-aware planner (SPEC §9–§10).
//!
//! Three cooperating pieces:
//!
//! 1. **Derived-mode inference** (§9.1): bottom-up over the SCCs of the
//!    derived-predicate call graph, a greatest fixpoint per SCC — start
//!    from "every binding pattern works", repeatedly remove patterns whose
//!    rules cannot be ordered, until stable. Declared `.mode` assertions
//!    are checked against the inferred set and then *replace* it as the
//!    public contract.
//! 2. **Demand-driven specialization** (§9.2): each derived predicate is
//!    compiled per distinct binding pattern that actually occurs, so
//!    bindings flow into rule bodies instead of evaluating whole strata.
//! 3. **The backtracking reorderer** (§10.2): per rule body, greedily place
//!    the goal with the cheapest satisfiable access path (tie-break: fewer
//!    free variables, then source order), scans considered only when no
//!    non-scan goal is placeable, with full backtracking over goal order.
//!    Mode choice never needs backtracking: whichever mode runs, the goal
//!    binds all its variables afterwards.
//!
//! Ordering heuristic for derived goals: a demanded derived call is costed
//! C1 (bound-ish rule bodies are definition-local-ish) and an
//! empty-pattern call — a scan of a derived predicate — C4. The *reported*
//! costs in the plan are exact (computed transitively after planning);
//! only the greedy ordering uses the heuristic.
//!
//! Determinism (§10.1): same program + same catalog ⇒ identical plan. All
//! state is in `BTreeMap`/`BTreeSet`s and all tie-breaks end in source
//! order.

use std::collections::{BTreeMap, BTreeSet};

use crate::catalog::Catalog;
use crate::error::{ModeAlternative, PlanError, UnsatisfiableGoal};
use crate::logic::{DerivedId, Goal, GoalRef, Program, Rule, Term, Var};
use crate::mode::{AccessKind, Binding, CostClass, Pattern};
use crate::plan::{Access, PhysicalPlan, PlannedGoal, PlannedRule, ScanUse, Specialization};
use crate::schema::ArgType;

/// Request-level planning options (SPEC §8.5, §14).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanOptions {
    /// `--no-scan` (protocol `deny_scans`): any scan in the plan is a
    /// plan-time error.
    pub deny_scans: bool,
}

/// Plan a program against the catalog (SPEC §10.1).
pub fn plan(
    program: &Program,
    catalog: &Catalog,
    options: PlanOptions,
) -> Result<PhysicalPlan, PlanError> {
    validate(program, catalog)?;
    let supports = infer_supports(program, catalog)?;
    let mut planner = Planner {
        program,
        catalog,
        deny_scans: options.deny_scans,
        supports,
        specs: BTreeMap::new(),
        negated_demands: BTreeSet::new(),
    };
    let query = planner.plan_rule(&program.query, BTreeSet::new(), 0)?;
    planner.finalize(query)
}

/// A derived predicate specialization: predicate id + demanded pattern.
type SpecKey = (usize, Pattern);

/// Exact transitive cost, scan usage, and demand-graph edges for one
/// specialization (computed by the [`Planner::finalize`] fixpoint).
type SpecInfo = (CostClass, bool, Vec<SpecKey>);

// ---------------------------------------------------------------------
// Validation (defensive: the lang layer typechecks first)
// ---------------------------------------------------------------------

fn validate(program: &Program, catalog: &Catalog) -> Result<(), PlanError> {
    let check_rule = |rule: &Rule, head_arity: Option<usize>| -> Result<(), PlanError> {
        if rule.body.len() > 128 {
            return Err(malformed("rule body exceeds 128 goals"));
        }
        if let Some(arity) = head_arity
            && rule.head.len() != arity
        {
            return Err(malformed("rule head arity differs from its predicate"));
        }
        let var_ok = |term: &Term| match term {
            Term::Var(var) => (var.0 as usize) < rule.vars.len(),
            Term::Const => true,
        };
        if !rule.head.iter().all(var_ok) || !rule.body.iter().flat_map(|g| &g.args).all(var_ok) {
            return Err(malformed("variable index outside the rule's variable table"));
        }
        for goal in &rule.body {
            let expected = match &goal.target {
                GoalRef::Extern(name) => {
                    let Some(predicate) = catalog.predicate(name) else {
                        return Err(malformed(&format!("unknown extern predicate `{name}`")));
                    };
                    if predicate.is_disabled() {
                        return Err(PlanError::PredicateDisabled { predicate: name.clone() });
                    }
                    predicate.arity()
                }
                GoalRef::Derived(id) => {
                    let Some(derived) = program.derived.get(id.0) else {
                        return Err(malformed("derived id out of range"));
                    };
                    derived.arity
                }
                GoalRef::Input(id) => {
                    let Some(input) = program.inputs.get(id.0) else {
                        return Err(malformed("input id out of range"));
                    };
                    input.arity
                }
                GoalRef::Builtin(id) => {
                    let Some(builtin) = program.builtins.get(id.0) else {
                        return Err(malformed("builtin id out of range"));
                    };
                    builtin.pattern.len()
                }
            };
            if goal.args.len() != expected {
                return Err(malformed(&format!(
                    "goal `{}` has {} arguments, predicate declares {expected}",
                    program.target_name(&goal.target),
                    goal.args.len(),
                )));
            }
        }
        Ok(())
    };
    check_rule(&program.query, None)?;
    for derived in &program.derived {
        for rule in &derived.rules {
            check_rule(rule, Some(derived.arity))?;
        }
        if let Some(declared) = &derived.declared_modes
            && declared.iter().any(|mode| mode.len() != derived.arity)
        {
            return Err(malformed(&format!(
                "declared mode arity mismatch on `{}`",
                derived.name,
            )));
        }
    }
    Ok(())
}

fn malformed(detail: &str) -> PlanError {
    PlanError::MalformedInput { detail: detail.to_owned() }
}

// ---------------------------------------------------------------------
// §9.1 Derived-mode inference
// ---------------------------------------------------------------------

fn infer_supports(
    program: &Program,
    catalog: &Catalog,
) -> Result<Vec<BTreeSet<Pattern>>, PlanError> {
    let n = program.derived.len();
    let mut supports: Vec<BTreeSet<Pattern>> = vec![BTreeSet::new(); n];

    for scc in derived_sccs(program) {
        for &p in &scc {
            supports[p] = Pattern::all(program.derived[p].arity).into_iter().collect();
        }
        // Greatest fixpoint: remove unsupportable patterns until stable.
        loop {
            let mut removals: Vec<SpecKey> = Vec::new();
            for &p in &scc {
                for pattern in &supports[p] {
                    if !derived_orderable(program, catalog, &supports, p, pattern) {
                        removals.push((p, pattern.clone()));
                    }
                }
            }
            if removals.is_empty() {
                break;
            }
            for (p, pattern) in removals {
                supports[p].remove(&pattern);
            }
        }
    }

    // Declared `.mode` assertions: must be inferable, then they become the
    // public contract (SPEC §9.1) — even for recursive self-calls, so a
    // rule that is only orderable through an undeclared recursive pattern
    // will fail honestly at planning time.
    for (p, derived) in program.derived.iter().enumerate() {
        let Some(declared) = &derived.declared_modes else {
            continue;
        };
        let mut contract = BTreeSet::new();
        for mode in declared {
            let pattern = Pattern::from_bindings(mode);
            if !supports[p].iter().any(|s| s.subset_of(&pattern)) {
                return Err(PlanError::DeclaredModeNotInferable {
                    predicate: derived.name.clone(),
                    mode: pattern.render(),
                });
            }
            contract.insert(pattern);
        }
        supports[p] = contract;
    }
    Ok(supports)
}

/// Every rule of derived predicate `p` is orderable under `pattern`.
fn derived_orderable(
    program: &Program,
    catalog: &Catalog,
    supports: &[BTreeSet<Pattern>],
    p: usize,
    pattern: &Pattern,
) -> bool {
    program.derived[p].rules.iter().all(|rule| {
        let bound = head_bound_vars(rule, pattern);
        // Inference is scan-inclusive: scan denial is a request option
        // applied at planning time, not a property of the predicate.
        try_order(program, catalog, supports, rule, bound, false).is_ok()
    })
}

/// The variables bound by the head under a binding pattern.
fn head_bound_vars(rule: &Rule, pattern: &Pattern) -> BTreeSet<Var> {
    rule.head
        .iter()
        .zip(pattern.iter())
        .filter_map(|(term, bound)| match (term, bound) {
            (Term::Var(var), true) => Some(*var),
            _ => None,
        })
        .collect()
}

/// Derived-predicate SCCs in bottom-up (callee-first) order: Tarjan emits
/// each component only after every component it calls into.
fn derived_sccs(program: &Program) -> Vec<Vec<usize>> {
    struct Tarjan<'p> {
        program: &'p Program,
        index: u32,
        indices: Vec<Option<u32>>,
        lowlink: Vec<u32>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        sccs: Vec<Vec<usize>>,
    }
    impl Tarjan<'_> {
        fn visit(&mut self, v: usize) {
            self.indices[v] = Some(self.index);
            self.lowlink[v] = self.index;
            self.index += 1;
            self.stack.push(v);
            self.on_stack[v] = true;
            let callees: Vec<usize> = self.program.derived[v]
                .rules
                .iter()
                .flat_map(|rule| &rule.body)
                .filter_map(|goal| match &goal.target {
                    GoalRef::Derived(id) => Some(id.0),
                    _ => None,
                })
                .collect();
            for w in callees {
                if self.indices[w].is_none() {
                    self.visit(w);
                    self.lowlink[v] = self.lowlink[v].min(self.lowlink[w]);
                } else if self.on_stack[w] {
                    self.lowlink[v] = self.lowlink[v].min(self.indices[w].unwrap());
                }
            }
            if self.lowlink[v] == self.indices[v].unwrap() {
                let mut scc = Vec::new();
                loop {
                    let w = self.stack.pop().unwrap();
                    self.on_stack[w] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                scc.sort_unstable();
                self.sccs.push(scc);
            }
        }
    }
    let n = program.derived.len();
    let mut tarjan = Tarjan {
        program,
        index: 0,
        indices: vec![None; n],
        lowlink: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        sccs: Vec::new(),
    };
    for v in 0..n {
        if tarjan.indices[v].is_none() {
            tarjan.visit(v);
        }
    }
    tarjan.sccs
}

// ---------------------------------------------------------------------
// §10.2 The backtracking reorderer
// ---------------------------------------------------------------------

/// One placed goal with its chosen access path.
#[derive(Clone, Debug)]
struct Placed {
    source_index: usize,
    access: Access,
    negated: bool,
}

/// Where the search got stuck, for the §10.3 error contract: the deepest
/// partial ordering reached (first such state in search order).
#[derive(Clone, Debug)]
struct OrderFailure {
    bound: BTreeSet<Var>,
    remaining: Vec<usize>,
}

fn try_order(
    program: &Program,
    catalog: &Catalog,
    supports: &[BTreeSet<Pattern>],
    rule: &Rule,
    initially_bound: BTreeSet<Var>,
    deny_scans: bool,
) -> Result<Vec<Placed>, OrderFailure> {
    // Variables of each negated goal that the rest of the rule also uses:
    // negation binds nothing, so these must be bound *before* the negated
    // goal runs (the lang layer's safety check guarantees they can be).
    let shared_with_negated: Vec<BTreeSet<Var>> = rule
        .body
        .iter()
        .enumerate()
        .map(|(index, goal)| {
            if !goal.negated {
                return BTreeSet::new();
            }
            let elsewhere: BTreeSet<Var> = rule
                .head
                .iter()
                .chain(rule.body.iter().enumerate().filter(|(i, _)| *i != index).flat_map(
                    |(_, other)| other.args.iter(),
                ))
                .filter_map(|term| match term {
                    Term::Var(var) => Some(*var),
                    Term::Const => None,
                })
                .collect();
            goal.args
                .iter()
                .filter_map(|term| match term {
                    Term::Var(var) => Some(*var),
                    Term::Const => None,
                })
                .filter(|var| elsewhere.contains(var))
                .collect()
        })
        .collect();

    let mut search = Search {
        program,
        catalog,
        supports,
        rule,
        deny_scans,
        shared_with_negated,
        placed: Vec::new(),
        bound: initially_bound,
        failed_masks: BTreeSet::new(),
        best_failure: None,
        best_failure_depth: 0,
    };
    let full_mask = (1u128 << rule.body.len()) - 1;
    if search.solve(full_mask) {
        Ok(search.placed)
    } else {
        Err(search.best_failure.unwrap_or(OrderFailure {
            bound: search.bound,
            remaining: (0..rule.body.len()).collect(),
        }))
    }
}

struct Search<'p> {
    program: &'p Program,
    catalog: &'p Catalog,
    supports: &'p [BTreeSet<Pattern>],
    rule: &'p Rule,
    deny_scans: bool,
    shared_with_negated: Vec<BTreeSet<Var>>,
    placed: Vec<Placed>,
    bound: BTreeSet<Var>,
    failed_masks: BTreeSet<u128>,
    best_failure: Option<OrderFailure>,
    best_failure_depth: usize,
}

/// A placement candidate for one goal at one search state.
struct Candidate {
    access: Access,
    cost: CostClass,
    is_scan: bool,
    free_vars: usize,
}

impl Search<'_> {
    fn solve(&mut self, remaining: u128) -> bool {
        if remaining == 0 {
            return true;
        }
        if self.failed_masks.contains(&remaining) {
            return false;
        }

        // Candidates per unplaced goal; scans join only when no non-scan
        // placement exists anywhere (SPEC §10.2).
        let mut non_scans: Vec<(CostClass, usize, usize, Candidate)> = Vec::new();
        let mut scans: Vec<(CostClass, usize, usize, Candidate)> = Vec::new();
        for index in 0..self.rule.body.len() {
            if remaining & (1 << index) == 0 {
                continue;
            }
            if let Some(candidate) = self.candidate(index) {
                let key = (candidate.cost, candidate.free_vars, index);
                if candidate.is_scan {
                    scans.push((key.0, key.1, key.2, candidate));
                } else {
                    non_scans.push((key.0, key.1, key.2, candidate));
                }
            }
        }
        let mut pool = if non_scans.is_empty() { scans } else { non_scans };
        pool.sort_by_key(|a| (a.0, a.1, a.2));

        if pool.is_empty() {
            let depth = self.placed.len();
            if self.best_failure.is_none() || depth > self.best_failure_depth {
                self.best_failure_depth = depth;
                self.best_failure = Some(OrderFailure {
                    bound: self.bound.clone(),
                    remaining: (0..self.rule.body.len())
                        .filter(|i| remaining & (1 << i) != 0)
                        .collect(),
                });
            }
            self.failed_masks.insert(remaining);
            return false;
        }

        for (_, _, index, candidate) in pool {
            let goal = &self.rule.body[index];
            let newly_bound: Vec<Var> = if goal.negated {
                Vec::new() // negation binds nothing
            } else {
                goal.args
                    .iter()
                    .filter_map(|term| match term {
                        Term::Var(var) if !self.bound.contains(var) => Some(*var),
                        _ => None,
                    })
                    .collect()
            };
            self.placed.push(Placed {
                source_index: index,
                access: candidate.access,
                negated: goal.negated,
            });
            self.bound.extend(newly_bound.iter().copied());
            if self.solve(remaining & !(1 << index)) {
                return true;
            }
            self.placed.pop();
            for var in &newly_bound {
                self.bound.remove(var);
            }
        }
        self.failed_masks.insert(remaining);
        false
    }

    /// The best access path for one goal at the current bindings, if any.
    fn candidate(&self, index: usize) -> Option<Candidate> {
        let goal = &self.rule.body[index];
        let flags = self.bound_flags(goal);
        let free_vars = goal
            .args
            .iter()
            .zip(flags.iter())
            .filter(|(term, bound)| matches!(term, Term::Var(_)) && !*bound)
            .count();
        if goal.negated && !self.shared_with_negated[index].iter().all(|v| self.bound.contains(v))
        {
            return None;
        }
        match &goal.target {
            GoalRef::Extern(name) => {
                let predicate = self.catalog.predicate(name).expect("validated");
                let mut modes = predicate.satisfiable_modes(&flags);
                if self.deny_scans || goal.negated {
                    // §8.5: goals under `not` must have a non-scan mode.
                    modes.retain(|mode| mode.access != AccessKind::Scan);
                }
                let mode = *modes.first()?;
                Some(Candidate {
                    access: Access::Extern { predicate: predicate.name, mode },
                    cost: mode.cost,
                    is_scan: mode.access == AccessKind::Scan,
                    free_vars,
                })
            }
            GoalRef::Derived(id) => {
                if !self.supports[id.0].iter().any(|s| s.subset_of(&flags)) {
                    return None;
                }
                // Unseeded call = a scan of a derived predicate (§9.2).
                let is_scan = flags.is_unseeded();
                if goal.negated && is_scan {
                    return None;
                }
                Some(Candidate {
                    access: Access::Derived { id: *id, pattern: flags },
                    cost: if is_scan { CostClass::C4 } else { CostClass::C1 },
                    is_scan,
                    free_vars,
                })
            }
            GoalRef::Input(id) => Some(Candidate {
                access: Access::Input { id: *id },
                cost: CostClass::C0,
                is_scan: false,
                free_vars,
            }),
            GoalRef::Builtin(id) => {
                let builtin = &self.program.builtins[id.0];
                let required_bound = builtin
                    .pattern
                    .iter()
                    .zip(flags.iter())
                    .all(|(binding, bound)| *binding == Binding::Free || bound);
                let negated_ok = !goal.negated || flags.iter().all(|bound| bound);
                (required_bound && negated_ok).then_some(Candidate {
                    access: Access::Builtin { id: *id },
                    cost: CostClass::C0,
                    is_scan: false,
                    free_vars,
                })
            }
        }
    }

    fn bound_flags(&self, goal: &Goal) -> Pattern {
        goal.args
            .iter()
            .map(|term| match term {
                Term::Const => true,
                Term::Var(var) => self.bound.contains(var),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------
// §9.2 Demand-driven planning
// ---------------------------------------------------------------------

struct Planner<'p> {
    program: &'p Program,
    catalog: &'p Catalog,
    deny_scans: bool,
    supports: Vec<BTreeSet<Pattern>>,
    /// `None` marks an in-progress (recursive) specialization.
    specs: BTreeMap<SpecKey, Option<Vec<PlannedRule>>>,
    /// Specializations demanded from under a negation (§8.5: must end up
    /// scan-free).
    negated_demands: BTreeSet<SpecKey>,
}

impl Planner<'_> {
    fn plan_rule(
        &mut self,
        rule: &Rule,
        initially_bound: BTreeSet<Var>,
        rule_index: usize,
    ) -> Result<PlannedRule, PlanError> {
        let placed = try_order(
            self.program,
            self.catalog,
            &self.supports,
            rule,
            initially_bound,
            self.deny_scans,
        )
        .map_err(|failure| self.order_error(rule, &failure))?;

        for goal in &placed {
            if let Access::Derived { id, pattern } = &goal.access {
                self.demand(*id, pattern.clone(), goal.negated)?;
            }
        }
        Ok(PlannedRule {
            rule_index,
            goals: placed
                .into_iter()
                .map(|p| PlannedGoal {
                    source_index: p.source_index,
                    access: p.access,
                    negated: p.negated,
                })
                .collect(),
        })
    }

    /// Plan (memoized) the specialization for `(id, pattern)` (§9.2).
    fn demand(
        &mut self,
        id: DerivedId,
        pattern: Pattern,
        under_negation: bool,
    ) -> Result<(), PlanError> {
        let key = (id.0, pattern.clone());
        if under_negation {
            self.negated_demands.insert(key.clone());
        }
        if self.specs.contains_key(&key) {
            return Ok(()); // planned, or in progress (recursion)
        }
        self.specs.insert(key.clone(), None);
        let program = self.program;
        let mut rules = Vec::new();
        for (rule_index, rule) in program.derived[id.0].rules.iter().enumerate() {
            let bound = head_bound_vars(rule, &pattern);
            rules.push(self.plan_rule(rule, bound, rule_index)?);
        }
        self.specs.insert(key, Some(rules));
        Ok(())
    }

    // -----------------------------------------------------------------
    // Metadata: exact costs, scan visibility, negation check (§8.5)
    // -----------------------------------------------------------------

    fn finalize(self, query: PlannedRule) -> Result<PhysicalPlan, PlanError> {
        let program = self.program;
        let specs: BTreeMap<SpecKey, Vec<PlannedRule>> = self
            .specs
            .into_iter()
            .map(|(key, draft)| (key, draft.expect("every demanded specialization is planned")))
            .collect();

        // Fixpoint over the demand graph: exact max cost and transitive
        // scan usage per specialization (max/or are monotone, so
        // iteration converges even through recursion).
        let local = |rules: &[PlannedRule]| -> SpecInfo {
            let mut cost = CostClass::C0;
            let mut has_scan = false;
            let mut refs = Vec::new();
            for goal in rules.iter().flat_map(|r| &r.goals) {
                match &goal.access {
                    Access::Extern { mode, .. } => {
                        cost = cost.max(mode.cost);
                        has_scan |= mode.access == AccessKind::Scan;
                    }
                    Access::Derived { id, pattern } => refs.push((id.0, pattern.clone())),
                    Access::Input { .. } | Access::Builtin { .. } => {}
                }
            }
            (cost, has_scan, refs)
        };

        let mut cost: BTreeMap<&SpecKey, CostClass> = BTreeMap::new();
        let mut scan_flag: BTreeMap<&SpecKey, bool> = BTreeMap::new();
        let locals: BTreeMap<&SpecKey, SpecInfo> =
            specs.iter().map(|(key, rules)| (key, local(rules))).collect();
        for (key, (c, s, _)) in &locals {
            cost.insert(key, *c);
            scan_flag.insert(key, *s || key.1.is_unseeded());
        }
        loop {
            let mut changed = false;
            for (key, (c, s, refs)) in &locals {
                let mut new_cost = *c;
                let mut new_flag = *s || key.1.is_unseeded();
                for reference in refs {
                    new_cost = new_cost.max(cost[reference]);
                    new_flag |= scan_flag[reference];
                }
                if new_cost > cost[*key] {
                    cost.insert(key, new_cost);
                    changed = true;
                }
                if new_flag && !scan_flag[*key] {
                    scan_flag.insert(key, true);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        // §8.5: nothing under a negation may contain a scan.
        for key in &self.negated_demands {
            if scan_flag[key] {
                let derived = &program.derived[key.0];
                return Err(PlanError::ScanUnderNegation {
                    goal: format!("{}{}", derived.name, key.1.render()),
                    cost: cost[key],
                });
            }
        }

        // Scan visibility (§8.5): first-use order, query first, then
        // depth-first through the demand graph, deduplicated by name.
        let mut scans: Vec<ScanUse> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut visited: BTreeSet<&SpecKey> = BTreeSet::new();
        collect_scans(
            program,
            &specs,
            &cost,
            &query,
            &mut scans,
            &mut seen,
            &mut visited,
        );

        let (query_cost, _, query_refs) = local(std::slice::from_ref(&query));
        let mut max_cost = query_cost;
        for reference in &query_refs {
            max_cost = max_cost.max(cost[reference]);
        }
        for c in cost.values() {
            max_cost = max_cost.max(*c);
        }

        let specializations = specs
            .iter()
            .map(|(key, rules)| Specialization {
                predicate: DerivedId(key.0),
                pattern: key.1.clone(),
                rules: rules.clone(),
                is_scan: key.1.is_unseeded(),
                max_cost: cost[key],
            })
            .collect();

        Ok(PhysicalPlan { query, specializations, scans, max_cost })
    }

    // -----------------------------------------------------------------
    // §10.3 error construction
    // -----------------------------------------------------------------

    /// Build the plan error for a failed ordering: RAQL0310 when a stuck
    /// goal is a pure-scan predicate under `deny_scans` (the plan cannot
    /// exist without that scan), RAQL0301 (the full message contract)
    /// otherwise.
    fn order_error(&self, rule: &Rule, failure: &OrderFailure) -> PlanError {
        let program = self.program;

        // A stuck pure-scan predicate under --no-scan *is* the scan
        // refusal, wherever it sits in the unplaced set: no binding can
        // ever satisfy it.
        if self.deny_scans {
            for index in &failure.remaining {
                let goal = &rule.body[*index];
                if goal.negated {
                    continue;
                }
                if let GoalRef::Extern(name) = &goal.target {
                    let predicate = self.catalog.predicate(name).expect("validated");
                    if predicate.modes.iter().all(|m| m.access == AccessKind::Scan) {
                        let cheapest =
                            predicate.modes.iter().map(|m| m.cost).min().unwrap_or(CostClass::C4);
                        return PlanError::ScanDenied { predicate: name.clone(), cost: cheapest };
                    }
                }
            }
        }

        let flags_of = |goal: &Goal| -> Pattern {
            goal.args
                .iter()
                .map(|term| match term {
                    Term::Const => true,
                    Term::Var(var) => failure.bound.contains(var),
                })
                .collect()
        };
        let arg_name = |goal: &Goal, i: usize| -> String {
            match &goal.args[i] {
                Term::Var(var) => rule.var_name(*var).to_owned(),
                Term::Const => "<const>".to_owned(),
            }
        };

        // Unlock distance per stuck goal: fewest additional bindings any
        // access path needs. Offending goal = smallest distance, then
        // source order.
        let distance = |index: &usize| -> usize {
            let goal = &rule.body[*index];
            let flags = flags_of(goal);
            let missing = |pattern: &Pattern| {
                pattern.iter().zip(flags.iter()).filter(|(need, have)| *need && !*have).count()
            };
            let candidates: Vec<usize> = match &goal.target {
                GoalRef::Extern(name) => {
                    let predicate = self.catalog.predicate(name).expect("validated");
                    predicate
                        .modes
                        .iter()
                        .filter(|mode| !(self.deny_scans && mode.access == AccessKind::Scan))
                        .map(|mode| missing(&Pattern::from_bindings(mode.pattern)))
                        .collect()
                }
                GoalRef::Derived(id) => {
                    self.supports[id.0].iter().map(missing).collect()
                }
                GoalRef::Builtin(id) => {
                    vec![missing(&Pattern::from_bindings(&program.builtins[id.0].pattern))]
                }
                GoalRef::Input(_) => Vec::new(),
            };
            // A negated goal additionally waits on its shared variables.
            let shared_unbound = if goal.negated {
                self.shared_unbound(rule, *index, &failure.bound).len()
            } else {
                0
            };
            candidates.into_iter().min().unwrap_or(usize::MAX).max(shared_unbound)
        };
        let offending = *failure
            .remaining
            .iter()
            .min_by_key(|index| (distance(index), **index))
            .expect("failure has unplaced goals");
        let goal = &rule.body[offending];
        let flags = flags_of(goal);

        let signature = |pattern: &Pattern| -> String {
            let inner = (0..goal.args.len())
                .map(|i| {
                    if pattern.is_bound(i) {
                        format!("+{}", arg_name(goal, i))
                    } else {
                        "-".to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({inner})", program.target_name(&goal.target))
        };

        let mut alternatives = Vec::new();
        let mut unlock_sets: Vec<Vec<String>> = Vec::new();
        let mut scans_denied = false;
        let mut needs_def = false;
        let mut push_unlock = |missing: Vec<String>| {
            if missing.is_empty() {
                return;
            }
            let redundant = unlock_sets
                .iter()
                .any(|existing| existing.iter().all(|arg| missing.contains(arg)));
            if !redundant && !unlock_sets.contains(&missing) {
                unlock_sets.push(missing);
            }
        };
        match &goal.target {
            GoalRef::Extern(name) => {
                let predicate = self.catalog.predicate(name).expect("validated");
                for mode in predicate.modes {
                    let pattern = Pattern::from_bindings(mode.pattern);
                    let denied = self.deny_scans && mode.access == AccessKind::Scan;
                    scans_denied |= denied;
                    if !denied {
                        let missing: Vec<String> = (0..goal.args.len())
                            .filter(|i| pattern.is_bound(*i) && !flags.is_bound(*i))
                            .map(|i| {
                                needs_def |= predicate.args[i].ty == ArgType::Def;
                                arg_name(goal, i)
                            })
                            .collect();
                        push_unlock(missing);
                    }
                    alternatives.push(ModeAlternative {
                        signature: signature(&pattern),
                        via: mode.operator.name().split('/').next_back().unwrap().to_owned(),
                        cost: Some(mode.cost),
                        is_scan: mode.access == AccessKind::Scan,
                        denied,
                    });
                }
            }
            GoalRef::Derived(id) => {
                for pattern in &self.supports[id.0] {
                    let missing: Vec<String> = (0..goal.args.len())
                        .filter(|i| pattern.is_bound(*i) && !flags.is_bound(*i))
                        .map(|i| arg_name(goal, i))
                        .collect();
                    push_unlock(missing);
                    alternatives.push(ModeAlternative {
                        signature: signature(pattern),
                        via: "derived".to_owned(),
                        cost: None,
                        is_scan: pattern.is_unseeded(),
                        denied: false,
                    });
                }
            }
            GoalRef::Builtin(id) => {
                let pattern = Pattern::from_bindings(&program.builtins[id.0].pattern);
                let missing: Vec<String> = (0..goal.args.len())
                    .filter(|i| pattern.is_bound(*i) && !flags.is_bound(*i))
                    .map(|i| arg_name(goal, i))
                    .collect();
                push_unlock(missing);
                alternatives.push(ModeAlternative {
                    signature: signature(&pattern),
                    via: "builtin".to_owned(),
                    cost: Some(CostClass::C0),
                    is_scan: false,
                    denied: false,
                });
            }
            GoalRef::Input(_) => {}
        }
        if goal.negated {
            push_unlock(
                self.shared_unbound(rule, offending, &failure.bound)
                    .iter()
                    .map(|var| rule.var_name(*var).to_owned())
                    .collect(),
            );
        }
        unlock_sets.sort_by_key(|set| set.len());

        let bound: Vec<String> = (0..goal.args.len())
            .filter(|i| flags.is_bound(*i))
            .map(|i| arg_name(goal, i))
            .collect();
        PlanError::UnsatisfiableModes(Box::new(UnsatisfiableGoal {
            goal: program.render_goal(rule, goal),
            bound,
            alternatives,
            unlock_sets,
            seed_hint: if needs_def { def_seed_predicates(self.catalog) } else { Vec::new() },
            scans_denied,
        }))
    }

    /// Variables a negated goal shares with the rest of its rule that are
    /// not bound yet.
    fn shared_unbound(&self, rule: &Rule, index: usize, bound: &BTreeSet<Var>) -> Vec<Var> {
        let goal = &rule.body[index];
        let elsewhere: BTreeSet<Var> = rule
            .head
            .iter()
            .chain(
                rule.body
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != index)
                    .flat_map(|(_, other)| other.args.iter()),
            )
            .filter_map(|term| match term {
                Term::Var(var) => Some(*var),
                Term::Const => None,
            })
            .collect();
        goal.args
            .iter()
            .filter_map(|term| match term {
                Term::Var(var) => Some(*var),
                Term::Const => None,
            })
            .filter(|var| elsewhere.contains(var) && !bound.contains(var))
            .collect()
    }
}

/// Catalog predicates that can bind a `Def` from non-`Def` inputs (the
/// `e.g. via ...` seed hint of RAQL0301) — derived mechanically, never
/// handwritten (SPEC §8.1).
fn def_seed_predicates(catalog: &Catalog) -> Vec<&'static str> {
    catalog
        .entries()
        .iter()
        .filter(|predicate| {
            predicate.modes.iter().any(|mode| {
                mode.access == AccessKind::Keyed
                    && mode.pattern.iter().zip(predicate.args).all(|(binding, arg)| {
                        *binding == Binding::Free || arg.ty != ArgType::Def
                    })
                    && mode.pattern.iter().zip(predicate.args).any(|(binding, arg)| {
                        *binding == Binding::Free && arg.ty == ArgType::Def
                    })
            })
        })
        .map(|predicate| predicate.name)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn collect_scans<'s>(
    program: &Program,
    specs: &'s BTreeMap<SpecKey, Vec<PlannedRule>>,
    cost: &BTreeMap<&SpecKey, CostClass>,
    rule: &PlannedRule,
    scans: &mut Vec<ScanUse>,
    seen: &mut BTreeSet<String>,
    visited: &mut BTreeSet<&'s SpecKey>,
) {
    for goal in &rule.goals {
        match &goal.access {
            Access::Extern { predicate, mode } => {
                if mode.access == AccessKind::Scan && seen.insert((*predicate).to_owned()) {
                    scans.push(ScanUse { predicate: (*predicate).to_owned(), cost: mode.cost });
                }
            }
            Access::Derived { id, pattern } => {
                let (key, rules) = specs
                    .get_key_value(&(id.0, pattern.clone()))
                    .expect("demanded specialization exists");
                if pattern.is_unseeded() {
                    let name = &program.derived[id.0].name;
                    if seen.insert(name.clone()) {
                        scans.push(ScanUse { predicate: name.clone(), cost: cost[key] });
                    }
                }
                if visited.insert(key) {
                    for planned in rules {
                        collect_scans(program, specs, cost, planned, scans, seen, visited);
                    }
                }
            }
            Access::Input { .. } | Access::Builtin { .. } => {}
        }
    }
}
