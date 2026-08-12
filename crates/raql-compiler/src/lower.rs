//! Lowering: typechecked, disjunction-flattened rules to the planner's
//! logical IR (SPEC §10.1), plus the provenance the engine needs to map
//! planned goals back to source AST.
//!
//! Shape decisions:
//!
//! - Every top-level body goal lowers to exactly one logic goal at the
//!   same body index, so a planned goal's `source_index` is directly a
//!   source body index.
//! - Aggregates and `choose_topk` lower to a *synthesized derived
//!   predicate* holding the sub-body (so the planner orders it honestly
//!   under the correlated bindings, and demand specialization plans it),
//!   called from the outer rule as a derived goal whose arguments are the
//!   correlated variables (bound), fresh throwaway slots for the
//!   sub-body's local variables, and the variables the binder itself
//!   binds. The engine recognizes the source goal kind and evaluates the
//!   specialization's rule env-wise, then applies the binder semantics —
//!   the synthesized head exists for the planner's boundness math only.
//! - Constraints lower to per-goal planner builtins: `=` accepts either
//!   side ground, comparisons need both sides ground, `:=` needs its
//!   expression ground and computes its target.
//! - Only rules reachable from the demand roots are lowered. Unreachable
//!   stdlib rules that reference `disabled` catalog predicates therefore
//!   compile until something actually demands them (SPEC §4.3).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use raql_plan::{
    Binding, BuiltinDef, BuiltinId, DerivedDef, DerivedId, GoalRef, InputDef, InputId, Pattern,
    Root, Var, v0_catalog,
};
use raql_syntax::{Atom, Constraint, DeclAttr, Expr, Goal, RelOp, Spanned, Term};

use crate::diagnostics::{CompilerDiagnostic, DiagBundle};
use crate::externs::{engine_builtin_patterns, is_scalar_input};
use crate::program::{ModeDir, TypedProgram, TypedRule};

/// Where a goal lives in the flattened rules: rule index, then body index,
/// then (for aggregate/choose sub-bodies) nested body indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalPath {
    pub rule: usize,
    pub path: Vec<usize>,
}

impl GoalPath {
    /// Resolve this path against the flattened rules.
    pub fn resolve<'r>(&self, rules: &'r [TypedRule]) -> Option<&'r Spanned<Goal>> {
        let rule = rules.get(self.rule)?;
        let (first, rest) = self.path.split_first()?;
        let mut goal = rule.rule().value.body.get(*first)?;
        for index in rest {
            let subs = match &goal.value {
                Goal::Aggregate(aggregate) => &aggregate.goals,
                Goal::ChooseTopK(choose) => &choose.goals,
                _ => return None,
            };
            goal = subs.get(*index)?;
        }
        Some(goal)
    }
}

/// The lowered program: the planner input plus provenance parallel to it.
#[derive(Clone, Debug)]
pub struct LoweredProgram {
    pub logic: raql_plan::Program,
    /// Parallel to `logic.derived`.
    pub derived: Vec<DerivedProvenance>,
    /// Parallel to `logic.inputs`.
    pub inputs: Vec<InputProvenance>,
}

impl LoweredProgram {
    pub fn derived_id(&self, name: &str) -> Option<DerivedId> {
        self.logic
            .derived
            .iter()
            .position(|derived| derived.name == name)
            .map(DerivedId)
    }
}

#[derive(Clone, Debug)]
pub struct DerivedProvenance {
    /// For a synthesized aggregate/choose body: the binder goal it was
    /// lowered from. `None` for user predicates.
    pub binder: Option<GoalPath>,
    /// Parallel to the derived def's rules.
    pub rules: Vec<RuleProvenance>,
}

#[derive(Clone, Debug)]
pub struct RuleProvenance {
    /// Index of the flattened source rule this logic rule lowers.
    pub rule: usize,
    /// Parallel to the logic rule's body: each goal's source location.
    pub goals: Vec<GoalPath>,
}

#[derive(Clone, Debug)]
pub struct InputProvenance {
    pub name: String,
    pub kind: InputKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// `input`-attributed: rows arrive with the request (plus any facts).
    Request,
    /// Populated only by facts in the program text.
    Facts,
    /// Reserved engine scalar (`path_limit`, `control_max_depth`, ...).
    Scalar,
}

/// Lower the reachable part of the program. Errors accumulate into
/// `diagnostics`; the return is `None` once anything failed.
pub(crate) fn lower(
    typed: &TypedProgram,
    rules: &[TypedRule],
    diagnostics: &mut DiagBundle,
) -> Option<LoweredProgram> {
    let catalog = v0_catalog();
    let rules_by_head: BTreeMap<&str, Vec<usize>> = {
        let mut map = BTreeMap::<&str, Vec<usize>>::new();
        for (index, rule) in rules.iter().enumerate() {
            map.entry(rule.head_predicate()).or_default().push(index);
        }
        map
    };
    let fact_names: BTreeSet<&str> = typed
        .facts()
        .iter()
        .map(|fact| fact.value.atom.value.name.value.as_str())
        .collect();

    // Demand roots: output predicates declared in the root file; else
    // every root-file rule head plus root-file non-extern declarations
    // (ad-hoc program shape); else — a root file that only includes —
    // every rule head anywhere.
    let root_file = typed.sources().files().first().map(|file| file.id());
    let mut roots = BTreeSet::<String>::new();
    if let Some(root_file) = root_file {
        for (name, decl) in typed.predicates() {
            if decl.span().file == root_file && decl.is_output() {
                roots.insert(name.clone());
            }
        }
        if roots.is_empty() {
            for rule in rules {
                if rule.rule().span.file == root_file {
                    roots.insert(rule.head_predicate().to_string());
                }
            }
            for (name, decl) in typed.predicates() {
                if decl.span().file == root_file && !decl.attrs().contains(&DeclAttr::Extern) {
                    roots.insert(name.clone());
                }
            }
        }
    }
    if roots.is_empty() {
        for rule in rules {
            roots.insert(rule.head_predicate().to_string());
        }
    }

    // Reachability over rule bodies (atoms, negations, binder sub-goals).
    let mut reachable = BTreeSet::<String>::new();
    let mut agenda: VecDeque<String> = roots.iter().cloned().collect();
    while let Some(name) = agenda.pop_front() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        for &rule_index in rules_by_head.get(name.as_str()).into_iter().flatten() {
            for goal in &rules[rule_index].rule().value.body {
                collect_goal_atom_names(goal, &mut agenda);
            }
        }
    }

    // Partition the reachable names. Catalog externs and engine builtins
    // are classified at their goal sites; everything else becomes an
    // input relation or a derived predicate.
    let mut derived_names = Vec::<String>::new();
    let mut inputs = Vec::<InputDef>::new();
    let mut input_prov = Vec::<InputProvenance>::new();
    let mut input_ids = BTreeMap::<String, InputId>::new();
    for name in &reachable {
        if catalog.predicate(name).is_some() || engine_builtin_patterns(name).is_some() {
            continue;
        }
        let Some(decl) = typed.predicates().get(name) else {
            continue; // typecheck infers declarations for every used name
        };
        let has_rules = rules_by_head.contains_key(name.as_str());
        let kind = if has_rules {
            None
        } else if decl.attrs().contains(&DeclAttr::Input) {
            Some(InputKind::Request)
        } else if is_scalar_input(name) {
            Some(InputKind::Scalar)
        } else if fact_names.contains(name.as_str()) && !roots.contains(name) {
            Some(InputKind::Facts)
        } else {
            None
        };
        match kind {
            Some(kind) => {
                input_ids.insert(name.clone(), InputId(inputs.len()));
                inputs.push(InputDef { name: name.clone(), arity: decl.args().len() });
                input_prov.push(InputProvenance { name: name.clone(), kind });
            }
            None => derived_names.push(name.clone()),
        }
    }

    let derived_ids: BTreeMap<String, DerivedId> = derived_names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), DerivedId(index)))
        .collect();

    let mut lowerer = Lowerer {
        rules,
        diagnostics,
        catalog: &catalog,
        derived_ids: &derived_ids,
        input_ids: &input_ids,
        derived: Vec::new(),
        derived_prov: Vec::new(),
        builtins: Vec::new(),
        shared_builtin_ids: BTreeMap::new(),
    };

    // User predicates first (their ids are pre-assigned); synthesized
    // binder defs append behind them during rule lowering.
    for name in &derived_names {
        let arity = typed.predicates().get(name).map_or(0, |decl| decl.args().len());
        let declared_modes = typed.modes(name).map(|sigs| {
            let mut patterns: Vec<Vec<Binding>> = sigs
                .iter()
                .map(|sig| {
                    sig.args()
                        .iter()
                        .map(|(dir, _)| match dir {
                            ModeDir::In => Binding::Bound,
                            ModeDir::Out => Binding::Free,
                        })
                        .collect()
                })
                .collect();
            patterns.sort();
            patterns.dedup();
            patterns
        });
        lowerer.derived.push(DerivedDef {
            name: name.clone(),
            arity,
            declared_modes,
            rules: Vec::new(),
        });
        lowerer.derived_prov.push(DerivedProvenance { binder: None, rules: Vec::new() });
    }
    for name in &derived_names {
        let id = derived_ids[name];
        for &rule_index in rules_by_head.get(name.as_str()).into_iter().flatten() {
            let (rule, provenance) = lowerer.lower_rule(rule_index);
            lowerer.derived[id.0].rules.push(rule);
            lowerer.derived_prov[id.0].rules.push(provenance);
        }
    }

    let roots = roots
        .iter()
        .filter_map(|name| {
            let id = *derived_ids.get(name)?;
            let arity = lowerer.derived[id.0].arity;
            Some(Root { predicate: id, pattern: Pattern::new(vec![false; arity]) })
        })
        .collect();

    let lowered = LoweredProgram {
        logic: raql_plan::Program {
            derived: lowerer.derived,
            inputs,
            builtins: lowerer.builtins,
            roots,
        },
        derived: lowerer.derived_prov,
        inputs: input_prov,
    };
    diagnostics.is_empty().then_some(lowered)
}

fn collect_goal_atom_names(goal: &Spanned<Goal>, agenda: &mut VecDeque<String>) {
    match &goal.value {
        Goal::Atom(atom) => agenda.push_back(atom.name.value.to_string()),
        Goal::Not(not_goal) => agenda.push_back(not_goal.atom.value.name.value.to_string()),
        Goal::Aggregate(aggregate) => {
            for sub in &aggregate.goals {
                collect_goal_atom_names(sub, agenda);
            }
        }
        Goal::ChooseTopK(choose) => {
            for sub in &choose.goals {
                collect_goal_atom_names(sub, agenda);
            }
        }
        Goal::Disjunction(group) => {
            for sub in group.branches.iter().flatten() {
                collect_goal_atom_names(sub, agenda);
            }
        }
        Goal::Constraint(_) => {}
    }
}

struct Lowerer<'t, 'd> {
    rules: &'t [TypedRule],
    diagnostics: &'d mut DiagBundle,
    catalog: &'t raql_plan::Catalog,
    derived_ids: &'t BTreeMap<String, DerivedId>,
    input_ids: &'t BTreeMap<String, InputId>,
    derived: Vec<DerivedDef>,
    derived_prov: Vec<DerivedProvenance>,
    builtins: Vec<BuiltinDef>,
    /// Fixed-pattern engine builtins share one `BuiltinDef` per name;
    /// constraint goals get their own defs (their patterns are per-goal).
    shared_builtin_ids: BTreeMap<String, BuiltinId>,
}

/// The head shape of a synthesized binder def: bound slots first, then
/// free slots (SPEC §9.2 — the sub-body plans under "correlated bound").
struct BinderShape<'s> {
    bound_slots: &'s [String],
    correlated: &'s [String],
    locals: &'s [String],
    outputs: &'s [String],
}

impl BinderShape<'_> {
    fn arity(&self) -> usize {
        self.bound_arity() + self.locals.len() + self.outputs.len()
    }

    fn bound_arity(&self) -> usize {
        self.bound_slots.len() + self.correlated.len()
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.bound_slots
            .iter()
            .chain(self.correlated)
            .chain(self.locals)
            .chain(self.outputs)
            .map(String::as_str)
    }
}

/// Variable interning scope of one logic rule under construction.
struct VarScope {
    names: Vec<String>,
    by_name: BTreeMap<String, Var>,
    fresh: usize,
}

impl VarScope {
    fn new() -> VarScope {
        VarScope { names: Vec::new(), by_name: BTreeMap::new(), fresh: 0 }
    }

    fn intern(&mut self, name: &str) -> Var {
        if let Some(var) = self.by_name.get(name) {
            return *var;
        }
        let var = Var(self.names.len() as u32);
        self.names.push(name.to_string());
        self.by_name.insert(name.to_string(), var);
        var
    }

    /// A fresh variable no source name can collide with (`_` wildcards,
    /// local slots of binder calls).
    fn fresh(&mut self, hint: &str) -> Var {
        let name = format!("{hint}#{}", self.fresh);
        self.fresh += 1;
        self.intern(&name)
    }
}

impl Lowerer<'_, '_> {
    fn lower_rule(&mut self, rule_index: usize) -> (raql_plan::Rule, RuleProvenance) {
        let rule = &self.rules[rule_index];
        let mut scope = VarScope::new();
        let head = rule
            .head_terms()
            .iter()
            .map(|term| match &term.value {
                Term::Var(name) => raql_plan::Term::Var(scope.intern(name.as_str())),
                // Compound head terms (`some(Site)`, constants) are always
                // bound conservatively: a seeded position never binds the
                // variables inside them; the body must.
                _ => raql_plan::Term::Const,
            })
            .collect();
        let head_var_names: BTreeSet<String> = rule
            .head_terms()
            .iter()
            .flat_map(|term| ordered_term_vars(&term.value))
            .collect();
        let (body, goal_paths) = self.lower_body(
            rule_index,
            &[],
            &rule.rule().value.body,
            &head_var_names,
            &mut scope,
        );
        (
            raql_plan::Rule { vars: scope.names, head, body },
            RuleProvenance { rule: rule_index, goals: goal_paths },
        )
    }

    /// Lower one goal sequence (a rule body or a binder sub-body). Every
    /// source goal produces exactly one logic goal.
    fn lower_body(
        &mut self,
        rule_index: usize,
        base_path: &[usize],
        goals: &[Spanned<Goal>],
        head_var_names: &BTreeSet<String>,
        scope: &mut VarScope,
    ) -> (Vec<raql_plan::Goal>, Vec<GoalPath>) {
        // "Used elsewhere" per goal: the enclosing head plus every sibling
        // goal's *outer-interface* variables — the correlation domain for
        // binders. A sibling binder exposes only what it binds into the
        // rule's env (its outputs, plus `k`/`group` requirements); its
        // sub-body locals stay invisible, so two aggregates reusing a
        // local name do not correlate.
        let goal_vars: Vec<Vec<String>> =
            goals.iter().map(|goal| outer_goal_vars(&goal.value)).collect();
        let mut lowered = Vec::new();
        let mut paths = Vec::new();
        for (index, goal) in goals.iter().enumerate() {
            let mut elsewhere: BTreeSet<String> = head_var_names.clone();
            for (other, vars) in goal_vars.iter().enumerate() {
                if other != index {
                    elsewhere.extend(vars.iter().cloned());
                }
            }
            let path = GoalPath {
                rule: rule_index,
                path: base_path.iter().copied().chain([index]).collect(),
            };
            let logic_goal = self.lower_goal(goal, &path, &elsewhere, scope);
            lowered.push(logic_goal);
            paths.push(path);
        }
        (lowered, paths)
    }

    fn lower_goal(
        &mut self,
        goal: &Spanned<Goal>,
        path: &GoalPath,
        elsewhere: &BTreeSet<String>,
        scope: &mut VarScope,
    ) -> raql_plan::Goal {
        match &goal.value {
            Goal::Atom(atom) => self.lower_atom(atom, goal, false, scope),
            Goal::Not(not_goal) => self.lower_atom(&not_goal.atom.value, goal, true, scope),
            Goal::Constraint(Constraint::Relational(rel)) => {
                let lhs_vars = ordered_term_vars(&rel.lhs.value);
                let rhs_vars = ordered_term_vars(&rel.rhs.value);
                let mut args = lhs_vars.clone();
                for var in &rhs_vars {
                    if !args.contains(var) {
                        args.push(var.clone());
                    }
                }
                let patterns = match rel.op.value {
                    // Unification runs with either side ground and binds
                    // the other.
                    RelOp::Eq => {
                        let lhs_bound: Vec<Binding> = args
                            .iter()
                            .map(|var| bound_if(lhs_vars.contains(var)))
                            .collect();
                        let rhs_bound: Vec<Binding> = args
                            .iter()
                            .map(|var| bound_if(rhs_vars.contains(var)))
                            .collect();
                        let mut patterns = vec![lhs_bound, rhs_bound];
                        patterns.dedup();
                        patterns
                    }
                    // Comparisons need both sides ground.
                    _ => vec![vec![Binding::Bound; args.len()]],
                };
                let name = match rel.op.value {
                    RelOp::Eq => "=",
                    RelOp::NotEq => "!=",
                    RelOp::Lt => "<",
                    RelOp::LtEq => "<=",
                    RelOp::Gt => ">",
                    RelOp::GtEq => ">=",
                };
                let id = self.push_builtin(name.to_string(), patterns);
                let args = args.iter().map(|name| raql_plan::Term::Var(scope.intern(name))).collect();
                raql_plan::Goal::positive(GoalRef::Builtin(id), args)
            }
            Goal::Constraint(Constraint::ArithmeticBind(bind)) => {
                let expr_vars = ordered_expr_vars(&bind.expr.value);
                let target = bind.target.value.to_string();
                let mut args = expr_vars.clone();
                if !args.contains(&target) {
                    args.push(target.clone());
                }
                let pattern = args
                    .iter()
                    .map(|var| bound_if(expr_vars.contains(var)))
                    .collect();
                let id = self.push_builtin(":=".to_string(), vec![pattern]);
                let args = args.iter().map(|name| raql_plan::Term::Var(scope.intern(name))).collect();
                raql_plan::Goal::positive(GoalRef::Builtin(id), args)
            }
            Goal::Aggregate(aggregate) => {
                let out = aggregate.out.value.to_string();
                let sub_vars = ordered_goals_vars(&aggregate.goals);
                let (correlated, locals): (Vec<String>, Vec<String>) = sub_vars
                    .iter()
                    .filter(|name| **name != out)
                    .cloned()
                    .partition(|name| elsewhere.contains(name));
                let synth = self.lower_binder_body(
                    format!("#{}:{}", aggregate_name(aggregate.name.value), out),
                    path,
                    &BinderShape {
                        bound_slots: &[],
                        correlated: &correlated,
                        locals: &locals,
                        outputs: std::slice::from_ref(&out),
                    },
                    &aggregate.goals,
                );
                let args = self.binder_call_args(&correlated, &locals, &[&out], scope);
                raql_plan::Goal::positive(GoalRef::Derived(synth), args)
            }
            Goal::ChooseTopK(choose) => {
                let score = choose.score_var.value.to_string();
                let item = choose.item_var.value.to_string();
                let sub_vars = ordered_goals_vars(&choose.goals);
                let (correlated, locals): (Vec<String>, Vec<String>) = sub_vars
                    .iter()
                    .filter(|name| **name != score && **name != item)
                    .cloned()
                    .partition(|name| elsewhere.contains(name));
                let bound_terms = [&choose.k, &choose.group];
                for term in bound_terms {
                    if term_has_vars(&term.value) && !matches!(term.value, Term::Var(_)) {
                        self.diagnostics.push(CompilerDiagnostic::error(
                            "RAQL0304",
                            "compound terms with variables are not supported as `choose_topk` \
                             `k`/`group` arguments",
                            Some(term.span),
                        ));
                    }
                }
                let bound_slots = ["__k".to_string(), "__group".to_string()];
                let outputs = [score.clone(), item.clone()];
                let synth = self.lower_binder_body(
                    format!("#topk:{}", choose.tag.value),
                    path,
                    &BinderShape {
                        bound_slots: &bound_slots,
                        correlated: &correlated,
                        locals: &locals,
                        outputs: &outputs,
                    },
                    &choose.goals,
                );
                let mut args = Vec::new();
                for term in bound_terms {
                    args.push(match &term.value {
                        Term::Var(name) => raql_plan::Term::Var(scope.intern(name.as_str())),
                        _ => raql_plan::Term::Const,
                    });
                }
                args.extend(self.binder_call_args(&correlated, &locals, &[&score, &item], scope));
                raql_plan::Goal::positive(GoalRef::Derived(synth), args)
            }
            Goal::Disjunction(_) => {
                // expand::flatten_rules eliminates disjunctions everywhere
                // (top level and inside binder sub-bodies) before lowering.
                self.diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0304",
                    "internal: disjunction survived rule flattening",
                    Some(goal.span),
                ));
                raql_plan::Goal::positive(GoalRef::Extern("def".to_string()), Vec::new())
            }
        }
    }

    /// Synthesize the derived def for a binder sub-body. The head is
    /// `shape` in order: synthetic bound slots (choose_topk's `k`/`group`),
    /// the correlated variables (bound), then locals and the binder's own
    /// outputs (free).
    fn lower_binder_body(
        &mut self,
        name: String,
        binder: &GoalPath,
        shape: &BinderShape<'_>,
        goals: &[Spanned<Goal>],
    ) -> DerivedId {
        let id = DerivedId(self.derived.len());
        // Reserve the slot first: sub-bodies may synthesize nested defs.
        let declared = (0..shape.arity()).map(|i| bound_if(i < shape.bound_arity())).collect();
        self.derived.push(DerivedDef {
            name,
            arity: shape.arity(),
            declared_modes: Some(vec![declared]),
            rules: Vec::new(),
        });
        self.derived_prov.push(DerivedProvenance { binder: Some(binder.clone()), rules: Vec::new() });

        let mut scope = VarScope::new();
        let head = shape.names().map(|name| raql_plan::Term::Var(scope.intern(name))).collect();
        let head_var_names: BTreeSet<String> = shape.names().map(str::to_string).collect();
        let (body, goal_paths) = self.lower_body(
            binder.rule,
            &binder.path,
            goals,
            &head_var_names,
            &mut scope,
        );
        self.derived[id.0].rules.push(raql_plan::Rule { vars: scope.names, head, body });
        self.derived_prov[id.0]
            .rules
            .push(RuleProvenance { rule: binder.rule, goals: goal_paths });
        id
    }

    /// The outer-rule argument list of a binder call: correlated variables
    /// as themselves, locals as fresh throwaway slots (the binder binds
    /// only its outputs at runtime; the fresh names keep the planner's
    /// bound-marking away from real variables), outputs as themselves.
    fn binder_call_args(
        &mut self,
        correlated: &[String],
        locals: &[String],
        outputs: &[&str],
        scope: &mut VarScope,
    ) -> Vec<raql_plan::Term> {
        let mut args = Vec::with_capacity(correlated.len() + locals.len() + outputs.len());
        for name in correlated {
            args.push(raql_plan::Term::Var(scope.intern(name)));
        }
        for name in locals {
            args.push(raql_plan::Term::Var(scope.fresh(name)));
        }
        for name in outputs {
            args.push(raql_plan::Term::Var(scope.intern(name)));
        }
        args
    }

    fn lower_atom(
        &mut self,
        atom: &Atom,
        goal: &Spanned<Goal>,
        negated: bool,
        scope: &mut VarScope,
    ) -> raql_plan::Goal {
        let name = atom.name.value.as_str();
        let target = if let Some(predicate) = self.catalog.predicate(name) {
            if predicate.is_disabled() {
                self.diagnostics.push(
                    CompilerDiagnostic::error(
                        "RAQL0302",
                        format!(
                            "predicate `{name}` is disabled — no honest RA-native operator \
                             exists yet (SPEC §4.3)",
                        ),
                        Some(goal.span),
                    )
                    .with_help("see `raql capabilities` for the roadmap families"),
                );
            }
            GoalRef::Extern(name.to_string())
        } else if let Some(patterns) = engine_builtin_patterns(name) {
            GoalRef::Builtin(self.shared_builtin(name, patterns))
        } else if let Some(id) = self.input_ids.get(name) {
            GoalRef::Input(*id)
        } else if let Some(id) = self.derived_ids.get(name) {
            GoalRef::Derived(*id)
        } else {
            // Unreachable in practice: typecheck infers a declaration for
            // every used name and reachability covers every lowered goal.
            self.diagnostics.push(CompilerDiagnostic::error(
                "RAQL0304",
                format!("internal: goal references unclassified predicate `{name}`"),
                Some(goal.span),
            ));
            GoalRef::Extern(name.to_string())
        };
        let args = atom
            .terms
            .iter()
            .map(|term| match &term.value {
                Term::Var(name) => raql_plan::Term::Var(scope.intern(name.as_str())),
                Term::Wildcard => raql_plan::Term::Var(scope.fresh("_")),
                other if term_has_vars(other) => {
                    self.diagnostics.push(CompilerDiagnostic::error(
                        "RAQL0304",
                        "compound terms with variables are not supported in body atom \
                         arguments yet — bind the variable in a separate `=` constraint",
                        Some(term.span),
                    ));
                    raql_plan::Term::Const
                }
                _ => raql_plan::Term::Const,
            })
            .collect();
        raql_plan::Goal { target, args, negated }
    }

    fn shared_builtin(&mut self, name: &str, patterns: Vec<Vec<Binding>>) -> BuiltinId {
        if let Some(id) = self.shared_builtin_ids.get(name) {
            return *id;
        }
        let id = self.push_builtin(name.to_string(), patterns);
        self.shared_builtin_ids.insert(name.to_string(), id);
        id
    }

    fn push_builtin(&mut self, name: String, patterns: Vec<Vec<Binding>>) -> BuiltinId {
        let id = BuiltinId(self.builtins.len());
        self.builtins.push(BuiltinDef { name, patterns });
        id
    }
}

fn bound_if(bound: bool) -> Binding {
    if bound { Binding::Bound } else { Binding::Free }
}

fn aggregate_name(name: raql_syntax::AggregateName) -> &'static str {
    match name {
        raql_syntax::AggregateName::Count => "count",
        raql_syntax::AggregateName::CountDistinct => "count_distinct",
        raql_syntax::AggregateName::Sum => "sum",
        raql_syntax::AggregateName::Min => "min",
        raql_syntax::AggregateName::Max => "max",
    }
}

// ---------------------------------------------------------------------
// Ordered variable collection (first occurrence wins — determinism)
// ---------------------------------------------------------------------

fn push_unique(out: &mut Vec<String>, name: &str) {
    if !out.iter().any(|existing| existing == name) {
        out.push(name.to_string());
    }
}

fn collect_term_vars_ordered(term: &Term, out: &mut Vec<String>) {
    match term {
        Term::Var(name) => push_unique(out, name.as_str()),
        Term::Some(inner) => collect_term_vars_ordered(&inner.value, out),
        Term::List { items, .. } => {
            for item in items {
                collect_term_vars_ordered(&item.value, out);
            }
        }
        _ => {}
    }
}

fn ordered_term_vars(term: &Term) -> Vec<String> {
    let mut out = Vec::new();
    collect_term_vars_ordered(term, &mut out);
    out
}

fn term_has_vars(term: &Term) -> bool {
    match term {
        Term::Var(_) => true,
        Term::Some(inner) => term_has_vars(&inner.value),
        Term::List { items, .. } => items.iter().any(|item| term_has_vars(&item.value)),
        _ => false,
    }
}

fn collect_expr_vars_ordered(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Term(term) => collect_term_vars_ordered(&term.value, out),
        Expr::UnaryNeg(inner) => collect_expr_vars_ordered(&inner.value, out),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_vars_ordered(&lhs.value, out);
            collect_expr_vars_ordered(&rhs.value, out);
        }
    }
}

fn ordered_expr_vars(expr: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    collect_expr_vars_ordered(expr, &mut out);
    out
}

/// Every variable a goal mentions, recursively through negation,
/// constraints, and binder sub-bodies (including binder outputs).
fn collect_goal_vars_ordered(goal: &Goal, out: &mut Vec<String>) {
    match goal {
        Goal::Atom(atom) => {
            for term in &atom.terms {
                collect_term_vars_ordered(&term.value, out);
            }
        }
        Goal::Not(not_goal) => {
            for term in &not_goal.atom.value.terms {
                collect_term_vars_ordered(&term.value, out);
            }
        }
        Goal::Constraint(Constraint::Relational(rel)) => {
            collect_term_vars_ordered(&rel.lhs.value, out);
            collect_term_vars_ordered(&rel.rhs.value, out);
        }
        Goal::Constraint(Constraint::ArithmeticBind(bind)) => {
            collect_expr_vars_ordered(&bind.expr.value, out);
            push_unique(out, bind.target.value.as_str());
        }
        Goal::Aggregate(aggregate) => {
            push_unique(out, aggregate.out.value.as_str());
            for sub in &aggregate.goals {
                collect_goal_vars_ordered(&sub.value, out);
            }
        }
        Goal::ChooseTopK(choose) => {
            collect_term_vars_ordered(&choose.k.value, out);
            collect_term_vars_ordered(&choose.group.value, out);
            push_unique(out, choose.score_var.value.as_str());
            push_unique(out, choose.item_var.value.as_str());
            for sub in &choose.goals {
                collect_goal_vars_ordered(&sub.value, out);
            }
        }
        Goal::Disjunction(group) => {
            for sub in group.branches.iter().flatten() {
                collect_goal_vars_ordered(&sub.value, out);
            }
        }
    }
}

/// The variables a goal exposes to the rest of its rule: everything for
/// plain goals, only the env-visible interface for binders (the old
/// engine's env semantics — a binder's sub-body variables vanish after it
/// runs, except what it binds).
fn outer_goal_vars(goal: &Goal) -> Vec<String> {
    match goal {
        Goal::Aggregate(aggregate) => vec![aggregate.out.value.to_string()],
        Goal::ChooseTopK(choose) => {
            let mut out = Vec::new();
            collect_term_vars_ordered(&choose.k.value, &mut out);
            collect_term_vars_ordered(&choose.group.value, &mut out);
            push_unique(&mut out, choose.score_var.value.as_str());
            push_unique(&mut out, choose.item_var.value.as_str());
            out
        }
        other => {
            let mut out = Vec::new();
            collect_goal_vars_ordered(other, &mut out);
            out
        }
    }
}

/// Union of the sub-body's variables in first-occurrence order.
fn ordered_goals_vars(goals: &[Spanned<Goal>]) -> Vec<String> {
    let mut out = Vec::new();
    for goal in goals {
        collect_goal_vars_ordered(&goal.value, &mut out);
    }
    out
}
