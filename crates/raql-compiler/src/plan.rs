//! Planning: goal ordering, mode selection, extern lookup shapes, and the
//! planned program the engine consumes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use raql_host::{
    ExternLookupShape, is_engine_managed_extern, is_runtime_scalar_input_predicate,
    supports_lookup_seed_predicate,
};
use raql_syntax::{Atom, Constraint, DeclAttr, DeclarationKind, Expr, Goal, RelOp, Spanned, Term};

use crate::diagnostics::{
    CompilerDiagnostic, DiagBundle, enrich_include_stack_context, format_span_excerpt,
};
use crate::expand::expand_rules;
use crate::program::{
    CompilerType, EnumDecl, ModeDir, ModeSig, PredicateDecl, TypedProgram, TypedRule,
    format_mode_signature, format_type_name,
};
use crate::strata::{SccPlan, compute_strata};

#[derive(Debug, Clone)]
pub struct GoalPlan {
    index: usize,
    chosen_mode: Option<usize>,
    extern_lookup: Option<ExternLookupPlan>,
}

impl GoalPlan {
    pub fn index(&self) -> usize {
        self.index
    }

    pub fn chosen_mode(&self) -> Option<usize> {
        self.chosen_mode
    }

    pub fn extern_lookup(&self) -> Option<&ExternLookupPlan> {
        self.extern_lookup.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternLookupPlan {
    shape: ExternLookupShape,
    bound_positions: Vec<usize>,
}

impl ExternLookupPlan {
    pub const fn shape(&self) -> ExternLookupShape {
        self.shape
    }

    pub fn bound_positions(&self) -> &[usize] {
        &self.bound_positions
    }
}

#[derive(Debug, Clone)]
pub struct RulePlan {
    predicate: String,
    ordered_goals: Vec<GoalPlan>,
}

impl RulePlan {
    pub fn predicate(&self) -> &str {
        &self.predicate
    }

    pub fn ordered_goals(&self) -> &[GoalPlan] {
        &self.ordered_goals
    }
}

#[derive(Debug, Clone)]
pub struct PlannedRule {
    typed_rule: TypedRule,
    plan: RulePlan,
}

impl PlannedRule {
    pub fn typed_rule(&self) -> &TypedRule {
        &self.typed_rule
    }

    pub fn plan(&self) -> &RulePlan {
        &self.plan
    }

    pub fn head_predicate(&self) -> &str {
        self.typed_rule.head_predicate()
    }

    pub fn head_terms(&self) -> &[Spanned<Term>] {
        self.typed_rule.head_terms()
    }

    pub fn var_types(&self) -> &BTreeMap<String, CompilerType> {
        self.typed_rule.var_types()
    }

    pub fn goal(&self, index: usize) -> Option<&Spanned<Goal>> {
        self.typed_rule.goal(index)
    }

    pub fn goals(&self) -> std::slice::Iter<'_, Spanned<Goal>> {
        self.typed_rule.goals()
    }

    pub fn ordered_goals(&self) -> &[GoalPlan] {
        self.plan.ordered_goals()
    }
}

#[derive(Debug, Clone)]
pub struct PlannedProgram {
    typed: TypedProgram,
    planned_rules: Vec<PlannedRule>,
    pub(crate) strata: BTreeMap<String, usize>,
    sccs: Vec<SccPlan>,
}

impl PlannedProgram {
    pub fn predicates(&self) -> &BTreeMap<String, PredicateDecl> {
        &self.typed.predicates
    }

    pub fn predicate_decl(&self, name: &str) -> Option<&PredicateDecl> {
        self.typed.predicates.get(name)
    }

    pub fn facts(&self) -> &[Spanned<raql_syntax::Fact>] {
        &self.typed.facts
    }

    pub fn source_map(&self) -> &raql_syntax::SourceMap {
        &self.typed.sources
    }

    pub fn pragma_i64(&self, name: &str) -> Option<i64> {
        self.typed.pragmas.get(name).copied()
    }

    pub fn modes(&self, predicate: &str) -> Option<&[ModeSig]> {
        self.typed.modes.get(predicate).map(Vec::as_slice)
    }

    pub fn enum_decl(&self, name: &str) -> Option<&EnumDecl> {
        self.typed.enums.get(name)
    }

    pub fn planned_rules(&self) -> &[PlannedRule] {
        &self.planned_rules
    }

    pub fn planned_rule(&self, index: usize) -> Option<&PlannedRule> {
        self.planned_rules.get(index)
    }

    pub fn rule_plan(&self, index: usize) -> Option<&RulePlan> {
        self.planned_rule(index).map(PlannedRule::plan)
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

pub fn reachable_predicates(program: &PlannedProgram) -> BTreeSet<String> {
    let root_file = program
        .source_map()
        .files()
        .first()
        .map(|file| file.id());
    let mut agenda = VecDeque::new();
    let mut seen_predicates = BTreeSet::new();

    if let Some(root_file) = root_file {
        let mut seeded = false;
        for (name, decl) in program.predicates() {
            if decl.span.file == root_file && decl.is_output() {
                agenda.push_back(name.clone());
                seeded = true;
            }
        }
        if seeded {
            // Output-rooted execution is the supported path. Helper predicates that
            // have been fully inlined into output rules should not stay alive just
            // because they were declared in the root file.
        } else {
            for rule in program.planned_rules() {
                if rule.typed_rule().rule().span.file == root_file {
                    agenda.push_back(rule.head_predicate().to_string());
                }
            }
            for (name, decl) in program.predicates() {
                if decl.span.file == root_file && !decl.attrs().contains(&DeclAttr::Extern) {
                    agenda.push_back(name.clone());
                }
            }
        }
    } else {
        for rule in program.planned_rules() {
            agenda.push_back(rule.head_predicate().to_string());
        }
    }

    while let Some(predicate) = agenda.pop_front() {
        if !seen_predicates.insert(predicate.clone()) {
            continue;
        }
        for rule in program
            .planned_rules()
            .iter()
            .filter(|rule| rule.head_predicate() == predicate)
        {
            collect_reachable_from_goals(program, rule.typed_rule().goals(), &mut agenda);
        }
    }
    seen_predicates
}

pub fn required_extern_capabilities(program: &PlannedProgram) -> BTreeSet<String> {
    let mut required = BTreeSet::new();
    let mut discard_agenda = VecDeque::new();
    for predicate in reachable_predicates(program) {
        for rule in program
            .planned_rules()
            .iter()
            .filter(|rule| rule.head_predicate() == predicate)
        {
            collect_required_from_goals(
                program,
                rule.typed_rule().goals(),
                &mut required,
                &mut discard_agenda,
            );
        }
    }
    required
}

fn collect_reachable_from_goals<'a>(
    program: &PlannedProgram,
    goals: impl Iterator<Item = &'a Spanned<Goal>>,
    agenda: &mut VecDeque<String>,
) {
    for goal in goals {
        match &goal.value {
            Goal::Atom(atom) => collect_reachable_from_atom(program, atom, agenda),
            Goal::Not(not_goal) => collect_reachable_from_atom(program, &not_goal.atom.value, agenda),
            Goal::Aggregate(aggregate) => {
                collect_reachable_from_goals(program, aggregate.goals.iter(), agenda);
            }
            Goal::ChooseTopK(choose) => {
                collect_reachable_from_goals(program, choose.goals.iter(), agenda);
            }
            Goal::Disjunction(group) => {
                for branch in &group.branches {
                    collect_reachable_from_goals(program, branch.iter(), agenda);
                }
            }
            Goal::Constraint(_) => {}
        }
    }
}

fn collect_reachable_from_atom(
    program: &PlannedProgram,
    atom: &Atom,
    agenda: &mut VecDeque<String>,
) {
    let name = atom.name.value.as_str();
    let Some(decl) = program.predicate_decl(name) else {
        return;
    };
    if decl.attrs().contains(&DeclAttr::Extern) {
        return;
    }
    if program
        .planned_rules()
        .iter()
        .any(|rule| rule.head_predicate() == name)
    {
        agenda.push_back(name.to_string());
    }
}

fn collect_required_from_goals<'a>(
    program: &PlannedProgram,
    goals: impl Iterator<Item = &'a Spanned<Goal>>,
    required: &mut BTreeSet<String>,
    agenda: &mut VecDeque<String>,
) {
    for goal in goals {
        match &goal.value {
            Goal::Atom(atom) => collect_required_from_atom(program, atom, required, agenda),
            Goal::Not(not_goal) => {
                collect_required_from_atom(program, &not_goal.atom.value, required, agenda)
            }
            Goal::Aggregate(aggregate) => {
                collect_required_from_goals(program, aggregate.goals.iter(), required, agenda);
            }
            Goal::ChooseTopK(choose) => {
                collect_required_from_goals(program, choose.goals.iter(), required, agenda);
            }
            Goal::Disjunction(group) => {
                for branch in &group.branches {
                    collect_required_from_goals(program, branch.iter(), required, agenda);
                }
            }
            Goal::Constraint(_) => {}
        }
    }
}

fn collect_required_from_atom(
    program: &PlannedProgram,
    atom: &Atom,
    required: &mut BTreeSet<String>,
    agenda: &mut VecDeque<String>,
) {
    let name = atom.name.value.as_str();
    let Some(decl) = program.predicate_decl(name) else {
        return;
    };
    if !decl.attrs().contains(&DeclAttr::Extern) {
        if program
            .planned_rules()
            .iter()
            .any(|rule| rule.head_predicate() == name)
        {
            agenda.push_back(name.to_string());
        }
        return;
    }
    if is_engine_managed_extern(name) || is_runtime_scalar_input_predicate(name) {
        return;
    }
    required.insert(name.to_string());
}

pub fn plan(typed: TypedProgram) -> Result<PlannedProgram, DiagBundle> {
    let mut diagnostics = Vec::new();
    let expanded_rules = expand_rules(&typed.rules);

    for typed_rule in &expanded_rules {
        check_range_restriction(typed_rule, &mut diagnostics);
    }
    if !diagnostics.is_empty() {
        enrich_include_stack_context(&mut diagnostics, &typed.sources);
        return Err(diagnostics);
    }

    let mut planned_rules = Vec::new();
    for typed_rule in expanded_rules {
        let head_pred = typed_rule.rule.value.head.value.name.value.to_string();
        if head_pred == "out_status" {
            diagnostics.push(CompilerDiagnostic::error(
                "RAQL0404",
                "`out_status/1` is reserved for engine output only",
                Some(typed_rule.rule.value.head.span),
            ));
        }
        let mut remaining: BTreeSet<usize> = (0..typed_rule.goals().len()).collect();
        let mut bound = BTreeSet::<String>::new();
        let mut ground = BTreeSet::<String>::new();
        let mut ordered = Vec::new();

        while !remaining.is_empty() {
            let mut progress = false;
            for idx in remaining.clone() {
                let goal = typed_rule
                    .goal(idx)
                    .expect("planner invariant: goal index must exist");
                if let Some(chosen_mode) =
                    goal_runnable(goal, &typed, &bound, &ground, &typed_rule.var_types)
                {
                    let extern_lookup =
                        planned_extern_lookup(goal, &typed, chosen_mode, &ground, &typed_rule.var_types);
                    apply_goal_bindings(goal, &mut bound, &mut ground);
                    ordered.push(GoalPlan {
                        index: idx,
                        chosen_mode,
                        extern_lookup,
                    });
                    remaining.remove(&idx);
                    progress = true;
                    break;
                }
            }
            if !progress {
                let first_blocked_idx = remaining.iter().next().copied().unwrap_or_default();
                let first_blocked_goal = typed_rule
                    .goal(first_blocked_idx)
                    .expect("planner invariant: blocked goal index must exist");
                let blocked_goal_text =
                    format_span_excerpt(&typed.sources, first_blocked_goal.span);
                let blocked_details = blocked_goal_details(
                    first_blocked_goal,
                    &typed,
                    &ground,
                    &typed_rule.var_types,
                );
                let missing_inputs = format_missing_inputs_summary(&blocked_details.missing_inputs);
                let selected_mode = blocked_details
                    .selected_mode_signature
                    .as_ref()
                    .map(|sig| format!("; selected mode: {sig}"))
                    .unwrap_or_default();
                let context = format!(
                    "{}; current variable context: {}",
                    blocked_details.reason,
                    format_var_context(&bound, &ground)
                );
                let missing_input_help = if blocked_details.missing_inputs.is_empty() {
                    "no specific ungrounded inputs were identified".to_string()
                } else {
                    format!("ground these first: {missing_inputs}")
                };
                let selected_mode_help = blocked_details
                    .selected_mode_signature
                    .as_ref()
                    .map(|sig| format!("; selected mode: {sig}"))
                    .unwrap_or_default();
                diagnostics.push(
                    CompilerDiagnostic::error(
                        "RAQL0301",
                        format!(
                            "mode planning got stuck; first blocked goal at body index {first_blocked_idx} is `{blocked_goal_text}`{selected_mode}; missing or ungrounded inputs: {missing_inputs}; {context}",
                        ),
                        Some(first_blocked_goal.span),
                    )
                    .with_help(format!(
                        "to make `{blocked_goal_text}` runnable, {missing_input_help}{selected_mode_help}; bind required inputs earlier or add mode declarations"
                    )),
                );
                break;
            }
        }

        let rule_plan = RulePlan {
            predicate: head_pred,
            ordered_goals: ordered,
        };
        planned_rules.push(PlannedRule {
            typed_rule,
            plan: rule_plan,
        });
    }

    let mut strata_input = typed.clone();
    strata_input.rules = planned_rules
        .iter()
        .map(|planned_rule| planned_rule.typed_rule.clone())
        .collect();
    let stratification = compute_strata(&strata_input, &mut diagnostics);
    if !diagnostics.is_empty() {
        enrich_include_stack_context(&mut diagnostics, &typed.sources);
        return Err(diagnostics);
    }

    Ok(PlannedProgram {
        typed,
        planned_rules,
        strata: stratification.strata,
        sccs: stratification.sccs,
    })
}

fn check_range_restriction(rule: &TypedRule, diagnostics: &mut DiagBundle) {
    let mut restricted = BTreeSet::<String>::new();

    let mut changed = true;
    while changed {
        changed = false;
        for goal in &rule.rule.value.body {
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

    for term in &rule.rule.value.head.value.terms {
        for var in term_vars(term) {
            if !restricted.contains(var.as_str()) {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0406",
                    format!("variable `{var}` in rule head is not range-restricted"),
                    Some(rule.rule.value.head.span),
                ));
            }
        }
    }

    for goal in &rule.rule.value.body {
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

fn goal_runnable(
    goal: &Spanned<Goal>,
    typed: &TypedProgram,
    _bound: &BTreeSet<String>,
    ground: &BTreeSet<String>,
    var_types: &BTreeMap<String, CompilerType>,
) -> Option<Option<usize>> {
    match &goal.value {
        Goal::Atom(a) => {
            if let Some(modes) = typed.modes.get(a.name.value.as_str()) {
                for (idx, mode) in modes.iter().enumerate() {
                    if mode.args.len() != a.terms.len() {
                        continue;
                    }
                    let ok = a
                        .terms
                        .iter()
                        .zip(&mode.args)
                        .all(|(term, (dir, _))| match dir {
                            ModeDir::Out => true,
                            ModeDir::In => term_is_ground(term, ground, var_types),
                        });
                    if ok {
                        return Some(Some(idx));
                    }
                }
                extern_lookup_seed_runnable(goal, typed, ground, var_types).then_some(None)
            } else {
                Some(None)
            }
        }
        Goal::Not(n) => {
            let ok = n
                .atom
                .value
                .terms
                .iter()
                .all(|t| term_is_ground(t, ground, var_types));
            if ok { Some(None) } else { None }
        }
        Goal::Constraint(c) => match c {
            Constraint::Relational(r) => match r.op.value {
                RelOp::Eq => {
                    let lhs = term_is_ground(&r.lhs, ground, var_types);
                    let rhs = term_is_ground(&r.rhs, ground, var_types);
                    if lhs || rhs { Some(None) } else { None }
                }
                RelOp::NotEq | RelOp::Lt | RelOp::LtEq | RelOp::Gt | RelOp::GtEq => {
                    let lhs = term_is_ground(&r.lhs, ground, var_types);
                    let rhs = term_is_ground(&r.rhs, ground, var_types);
                    if lhs && rhs { Some(None) } else { None }
                }
            },
            Constraint::ArithmeticBind(b) => {
                if expr_is_ground(&b.expr, ground, var_types) {
                    Some(None)
                } else {
                    None
                }
            }
        },
        Goal::Aggregate(_) => Some(None),
        Goal::ChooseTopK(c) => {
            if term_is_ground(&c.group, ground, var_types)
                && term_is_ground(&c.k, ground, var_types)
            {
                Some(None)
            } else {
                None
            }
        }
        Goal::Disjunction(d) => {
            let all_ok = d.branches.iter().all(|branch| {
                branch
                    .iter()
                    .all(|g| goal_runnable(g, typed, _bound, ground, var_types).is_some())
            });
            if all_ok { Some(None) } else { None }
        }
    }
}

fn extern_lookup_seed_runnable(
    goal: &Spanned<Goal>,
    typed: &TypedProgram,
    ground: &BTreeSet<String>,
    var_types: &BTreeMap<String, CompilerType>,
) -> bool {
    let Goal::Atom(atom) = &goal.value else {
        return false;
    };
    supports_lookup_seed_predicate(atom.name.value.as_str())
        && planned_extern_lookup(goal, typed, None, ground, var_types).is_some()
}

fn planned_extern_lookup(
    goal: &Spanned<Goal>,
    typed: &TypedProgram,
    chosen_mode: Option<usize>,
    ground: &BTreeSet<String>,
    var_types: &BTreeMap<String, CompilerType>,
) -> Option<ExternLookupPlan> {
    let Goal::Atom(atom) = &goal.value else {
        return None;
    };
    let predicate = atom.name.value.as_str();
    let decl = typed.predicates.get(predicate)?;
    if !decl.attrs().contains(&DeclAttr::Extern)
        || is_engine_managed_extern(predicate)
        || is_runtime_scalar_input_predicate(predicate)
    {
        return None;
    }

    let bound_positions = atom
        .terms
        .iter()
        .enumerate()
        .filter_map(|(idx, term)| term_is_ground(term, ground, var_types).then_some(idx))
        .collect::<Vec<_>>();
    if bound_positions.is_empty() && !supports_zero_bound_relation_lookup(predicate, decl.kind()) {
        return None;
    }

    let shape = match decl.kind() {
        DeclarationKind::Function => ExternLookupShape::FunctionExactBindings,
        DeclarationKind::Relation => {
            if let Some(mode) = typed
                .modes
                .get(predicate)
                .and_then(|modes| chosen_mode.and_then(|idx| modes.get(idx)))
            {
                let input_positions = mode
                    .args()
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, (dir, _))| (*dir == ModeDir::In).then_some(idx))
                    .collect::<Vec<_>>();
                if input_positions.iter().any(|idx| !bound_positions.contains(idx)) {
                    return None;
                }
            }
            ExternLookupShape::RelationExactBindings
        }
    };

    Some(ExternLookupPlan {
        shape,
        bound_positions,
    })
}

fn supports_zero_bound_relation_lookup(
    predicate: &str,
    kind: DeclarationKind,
) -> bool {
    matches!(kind, DeclarationKind::Relation)
        && matches!(predicate, "def" | "is_public" | "in_test")
}

#[derive(Debug, Clone)]
struct BlockedGoalDetails {
    reason: String,
    selected_mode_signature: Option<String>,
    missing_inputs: Vec<String>,
}

fn blocked_goal_details(
    goal: &Spanned<Goal>,
    typed: &TypedProgram,
    ground: &BTreeSet<String>,
    var_types: &BTreeMap<String, CompilerType>,
) -> BlockedGoalDetails {
    match &goal.value {
        Goal::Atom(atom) => blocked_atom_details(atom, typed, ground, var_types),
        Goal::Not(not) => {
            let mut missing = BTreeSet::new();
            for term in &not.atom.value.terms {
                if !term_is_ground(term, ground, var_types) {
                    collect_missing_term_ground_vars(term, ground, var_types, &mut missing);
                }
            }
            BlockedGoalDetails {
                reason: format!(
                    "goal kind `not`: negated atom `{}` requires all terms grounded ({})",
                    not.atom.value.name.value,
                    format_missing_vars_clause(&missing)
                ),
                selected_mode_signature: None,
                missing_inputs: missing_inputs_from_vars(&missing),
            }
        }
        Goal::Constraint(Constraint::Relational(rel)) => match rel.op.value {
            RelOp::Eq => {
                let lhs_ground = term_is_ground(&rel.lhs, ground, var_types);
                let rhs_ground = term_is_ground(&rel.rhs, ground, var_types);
                let mut missing = BTreeSet::new();
                if !lhs_ground {
                    collect_missing_term_ground_vars(&rel.lhs, ground, var_types, &mut missing);
                }
                if !rhs_ground {
                    collect_missing_term_ground_vars(&rel.rhs, ground, var_types, &mut missing);
                }
                BlockedGoalDetails {
                    reason: format!(
                        "goal kind `constraint (=)`: needs one grounded side ({})",
                        format_missing_vars_clause(&missing)
                    ),
                    selected_mode_signature: None,
                    missing_inputs: missing_inputs_from_vars(&missing),
                }
            }
            RelOp::NotEq | RelOp::Lt | RelOp::LtEq | RelOp::Gt | RelOp::GtEq => {
                let mut missing = BTreeSet::new();
                if !term_is_ground(&rel.lhs, ground, var_types) {
                    collect_missing_term_ground_vars(&rel.lhs, ground, var_types, &mut missing);
                }
                if !term_is_ground(&rel.rhs, ground, var_types) {
                    collect_missing_term_ground_vars(&rel.rhs, ground, var_types, &mut missing);
                }
                BlockedGoalDetails {
                    reason: format!(
                        "goal kind `constraint ({:?})`: needs both sides grounded ({})",
                        rel.op.value,
                        format_missing_vars_clause(&missing)
                    ),
                    selected_mode_signature: None,
                    missing_inputs: missing_inputs_from_vars(&missing),
                }
            }
        },
        Goal::Constraint(Constraint::ArithmeticBind(bind)) => {
            let mut missing = BTreeSet::new();
            if !expr_is_ground(&bind.expr, ground, var_types) {
                collect_missing_expr_ground_vars(&bind.expr, ground, var_types, &mut missing);
            }
            BlockedGoalDetails {
                reason: format!(
                    "goal kind `arithmetic bind`: expression for `{}` must be grounded ({})",
                    bind.target.value,
                    format_missing_vars_clause(&missing)
                ),
                selected_mode_signature: None,
                missing_inputs: missing_inputs_from_vars(&missing),
            }
        }
        Goal::Aggregate(_) => BlockedGoalDetails {
            reason: "goal kind `aggregate` is runnable but appears in blocked set".to_string(),
            selected_mode_signature: None,
            missing_inputs: Vec::new(),
        },
        Goal::ChooseTopK(choose) => {
            let mut missing = BTreeSet::new();
            if !term_is_ground(&choose.group, ground, var_types) {
                collect_missing_term_ground_vars(&choose.group, ground, var_types, &mut missing);
            }
            if !term_is_ground(&choose.k, ground, var_types) {
                collect_missing_term_ground_vars(&choose.k, ground, var_types, &mut missing);
            }
            BlockedGoalDetails {
                reason: format!(
                    "goal kind `choose_topk`: `group` and `k` must be grounded ({})",
                    format_missing_vars_clause(&missing)
                ),
                selected_mode_signature: None,
                missing_inputs: missing_inputs_from_vars(&missing),
            }
        }
        Goal::Disjunction(disjunction) => {
            for (branch_idx, branch) in disjunction.branches.iter().enumerate() {
                if let Some((goal_idx, blocked)) = branch.iter().enumerate().find(|(_, g)| {
                    goal_runnable(g, typed, &BTreeSet::new(), ground, var_types).is_none()
                }) {
                    let nested = blocked_goal_details(blocked, typed, ground, var_types);
                    return BlockedGoalDetails {
                        reason: format!(
                            "goal kind `disjunction`: branch {branch_idx} goal {goal_idx} is blocked ({})",
                            nested.reason
                        ),
                        selected_mode_signature: nested.selected_mode_signature,
                        missing_inputs: nested.missing_inputs,
                    };
                }
            }
            BlockedGoalDetails {
                reason: "goal kind `disjunction` has no runnable branch".to_string(),
                selected_mode_signature: None,
                missing_inputs: Vec::new(),
            }
        }
    }
}

fn missing_inputs_from_vars(missing_vars: &BTreeSet<String>) -> Vec<String> {
    missing_vars
        .iter()
        .map(|var| format!("variable `{var}`"))
        .collect()
}

fn format_missing_inputs_summary(missing_inputs: &[String]) -> String {
    if missing_inputs.is_empty() {
        "none identified".to_string()
    } else {
        missing_inputs
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn blocked_atom_details(
    atom: &raql_syntax::Atom,
    typed: &TypedProgram,
    ground: &BTreeSet<String>,
    var_types: &BTreeMap<String, CompilerType>,
) -> BlockedGoalDetails {
    let pred = atom.name.value.as_str();
    let arity = atom.terms.len();
    let Some(modes) = typed.modes.get(pred) else {
        return BlockedGoalDetails {
            reason: format!("goal kind `atom`: `{pred}/{arity}` has no mode declaration"),
            selected_mode_signature: None,
            missing_inputs: vec!["a `.mode` declaration".to_string()],
        };
    };
    if modes.is_empty() {
        return BlockedGoalDetails {
            reason: format!("goal kind `atom`: `{pred}/{arity}` has no available mode signatures"),
            selected_mode_signature: None,
            missing_inputs: vec!["at least one mode signature".to_string()],
        };
    }
    let available_mode_signatures = modes.iter().map(format_mode_signature).collect::<Vec<_>>();
    let mut best: Option<(usize, String, Vec<String>, BTreeSet<String>)> = None;
    for (mode_idx, mode) in modes.iter().enumerate() {
        if mode.args.len() != atom.terms.len() {
            continue;
        }
        let mut required_inputs = Vec::new();
        let mut missing_vars = BTreeSet::new();
        for (arg_idx, (term, (dir, ty))) in atom.terms.iter().zip(&mode.args).enumerate() {
            if *dir == ModeDir::In && !term_is_ground(term, ground, var_types) {
                required_inputs.push(format!(
                    "arg {} `{}` ({} {} `{}`)",
                    arg_idx + 1,
                    format_term_snippet(term),
                    dir.symbol(),
                    dir.label(),
                    format_type_name(ty)
                ));
                collect_missing_term_ground_vars(term, ground, var_types, &mut missing_vars);
            }
        }
        if required_inputs.is_empty() {
            continue;
        }
        let candidate = (
            mode_idx,
            format_mode_signature(mode),
            required_inputs,
            missing_vars,
        );
        if let Some(current) = &best {
            if candidate.2.len() < current.2.len() {
                best = Some(candidate);
            }
        } else {
            best = Some(candidate);
        }
    }
    if let Some((mode_idx, mode_signature, required_inputs, missing_vars)) = best {
        let required = required_inputs.join(", ");
        let mut missing_inputs = required_inputs;
        missing_inputs.extend(missing_inputs_from_vars(&missing_vars));
        missing_inputs.sort();
        missing_inputs.dedup();
        BlockedGoalDetails {
            reason: format!(
                "goal kind `atom`: `{pred}/{arity}` cannot run; mode {} `{mode_signature}` requires grounded input argument(s) [{required}] ({})",
                mode_idx + 1,
                format_missing_vars_clause(&missing_vars)
            ),
            selected_mode_signature: Some(format!("mode {} `{mode_signature}`", mode_idx + 1)),
            missing_inputs,
        }
    } else {
        BlockedGoalDetails {
            reason: format!(
                "goal kind `atom`: `{pred}/{arity}` cannot run; declared modes are [{}], but none match this goal arity",
                available_mode_signatures.join(", ")
            ),
            selected_mode_signature: None,
            missing_inputs: vec![format!("a mode signature with arity {arity}")],
        }
    }
}

fn format_term_snippet(term: &Spanned<Term>) -> String {
    match &term.value {
        Term::Var(v) => v.to_string(),
        Term::Wildcard => "_".to_string(),
        Term::Int(n) => n.to_string(),
        Term::String(s) => format!("{s:?}"),
        Term::Bool(b) => b.to_string(),
        Term::EnumAtom {
            enum_name,
            variant_name,
        } => format!("{}::{}", enum_name.value, variant_name.value),
        Term::None { .. } => "none".to_string(),
        Term::Some(inner) => format!("some({})", format_term_snippet(inner)),
        Term::List { items, .. } => format!(
            "[{}]",
            items
                .iter()
                .map(format_term_snippet)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn format_missing_vars_clause(missing_vars: &BTreeSet<String>) -> String {
    if missing_vars.is_empty() {
        "missing grounded vars: none (non-variable term remains non-ground)".to_string()
    } else {
        format!(
            "missing grounded vars: {}",
            missing_vars.iter().cloned().collect::<Vec<_>>().join(", ")
        )
    }
}

fn format_var_context(bound: &BTreeSet<String>, ground: &BTreeSet<String>) -> String {
    format!(
        "bound vars: {}; grounded vars: {}",
        format_var_set(bound),
        format_var_set(ground)
    )
}

fn format_var_set(vars: &BTreeSet<String>) -> String {
    if vars.is_empty() {
        "none".to_string()
    } else {
        vars.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn collect_missing_term_ground_vars(
    term: &Spanned<Term>,
    ground: &BTreeSet<String>,
    var_types: &BTreeMap<String, CompilerType>,
    out: &mut BTreeSet<String>,
) {
    match &term.value {
        Term::Var(v) => {
            if var_types.contains_key(v.as_str()) && !ground.contains(v.as_str()) {
                out.insert(v.to_string());
            }
        }
        Term::Some(inner) => collect_missing_term_ground_vars(inner, ground, var_types, out),
        Term::List { items, .. } => {
            for item in items {
                collect_missing_term_ground_vars(item, ground, var_types, out);
            }
        }
        _ => {}
    }
}

fn collect_missing_expr_ground_vars(
    expr: &Spanned<Expr>,
    ground: &BTreeSet<String>,
    var_types: &BTreeMap<String, CompilerType>,
    out: &mut BTreeSet<String>,
) {
    match &expr.value {
        Expr::Term(term) => collect_missing_term_ground_vars(term, ground, var_types, out),
        Expr::UnaryNeg(inner) => collect_missing_expr_ground_vars(inner, ground, var_types, out),
        Expr::Binary { lhs, rhs, .. } => {
            collect_missing_expr_ground_vars(lhs, ground, var_types, out);
            collect_missing_expr_ground_vars(rhs, ground, var_types, out);
        }
    }
}

fn apply_goal_bindings(
    goal: &Spanned<Goal>,
    bound: &mut BTreeSet<String>,
    ground: &mut BTreeSet<String>,
) {
    match &goal.value {
        Goal::Atom(a) => {
            for t in &a.terms {
                bind_term_vars(t, bound, ground);
            }
        }
        Goal::Not(_) => {}
        Goal::Constraint(c) => match c {
            Constraint::Relational(r) => {
                let lhs_ground = term_ground_if_vars_known(&r.lhs, ground);
                let rhs_ground = term_ground_if_vars_known(&r.rhs, ground);
                if lhs_ground {
                    bind_term_vars(&r.rhs, bound, ground);
                } else if rhs_ground {
                    bind_term_vars(&r.lhs, bound, ground);
                }
            }
            Constraint::ArithmeticBind(b) => {
                bound.insert(b.target.value.to_string());
                ground.insert(b.target.value.to_string());
            }
        },
        Goal::Aggregate(a) => {
            bound.insert(a.out.value.to_string());
            ground.insert(a.out.value.to_string());
        }
        Goal::ChooseTopK(c) => {
            bound.insert(c.score_var.value.to_string());
            bound.insert(c.item_var.value.to_string());
            ground.insert(c.score_var.value.to_string());
            ground.insert(c.item_var.value.to_string());
        }
        Goal::Disjunction(_) => {}
    }
}

fn bind_term_vars(
    term: &Spanned<Term>,
    bound: &mut BTreeSet<String>,
    ground: &mut BTreeSet<String>,
) {
    match &term.value {
        Term::Var(v) => {
            bound.insert(v.to_string());
            ground.insert(v.to_string());
        }
        Term::Some(inner) => bind_term_vars(inner, bound, ground),
        Term::List { items, .. } => {
            for i in items {
                bind_term_vars(i, bound, ground);
            }
        }
        _ => {}
    }
}

fn term_ground_if_vars_known(term: &Spanned<Term>, ground: &BTreeSet<String>) -> bool {
    match &term.value {
        Term::Var(v) => ground.contains(v.as_str()),
        Term::Wildcard => false,
        Term::Int(_) | Term::String(_) | Term::Bool(_) | Term::EnumAtom { .. } => true,
        Term::None { .. } => true,
        Term::Some(inner) => term_ground_if_vars_known(inner, ground),
        Term::List { items, .. } => items.iter().all(|i| term_ground_if_vars_known(i, ground)),
    }
}

fn term_is_ground(
    term: &Spanned<Term>,
    ground: &BTreeSet<String>,
    var_types: &BTreeMap<String, CompilerType>,
) -> bool {
    match &term.value {
        Term::Var(v) => ground.contains(v.as_str()) || !var_types.contains_key(v.as_str()),
        _ => term_ground_if_vars_known(term, ground),
    }
}

fn expr_is_ground(
    expr: &Spanned<Expr>,
    ground: &BTreeSet<String>,
    var_types: &BTreeMap<String, CompilerType>,
) -> bool {
    match &expr.value {
        Expr::Term(t) => term_is_ground(t, ground, var_types),
        Expr::UnaryNeg(inner) => expr_is_ground(inner, ground, var_types),
        Expr::Binary { lhs, rhs, .. } => {
            expr_is_ground(lhs, ground, var_types) && expr_is_ground(rhs, ground, var_types)
        }
    }
}
