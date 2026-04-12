//! RAQL compiler pipeline: resolve -> typecheck -> plan.
#![forbid(unsafe_code)]

use core::fmt;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use camino::Utf8PathBuf;
use miette::{Diagnostic, Severity};
use petgraph::visit::EdgeRef;
use petgraph::{
    algo::toposort,
    graph::{DiGraph, NodeIndex},
};
use raql_host::{
    ExternLookupShape, is_engine_managed_extern, is_runtime_scalar_input_predicate,
    supports_lookup_seed_predicate,
};
use raql_syntax::{
    AggregateName, AstPhase, Atom, Constraint, DeclAttr, DeclarationKind, Directive, Expr, Goal,
    ModeDirection, Program, RelOp, Rule, Spanned, SrcSpan, Stmt, Term, TypeAst,
};
use thiserror::Error;

pub type DiagBundle = Vec<CompilerDiagnostic>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CompilerType {
    Int,
    String,
    Bool,
    Named(String),
    Option(Box<CompilerType>),
    List(Box<CompilerType>),
    Var(u32),
}

impl CompilerType {
    fn is_orderable(&self) -> bool {
        match self {
            Self::Int | Self::String | Self::Bool | Self::Named(_) => true,
            Self::Option(inner) | Self::List(inner) => inner.is_orderable(),
            Self::Var(_) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeDir {
    In,
    Out,
}

impl ModeDir {
    fn symbol(self) -> &'static str {
        match self {
            Self::In => "+",
            Self::Out => "-",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::In => "input",
            Self::Out => "output",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeSig {
    args: Vec<(ModeDir, CompilerType)>,
}

impl ModeSig {
    pub fn args(&self) -> &[(ModeDir, CompilerType)] {
        &self.args
    }
}

fn format_type_name(ty: &CompilerType) -> String {
    match ty {
        CompilerType::Int => "int".to_string(),
        CompilerType::String => "string".to_string(),
        CompilerType::Bool => "bool".to_string(),
        CompilerType::Named(name) => name.clone(),
        CompilerType::Option(inner) => format!("{}?", format_type_name(inner)),
        CompilerType::List(inner) => format!("[{}]", format_type_name(inner)),
        CompilerType::Var(v) => format!("_t{v}"),
    }
}

fn format_mode_signature(sig: &ModeSig) -> String {
    let args = sig
        .args
        .iter()
        .map(|(dir, ty)| format!("{}{}", dir.symbol(), format_type_name(ty)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("({args})")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicateDecl {
    kind: DeclarationKind,
    args: Vec<CompilerType>,
    attrs: Vec<DeclAttr>,
    span: SrcSpan,
    inferred: bool,
    inferred_from: Option<PredicateUsageKind>,
}

impl PredicateDecl {
    pub fn kind(&self) -> DeclarationKind {
        self.kind
    }

    pub fn args(&self) -> &[CompilerType] {
        &self.args
    }

    pub fn attrs(&self) -> &[DeclAttr] {
        &self.attrs
    }

    pub fn span(&self) -> SrcSpan {
        self.span
    }

    pub fn inferred(&self) -> bool {
        self.inferred
    }

    fn is_output(&self) -> bool {
        self.attrs.contains(&DeclAttr::Output)
    }

    fn schema_origin(&self) -> PredicateSchemaOrigin {
        if self.inferred {
            self.inferred_from
                .map(PredicateSchemaOrigin::from_inference_source)
                .unwrap_or(PredicateSchemaOrigin::InferredFromUsage)
        } else {
            PredicateSchemaOrigin::Declaration
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDecl {
    variants: Vec<String>,
    span: SrcSpan,
}

impl EnumDecl {
    pub fn variants(&self) -> &[String] {
        &self.variants
    }

    pub fn span(&self) -> SrcSpan {
        self.span
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedProgram {
    sources: raql_syntax::SourceMap,
    predicates: BTreeMap<String, PredicateDecl>,
    enums: BTreeMap<String, EnumDecl>,
    modes: BTreeMap<String, Vec<ModeSig>>,
    mode_spans: BTreeMap<String, Vec<SrcSpan>>,
    pragmas: BTreeMap<String, i64>,
    facts: Vec<Spanned<raql_syntax::Fact>>,
    rules: Vec<Spanned<Rule>>,
}

#[derive(Debug, Clone)]
pub struct TypedRule {
    rule: Spanned<Rule>,
    var_types: BTreeMap<String, CompilerType>,
}

impl TypedRule {
    pub fn rule(&self) -> &Spanned<Rule> {
        &self.rule
    }

    pub fn var_types(&self) -> &BTreeMap<String, CompilerType> {
        &self.var_types
    }

    pub fn head_predicate(&self) -> &str {
        self.rule.value.head.value.name.value.as_str()
    }

    pub fn head_terms(&self) -> &[Spanned<Term>] {
        &self.rule.value.head.value.terms
    }

    pub fn goal(&self, index: usize) -> Option<&Spanned<Goal>> {
        self.rule.value.body.get(index)
    }

    pub fn goals(&self) -> std::slice::Iter<'_, Spanned<Goal>> {
        self.rule.value.body.iter()
    }
}

#[derive(Debug, Clone)]
pub struct TypedProgram {
    sources: raql_syntax::SourceMap,
    predicates: BTreeMap<String, PredicateDecl>,
    enums: BTreeMap<String, EnumDecl>,
    modes: BTreeMap<String, Vec<ModeSig>>,
    pragmas: BTreeMap<String, i64>,
    facts: Vec<Spanned<raql_syntax::Fact>>,
    rules: Vec<TypedRule>,
}

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
    strata: BTreeMap<String, usize>,
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

#[derive(Debug, Clone)]
pub struct SccPlan {
    name: String,
    stratum: usize,
    predicates: Vec<String>,
    recursive: bool,
}

impl SccPlan {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn stratum(&self) -> usize {
        self.stratum
    }

    pub fn predicates(&self) -> &[String] {
        &self.predicates
    }

    pub fn recursive(&self) -> bool {
        self.recursive
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DepKind {
    Positive,
    Negative,
    Aggregate,
    Selection,
}

#[derive(Debug, Clone)]
struct StratificationData {
    strata: BTreeMap<String, usize>,
    sccs: Vec<SccPlan>,
}

#[derive(Debug, Clone)]
struct EdgeDiagnosticContext {
    span: SrcSpan,
    detail: String,
}

#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct CompilerDiagnostic {
    code: &'static str,
    message: String,
    severity: Severity,
    span: Option<SrcSpan>,
    help: Option<String>,
    include_stack: Box<[Utf8PathBuf]>,
}

impl CompilerDiagnostic {
    pub fn error(code: &'static str, message: impl Into<String>, span: Option<SrcSpan>) -> Self {
        Self {
            code,
            message: message.into(),
            severity: Severity::Error,
            span,
            help: None,
            include_stack: Vec::new().into_boxed_slice(),
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn code_str(&self) -> &'static str {
        self.code
    }

    pub fn span(&self) -> Option<SrcSpan> {
        self.span
    }

    pub fn include_stack(&self) -> &[Utf8PathBuf] {
        &self.include_stack
    }

    fn attach_include_stack_from_sources(&mut self, sources: &raql_syntax::SourceMap) {
        if !self.include_stack.is_empty() {
            return;
        }
        let Some(span) = self.span else {
            return;
        };
        let Some(file) = sources.get(span.file) else {
            return;
        };
        if file.include_stack().is_empty() {
            return;
        }
        self.include_stack = file.include_stack().to_vec().into_boxed_slice();
    }
}

impl Diagnostic for CompilerDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }

    fn severity(&self) -> Option<Severity> {
        Some(self.severity)
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        let include_note = if self.include_stack.is_empty() {
            None
        } else {
            Some(format!(
                "include stack: {}",
                self.include_stack
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ))
        };
        match (self.help.as_deref(), include_note) {
            (None, None) => None,
            (Some(help), None) => Some(Box::new(help) as Box<dyn fmt::Display>),
            (None, Some(note)) => Some(Box::new(note) as Box<dyn fmt::Display>),
            (Some(help), Some(note)) => Some(Box::new(format!("{help}\n{note}"))),
        }
    }
}

fn enrich_include_stack_context(
    diagnostics: &mut [CompilerDiagnostic],
    sources: &raql_syntax::SourceMap,
) {
    for diagnostic in diagnostics {
        diagnostic.attach_include_stack_from_sources(sources);
    }
}

fn clamp_to_char_boundary(text: &str, byte_offset: usize) -> usize {
    let mut clamped = byte_offset.min(text.len());
    while clamped > 0 && !text.is_char_boundary(clamped) {
        clamped -= 1;
    }
    clamped
}

fn line_col_for_offset(text: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for ch in text[..byte_offset].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn format_span_brief(sources: &raql_syntax::SourceMap, span: SrcSpan) -> String {
    let Some(file) = sources.get(span.file) else {
        return format!("file-{}:?:?", span.file.0);
    };
    let start = clamp_to_char_boundary(file.text(), u32::from(span.range.start()) as usize);
    let end = clamp_to_char_boundary(file.text(), u32::from(span.range.end()) as usize);
    let (start_line, start_col) = line_col_for_offset(file.text(), start);
    let (end_line, end_col) = line_col_for_offset(file.text(), end);
    if start_line == end_line && start_col == end_col {
        format!("{}:{start_line}:{start_col}", file.path())
    } else {
        format!(
            "{}:{start_line}:{start_col}-{end_line}:{end_col}",
            file.path()
        )
    }
}

fn format_span_excerpt(sources: &raql_syntax::SourceMap, span: SrcSpan) -> String {
    let Some(file) = sources.get(span.file) else {
        return "<unknown>".to_string();
    };
    let start = clamp_to_char_boundary(file.text(), u32::from(span.range.start()) as usize);
    let end = clamp_to_char_boundary(file.text(), u32::from(span.range.end()) as usize);
    let Some(snippet) = file.text().get(start..end) else {
        return "<unknown>".to_string();
    };
    let normalized = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        "<unknown>".to_string()
    } else {
        normalized
    }
}

#[derive(Debug, Clone)]
enum TyOrigin {
    Generic,
    NoneLiteral,
    EmptyListLiteral,
}

#[derive(Debug, Default)]
struct InferCtx {
    next_var: u32,
    bindings: BTreeMap<u32, CompilerType>,
    origins: BTreeMap<u32, TyOrigin>,
}

impl InferCtx {
    fn fresh_var(&mut self, origin: TyOrigin) -> CompilerType {
        let id = self.next_var;
        self.next_var += 1;
        self.origins.insert(id, origin);
        CompilerType::Var(id)
    }

    fn resolve(&mut self, ty: CompilerType) -> CompilerType {
        match ty {
            CompilerType::Var(v) => {
                if let Some(bound) = self.bindings.get(&v).cloned() {
                    let resolved = self.resolve(bound);
                    self.bindings.insert(v, resolved.clone());
                    resolved
                } else {
                    CompilerType::Var(v)
                }
            }
            CompilerType::Option(inner) => CompilerType::Option(Box::new(self.resolve(*inner))),
            CompilerType::List(inner) => CompilerType::List(Box::new(self.resolve(*inner))),
            other => other,
        }
    }

    fn occurs(&mut self, needle: u32, ty: CompilerType) -> bool {
        match self.resolve(ty) {
            CompilerType::Var(v) => v == needle,
            CompilerType::Option(inner) | CompilerType::List(inner) => self.occurs(needle, *inner),
            _ => false,
        }
    }

    fn unify(
        &mut self,
        lhs: CompilerType,
        rhs: CompilerType,
        span: Option<SrcSpan>,
    ) -> Result<CompilerType, CompilerDiagnostic> {
        let lhs = self.resolve(lhs);
        let rhs = self.resolve(rhs);
        match (lhs.clone(), rhs.clone()) {
            (CompilerType::Var(a), CompilerType::Var(b)) if a == b => Ok(lhs),
            (CompilerType::Var(v), ty) | (ty, CompilerType::Var(v)) => {
                if self.occurs(v, ty.clone()) {
                    return Err(CompilerDiagnostic::error(
                        "RAQL0205",
                        format!(
                            "infinite type detected during unification: variable `{}` occurs inside recursive term `{}`",
                            format_type_name(&CompilerType::Var(v)),
                            format_type_name(&ty)
                        ),
                        span,
                    ));
                }
                self.bindings.insert(v, ty.clone());
                Ok(ty)
            }
            (CompilerType::Int, CompilerType::Int)
            | (CompilerType::String, CompilerType::String)
            | (CompilerType::Bool, CompilerType::Bool) => Ok(lhs),
            (CompilerType::Named(a), CompilerType::Named(b)) if a == b => Ok(lhs),
            (CompilerType::Option(a), CompilerType::Option(b)) => {
                let inner = self.unify(*a, *b, span)?;
                Ok(CompilerType::Option(Box::new(inner)))
            }
            (CompilerType::List(a), CompilerType::List(b)) => {
                let inner = self.unify(*a, *b, span)?;
                Ok(CompilerType::List(Box::new(inner)))
            }
            _ => Err(CompilerDiagnostic::error(
                "RAQL0200",
                format!(
                    "type mismatch: cannot unify `{}` with `{}`",
                    format_type_name(&lhs),
                    format_type_name(&rhs)
                ),
                span,
            )),
        }
    }

    fn ensure_concrete(
        &mut self,
        ty: CompilerType,
        span: Option<SrcSpan>,
    ) -> Result<CompilerType, CompilerDiagnostic> {
        let resolved = self.resolve(ty);
        match resolved.clone() {
            CompilerType::Var(v) => {
                let code = match self.origins.get(&v) {
                    Some(TyOrigin::NoneLiteral) => "RAQL0201",
                    Some(TyOrigin::EmptyListLiteral) => "RAQL0202",
                    _ => "RAQL0203",
                };
                let diagnostic = match self.origins.get(&v) {
                    Some(TyOrigin::NoneLiteral) => CompilerDiagnostic::error(
                        code,
                        "cannot infer concrete option type for `none` literal",
                        span,
                    )
                    .with_help(
                        "use `none` where an option type is already known (for example via a declaration) or use `some(...)`",
                    ),
                    Some(TyOrigin::EmptyListLiteral) => CompilerDiagnostic::error(
                        code,
                        "cannot infer element type for empty list literal `[]`",
                        span,
                    )
                    .with_help(
                        "add a typed element (for example `[1]`) or use `[]` where a list element type is already known",
                    ),
                    _ => CompilerDiagnostic::error(code, "unable to infer concrete type", span),
                };
                Err(diagnostic)
            }
            CompilerType::Option(inner) => Ok(CompilerType::Option(Box::new(
                self.ensure_concrete(*inner, span)?,
            ))),
            CompilerType::List(inner) => Ok(CompilerType::List(Box::new(
                self.ensure_concrete(*inner, span)?,
            ))),
            _ => Ok(resolved),
        }
    }
}

pub fn resolve(program: Program<AstPhase>) -> Result<ResolvedProgram, DiagBundle> {
    let (sources, phase) = program.into_parts();
    let mut diagnostics = Vec::new();
    let mut predicates = BTreeMap::<String, PredicateDecl>::new();
    let mut enums = BTreeMap::<String, EnumDecl>::new();
    let mut modes = BTreeMap::<String, Vec<ModeSig>>::new();
    let mut mode_spans = BTreeMap::<String, Vec<SrcSpan>>::new();
    let mut pragmas = BTreeMap::<String, i64>::new();
    let mut facts = Vec::new();
    let mut rules = Vec::new();

    for stmt in &phase.statements {
        match &stmt.value {
            Stmt::Directive(dir) => match dir {
                Directive::Include(_) => {}
                Directive::Type(td) => {
                    let name = td.name.value.to_string();
                    let mut variants = Vec::new();
                    let mut seen = BTreeSet::new();
                    for v in &td.variants {
                        let variant = v.value.to_string();
                        if seen.insert(variant.clone()) {
                            variants.push(variant);
                        }
                    }
                    if enums
                        .insert(
                            name.clone(),
                            EnumDecl {
                                variants,
                                span: stmt.span,
                            },
                        )
                        .is_some()
                    {
                        diagnostics.push(CompilerDiagnostic::error(
                            "RAQL0102",
                            format!("duplicate enum `{name}`"),
                            Some(td.name.span),
                        ));
                    }
                }
                Directive::Mode(md) => {
                    let name = md.predicate.value.to_string();
                    let mut expanded = vec![Vec::<(ModeDir, CompilerType)>::new()];
                    for arg in &md.args {
                        let arg_ty = parse_type_ast(
                            &arg.value.ty.value,
                            Some(arg.value.ty.span),
                            &mut diagnostics,
                        );
                        let dirs = match arg.value.direction.value {
                            ModeDirection::In => vec![ModeDir::In],
                            ModeDirection::Out => vec![ModeDir::Out],
                            ModeDirection::Any => vec![ModeDir::In, ModeDir::Out],
                        };
                        let mut next = Vec::new();
                        for base in &expanded {
                            for d in &dirs {
                                let mut row = base.clone();
                                row.push((*d, arg_ty.clone()));
                                next.push(row);
                            }
                        }
                        expanded = next;
                    }
                    let entry = modes.entry(name).or_default();
                    let span_entry = mode_spans
                        .entry(md.predicate.value.to_string())
                        .or_default();
                    for args in expanded {
                        entry.push(ModeSig { args });
                        span_entry.push(md.predicate.span);
                    }
                }
                Directive::Pragma(p) => {
                    pragmas.insert(p.name.value.to_string(), p.value.value);
                }
            },
            Stmt::Declaration(d) => {
                let name = d.name.value.to_string();
                let mut args = Vec::new();
                for arg in &d.args {
                    args.push(parse_type_ast(
                        &arg.value.ty.value,
                        Some(arg.value.ty.span),
                        &mut diagnostics,
                    ));
                }
                let attrs = d.attrs.iter().map(|a| a.value).collect::<Vec<_>>();
                if predicates
                    .insert(
                        name.clone(),
                        PredicateDecl {
                            kind: d.kind,
                            args,
                            attrs,
                            span: d.name.span,
                            inferred: false,
                            inferred_from: None,
                        },
                    )
                    .is_some()
                {
                    diagnostics.push(CompilerDiagnostic::error(
                        "RAQL0101",
                        format!("duplicate predicate declaration `{name}`"),
                        Some(d.name.span),
                    ));
                }
            }
            Stmt::Fact(f) => facts.push(Spanned::new(stmt.span, f.clone())),
            Stmt::Rule(r) => rules.push(Spanned::new(stmt.span, r.clone())),
        }
    }

    if !diagnostics.is_empty() {
        enrich_include_stack_context(&mut diagnostics, &sources);
        return Err(diagnostics);
    }

    Ok(ResolvedProgram {
        sources,
        predicates,
        enums,
        modes,
        mode_spans,
        pragmas,
        facts,
        rules,
    })
}

pub fn typecheck(mut resolved: ResolvedProgram) -> Result<TypedProgram, DiagBundle> {
    let mut diagnostics = Vec::new();
    let mut infer = InferCtx::default();

    collect_missing_predicates(
        &mut resolved.predicates,
        &resolved.facts,
        &resolved.rules,
        &resolved.sources,
        &mut infer,
        &mut diagnostics,
    );
    if !diagnostics.is_empty() {
        enrich_include_stack_context(&mut diagnostics, &resolved.sources);
        return Err(diagnostics);
    }

    for (name, predicate) in &resolved.predicates {
        if predicate.is_output() && name == "out_status" && predicate.args.len() == 1 {
            diagnostics.push(CompilerDiagnostic::error(
                "RAQL0404",
                "`out_status/1` is reserved for engine output only",
                Some(predicate.span),
            ));
        }
    }

    for fact in &resolved.facts {
        if fact.value.atom.value.name.value == "out_status" {
            diagnostics.push(CompilerDiagnostic::error(
                "RAQL0404",
                "`out_status/1` is reserved for engine output only",
                Some(fact.value.atom.span),
            ));
        }
        type_atom(
            &fact.value.atom.value,
            Some(fact.value.atom.span),
            &resolved.enums,
            &resolved.sources,
            &mut resolved.predicates,
            &mut infer,
            &mut BTreeMap::new(),
            &mut diagnostics,
        );
        if fact_contains_var(&fact.value.atom.value) {
            diagnostics.push(CompilerDiagnostic::error(
                "RAQL0402",
                "facts must be ground and cannot contain variables or wildcard",
                Some(fact.span),
            ));
        }
    }

    let mut typed_rules = Vec::new();
    for rule in &resolved.rules {
        let mut locals = BTreeMap::<String, CompilerType>::new();
        type_atom(
            &rule.value.head.value,
            Some(rule.value.head.span),
            &resolved.enums,
            &resolved.sources,
            &mut resolved.predicates,
            &mut infer,
            &mut locals,
            &mut diagnostics,
        );

        for goal in &rule.value.body {
            type_goal(
                goal,
                &resolved.enums,
                &resolved.sources,
                &mut resolved.predicates,
                &mut infer,
                &mut locals,
                &mut diagnostics,
            );
        }

        let mut concrete_locals = BTreeMap::new();
        for (name, ty) in locals {
            match infer.ensure_concrete(ty, Some(rule.span)) {
                Ok(t) => {
                    concrete_locals.insert(name, t);
                }
                Err(e) => diagnostics.push(e),
            }
        }
        typed_rules.push(TypedRule {
            rule: rule.clone(),
            var_types: concrete_locals,
        });
    }

    let pred_keys = resolved.predicates.keys().cloned().collect::<Vec<_>>();
    for key in pred_keys {
        let Some(pred) = resolved.predicates.get(&key).cloned() else {
            continue;
        };
        let mut concrete_args = Vec::new();
        for ty in pred.args {
            match infer.ensure_concrete(ty, Some(pred.span)) {
                Ok(t) => concrete_args.push(t),
                Err(e) => diagnostics.push(e),
            }
        }
        if let Some(slot) = resolved.predicates.get_mut(&key) {
            slot.args = concrete_args;
        }
    }

    validate_graph_edge_contract(&resolved.predicates, &mut diagnostics);
    validate_reserved_engine_contracts(&resolved.predicates, &mut diagnostics);

    for (pred, sigs) in &resolved.modes {
        if let Some(decl) = resolved.predicates.get(pred) {
            let spans = resolved.mode_spans.get(pred);
            for (mode_idx, sig) in sigs.iter().enumerate() {
                let mode_span = spans
                    .and_then(|items| items.get(mode_idx))
                    .copied()
                    .or(Some(decl.span));
                let mode_signature = format_mode_signature(sig);
                if sig.args.len() != decl.args.len() {
                    diagnostics.push(
                        CompilerDiagnostic::error(
                            "RAQL0204",
                            format!(
                                "mode `{mode_signature}` for `{pred}` has {} argument(s), but predicate declaration expects {}",
                                sig.args.len(),
                                decl.args.len()
                            ),
                            mode_span,
                        )
                        .with_help(format!(
                            "`{pred}` is declared with arity {} at {}",
                            decl.args.len(),
                            format_span_brief(&resolved.sources, decl.span)
                        )),
                    );
                    continue;
                }
                for (idx, ((mode_dir, mode_ty), decl_ty)) in
                    sig.args.iter().zip(&decl.args).enumerate()
                {
                    let mut local = InferCtx::default();
                    if local
                        .unify(decl_ty.clone(), mode_ty.clone(), mode_span)
                        .is_err()
                    {
                        diagnostics.push(
                            CompilerDiagnostic::error(
                                "RAQL0204",
                                format!(
                                    "mode `{mode_signature}` for `{pred}` argument {} ({} {}) expects `{}`, but mode provides `{}`",
                                    idx + 1,
                                    mode_dir.symbol(),
                                    mode_dir.label(),
                                    format_type_name(decl_ty),
                                    format_type_name(mode_ty)
                                ),
                                mode_span,
                            )
                            .with_help(format!(
                                "`{pred}` declaration at {} uses `{}` for argument {}",
                                format_span_brief(&resolved.sources, decl.span),
                                format_type_name(decl_ty),
                                idx + 1
                            )),
                        );
                    }
                }
            }
        } else {
            let mode_span = resolved
                .mode_spans
                .get(pred)
                .and_then(|v| v.first())
                .copied();
            diagnostics.push(CompilerDiagnostic::error(
                "RAQL0103",
                format!("mode references unknown predicate `{pred}`"),
                mode_span,
            ));
        }
    }

    if !diagnostics.is_empty() {
        enrich_include_stack_context(&mut diagnostics, &resolved.sources);
        return Err(diagnostics);
    }

    Ok(TypedProgram {
        sources: resolved.sources,
        predicates: resolved.predicates,
        enums: resolved.enums,
        modes: resolved.modes,
        pragmas: resolved.pragmas,
        facts: resolved.facts,
        rules: typed_rules,
    })
}

fn validate_graph_edge_contract(
    predicates: &BTreeMap<String, PredicateDecl>,
    diagnostics: &mut DiagBundle,
) {
    let Some(pred) = predicates.get("graph_edge") else {
        return;
    };
    let shape_ok = pred.kind == DeclarationKind::Relation
        && pred.args.len() == 5
        && pred.args[0] == CompilerType::String
        && pred.args[1] == CompilerType::Named("Def".to_string())
        && pred.args[2] == CompilerType::Named("Def".to_string())
        && pred.args[3] == CompilerType::String
        && pred.args[4] == CompilerType::Named("Span".to_string());
    if !shape_ok {
        diagnostics.push(
            CompilerDiagnostic::error(
                "RAQL0405",
                "`graph_edge/5` must be `.decl graph_edge(Graph: string, From: Def, To: Def, EdgeKind: string, Evidence: Span).`",
                Some(pred.span),
            )
            .with_help("use the exact required schema from spec §11.2"),
        );
    }
}

fn validate_reserved_engine_contracts(
    predicates: &BTreeMap<String, PredicateDecl>,
    diagnostics: &mut DiagBundle,
) {
    for (name, predicate) in predicates {
        match name.as_str() {
            "contains" => validate_reserved_predicate_shape(
                predicate,
                Some(DeclarationKind::Relation),
                &[CompilerType::String, CompilerType::String],
                diagnostics,
                "`contains/2` must be `.decl contains(Haystack: string, Needle: string) extern.`",
            ),
            "starts_with" => validate_reserved_predicate_shape(
                predicate,
                Some(DeclarationKind::Relation),
                &[CompilerType::String, CompilerType::String],
                diagnostics,
                "`starts_with/2` must be `.decl starts_with(S: string, Prefix: string) extern.`",
            ),
            "fmt" => validate_reserved_predicate_shape(
                predicate,
                Some(DeclarationKind::Relation),
                &[
                    CompilerType::String,
                    CompilerType::List(Box::new(CompilerType::String)),
                    CompilerType::String,
                ],
                diagnostics,
                "`fmt/3` must be `.decl fmt(Format: string, Args: list<string>, Out: string) extern.`",
            ),
            "witness_path" => validate_reserved_predicate_shape(
                predicate,
                Some(DeclarationKind::Relation),
                &[
                    CompilerType::String,
                    CompilerType::Named("Def".to_string()),
                    CompilerType::Named("Def".to_string()),
                    CompilerType::Named("Path".to_string()),
                ],
                diagnostics,
                "`witness_path/4` must be `.decl witness_path(Graph: string, From: Def, To: Def, P: Path) extern.`",
            ),
            "path_hop" => validate_reserved_predicate_shape(
                predicate,
                Some(DeclarationKind::Relation),
                &[
                    CompilerType::Named("Path".to_string()),
                    CompilerType::Int,
                    CompilerType::Named("Def".to_string()),
                    CompilerType::Named("Def".to_string()),
                    CompilerType::String,
                    CompilerType::Named("Span".to_string()),
                ],
                diagnostics,
                "`path_hop/6` must be `.decl path_hop(P: Path, Seq: int, From: Def, To: Def, EdgeKind: string, Evidence: Span) extern.`",
            ),
            "world_stamp" => validate_reserved_predicate_shape(
                predicate,
                Some(DeclarationKind::Function),
                &[CompilerType::String],
                diagnostics,
                "`world_stamp/1` must be `.func world_stamp(Stamp: string) extern.`",
            ),
            "path_limit" | "path_max_depth" | "control_max_depth" => {
                validate_reserved_predicate_shape(
                    predicate,
                    None,
                    &[CompilerType::Int],
                    diagnostics,
                    &format!("`{name}/1` must use exactly one `int` scalar argument"),
                );
            }
            "opt_max_iters" => validate_reserved_predicate_shape(
                predicate,
                None,
                &[CompilerType::Option(Box::new(CompilerType::Int))],
                diagnostics,
                "`opt_max_iters/1` must use exactly one `option<int>` scalar argument",
            ),
            "coalesce" => {
                if predicate.kind != DeclarationKind::Function || predicate.args.len() != 3 {
                    diagnostics.push(
                        CompilerDiagnostic::error(
                            "RAQL0406",
                            "`coalesce/3` is reserved and must be `.func coalesce(Opt: option<T>, Default: T, Out: T) extern.`",
                            Some(predicate.span),
                        )
                        .with_help("use the reserved engine-managed `coalesce/3` signature"),
                    );
                }
            }
            _ => {}
        }
    }
}

fn validate_reserved_predicate_shape(
    predicate: &PredicateDecl,
    expected_kind: Option<DeclarationKind>,
    expected_args: &[CompilerType],
    diagnostics: &mut DiagBundle,
    message: &str,
) {
    let shape_ok = expected_kind.is_none_or(|kind| predicate.kind == kind)
        && predicate.args.len() == expected_args.len()
        && predicate.args.iter().zip(expected_args.iter()).all(|(lhs, rhs)| lhs == rhs);
    if shape_ok {
        return;
    }
    diagnostics.push(
        CompilerDiagnostic::error("RAQL0406", message, Some(predicate.span))
            .with_help("use the reserved engine-managed schema exactly"),
    );
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

fn term_vars(term: &Spanned<Term>) -> BTreeSet<String> {
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

fn goals_mention_var(goals: &[Spanned<Goal>], var: &str) -> bool {
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

fn parse_type_ast(
    ast: &TypeAst,
    _span: Option<SrcSpan>,
    diagnostics: &mut DiagBundle,
) -> CompilerType {
    match ast {
        TypeAst::Int => CompilerType::Int,
        TypeAst::String => CompilerType::String,
        TypeAst::Bool => CompilerType::Bool,
        TypeAst::Named(name) => CompilerType::Named(name.to_string()),
        TypeAst::Option(inner) => CompilerType::Option(Box::new(parse_type_ast(
            &inner.value,
            Some(inner.span),
            diagnostics,
        ))),
        TypeAst::List(inner) => CompilerType::List(Box::new(parse_type_ast(
            &inner.value,
            Some(inner.span),
            diagnostics,
        ))),
    }
}

fn collect_missing_predicates(
    predicates: &mut BTreeMap<String, PredicateDecl>,
    facts: &[Spanned<raql_syntax::Fact>],
    rules: &[Spanned<Rule>],
    sources: &raql_syntax::SourceMap,
    infer: &mut InferCtx,
    diagnostics: &mut DiagBundle,
) {
    let mut usages = BTreeMap::<String, Vec<PredicateUsage>>::new();

    for fact in facts {
        let atom = &fact.value.atom.value;
        record_predicate_usage(
            &mut usages,
            atom.name.value.as_str(),
            atom.terms.len(),
            atom.name.span,
            PredicateUsageKind::Fact,
        );
    }
    for rule in rules {
        collect_rule_atoms(&rule.value, &mut usages);
    }

    for (name, predicate_usages) in usages {
        if let Some(existing) = predicates.get(&name) {
            for usage in &predicate_usages {
                if usage.arity != existing.args.len() {
                    push_arity_mismatch_diagnostic(
                        diagnostics,
                        &name,
                        usage,
                        existing.args.len(),
                        existing.span,
                        existing.schema_origin(),
                        sources,
                    );
                }
            }
            continue;
        }

        let inferred_schema = predicate_usages
            .iter()
            .find(|usage| usage.kind.can_infer_schema())
            .copied();
        let Some(schema_usage) = inferred_schema else {
            for usage in &predicate_usages {
                diagnostics.push(
                    CompilerDiagnostic::error(
                        "RAQL0100",
                        format!(
                            "unknown predicate `{name}` referenced in {}",
                            usage.kind.usage_label()
                        ),
                        Some(usage.span),
                    )
                    .with_help(
                        "add a `.decl` for this predicate or define it with at least one rule head",
                    ),
                );
            }
            continue;
        };

        for usage in &predicate_usages {
            if usage.arity != schema_usage.arity {
                push_arity_mismatch_diagnostic(
                    diagnostics,
                    &name,
                    usage,
                    schema_usage.arity,
                    schema_usage.span,
                    PredicateSchemaOrigin::from_inference_source(schema_usage.kind),
                    sources,
                );
            }
        }

        predicates.insert(
            name,
            PredicateDecl {
                kind: DeclarationKind::Relation,
                args: (0..schema_usage.arity)
                    .map(|_| infer.fresh_var(TyOrigin::Generic))
                    .collect(),
                attrs: Vec::new(),
                span: schema_usage.span,
                inferred: true,
                inferred_from: Some(schema_usage.kind),
            },
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PredicateUsageKind {
    Fact,
    RuleHead,
    RuleBody,
    RuleBodyNegated,
}

impl PredicateUsageKind {
    fn can_infer_schema(self) -> bool {
        matches!(self, Self::Fact | Self::RuleHead)
    }

    fn usage_label(self) -> &'static str {
        match self {
            Self::Fact => "a fact",
            Self::RuleHead => "a rule head",
            Self::RuleBody => "a rule body atom",
            Self::RuleBodyNegated => "a negated rule body atom",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PredicateUsage {
    arity: usize,
    span: SrcSpan,
    kind: PredicateUsageKind,
}

#[derive(Debug, Clone, Copy)]
enum PredicateSchemaOrigin {
    Declaration,
    InferredFromFact,
    InferredFromRuleHead,
    InferredFromUsage,
}

impl PredicateSchemaOrigin {
    fn from_inference_source(kind: PredicateUsageKind) -> Self {
        match kind {
            PredicateUsageKind::Fact => Self::InferredFromFact,
            PredicateUsageKind::RuleHead => Self::InferredFromRuleHead,
            PredicateUsageKind::RuleBody | PredicateUsageKind::RuleBodyNegated => {
                Self::InferredFromRuleHead
            }
        }
    }

    fn anchor_label(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::InferredFromFact => "fact",
            Self::InferredFromRuleHead => "rule head",
            Self::InferredFromUsage => "prior usage",
        }
    }
}

fn format_arity_mismatch_help(
    usage_arity: usize,
    expected_arity: usize,
    expected_span: SrcSpan,
    origin: PredicateSchemaOrigin,
    sources: &raql_syntax::SourceMap,
) -> String {
    format!(
        "make the argument counts match: either change this usage to {expected_arity} argument(s), or edit the {} at {} to use {usage_arity} argument(s)",
        origin.anchor_label(),
        format_span_brief(sources, expected_span),
    )
}

fn record_predicate_usage(
    usages: &mut BTreeMap<String, Vec<PredicateUsage>>,
    name: &str,
    arity: usize,
    span: SrcSpan,
    kind: PredicateUsageKind,
) {
    usages
        .entry(name.to_string())
        .or_default()
        .push(PredicateUsage { arity, span, kind });
}

fn push_arity_mismatch_diagnostic(
    diagnostics: &mut DiagBundle,
    name: &str,
    usage: &PredicateUsage,
    expected_arity: usize,
    expected_span: SrcSpan,
    origin: PredicateSchemaOrigin,
    sources: &raql_syntax::SourceMap,
) {
    diagnostics.push(
        CompilerDiagnostic::error(
            "RAQL0203",
            format!(
                "predicate `{name}` is used with {} argument(s) in {} at {}; the {} at {} expects {} argument(s)",
                usage.arity,
                usage.kind.usage_label(),
                format_span_brief(sources, usage.span),
                origin.anchor_label(),
                format_span_brief(sources, expected_span),
                expected_arity,
            ),
            Some(usage.span),
        )
        .with_help(format_arity_mismatch_help(
            usage.arity,
            expected_arity,
            expected_span,
            origin,
            sources,
        )),
    );
}

fn collect_rule_atoms(rule: &Rule, usages: &mut BTreeMap<String, Vec<PredicateUsage>>) {
    let head = &rule.head.value;
    record_predicate_usage(
        usages,
        head.name.value.as_str(),
        head.terms.len(),
        head.name.span,
        PredicateUsageKind::RuleHead,
    );
    for goal in &rule.body {
        collect_goal_atoms(&goal.value, usages);
    }
}

fn collect_goal_atoms(goal: &Goal, usages: &mut BTreeMap<String, Vec<PredicateUsage>>) {
    match goal {
        Goal::Atom(a) => {
            record_predicate_usage(
                usages,
                a.name.value.as_str(),
                a.terms.len(),
                a.name.span,
                PredicateUsageKind::RuleBody,
            );
        }
        Goal::Not(n) => {
            record_predicate_usage(
                usages,
                n.atom.value.name.value.as_str(),
                n.atom.value.terms.len(),
                n.atom.value.name.span,
                PredicateUsageKind::RuleBodyNegated,
            );
        }
        Goal::Constraint(_) => {}
        Goal::Aggregate(a) => {
            for g in &a.goals {
                collect_goal_atoms(&g.value, usages);
            }
        }
        Goal::ChooseTopK(c) => {
            for g in &c.goals {
                collect_goal_atoms(&g.value, usages);
            }
        }
        Goal::Disjunction(d) => {
            for b in &d.branches {
                for g in b {
                    collect_goal_atoms(&g.value, usages);
                }
            }
        }
    }
}

fn expand_rules(rules: &[TypedRule]) -> Vec<TypedRule> {
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

fn type_goal(
    goal: &Spanned<Goal>,
    enums: &BTreeMap<String, EnumDecl>,
    sources: &raql_syntax::SourceMap,
    predicates: &mut BTreeMap<String, PredicateDecl>,
    infer: &mut InferCtx,
    locals: &mut BTreeMap<String, CompilerType>,
    diagnostics: &mut DiagBundle,
) {
    match &goal.value {
        Goal::Atom(atom) => type_atom(
            atom,
            Some(goal.span),
            enums,
            sources,
            predicates,
            infer,
            locals,
            diagnostics,
        ),
        Goal::Not(not) => type_atom(
            &not.atom.value,
            Some(not.atom.span),
            enums,
            sources,
            predicates,
            infer,
            locals,
            diagnostics,
        ),
        Goal::Constraint(c) => type_constraint(c, enums, infer, locals, diagnostics),
        Goal::Aggregate(a) => {
            for inner in &a.goals {
                type_goal(
                    inner,
                    enums,
                    sources,
                    predicates,
                    infer,
                    locals,
                    diagnostics,
                );
            }
            if let Some(p) = &a.projection_var {
                if !goals_mention_var(&a.goals, p.value.as_str()) {
                    diagnostics.push(CompilerDiagnostic::error(
                        "RAQL0206",
                        format!(
                            "aggregate projection variable `{}` must appear in aggregate goals",
                            p.value
                        ),
                        Some(p.span),
                    ));
                }
            }

            let out_ty = match a.name.value {
                AggregateName::Count | AggregateName::CountDistinct => CompilerType::Int,
                AggregateName::Sum => {
                    if let Some(p) = &a.projection_var {
                        let proj = locals
                            .entry(p.value.to_string())
                            .or_insert_with(|| infer.fresh_var(TyOrigin::Generic))
                            .clone();
                        if let Err(e) = infer.unify(proj, CompilerType::Int, Some(p.span)) {
                            diagnostics.push(e);
                        }
                    } else {
                        diagnostics.push(CompilerDiagnostic::error(
                            "RAQL0206",
                            "sum aggregate requires projection form `sum(V : Goals)`",
                            Some(goal.span),
                        ));
                    }
                    CompilerType::Int
                }
                AggregateName::Min | AggregateName::Max => {
                    if let Some(p) = &a.projection_var {
                        let proj = locals
                            .entry(p.value.to_string())
                            .or_insert_with(|| infer.fresh_var(TyOrigin::Generic))
                            .clone();
                        let resolved = infer.resolve(proj.clone());
                        if !matches!(resolved, CompilerType::Var(_)) && !resolved.is_orderable() {
                            diagnostics.push(CompilerDiagnostic::error(
                                "RAQL0206",
                                "min/max aggregate projection must be orderable",
                                Some(p.span),
                            ));
                        }
                        proj
                    } else {
                        diagnostics.push(CompilerDiagnostic::error(
                            "RAQL0206",
                            "min/max aggregates require projection form `min(V : Goals)` / `max(V : Goals)`",
                            Some(goal.span),
                        ));
                        infer.fresh_var(TyOrigin::Generic)
                    }
                }
            };
            let slot = locals
                .entry(a.out.value.to_string())
                .or_insert_with(|| infer.fresh_var(TyOrigin::Generic))
                .clone();
            if let Err(e) = infer.unify(slot, out_ty, Some(goal.span)) {
                diagnostics.push(e);
            }
        }
        Goal::ChooseTopK(c) => {
            if !matches!(c.k.value, Term::Int(_)) {
                let kt = infer_term(&c.k, enums, infer, locals, diagnostics);
                if let Err(e) = infer.unify(kt, CompilerType::Int, Some(c.k.span)) {
                    diagnostics.push(e);
                }
            }
            let score = locals
                .entry(c.score_var.value.to_string())
                .or_insert_with(|| infer.fresh_var(TyOrigin::Generic))
                .clone();
            if let Err(e) = infer.unify(score, CompilerType::Int, Some(c.score_var.span)) {
                diagnostics.push(e);
            }
            let item = locals
                .entry(c.item_var.value.to_string())
                .or_insert_with(|| infer.fresh_var(TyOrigin::Generic))
                .clone();
            let gty = infer_term(&c.group, enums, infer, locals, diagnostics);
            for inner in &c.goals {
                type_goal(
                    inner,
                    enums,
                    sources,
                    predicates,
                    infer,
                    locals,
                    diagnostics,
                );
            }
            if !infer.resolve(gty).is_orderable() {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0206",
                    "choose_topk group must be orderable",
                    Some(c.group.span),
                ));
            }
            if !infer.resolve(item).is_orderable() {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0206",
                    "choose_topk item must be orderable",
                    Some(c.item_var.span),
                ));
            }
            if !goals_mention_var(&c.goals, c.score_var.value.as_str()) {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0206",
                    format!(
                        "choose_topk score variable `{}` must appear in binder goals",
                        c.score_var.value
                    ),
                    Some(c.score_var.span),
                ));
            }
            if !goals_mention_var(&c.goals, c.item_var.value.as_str()) {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0206",
                    format!(
                        "choose_topk item variable `{}` must appear in binder goals",
                        c.item_var.value
                    ),
                    Some(c.item_var.span),
                ));
            }
        }
        Goal::Disjunction(d) => {
            for branch in &d.branches {
                let mut branch_locals = locals.clone();
                for inner in branch {
                    type_goal(
                        inner,
                        enums,
                        sources,
                        predicates,
                        infer,
                        &mut branch_locals,
                        diagnostics,
                    );
                }
                for (k, v) in branch_locals {
                    locals.entry(k).or_insert(v);
                }
            }
        }
    }
}

fn type_constraint(
    c: &Constraint,
    enums: &BTreeMap<String, EnumDecl>,
    infer: &mut InferCtx,
    locals: &mut BTreeMap<String, CompilerType>,
    diagnostics: &mut DiagBundle,
) {
    match c {
        Constraint::Relational(r) => {
            let lt = infer_term(&r.lhs, enums, infer, locals, diagnostics);
            let rt = infer_term(&r.rhs, enums, infer, locals, diagnostics);
            match r.op.value {
                RelOp::Eq => {
                    if let Err(e) = infer.unify(lt, rt, Some(r.op.span)) {
                        diagnostics.push(e);
                    }
                }
                RelOp::NotEq => {
                    if let Err(e) = infer.unify(lt, rt, Some(r.op.span)) {
                        diagnostics.push(e);
                    }
                }
                RelOp::Lt | RelOp::LtEq | RelOp::Gt | RelOp::GtEq => {
                    match infer.unify(lt, rt, Some(r.op.span)) {
                        Ok(ty) => {
                            if !infer.resolve(ty).is_orderable() {
                                diagnostics.push(CompilerDiagnostic::error(
                                    "RAQL0206",
                                    "order comparison operands must be orderable",
                                    Some(r.op.span),
                                ));
                            }
                        }
                        Err(e) => diagnostics.push(e),
                    }
                }
            }
        }
        Constraint::ArithmeticBind(b) => {
            let expr_ty = infer_expr(&b.expr, enums, infer, locals, diagnostics);
            if let Err(e) = infer.unify(expr_ty, CompilerType::Int, Some(b.expr.span)) {
                diagnostics.push(e);
            }
            let slot = locals
                .entry(b.target.value.to_string())
                .or_insert_with(|| infer.fresh_var(TyOrigin::Generic))
                .clone();
            if let Err(e) = infer.unify(slot, CompilerType::Int, Some(b.target.span)) {
                diagnostics.push(e);
            }
        }
    }
}

fn infer_expr(
    e: &Spanned<Expr>,
    enums: &BTreeMap<String, EnumDecl>,
    infer: &mut InferCtx,
    locals: &mut BTreeMap<String, CompilerType>,
    diagnostics: &mut DiagBundle,
) -> CompilerType {
    match &e.value {
        Expr::Term(t) => infer_term(t, enums, infer, locals, diagnostics),
        Expr::UnaryNeg(inner) => {
            let ty = infer_expr(inner, enums, infer, locals, diagnostics);
            if let Err(err) = infer.unify(ty, CompilerType::Int, Some(e.span)) {
                diagnostics.push(err);
            }
            CompilerType::Int
        }
        Expr::Binary { lhs, rhs, .. } => {
            let lt = infer_expr(lhs, enums, infer, locals, diagnostics);
            let rt = infer_expr(rhs, enums, infer, locals, diagnostics);
            if let Err(err) = infer.unify(lt, CompilerType::Int, Some(lhs.span)) {
                diagnostics.push(err);
            }
            if let Err(err) = infer.unify(rt, CompilerType::Int, Some(rhs.span)) {
                diagnostics.push(err);
            }
            CompilerType::Int
        }
    }
}

fn type_atom(
    atom: &raql_syntax::Atom,
    _atom_span: Option<SrcSpan>,
    enums: &BTreeMap<String, EnumDecl>,
    sources: &raql_syntax::SourceMap,
    predicates: &mut BTreeMap<String, PredicateDecl>,
    infer: &mut InferCtx,
    locals: &mut BTreeMap<String, CompilerType>,
    diagnostics: &mut DiagBundle,
) {
    let name = atom.name.value.to_string();
    let Some(pred) = predicates.get_mut(&name) else {
        diagnostics.push(CompilerDiagnostic::error(
            "RAQL0100",
            format!("unknown predicate `{name}`"),
            Some(atom.name.span),
        ));
        return;
    };
    if pred.args.len() != atom.terms.len() {
        let decl_span = pred.span;
        let decl_arity = pred.args.len();
        let schema_origin = pred.schema_origin();
        diagnostics.push(
            CompilerDiagnostic::error(
                "RAQL0203",
                format!(
                    "predicate `{name}` is used with {} argument(s) at {}; the {} at {} expects {decl_arity} argument(s)",
                    atom.terms.len(),
                    format_span_brief(sources, atom.name.span),
                    schema_origin.anchor_label(),
                    format_span_brief(sources, decl_span),
                ),
                Some(atom.name.span),
            )
            .with_help(format_arity_mismatch_help(
                atom.terms.len(),
                decl_arity,
                decl_span,
                schema_origin,
                sources,
            )),
        );
        return;
    }
    for (idx, term) in atom.terms.iter().enumerate() {
        let arg_ty = infer_term(term, enums, infer, locals, diagnostics);
        let schema_ty = pred.args[idx].clone();
        let unified = infer.unify(schema_ty, arg_ty, Some(term.span));
        if let Err(e) = unified {
            diagnostics.push(e);
        } else if pred.inferred {
            pred.args[idx] = infer.resolve(pred.args[idx].clone());
        }
    }
}

fn infer_term(
    term: &Spanned<Term>,
    enums: &BTreeMap<String, EnumDecl>,
    infer: &mut InferCtx,
    locals: &mut BTreeMap<String, CompilerType>,
    diagnostics: &mut DiagBundle,
) -> CompilerType {
    match &term.value {
        Term::Var(v) => locals
            .entry(v.to_string())
            .or_insert_with(|| infer.fresh_var(TyOrigin::Generic))
            .clone(),
        Term::Wildcard => infer.fresh_var(TyOrigin::Generic),
        Term::Int(_) => CompilerType::Int,
        Term::String(_) => CompilerType::String,
        Term::Bool(_) => CompilerType::Bool,
        Term::EnumAtom {
            enum_name,
            variant_name,
        } => {
            if let Some(ed) = enums.get(enum_name.value.as_str()) {
                if !ed
                    .variants
                    .iter()
                    .any(|variant| variant == variant_name.value.as_str())
                {
                    diagnostics.push(CompilerDiagnostic::error(
                        "RAQL0104",
                        format!(
                            "unknown variant `{}::{}`",
                            enum_name.value, variant_name.value
                        ),
                        Some(variant_name.span),
                    ));
                }
            } else {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0104",
                    format!("unknown enum `{}`", enum_name.value),
                    Some(enum_name.span),
                ));
            }
            CompilerType::Named(enum_name.value.to_string())
        }
        Term::None { turbofish } => {
            let inner = if let Some(t) = turbofish {
                parse_type_ast(&t.value, Some(t.span), diagnostics)
            } else {
                infer.fresh_var(TyOrigin::NoneLiteral)
            };
            CompilerType::Option(Box::new(inner))
        }
        Term::Some(inner) => {
            let ty = infer_term(inner, enums, infer, locals, diagnostics);
            CompilerType::Option(Box::new(ty))
        }
        Term::List { items, turbofish } => {
            let item_ty = if let Some(t) = turbofish {
                parse_type_ast(&t.value, Some(t.span), diagnostics)
            } else if items.is_empty() {
                infer.fresh_var(TyOrigin::EmptyListLiteral)
            } else {
                infer.fresh_var(TyOrigin::Generic)
            };
            for it in items {
                let ty = infer_term(it, enums, infer, locals, diagnostics);
                if let Err(e) = infer.unify(item_ty.clone(), ty, Some(it.span)) {
                    diagnostics.push(e);
                }
            }
            CompilerType::List(Box::new(item_ty))
        }
    }
}

fn fact_contains_var(atom: &raql_syntax::Atom) -> bool {
    atom.terms.iter().any(term_contains_var)
}

fn term_contains_var(t: &Spanned<Term>) -> bool {
    match &t.value {
        Term::Var(_) | Term::Wildcard => true,
        Term::Some(inner) => term_contains_var(inner),
        Term::List { items, .. } => items.iter().any(term_contains_var),
        _ => false,
    }
}

fn compute_strata(typed: &TypedProgram, diagnostics: &mut DiagBundle) -> StratificationData {
    let mut graph = DiGraph::<String, DepKind>::new();
    let mut nodes = BTreeMap::<String, NodeIndex>::new();
    let mut edge_contexts = BTreeMap::<usize, EdgeDiagnosticContext>::new();
    for name in typed.predicates.keys() {
        let idx = graph.add_node(name.clone());
        nodes.insert(name.clone(), idx);
    }

    for tr in &typed.rules {
        let head = tr.rule.value.head.value.name.value.to_string();
        for goal in &tr.rule.value.body {
            add_goal_edges(&head, goal, &mut graph, &nodes, &mut edge_contexts);
        }
    }

    let sccs = petgraph::algo::kosaraju_scc(&graph);
    let mut node_to_scc = BTreeMap::<NodeIndex, usize>::new();
    for (i, scc) in sccs.iter().enumerate() {
        for n in scc {
            node_to_scc.insert(*n, i);
        }
    }

    for edge in graph.edge_references() {
        let from = edge.source();
        let to = edge.target();
        if node_to_scc.get(&from) == node_to_scc.get(&to) {
            let Some(scc_idx) = node_to_scc.get(&from).copied() else {
                continue;
            };
            let kind = edge.weight();
            if *kind != DepKind::Positive {
                let cycle_path =
                    describe_cycle_path(&graph, sccs[scc_idx].as_slice(), from, to, *kind);
                let edge_context = edge_contexts.get(&edge.id().index());
                let mut diagnostic = CompilerDiagnostic::error(
                    "RAQL0401",
                    format!(
                        "non-stratifiable cycle detected: {cycle_path}; non-positive edge kind is {}",
                        dep_kind_label(*kind)
                    ),
                    edge_context.map(|ctx| ctx.span),
                );
                if let Some(ctx) = edge_context {
                    diagnostic =
                        diagnostic.with_help(format!("cycle edge context: {}", ctx.detail));
                }
                diagnostics.push(diagnostic);
            }
        }
    }

    let mut strata = BTreeMap::<String, usize>::new();
    for name in typed.predicates.keys() {
        strata.insert(name.clone(), 0);
    }

    let mut changed = true;
    let mut rounds = 0usize;
    while changed && rounds < 1024 {
        rounds += 1;
        changed = false;
        for edge in graph.edge_references() {
            let head = graph[edge.source()].clone();
            let dep = graph[edge.target()].clone();
            let dep_level = *strata.get(&dep).unwrap_or(&0);
            let required = dep_level
                + if *edge.weight() == DepKind::Positive {
                    0
                } else {
                    1
                };
            let slot = strata.entry(head).or_insert(0);
            if *slot < required {
                *slot = required;
                changed = true;
            }
        }
    }

    let mut recursive_scc = BTreeMap::<usize, bool>::new();
    let mut scc_name = BTreeMap::<usize, String>::new();
    for (idx, scc) in sccs.iter().enumerate() {
        let mut predicates = scc.iter().map(|n| graph[*n].clone()).collect::<Vec<_>>();
        predicates.sort();
        let recursive = predicates.len() > 1
            || scc.iter().any(|n| {
                graph
                    .edges(*n)
                    .any(|e| e.target() == *n && *e.weight() == DepKind::Positive)
            });
        recursive_scc.insert(idx, recursive);
        let name = if predicates.len() == 1 {
            predicates[0].clone()
        } else {
            predicates.join("+")
        };
        scc_name.insert(idx, name);
    }

    for tr in &typed.rules {
        let head = tr.rule.value.head.value.name.value.to_string();
        let Some(node) = nodes.get(&head).copied() else {
            continue;
        };
        let Some(scc_idx) = node_to_scc.get(&node).copied() else {
            continue;
        };
        if !recursive_scc.get(&scc_idx).copied().unwrap_or(false) {
            continue;
        }
        let cluster = scc_name
            .get(&scc_idx)
            .cloned()
            .unwrap_or_else(|| head.clone());
        for goal in &tr.rule.value.body {
            if let Some(kind) = recursive_forbidden_goal_kind(&goal.value) {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0401",
                    format!("`{kind}` is forbidden inside recursive SCC `{cluster}`"),
                    Some(goal.span),
                ));
            }
        }
    }

    let mut cond = DiGraph::<usize, ()>::new();
    let mut cond_nodes = Vec::with_capacity(sccs.len());
    for i in 0..sccs.len() {
        cond_nodes.push(cond.add_node(i));
    }
    for edge in graph.edge_references() {
        let from = edge.source();
        let to = edge.target();
        let Some(from_scc) = node_to_scc.get(&from).copied() else {
            continue;
        };
        let Some(to_scc) = node_to_scc.get(&to).copied() else {
            continue;
        };
        if from_scc == to_scc {
            continue;
        }
        // Original edge is head -> dependency; evaluation order is dependency -> head.
        cond.update_edge(cond_nodes[to_scc], cond_nodes[from_scc], ());
    }

    let topo = toposort(&cond, None).unwrap_or_default();
    let mut scc_order = BTreeMap::<usize, usize>::new();
    for (order, node) in topo.into_iter().enumerate() {
        scc_order.insert(cond[node], order);
    }

    let mut plans = Vec::with_capacity(sccs.len());
    for (idx, scc) in sccs.iter().enumerate() {
        let mut predicates = scc.iter().map(|n| graph[*n].clone()).collect::<Vec<_>>();
        predicates.sort();
        let recursive = recursive_scc.get(&idx).copied().unwrap_or(false);
        let stratum = predicates
            .iter()
            .filter_map(|p| strata.get(p).copied())
            .max()
            .unwrap_or(0);
        let name = scc_name
            .get(&idx)
            .cloned()
            .unwrap_or_else(|| predicates.join("+"));
        plans.push((
            stratum,
            scc_order.get(&idx).copied().unwrap_or(usize::MAX),
            name.clone(),
            SccPlan {
                name,
                stratum,
                predicates,
                recursive,
            },
        ));
    }
    plans.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let sccs = plans.into_iter().map(|(_, _, _, p)| p).collect::<Vec<_>>();

    StratificationData { strata, sccs }
}

fn dep_kind_label(kind: DepKind) -> &'static str {
    match kind {
        DepKind::Positive => "positive",
        DepKind::Negative => "negative",
        DepKind::Aggregate => "aggregate",
        DepKind::Selection => "selection",
    }
}

fn describe_cycle_path(
    graph: &DiGraph<String, DepKind>,
    scc: &[NodeIndex],
    from: NodeIndex,
    to: NodeIndex,
    first_kind: DepKind,
) -> String {
    let mut parts = vec![format!(
        "`{}` -{}-> `{}`",
        graph[from],
        dep_kind_label(first_kind),
        graph[to]
    )];
    if from == to {
        return parts.join("");
    }

    let mut allowed = BTreeSet::new();
    allowed.extend(scc.iter().copied());
    if let Some(path_edges) = find_path_within_scc(graph, to, from, &allowed) {
        for (edge_from, edge_to, kind) in path_edges {
            parts.push(format!(
                "`{}` -{}-> `{}`",
                graph[edge_from],
                dep_kind_label(kind),
                graph[edge_to]
            ));
        }
    }
    parts.join(", ")
}

fn find_path_within_scc(
    graph: &DiGraph<String, DepKind>,
    start: NodeIndex,
    goal: NodeIndex,
    allowed: &BTreeSet<NodeIndex>,
) -> Option<Vec<(NodeIndex, NodeIndex, DepKind)>> {
    if start == goal {
        return Some(Vec::new());
    }

    let mut queue = VecDeque::from([start]);
    let mut seen = BTreeSet::from([start]);
    let mut parent = BTreeMap::<NodeIndex, (NodeIndex, DepKind)>::new();

    while let Some(node) = queue.pop_front() {
        for edge in graph.edges(node) {
            let next = edge.target();
            if !allowed.contains(&next) || seen.contains(&next) {
                continue;
            }
            seen.insert(next);
            parent.insert(next, (node, *edge.weight()));
            if next == goal {
                let mut cur = goal;
                let mut edges_rev = Vec::new();
                while cur != start {
                    let (prev, kind) = parent.get(&cur).copied()?;
                    edges_rev.push((prev, cur, kind));
                    cur = prev;
                }
                edges_rev.reverse();
                return Some(edges_rev);
            }
            queue.push_back(next);
        }
    }

    None
}

fn recursive_forbidden_goal_kind(goal: &Goal) -> Option<&'static str> {
    match goal {
        Goal::Aggregate(_) => Some("aggregate"),
        Goal::ChooseTopK(_) => Some("choose_topk"),
        Goal::Atom(a) => match a.name.value.as_str() {
            "witness_path" => Some("witness_path"),
            "path_hop" => Some("path_hop"),
            _ => None,
        },
        Goal::Disjunction(d) => d
            .branches
            .iter()
            .flat_map(|branch| branch.iter())
            .find_map(|g| recursive_forbidden_goal_kind(&g.value)),
        Goal::Not(_) | Goal::Constraint(_) => None,
    }
}

fn add_goal_edges(
    head: &str,
    goal: &Spanned<Goal>,
    graph: &mut DiGraph<String, DepKind>,
    nodes: &BTreeMap<String, NodeIndex>,
    edge_contexts: &mut BTreeMap<usize, EdgeDiagnosticContext>,
) {
    match &goal.value {
        Goal::Atom(a) => {
            add_edge(
                head,
                a.name.value.as_str(),
                DepKind::Positive,
                graph,
                nodes,
                Some(goal.span),
                Some(format!(
                    "rule `{head}` references `{}` in body",
                    a.name.value
                )),
                edge_contexts,
            );
            add_witness_dependencies(
                head,
                a.name.value.as_str(),
                graph,
                nodes,
                Some(goal.span),
                edge_contexts,
            );
        }
        Goal::Not(n) => add_edge(
            head,
            n.atom.value.name.value.as_str(),
            DepKind::Negative,
            graph,
            nodes,
            Some(goal.span),
            Some(format!(
                "rule `{head}` has negated goal `not {}`",
                n.atom.value.name.value
            )),
            edge_contexts,
        ),
        Goal::Constraint(_) => {}
        Goal::Aggregate(a) => {
            for g in &a.goals {
                add_goal_edges_kind(head, g, DepKind::Aggregate, graph, nodes, edge_contexts);
            }
        }
        Goal::ChooseTopK(c) => {
            for g in &c.goals {
                add_goal_edges_kind(head, g, DepKind::Selection, graph, nodes, edge_contexts);
            }
        }
        Goal::Disjunction(d) => {
            for b in &d.branches {
                for g in b {
                    add_goal_edges(head, g, graph, nodes, edge_contexts);
                }
            }
        }
    }
}

fn add_goal_edges_kind(
    head: &str,
    goal: &Spanned<Goal>,
    kind: DepKind,
    graph: &mut DiGraph<String, DepKind>,
    nodes: &BTreeMap<String, NodeIndex>,
    edge_contexts: &mut BTreeMap<usize, EdgeDiagnosticContext>,
) {
    match &goal.value {
        Goal::Atom(a) => {
            add_edge(
                head,
                a.name.value.as_str(),
                kind,
                graph,
                nodes,
                Some(goal.span),
                Some(format!(
                    "rule `{head}` references `{}` in {} context",
                    a.name.value,
                    dep_kind_label(kind)
                )),
                edge_contexts,
            );
            add_witness_dependencies(
                head,
                a.name.value.as_str(),
                graph,
                nodes,
                Some(goal.span),
                edge_contexts,
            );
        }
        Goal::Not(n) => add_edge(
            head,
            n.atom.value.name.value.as_str(),
            kind,
            graph,
            nodes,
            Some(goal.span),
            Some(format!(
                "rule `{head}` negates `{}` in {} context",
                n.atom.value.name.value,
                dep_kind_label(kind)
            )),
            edge_contexts,
        ),
        Goal::Constraint(_) => {}
        Goal::Aggregate(a) => {
            for g in &a.goals {
                add_goal_edges_kind(head, g, kind, graph, nodes, edge_contexts);
            }
        }
        Goal::ChooseTopK(c) => {
            for g in &c.goals {
                add_goal_edges_kind(head, g, kind, graph, nodes, edge_contexts);
            }
        }
        Goal::Disjunction(d) => {
            for b in &d.branches {
                for g in b {
                    add_goal_edges_kind(head, g, kind, graph, nodes, edge_contexts);
                }
            }
        }
    }
}

fn add_witness_dependencies(
    head: &str,
    predicate: &str,
    graph: &mut DiGraph<String, DepKind>,
    nodes: &BTreeMap<String, NodeIndex>,
    span: Option<SrcSpan>,
    edge_contexts: &mut BTreeMap<usize, EdgeDiagnosticContext>,
) {
    if matches!(predicate, "witness_path" | "path_hop") {
        add_edge(
            head,
            "graph_edge",
            DepKind::Selection,
            graph,
            nodes,
            span,
            Some(format!(
                "rule `{head}` uses `{predicate}`, which implies dependency on `graph_edge`"
            )),
            edge_contexts,
        );
    }
}

fn add_edge(
    head: &str,
    dep: &str,
    kind: DepKind,
    graph: &mut DiGraph<String, DepKind>,
    nodes: &BTreeMap<String, NodeIndex>,
    span: Option<SrcSpan>,
    detail: Option<String>,
    edge_contexts: &mut BTreeMap<usize, EdgeDiagnosticContext>,
) {
    let Some(from) = nodes.get(head).copied() else {
        return;
    };
    let Some(to) = nodes.get(dep).copied() else {
        return;
    };
    let edge_id = graph.add_edge(from, to, kind);
    if let (Some(span), Some(detail)) = (span, detail) {
        edge_contexts.insert(edge_id.index(), EdgeDiagnosticContext { span, detail });
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
    matches!(kind, DeclarationKind::Relation) && matches!(predicate, "def")
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

#[cfg(test)]
mod tests;
