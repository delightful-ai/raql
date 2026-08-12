//! The demand-driven evaluator (SPEC §9.2, §11.1).
//!
//! Specializations evaluate on demand with per-request memoization keyed
//! by `(predicate, pattern, seed values)`. Recursion runs as naive
//! iteration over the demanded subset: a specialization that reads its own
//! in-progress partial iterates to fixpoint (rows grow monotonically, so
//! the least fixpoint is reached); one that reads an *ancestor's* partial
//! completes provisionally without memoizing and is re-evaluated by the
//! ancestor's loop. The iteration cap (pragma `max_iters`, input
//! `opt_max_iters`) degrades the result to `Partial`, never silently.
//!
//! The engine executes from the source AST: each planned goal's
//! `source_index` resolves through the lowering provenance to the
//! `raql_syntax` goal carrying constants and variable names, while the
//! access tells it *how* to run — operator invocation, input rows, a
//! demanded specialization, an engine builtin, or a binder
//! (aggregate/choose) over its synthesized sub-body specialization.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use indexmap::IndexSet;
use raql_compiler::{GoalPath, PlannedProgram};
use raql_plan::{
    Access, DerivedId, EngineValue, ModeDef, OperatorSet, Pattern, PlannedGoal, Root,
    Specialization,
};
use raql_syntax::{AggregateBinder, AggregateName, Atom, ChooseTopkBinder, Constraint, Goal,
    RelOp, Spanned, Term};

use crate::terms::{Env, NameMap, env_get, env_set, eval_int_expr, eval_term, term_ground,
    unify_term};
use crate::{EvalNote, RuntimeError};

type Key<V> = (usize, Pattern, Vec<V>);
type Rows<V> = Rc<IndexSet<Vec<V>>>;

enum Memo<V> {
    InProgress,
    Done(Rows<V>),
}

pub(crate) struct Evaluation<'p, Ops: OperatorSet> {
    program: &'p PlannedProgram,
    ops: &'p mut Ops,
    /// Input relation extents: request rows plus program facts.
    inputs: BTreeMap<String, IndexSet<Vec<Ops::Value>>>,
    /// Fact rows of derived predicates, unioned into their
    /// specializations.
    derived_facts: BTreeMap<usize, Vec<Vec<Ops::Value>>>,
    /// Specialization lookup: (predicate, pattern) → planned rules.
    specs: HashMap<(usize, Pattern), &'p Specialization>,
    memo: HashMap<Key<Ops::Value>, Memo<Ops::Value>>,
    partial: HashMap<Key<Ops::Value>, IndexSet<Vec<Ops::Value>>>,
    /// In-progress keys read since the current frame's last drain.
    touched: HashSet<Key<Ops::Value>>,
    path_cache: BTreeMap<String, Vec<Vec<Ops::Value>>>,
    next_path_id: u64,
    iterations: usize,
    max_iters: usize,
    hit_cap: bool,
    notes: Vec<EvalNote>,
}

impl<'p, Ops> Evaluation<'p, Ops>
where
    Ops: OperatorSet,
    Ops::Value: EngineValue,
    Ops::Error: std::fmt::Display,
{
    pub(crate) fn new(
        program: &'p PlannedProgram,
        request_inputs: &BTreeMap<String, Vec<Vec<Ops::Value>>>,
        ops: &'p mut Ops,
    ) -> Self {
        let lowered = program.lowered();
        let empty_names = NameMap::new(&[]);
        let empty_env: Env<Ops::Value> = Vec::new();

        // Facts, converted once. Ground by RAQL0402.
        let mut fact_rows = BTreeMap::<&str, Vec<Vec<Ops::Value>>>::new();
        for fact in program.facts() {
            let atom = &fact.value.atom.value;
            let row: Result<Vec<_>, _> = atom
                .terms
                .iter()
                .map(|term| eval_term(term, &empty_env, &empty_names))
                .collect();
            if let Ok(row) = row {
                fact_rows.entry(atom.name.value.as_str()).or_default().push(row);
            }
        }

        let mut inputs = BTreeMap::new();
        for provenance in &lowered.inputs {
            let mut rows = IndexSet::new();
            if let Some(request_rows) = request_inputs.get(&provenance.name) {
                rows.extend(request_rows.iter().cloned());
            }
            if let Some(facts) = fact_rows.get(provenance.name.as_str()) {
                rows.extend(facts.iter().cloned());
            }
            inputs.insert(provenance.name.clone(), rows);
        }

        let mut derived_facts = BTreeMap::new();
        for (index, derived) in lowered.logic.derived.iter().enumerate() {
            if let Some(rows) = fact_rows.get(derived.name.as_str()) {
                derived_facts.insert(index, rows.clone());
            }
        }

        let specs = program
            .physical()
            .specializations
            .iter()
            .map(|spec| ((spec.predicate.0, spec.pattern.clone()), spec))
            .collect();

        let mut notes = Vec::new();
        let pragma_default = program.pragma_i64("max_iters").unwrap_or(128).max(1) as usize;
        let max_iters = match scalar_option_int(&inputs, "opt_max_iters") {
            Ok(Some(value)) => value.max(1) as usize,
            Ok(None) => pragma_default,
            Err(error) => {
                notes.push(EvalNote {
                    section: "Errors".to_string(),
                    message: format!("runtime error [{}]: {error}", error.code().as_str()),
                });
                pragma_default
            }
        };

        Evaluation {
            program,
            ops,
            inputs,
            derived_facts,
            specs,
            memo: HashMap::new(),
            partial: HashMap::new(),
            touched: HashSet::new(),
            path_cache: BTreeMap::new(),
            next_path_id: 0,
            iterations: 0,
            max_iters,
            hit_cap: false,
            notes,
        }
    }

    pub(crate) fn eval_root(
        &mut self,
        root: &Root,
    ) -> Result<IndexSet<Vec<Ops::Value>>, RuntimeError> {
        let (rows, _) = self.eval_spec(root.predicate, &root.pattern, Vec::new())?;
        Ok((*rows).clone())
    }

    pub(crate) fn iterations(&self) -> usize {
        self.iterations
    }

    pub(crate) fn hit_iteration_cap(&self) -> bool {
        self.hit_cap
    }

    pub(crate) fn take_notes(&mut self) -> Vec<EvalNote> {
        std::mem::take(&mut self.notes)
    }

    // -----------------------------------------------------------------
    // Specialization evaluation + recursion (SPEC §9.2, §11.1)
    // -----------------------------------------------------------------

    /// Evaluate one demanded specialization for one seed. Returns the rows
    /// and whether they are complete (false only while an ancestor's
    /// fixpoint is still running).
    fn eval_spec(
        &mut self,
        id: DerivedId,
        pattern: &Pattern,
        seed: Vec<Ops::Value>,
    ) -> Result<(Rows<Ops::Value>, bool), RuntimeError> {
        let key: Key<Ops::Value> = (id.0, pattern.clone(), seed);
        match self.memo.get(&key) {
            Some(Memo::Done(rows)) => return Ok((rows.clone(), true)),
            Some(Memo::InProgress) => {
                self.touched.insert(key.clone());
                let rows = Rc::new(self.partial.get(&key).cloned().unwrap_or_default());
                return Ok((rows, false));
            }
            None => {}
        }

        self.memo.insert(key.clone(), Memo::InProgress);
        let mut initial = IndexSet::new();
        if let Some(facts) = self.derived_facts.get(&id.0) {
            for row in facts {
                if seed_matches(pattern, &key.2, row) {
                    initial.insert(row.clone());
                }
            }
        }
        self.partial.insert(key.clone(), initial);

        let mut external: HashSet<Key<Ops::Value>> = HashSet::new();
        loop {
            let outer_touched = std::mem::take(&mut self.touched);
            let fresh = self.eval_spec_rules(id, pattern, &key.2);
            let my_touched = std::mem::take(&mut self.touched);
            self.touched = outer_touched;
            let fresh = match fresh {
                Ok(rows) => rows,
                Err(error) => {
                    // Leave no dangling in-progress marker behind an error.
                    self.memo.remove(&key);
                    self.partial.remove(&key);
                    return Err(error);
                }
            };

            let looped = my_touched.contains(&key);
            for touched in my_touched {
                if touched != key {
                    external.insert(touched);
                }
            }

            let partial = self.partial.get_mut(&key).expect("in-progress partial exists");
            let mut grew = false;
            for row in fresh {
                grew |= partial.insert(row);
            }

            if !looped || !grew {
                break;
            }
            self.iterations += 1;
            if self.iterations > self.max_iters {
                self.hit_cap = true;
                self.notes.push(EvalNote {
                    section: "Notes".to_string(),
                    message: format!(
                        "runtime error [{}]: {}",
                        RuntimeError::IterationLimit { max_iters: self.max_iters }
                            .code()
                            .as_str(),
                        RuntimeError::IterationLimit { max_iters: self.max_iters },
                    ),
                });
                break;
            }
        }

        if external.is_empty() {
            let rows = Rc::new(self.partial.remove(&key).expect("partial exists"));
            self.memo.insert(key, Memo::Done(rows.clone()));
            Ok((rows, true))
        } else {
            // Depends on an ancestor's unfinished fixpoint: keep the
            // accumulated partial, drop the marker so the ancestor's next
            // iteration re-evaluates, and report the reads upward.
            self.memo.remove(&key);
            self.touched.extend(external);
            let rows = Rc::new(self.partial.get(&key).cloned().unwrap_or_default());
            Ok((rows, false))
        }
    }

    /// One pass over every rule of the specialization.
    fn eval_spec_rules(
        &mut self,
        id: DerivedId,
        pattern: &Pattern,
        seed: &[Ops::Value],
    ) -> Result<Vec<Vec<Ops::Value>>, RuntimeError> {
        let spec = *self
            .specs
            .get(&(id.0, pattern.clone()))
            .ok_or_else(|| RuntimeError::Internal {
                detail: format!("specialization ({}, {}) was never planned", id.0, pattern.render()),
            })?;
        let derived = &self.program.lowered().logic.derived[id.0];
        let provenance = &self.program.lowered().derived[id.0];
        // Aggregate sub-bodies never bind the binder's output column; drop
        // it from the projection (the binder computes it).
        let projected = if provenance.binder.is_some() && self.binder_is_aggregate(id) {
            derived.arity - 1
        } else {
            derived.arity
        };

        let mut out = Vec::new();
        for planned in &spec.rules {
            let logic_rule = &derived.rules[planned.rule_index];
            let rule_prov = &provenance.rules[planned.rule_index];
            let names = NameMap::new(&logic_rule.vars);
            let head_terms = provenance
                .binder
                .is_none()
                .then(|| self.program.rules()[rule_prov.rule].head_terms());

            // Seed the head bindings; a constant head term at a seeded
            // position filters the whole rule.
            let mut env: Env<Ops::Value> = vec![None; logic_rule.vars.len()];
            let mut applicable = true;
            let mut next_seed = seed.iter();
            for (position, head_term) in logic_rule.head.iter().enumerate() {
                if !pattern.is_bound(position) {
                    continue;
                }
                let value = next_seed.next().ok_or_else(|| RuntimeError::Internal {
                    detail: "seed shorter than its binding pattern".to_string(),
                })?;
                match head_term {
                    raql_plan::Term::Var(var) => match env_get(&env, *var) {
                        Some(bound) if bound != value => {
                            applicable = false;
                            break;
                        }
                        Some(_) => {}
                        None => env_set(&mut env, *var, value.clone()),
                    },
                    raql_plan::Term::Const => {
                        let Some(head_terms) = head_terms else {
                            return Err(RuntimeError::Internal {
                                detail: "constant head slot in a synthesized binder".to_string(),
                            });
                        };
                        let constant =
                            eval_term(&head_terms[position], &env, &names)?;
                        if &constant != value {
                            applicable = false;
                            break;
                        }
                    }
                }
            }
            if !applicable {
                continue;
            }

            let mut envs = vec![env];
            for goal in &planned.goals {
                envs = self.eval_goal(goal, rule_prov, logic_rule, &names, envs)?;
                if envs.is_empty() {
                    break;
                }
            }

            for env in envs {
                let mut row = Vec::with_capacity(projected);
                for (position, head_term) in logic_rule.head.iter().take(projected).enumerate() {
                    let value = match head_term {
                        raql_plan::Term::Var(var) => env_get(&env, *var).cloned(),
                        raql_plan::Term::Const => head_terms
                            .map(|terms| eval_term(&terms[position], &env, &names))
                            .transpose()?,
                    };
                    let value = value.ok_or_else(|| RuntimeError::Internal {
                        detail: format!(
                            "head position {position} of `{}` unbound after evaluation",
                            derived.name,
                        ),
                    })?;
                    row.push(value);
                }
                out.push(row);
            }
        }
        Ok(out)
    }

    fn binder_is_aggregate(&self, id: DerivedId) -> bool {
        self.program.lowered().derived[id.0]
            .binder
            .as_ref()
            .and_then(|path| self.program.goal_at(path))
            .is_some_and(|goal| matches!(goal.value, Goal::Aggregate(_)))
    }

    // -----------------------------------------------------------------
    // Goal evaluation
    // -----------------------------------------------------------------

    fn eval_goal(
        &mut self,
        planned: &PlannedGoal,
        rule_prov: &raql_compiler::RuleProvenance,
        logic_rule: &raql_plan::Rule,
        names: &NameMap,
        envs: Vec<Env<Ops::Value>>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let path: &GoalPath =
            rule_prov.goals.get(planned.source_index).ok_or_else(|| RuntimeError::Internal {
                detail: "planned goal without provenance".to_string(),
            })?;
        let source = self.program.goal_at(path).ok_or_else(|| RuntimeError::Internal {
            detail: "goal provenance points outside the program".to_string(),
        })?;
        let source = source.clone();
        let logic_goal = &logic_rule.body[planned.source_index];

        match (&planned.access, &source.value) {
            (Access::Extern { mode, .. }, Goal::Atom(atom)) => {
                self.eval_extern(atom, logic_goal, mode, false, names, envs)
            }
            (Access::Extern { mode, .. }, Goal::Not(not_goal)) => {
                self.eval_extern(&not_goal.atom.value, logic_goal, mode, true, names, envs)
            }
            (Access::Input { id }, Goal::Atom(atom)) => {
                self.eval_input(atom, logic_goal, *id, false, names, envs)
            }
            (Access::Input { id }, Goal::Not(not_goal)) => {
                self.eval_input(&not_goal.atom.value, logic_goal, *id, true, names, envs)
            }
            (Access::Derived { id, pattern }, Goal::Atom(atom)) => {
                self.eval_derived(atom, logic_goal, *id, pattern, false, names, envs)
            }
            (Access::Derived { id, pattern }, Goal::Not(not_goal)) => {
                self.eval_derived(&not_goal.atom.value, logic_goal, *id, pattern, true, names, envs)
            }
            (Access::Derived { id, pattern }, Goal::Aggregate(binder)) => {
                self.eval_aggregate(binder, logic_goal, *id, pattern, names, envs)
            }
            (Access::Derived { id, pattern }, Goal::ChooseTopK(binder)) => {
                self.eval_choose(binder, logic_goal, *id, pattern, names, envs)
            }
            (Access::Builtin { .. }, Goal::Constraint(constraint)) => {
                self.eval_constraint(constraint, names, envs)
            }
            (Access::Builtin { .. }, Goal::Atom(atom)) => {
                self.eval_engine_builtin(atom, false, names, envs)
            }
            (Access::Builtin { .. }, Goal::Not(not_goal)) => {
                self.eval_engine_builtin(&not_goal.atom.value, true, names, envs)
            }
            _ => Err(RuntimeError::Internal {
                detail: "planned access does not match its source goal kind".to_string(),
            }),
        }
    }

    /// Ground constants of an atom's argument positions, evaluated once
    /// per goal (compound constants with variables are rejected by the
    /// lowering for positional atoms).
    fn positional_consts(
        &self,
        atom: &Atom,
        logic_goal: &raql_plan::Goal,
        names: &NameMap,
    ) -> Result<Vec<Option<Ops::Value>>, RuntimeError> {
        let empty: Env<Ops::Value> = Vec::new();
        logic_goal
            .args
            .iter()
            .zip(&atom.terms)
            .map(|(arg, term)| match arg {
                raql_plan::Term::Const => eval_term(term, &empty, names).map(Some),
                raql_plan::Term::Var(_) => Ok(None),
            })
            .collect()
    }

    fn unify_positional_row(
        env: &Env<Ops::Value>,
        logic_goal: &raql_plan::Goal,
        consts: &[Option<Ops::Value>],
        row: &[Ops::Value],
    ) -> Option<Env<Ops::Value>> {
        let mut candidate = env.clone();
        for (position, arg) in logic_goal.args.iter().enumerate() {
            match arg {
                raql_plan::Term::Var(var) => match env_get(&candidate, *var) {
                    Some(bound) => {
                        if bound != &row[position] {
                            return None;
                        }
                    }
                    None => env_set(&mut candidate, *var, row[position].clone()),
                },
                raql_plan::Term::Const => {
                    if consts[position].as_ref() != Some(&row[position]) {
                        return None;
                    }
                }
            }
        }
        Some(candidate)
    }

    fn eval_extern(
        &mut self,
        atom: &Atom,
        logic_goal: &raql_plan::Goal,
        mode: &'static ModeDef,
        negated: bool,
        names: &NameMap,
        envs: Vec<Env<Ops::Value>>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let consts = self.positional_consts(atom, logic_goal, names)?;
        let bound_positions: Vec<usize> = mode
            .pattern
            .iter()
            .enumerate()
            .filter(|(_, binding)| **binding == raql_plan::Binding::Bound)
            .map(|(position, _)| position)
            .collect();

        let mut out = Vec::new();
        for env in envs {
            let mut operator_inputs = Vec::with_capacity(bound_positions.len());
            for &position in &bound_positions {
                let value = match &logic_goal.args[position] {
                    raql_plan::Term::Var(var) => env_get(&env, *var).cloned(),
                    raql_plan::Term::Const => consts[position].clone(),
                };
                operator_inputs.push(value.ok_or_else(|| RuntimeError::Internal {
                    detail: format!(
                        "operator `{}` input position {position} unbound at runtime",
                        mode.operator.name(),
                    ),
                })?);
            }
            let rows = self.ops.invoke(mode.operator, &operator_inputs).map_err(|error| {
                RuntimeError::OperatorFailed {
                    operator: mode.operator.name(),
                    message: error.to_string(),
                }
            })?;
            let mut matched = false;
            for row in &rows {
                if row.len() != logic_goal.args.len() {
                    return Err(RuntimeError::Internal {
                        detail: format!(
                            "operator `{}` returned a row of arity {}, predicate declares {}",
                            mode.operator.name(),
                            row.len(),
                            logic_goal.args.len(),
                        ),
                    });
                }
                if let Some(candidate) = Self::unify_positional_row(&env, logic_goal, &consts, row)
                {
                    if negated {
                        matched = true;
                        break;
                    }
                    out.push(candidate);
                }
            }
            if negated && !matched {
                out.push(env);
            }
        }
        Ok(out)
    }

    fn eval_input(
        &mut self,
        atom: &Atom,
        logic_goal: &raql_plan::Goal,
        id: raql_plan::InputId,
        negated: bool,
        names: &NameMap,
        envs: Vec<Env<Ops::Value>>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let consts = self.positional_consts(atom, logic_goal, names)?;
        let name = &self.program.lowered().logic.inputs[id.0].name;
        let rows = self.inputs.get(name).cloned().unwrap_or_default();
        let mut out = Vec::new();
        for env in envs {
            let mut matched = false;
            for row in &rows {
                if row.len() != logic_goal.args.len() {
                    continue;
                }
                if let Some(candidate) = Self::unify_positional_row(&env, logic_goal, &consts, row)
                {
                    if negated {
                        matched = true;
                        break;
                    }
                    out.push(candidate);
                }
            }
            if negated && !matched {
                out.push(env);
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)] // mirrors eval_goal's dispatch row
    fn eval_derived(
        &mut self,
        atom: &Atom,
        logic_goal: &raql_plan::Goal,
        id: DerivedId,
        pattern: &Pattern,
        negated: bool,
        names: &NameMap,
        envs: Vec<Env<Ops::Value>>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let consts = self.positional_consts(atom, logic_goal, names)?;
        let mut out = Vec::new();
        for env in envs {
            let mut seed = Vec::new();
            for (position, arg) in logic_goal.args.iter().enumerate() {
                if !pattern.is_bound(position) {
                    continue;
                }
                let value = match arg {
                    raql_plan::Term::Var(var) => env_get(&env, *var).cloned(),
                    raql_plan::Term::Const => consts[position].clone(),
                };
                seed.push(value.ok_or_else(|| RuntimeError::Internal {
                    detail: format!("derived seed position {position} unbound at runtime"),
                })?);
            }
            let (rows, _) = self.eval_spec(id, pattern, seed)?;
            let mut matched = false;
            for row in rows.iter() {
                if let Some(candidate) = Self::unify_positional_row(&env, logic_goal, &consts, row)
                {
                    if negated {
                        matched = true;
                        break;
                    }
                    out.push(candidate);
                }
            }
            if negated && !matched {
                out.push(env);
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // Binders: aggregates and choose_topk over synthesized sub-bodies
    // -----------------------------------------------------------------

    /// The head column names of a synthesized binder specialization, in
    /// projection order.
    fn binder_columns(&self, id: DerivedId) -> Vec<String> {
        let derived = &self.program.lowered().logic.derived[id.0];
        let rule = &derived.rules[0];
        derived.rules[0]
            .head
            .iter()
            .map(|term| match term {
                raql_plan::Term::Var(var) => rule.vars[var.0 as usize].clone(),
                raql_plan::Term::Const => String::new(),
            })
            .collect()
    }

    fn binder_seed(
        &self,
        logic_goal: &raql_plan::Goal,
        pattern: &Pattern,
        env: &Env<Ops::Value>,
        k_group: Option<(&Spanned<Term>, &Spanned<Term>)>,
        names: &NameMap,
    ) -> Result<Vec<Ops::Value>, RuntimeError> {
        let mut seed = Vec::new();
        for (position, arg) in logic_goal.args.iter().enumerate() {
            if !pattern.is_bound(position) {
                continue;
            }
            let value = match arg {
                raql_plan::Term::Var(var) => {
                    env_get(env, *var).cloned().ok_or_else(|| RuntimeError::Internal {
                        detail: format!("binder seed position {position} unbound at runtime"),
                    })?
                }
                raql_plan::Term::Const => {
                    // Constant binder slots exist only for choose_topk's
                    // `k`/`group` leading positions.
                    let (k, group) = k_group.ok_or_else(|| RuntimeError::Internal {
                        detail: "constant binder slot outside choose_topk".to_string(),
                    })?;
                    let term = if position == 0 { k } else { group };
                    eval_term(term, env, names)?
                }
            };
            seed.push(value);
        }
        Ok(seed)
    }

    fn eval_aggregate(
        &mut self,
        binder: &AggregateBinder,
        logic_goal: &raql_plan::Goal,
        id: DerivedId,
        pattern: &Pattern,
        names: &NameMap,
        envs: Vec<Env<Ops::Value>>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let columns = self.binder_columns(id);
        let projection_column = binder
            .projection_var
            .as_ref()
            .map(|var| {
                columns.iter().position(|name| name == var.value.as_str()).ok_or_else(|| {
                    RuntimeError::Internal {
                        detail: format!("projection variable `{}` not in sub-body", var.value),
                    }
                })
            })
            .transpose()?;
        let out_var = match logic_goal.args.last() {
            Some(raql_plan::Term::Var(var)) => *var,
            _ => {
                return Err(RuntimeError::Internal {
                    detail: "aggregate goal without an output variable".to_string(),
                });
            }
        };

        let mut out = Vec::new();
        for env in envs {
            let seed = self.binder_seed(logic_goal, pattern, &env, None, names)?;
            let (rows, _) = self.eval_spec(id, pattern, seed)?;

            let projected: Option<IndexSet<&Ops::Value>> = projection_column
                .map(|column| rows.iter().map(|row| &row[column]).collect());

            let result = match binder.name.value {
                AggregateName::Count | AggregateName::CountDistinct => {
                    let count = match &projected {
                        Some(values) => values.len(),
                        None => rows.len(),
                    };
                    Some(Ops::Value::int(count as i64))
                }
                AggregateName::Sum => {
                    let values = projected.as_ref().ok_or_else(|| RuntimeError::Internal {
                        detail: "sum without a projection variable".to_string(),
                    })?;
                    let mut acc: i64 = 0;
                    for value in values {
                        let term = value.as_int().ok_or_else(|| {
                            RuntimeError::TypeMismatchContext {
                                context: format!(
                                    "aggregate `sum` for output `{}` expected `int` projections, \
                                     found {}",
                                    binder.out.value,
                                    value.type_tag(),
                                ),
                            }
                        })?;
                        acc = acc.checked_add(term).ok_or(RuntimeError::Overflow)?;
                    }
                    Some(Ops::Value::int(acc))
                }
                AggregateName::Min | AggregateName::Max => {
                    let values = projected.as_ref().ok_or_else(|| RuntimeError::Internal {
                        detail: "min/max without a projection variable".to_string(),
                    })?;
                    let mut best: Option<&Ops::Value> = None;
                    for value in values {
                        best = Some(match best {
                            None => value,
                            Some(current) => {
                                let ordering = value.plain_cmp(current).ok_or_else(|| {
                                    RuntimeError::TypeMismatchContext {
                                        context: format!(
                                            "aggregate `{}` cannot order {} values",
                                            aggregate_label(binder.name.value),
                                            value.type_tag(),
                                        ),
                                    }
                                })?;
                                let take = match binder.name.value {
                                    AggregateName::Min => ordering.is_lt(),
                                    _ => ordering.is_gt(),
                                };
                                if take { value } else { current }
                            }
                        });
                    }
                    best.cloned()
                }
                // Sum handled above; count variants handled above.
            };

            // min/max over an empty sub-extent fail the goal (no rows);
            // count/sum bind their zero.
            let Some(result) = result else {
                continue;
            };
            let mut candidate = env;
            match env_get(&candidate, out_var) {
                Some(bound) if bound != &result => continue,
                Some(_) => out.push(candidate),
                None => {
                    env_set(&mut candidate, out_var, result);
                    out.push(candidate);
                }
            }
        }
        Ok(out)
    }

    fn eval_choose(
        &mut self,
        binder: &ChooseTopkBinder,
        logic_goal: &raql_plan::Goal,
        id: DerivedId,
        pattern: &Pattern,
        names: &NameMap,
        envs: Vec<Env<Ops::Value>>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let columns = self.binder_columns(id);
        let score_column = columns.len() - 2;
        let item_column = columns.len() - 1;
        let (score_var, item_var) = match &logic_goal.args[logic_goal.args.len() - 2..] {
            [raql_plan::Term::Var(score), raql_plan::Term::Var(item)] => (*score, *item),
            _ => {
                return Err(RuntimeError::Internal {
                    detail: "choose_topk goal without output variables".to_string(),
                });
            }
        };

        let mut out = Vec::new();
        for env in envs {
            let k_value = eval_term(&binder.k, &env, names)?;
            let k = k_value.as_int().filter(|k| *k > 0).ok_or_else(|| {
                RuntimeError::TypeMismatchContext {
                    context: format!(
                        "choose_topk `{}` expected `k` to evaluate to a positive `int`, found {}",
                        binder.tag.value,
                        k_value.type_tag(),
                    ),
                }
            })?;
            let k = usize::try_from(k).unwrap_or(usize::MAX);

            let seed =
                self.binder_seed(logic_goal, pattern, &env, Some((&binder.k, &binder.group)), names)?;
            let (rows, _) = self.eval_spec(id, pattern, seed)?;

            let mut candidates: IndexSet<(Ops::Value, Ops::Value)> = IndexSet::new();
            for row in rows.iter() {
                candidates.insert((row[score_column].clone(), row[item_column].clone()));
            }
            let mut sorted: Vec<(Ops::Value, Ops::Value)> = candidates.into_iter().collect();
            let mut order_error = None;
            // Stable: score descending; ties keep first-derived order.
            sorted.sort_by(|(left, _), (right, _)| match right.plain_cmp(left) {
                Some(ordering) => ordering,
                None => {
                    order_error.get_or_insert_with(|| RuntimeError::TypeMismatchContext {
                        context: format!(
                            "choose_topk `{}` cannot order {} scores",
                            binder.tag.value,
                            left.type_tag(),
                        ),
                    });
                    std::cmp::Ordering::Equal
                }
            });
            if let Some(error) = order_error {
                return Err(error);
            }

            for (score, item) in sorted.into_iter().take(k) {
                let mut candidate = env.clone();
                let score_ok = match env_get(&candidate, score_var) {
                    Some(bound) => bound == &score,
                    None => {
                        env_set(&mut candidate, score_var, score);
                        true
                    }
                };
                if !score_ok {
                    continue;
                }
                let item_ok = match env_get(&candidate, item_var) {
                    Some(bound) => bound == &item,
                    None => {
                        env_set(&mut candidate, item_var, item);
                        true
                    }
                };
                if item_ok {
                    out.push(candidate);
                }
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // Builtins: constraints and engine-managed atoms
    // -----------------------------------------------------------------

    fn eval_constraint(
        &mut self,
        constraint: &Constraint,
        names: &NameMap,
        envs: Vec<Env<Ops::Value>>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let mut out = Vec::new();
        match constraint {
            Constraint::Relational(relation) => {
                for env in envs {
                    match relation.op.value {
                        RelOp::Eq => {
                            let mut candidate = env;
                            let unified = if term_ground(&relation.lhs, &candidate, names) {
                                let value = eval_term(&relation.lhs, &candidate, names)?;
                                unify_term(&relation.rhs, &value, &mut candidate, names)?
                            } else if term_ground(&relation.rhs, &candidate, names) {
                                let value = eval_term(&relation.rhs, &candidate, names)?;
                                unify_term(&relation.lhs, &value, &mut candidate, names)?
                            } else {
                                return Err(RuntimeError::Internal {
                                    detail: "`=` reached with neither side ground".to_string(),
                                });
                            };
                            if unified {
                                out.push(candidate);
                            }
                        }
                        RelOp::NotEq => {
                            let lhs = eval_term(&relation.lhs, &env, names)?;
                            let rhs = eval_term(&relation.rhs, &env, names)?;
                            if lhs != rhs {
                                out.push(env);
                            }
                        }
                        RelOp::Lt | RelOp::LtEq | RelOp::Gt | RelOp::GtEq => {
                            let lhs = eval_term(&relation.lhs, &env, names)?;
                            let rhs = eval_term(&relation.rhs, &env, names)?;
                            let ordering = lhs.plain_cmp(&rhs).ok_or_else(|| {
                                RuntimeError::TypeMismatchContext {
                                    context: format!(
                                        "comparison cannot order {} against {}",
                                        lhs.type_tag(),
                                        rhs.type_tag(),
                                    ),
                                }
                            })?;
                            let keep = match relation.op.value {
                                RelOp::Lt => ordering.is_lt(),
                                RelOp::LtEq => !ordering.is_gt(),
                                RelOp::Gt => ordering.is_gt(),
                                RelOp::GtEq => !ordering.is_lt(),
                                _ => unreachable!(),
                            };
                            if keep {
                                out.push(env);
                            }
                        }
                    }
                }
            }
            Constraint::ArithmeticBind(bind) => {
                for env in envs {
                    let value = eval_int_expr(&bind.expr, &env, names)?;
                    let target = names.var(&bind.target.value).ok_or_else(|| {
                        RuntimeError::Internal {
                            detail: format!("`:=` target `{}` not in var table", bind.target.value),
                        }
                    })?;
                    let mut candidate = env;
                    match env_get(&candidate, target) {
                        Some(bound) => {
                            let bound_int = bound.as_int().ok_or_else(|| {
                                RuntimeError::TypeMismatchContext {
                                    context: format!(
                                        "arithmetic bind target `{}` expected `int`, found {}",
                                        bind.target.value,
                                        bound.type_tag(),
                                    ),
                                }
                            })?;
                            if bound_int == value {
                                out.push(candidate);
                            }
                        }
                        None => {
                            env_set(&mut candidate, target, Ops::Value::int(value));
                            out.push(candidate);
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    fn eval_engine_builtin(
        &mut self,
        atom: &Atom,
        negated: bool,
        names: &NameMap,
        envs: Vec<Env<Ops::Value>>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let mut out = Vec::new();
        for env in envs {
            let produced = self.eval_engine_builtin_env(atom, names, env.clone())?;
            if negated {
                if produced.is_empty() {
                    out.push(env);
                }
            } else {
                out.extend(produced);
            }
        }
        Ok(out)
    }

    fn eval_engine_builtin_env(
        &mut self,
        atom: &Atom,
        names: &NameMap,
        env: Env<Ops::Value>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let name = atom.name.value.as_str();
        match name {
            "contains" | "starts_with" => {
                let left = eval_term(&atom.terms[0], &env, names)?;
                let right = eval_term(&atom.terms[1], &env, names)?;
                let (Some(left), Some(right)) = (left.as_str(), right.as_str()) else {
                    return Err(builtin_type_mismatch(name, "string"));
                };
                let holds = match name {
                    "contains" => left.contains(right),
                    _ => left.starts_with(right),
                };
                Ok(if holds { vec![env] } else { Vec::new() })
            }
            "fmt" => {
                let format = eval_term(&atom.terms[0], &env, names)?;
                let args = eval_term(&atom.terms[1], &env, names)?;
                let Some(format) = format.as_str() else {
                    return Err(builtin_type_mismatch("fmt", "string"));
                };
                let Some(args) = args.as_list() else {
                    return Err(builtin_type_mismatch("fmt", "list<string>"));
                };
                let mut rendered_args = Vec::with_capacity(args.len());
                for value in args {
                    let Some(text) = value.as_str() else {
                        return Err(builtin_type_mismatch("fmt", "string"));
                    };
                    rendered_args.push(text.to_string());
                }
                let rendered = render_fmt(format, &rendered_args);
                let mut candidate = env;
                if unify_term(&atom.terms[2], &Ops::Value::string(&rendered), &mut candidate, names)? {
                    Ok(vec![candidate])
                } else {
                    Ok(Vec::new())
                }
            }
            "coalesce" => {
                let option = eval_term(&atom.terms[0], &env, names)?;
                let default = eval_term(&atom.terms[1], &env, names)?;
                let value = match option.as_option() {
                    Some(Some(inner)) => inner.clone(),
                    Some(None) => default,
                    None => return Err(builtin_type_mismatch("coalesce", "option<_>")),
                };
                let mut candidate = env;
                if unify_term(&atom.terms[2], &value, &mut candidate, names)? {
                    Ok(vec![candidate])
                } else {
                    Ok(Vec::new())
                }
            }
            "dispatch_str" => {
                let dispatch = eval_term(&atom.terms[0], &env, names)?;
                let Some((_, variant)) = dispatch.as_enum() else {
                    return Err(builtin_type_mismatch("dispatch_str", "DispatchKind"));
                };
                let rendered = variant.to_lowercase();
                let mut candidate = env;
                if unify_term(&atom.terms[1], &Ops::Value::string(&rendered), &mut candidate, names)?
                {
                    Ok(vec![candidate])
                } else {
                    Ok(Vec::new())
                }
            }
            "witness_path" => self.eval_witness_path(atom, names, env),
            "path_hop" => {
                let mut out = Vec::new();
                // BTreeMap: path ids allocate in discovery order and the
                // iteration must stay deterministic.
                let cached: Vec<Vec<Ops::Value>> =
                    self.path_cache.values().flatten().cloned().collect();
                for row in cached {
                    let mut candidate = env.clone();
                    let mut unified = true;
                    for (term, value) in atom.terms.iter().zip(&row) {
                        if !unify_term(term, value, &mut candidate, names)? {
                            unified = false;
                            break;
                        }
                    }
                    if unified {
                        out.push(candidate);
                    }
                }
                Ok(out)
            }
            other => Err(RuntimeError::Internal {
                detail: format!("unknown engine builtin `{other}`"),
            }),
        }
    }

    /// Bounded reachability over the `graph_edge` extent (SPEC §11.2 as
    /// carried over): breadth-first, cycle-free, `path_limit` witnesses of
    /// at most `path_max_depth` hops, persisted for `path_hop`.
    fn eval_witness_path(
        &mut self,
        atom: &Atom,
        names: &NameMap,
        env: Env<Ops::Value>,
    ) -> Result<Vec<Env<Ops::Value>>, RuntimeError> {
        let graph = eval_term(&atom.terms[0], &env, names)?;
        let from = eval_term(&atom.terms[1], &env, names)?;
        let to = eval_term(&atom.terms[2], &env, names)?;

        let max_depth = scalar_int(&self.inputs, "path_max_depth", 8)?;
        let max_depth = usize::try_from(max_depth).map_err(|_| {
            RuntimeError::TypeMismatchContext {
                context: format!(
                    "input `path_max_depth` expected `int >= 0`, found {max_depth}",
                ),
            }
        })?;
        let path_limit = scalar_int(&self.inputs, "path_limit", 1)?;
        if path_limit <= 0 {
            return Err(RuntimeError::TypeMismatchContext {
                context: format!("input `path_limit` expected `int > 0`, found {path_limit}"),
            });
        }
        let path_limit = usize::try_from(path_limit).unwrap_or(usize::MAX);

        let edges: Vec<Vec<Ops::Value>> =
            if let Some(id) = self.program.lowered().derived_id("graph_edge") {
                let arity = self.program.lowered().logic.derived[id.0].arity;
                let (rows, _) = self.eval_spec(id, &Pattern::new(vec![false; arity]), Vec::new())?;
                rows.iter().cloned().collect()
            } else if let Some(rows) = self.inputs.get("graph_edge") {
                rows.iter().cloned().collect()
            } else {
                Vec::new()
            };

        // (to, edge-kind, evidence) per from-node, in extent order — the
        // extent order is deterministic, so path selection is too.
        type Out<'v, V> = Vec<(&'v V, &'v V, &'v V)>;
        let mut adjacency: HashMap<&Ops::Value, Out<'_, Ops::Value>> = HashMap::new();
        for edge in &edges {
            if edge.len() != 5 || edge[0] != graph {
                continue;
            }
            adjacency.entry(&edge[1]).or_default().push((&edge[2], &edge[3], &edge[4]));
        }

        // (from-node, to-node, kind, evidence) per hop of a partial path.
        type Hop<'v, V> = (V, &'v V, &'v V, &'v V);
        type Frontier<'v, V> = Vec<(V, Vec<V>, Vec<Hop<'v, V>>)>;
        let mut queue: Frontier<'_, Ops::Value> =
            vec![(from.clone(), vec![from.clone()], Vec::new())];
        let mut found = Vec::new();
        while let Some((node, visited, hops)) = queue.pop() {
            if node == to {
                found.push(hops.clone());
                if found.len() >= path_limit {
                    break;
                }
                continue;
            }
            if hops.len() >= max_depth {
                continue;
            }
            if let Some(nexts) = adjacency.get(&node) {
                for (next, kind, evidence) in nexts {
                    if visited.contains(next) {
                        continue;
                    }
                    let mut next_visited = visited.clone();
                    next_visited.push((*next).clone());
                    let mut next_hops = hops.clone();
                    next_hops.push((node.clone(), next, kind, evidence));
                    queue.insert(0, ((*next).clone(), next_visited, next_hops));
                }
            }
        }

        let mut out = Vec::new();
        for hops in found {
            let path_id = format!("path:{}", self.next_path_id);
            self.next_path_id += 1;
            let path_value = Ops::Value::string(&path_id);
            let mut candidate = env.clone();
            if unify_term(&atom.terms[3], &path_value, &mut candidate, names)? {
                out.push(candidate);
                let rows = hops
                    .into_iter()
                    .enumerate()
                    .map(|(sequence, (hop_from, hop_to, kind, evidence))| {
                        vec![
                            path_value.clone(),
                            Ops::Value::int(sequence as i64),
                            hop_from,
                            hop_to.clone(),
                            kind.clone(),
                            evidence.clone(),
                        ]
                    })
                    .collect();
                self.path_cache.insert(path_id, rows);
            }
        }
        Ok(out)
    }
}

/// A fact row matches a seed when every bound position agrees.
fn seed_matches<V: EngineValue>(pattern: &Pattern, seed: &[V], row: &[V]) -> bool {
    if row.len() != pattern.arity() {
        return false;
    }
    let mut next = seed.iter();
    for (position, value) in row.iter().enumerate() {
        if pattern.is_bound(position) {
            match next.next() {
                Some(expected) if expected == value => {}
                _ => return false,
            }
        }
    }
    true
}

fn aggregate_label(name: AggregateName) -> &'static str {
    match name {
        AggregateName::Count => "count",
        AggregateName::CountDistinct => "count_distinct",
        AggregateName::Sum => "sum",
        AggregateName::Min => "min",
        AggregateName::Max => "max",
    }
}

fn builtin_type_mismatch(name: &str, expected: &str) -> RuntimeError {
    RuntimeError::TypeMismatchContext {
        context: format!("builtin `{name}` expected {expected} arguments"),
    }
}

fn render_fmt(format: &str, args: &[String]) -> String {
    let mut out = String::new();
    let mut rest = format;
    let mut index = 0usize;
    while let Some(position) = rest.find("{}") {
        out.push_str(&rest[..position]);
        if let Some(arg) = args.get(index) {
            out.push_str(arg);
        } else {
            out.push_str("{}");
        }
        index += 1;
        rest = &rest[position + 2..];
    }
    out.push_str(rest);
    out
}

/// A single-row, single-column `int` input with a default.
fn scalar_int<V: EngineValue>(
    inputs: &BTreeMap<String, IndexSet<Vec<V>>>,
    name: &str,
    default: i64,
) -> Result<i64, RuntimeError> {
    let Some(rows) = inputs.get(name) else {
        return Ok(default);
    };
    let Some(first) = rows.first() else {
        return Ok(default);
    };
    if rows.len() != 1 || first.len() != 1 {
        return Err(RuntimeError::ScalarCardinality {
            predicate: name.to_string(),
            context: format!("expected exactly one scalar row, found {}", rows.len()),
        });
    }
    first[0].as_int().ok_or_else(|| RuntimeError::TypeMismatchContext {
        context: format!("input `{name}` expected `int`, found {}", first[0].type_tag()),
    })
}

/// A single-row, single-column `option<int>` input.
fn scalar_option_int<V: EngineValue>(
    inputs: &BTreeMap<String, IndexSet<Vec<V>>>,
    name: &str,
) -> Result<Option<i64>, RuntimeError> {
    let Some(rows) = inputs.get(name) else {
        return Ok(None);
    };
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    if rows.len() != 1 || first.len() != 1 {
        return Err(RuntimeError::ScalarCardinality {
            predicate: name.to_string(),
            context: format!("expected exactly one optional scalar row, found {}", rows.len()),
        });
    }
    match first[0].as_option() {
        Some(None) => Ok(None),
        Some(Some(inner)) => inner.as_int().map(Some).ok_or_else(|| {
            RuntimeError::TypeMismatchContext {
                context: format!(
                    "input `{name}` expected `option<int>`, found {}",
                    first[0].type_tag(),
                ),
            }
        }),
        None => Err(RuntimeError::TypeMismatchContext {
            context: format!(
                "input `{name}` expected `option<int>`, found {}",
                first[0].type_tag(),
            ),
        }),
    }
}
