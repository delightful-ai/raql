//! RAQL evaluation engine.
#![forbid(unsafe_code)]

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexSet;
use raql_compiler::{CompilerType, ModeDir, PlannedProgram, PlannedRule};
use raql_host::HostRuntime;
use raql_host::{CallId, DefId, ImplId, NodeId, RefId, SpanId, TypeRefId};
use raql_ir::StableId;
use raql_syntax::{
    Constraint, DeclAttr, DeclarationKind, Expr, Goal, RelOp, Spanned, SrcSpan, Term, TypeAst,
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeValue {
    Int(i64),
    String(String),
    Bool(bool),
    Enum { name: String, variant: String },
    Host { kind: HostValueKind, id: u64 },
    None,
    Some(Box<RuntimeValue>),
    List(Vec<RuntimeValue>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostValueKind {
    Def,
    Span,
    TypeRef,
    Node,
    Call,
    Ref,
    Impl,
}

impl HostValueKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Def => "Def",
            Self::Span => "Span",
            Self::TypeRef => "TypeRef",
            Self::Node => "Node",
            Self::Call => "Call",
            Self::Ref => "Ref",
            Self::Impl => "Impl",
        }
    }

    const fn type_tag(self) -> &'static str {
        self.label()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalStatus {
    Ok,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalNote {
    pub section: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalResult {
    pub status: EvalStatus,
    pub notes: Vec<EvalNote>,
    pub relations: BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    pub iterations: usize,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeError {
    #[error("division by zero")]
    DivisionByZero,
    #[error("integer overflow")]
    Overflow,
    #[error("missing relation `{relation}` ({context})")]
    MissingRelation { relation: String, context: String },
    #[error("unbound variable `{0}`")]
    UnboundVar(String),
    #[error("{context}")]
    TypeMismatchContext { context: String },
    #[error("witness predicate `{predicate}` arity mismatch: expected {expected}, got {got}")]
    WitnessArity {
        predicate: String,
        expected: usize,
        got: usize,
    },
    #[error(
        "witness predicate `path_hop` cached row for path `{path_id}` has arity {got}, expected {expected}"
    )]
    WitnessPathHopRowArity {
        path_id: String,
        expected: usize,
        got: usize,
    },
    #[error(
        "extern relation `{predicate}` row {row_index} has arity {got}, expected {expected}; host `extern_relation_rows` must return declaration-matching rows"
    )]
    ExternRowArity {
        predicate: String,
        row_index: usize,
        expected: usize,
        got: usize,
    },
    #[error(
        "functional predicate `{predicate}` cardinality violation: expected exactly 1 result, got {got} ({context})"
    )]
    FunctionCardinality {
        predicate: String,
        got: usize,
        context: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RuntimeErrorCode {
    DivisionByZero,
    Overflow,
    MissingRelation,
    UnboundVar,
    TypeMismatch,
    WitnessInvalid,
    FunctionCardinality,
    ExternRowArity,
}

impl RuntimeErrorCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DivisionByZero => "RAQL0901",
            Self::Overflow => "RAQL0902",
            Self::MissingRelation => "RAQL0903",
            Self::UnboundVar => "RAQL0904",
            Self::TypeMismatch => "RAQL0905",
            Self::WitnessInvalid => "RAQL0906",
            Self::FunctionCardinality => "RAQL0907",
            Self::ExternRowArity => "RAQL0908",
        }
    }
}

impl RuntimeError {
    pub(crate) const fn code(&self) -> RuntimeErrorCode {
        match self {
            Self::DivisionByZero => RuntimeErrorCode::DivisionByZero,
            Self::Overflow => RuntimeErrorCode::Overflow,
            Self::MissingRelation { .. } => RuntimeErrorCode::MissingRelation,
            Self::UnboundVar(_) => RuntimeErrorCode::UnboundVar,
            Self::TypeMismatchContext { .. } => RuntimeErrorCode::TypeMismatch,
            Self::WitnessArity { .. } | Self::WitnessPathHopRowArity { .. } => {
                RuntimeErrorCode::WitnessInvalid
            }
            Self::FunctionCardinality { .. } => RuntimeErrorCode::FunctionCardinality,
            Self::ExternRowArity { .. } => RuntimeErrorCode::ExternRowArity,
        }
    }

    pub(crate) const fn code_str(&self) -> &'static str {
        self.code().as_str()
    }
}

#[derive(Debug, Default)]
struct ExecutionContext {
    path_hop_cache: BTreeMap<String, Vec<Vec<RuntimeValue>>>,
    next_path_id: u64,
}

impl ExecutionContext {
    fn alloc_path_id(&mut self) -> String {
        let id = self.next_path_id;
        self.next_path_id = self.next_path_id.saturating_add(1);
        format!("path:{id}")
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("host `{operation}` failed: {message}")]
pub struct EngineHostError {
    operation: &'static str,
    message: String,
}

impl EngineHostError {
    pub fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }

    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

pub trait EnginePlanView {
    fn planned(&self) -> &PlannedProgram;
}

impl EnginePlanView for PlannedProgram {
    fn planned(&self) -> &PlannedProgram {
        self
    }
}

pub trait EngineHostView {
    fn world_stamp(&mut self) -> String;
    fn stable_key(&mut self, value: &RuntimeValue) -> String;
    fn take_runtime_notes(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn set_control_max_depth(&mut self, _depth: i64) {}
    fn extern_relation_rows(
        &mut self,
        _predicate: &str,
    ) -> Result<Option<Vec<Vec<RuntimeValue>>>, EngineHostError> {
        Ok(None)
    }
}

impl EngineHostView for raql_host::MockHostRuntime {
    fn world_stamp(&mut self) -> String {
        match HostRuntime::world_stamp(self) {
            Ok(stamp) => stamp.as_str().to_string(),
            Err(never) => match never {},
        }
    }

    fn stable_key(&mut self, value: &RuntimeValue) -> String {
        match value {
            RuntimeValue::Host { kind, id } => {
                let sid = StableId::new(*id);
                match kind {
                    HostValueKind::Def => match HostRuntime::handle(self, DefId::new(sid)) {
                        Ok(handle) => handle.as_str().to_string(),
                        Err(never) => match never {},
                    },
                    HostValueKind::Span => match HostRuntime::span_key(self, SpanId::new(sid)) {
                        Ok(key) => format!(
                            "{}:{}:{}:{}:{}",
                            key.rel_path(),
                            key.start().line(),
                            key.start().column(),
                            key.end().line(),
                            key.end().column()
                        ),
                        Err(never) => match never {},
                    },
                    HostValueKind::TypeRef => {
                        match HostRuntime::typeref_id(self, TypeRefId::new(sid)) {
                            Ok(handle) => handle.as_str().to_string(),
                            Err(never) => match never {},
                        }
                    }
                    HostValueKind::Node => match HostRuntime::node_id(self, NodeId::new(sid)) {
                        Ok(handle) => handle.as_str().to_string(),
                        Err(never) => match never {},
                    },
                    HostValueKind::Call => match HostRuntime::call_id(self, CallId::new(sid)) {
                        Ok(handle) => handle.as_str().to_string(),
                        Err(never) => match never {},
                    },
                    HostValueKind::Ref => match HostRuntime::ref_id(self, RefId::new(sid)) {
                        Ok(handle) => handle.as_str().to_string(),
                        Err(never) => match never {},
                    },
                    HostValueKind::Impl => match HostRuntime::impl_id(self, ImplId::new(sid)) {
                        Ok(handle) => handle.as_str().to_string(),
                        Err(never) => match never {},
                    },
                }
            }
            _ => format!("{value:?}"),
        }
    }
}

pub fn execute(plan: &impl EnginePlanView, host: &mut impl EngineHostView) -> EvalResult {
    let mut context = ExecutionContext::default();
    let program = plan.planned();
    let mut relations = BTreeMap::<String, IndexSet<Vec<RuntimeValue>>>::new();
    for pred in program.predicates().keys() {
        relations.entry(pred.clone()).or_default();
    }
    ensure_engine_output_relations(&mut relations);
    for fact in program.facts() {
        let pred = fact.value.atom.value.name.value.to_string();
        let tuple = fact
            .value
            .atom
            .value
            .terms
            .iter()
            .filter_map(term_to_ground_value)
            .collect::<Vec<_>>();
        relations.entry(pred).or_default().insert(tuple);
    }

    let mut notes = Vec::new();
    let mut has_non_fatal_partial_note = false;
    let _ = host.take_runtime_notes();
    let extern_predicates = collect_extern_predicates(program);

    inject_required_host_relations(&mut relations, host);
    let scalar_extern_notes = match inject_host_extern_relations(
        program,
        extern_predicates.scalar.as_slice(),
        &mut relations,
        host,
    ) {
        Ok(notes) => notes,
        Err(err) => {
            let msg = format_runtime_error(&err);
            emit_partial_status(&mut relations, &mut notes, "Errors", msg);
            let _ = drain_host_runtime_notes(&mut relations, &mut notes, host);
            return EvalResult {
                status: EvalStatus::Partial,
                notes,
                relations,
                iterations: 0,
            };
        }
    };
    for note in scalar_extern_notes {
        has_non_fatal_partial_note = true;
        emit_partial_status(&mut relations, &mut notes, "Notes", note);
    }
    inject_default_scalar_inputs(&mut relations);
    if let Err(err) = sync_host_control_inputs(&relations, host) {
        let msg = format_runtime_error(&err);
        emit_partial_status(&mut relations, &mut notes, "Errors", msg);
        let _ = drain_host_runtime_notes(&mut relations, &mut notes, host);
        return EvalResult {
            status: EvalStatus::Partial,
            notes,
            relations,
            iterations: 0,
        };
    }
    let extern_notes = match inject_host_extern_relations(
        program,
        extern_predicates.non_scalar.as_slice(),
        &mut relations,
        host,
    ) {
        Ok(notes) => notes,
        Err(err) => {
            let msg = format_runtime_error(&err);
            emit_partial_status(&mut relations, &mut notes, "Errors", msg);
            let _ = drain_host_runtime_notes(&mut relations, &mut notes, host);
            return EvalResult {
                status: EvalStatus::Partial,
                notes,
                relations,
                iterations: 0,
            };
        }
    };
    for note in extern_notes {
        has_non_fatal_partial_note = true;
        emit_partial_status(&mut relations, &mut notes, "Notes", note);
    }
    if drain_host_runtime_notes(&mut relations, &mut notes, host) {
        has_non_fatal_partial_note = true;
    }
    let max_iters = match effective_max_iters(program, &relations) {
        Ok(v) => v,
        Err(err) => {
            let msg = format_runtime_error(&err);
            emit_partial_status(&mut relations, &mut notes, "Errors", msg);
            let _ = drain_host_runtime_notes(&mut relations, &mut notes, host);
            return EvalResult {
                status: EvalStatus::Partial,
                notes,
                relations,
                iterations: 0,
            };
        }
    };

    let mut total_iterations = 0usize;

    let mut rules_by_pred = BTreeMap::<String, Vec<&PlannedRule>>::new();
    for planned_rule in program.planned_rules() {
        let head = planned_rule.head_predicate().to_string();
        rules_by_pred.entry(head).or_default().push(planned_rule);
    }

    for scc in program.sccs() {
        let mut rules = Vec::<&PlannedRule>::new();
        for pred in scc.predicates() {
            if let Some(pred_rules) = rules_by_pred.get(pred) {
                rules.extend(pred_rules.iter().copied());
            }
        }
        if rules.is_empty() {
            continue;
        }

        if scc.recursive() {
            let mut changed = true;
            let mut iter_in_scc = 0usize;
            while changed {
                iter_in_scc += 1;
                total_iterations += 1;
                if iter_in_scc > max_iters {
                    let msg = format!(
                        "fixpoint iteration limit exceeded in SCC `{}`: configured max_iters={}, triggered at iteration {}",
                        scc.name(),
                        max_iters,
                        iter_in_scc
                    );
                    notes.push(EvalNote {
                        section: "Notes".to_string(),
                        message: msg.clone(),
                    });
                    emit_partial_status(&mut relations, &mut notes, "Notes", msg);
                    let _ = drain_host_runtime_notes(&mut relations, &mut notes, host);
                    return EvalResult {
                        status: EvalStatus::Partial,
                        notes,
                        relations,
                        iterations: total_iterations,
                    };
                }
                changed = match apply_rules_once(
                    program,
                    rules.as_slice(),
                    &mut context,
                    &mut relations,
                    &mut notes,
                    host,
                    &mut has_non_fatal_partial_note,
                ) {
                    Ok(changed) => changed,
                    Err(err) => {
                        let msg = format_runtime_error(&err);
                        notes.push(EvalNote {
                            section: "Errors".to_string(),
                            message: msg.clone(),
                        });
                        emit_partial_status(&mut relations, &mut notes, "Errors", msg);
                        let _ = drain_host_runtime_notes(&mut relations, &mut notes, host);
                        return EvalResult {
                            status: EvalStatus::Partial,
                            notes,
                            relations,
                            iterations: total_iterations,
                        };
                    }
                }
            }
        } else {
            total_iterations += 1;
            if let Err(err) = apply_rules_once(
                program,
                rules.as_slice(),
                &mut context,
                &mut relations,
                &mut notes,
                host,
                &mut has_non_fatal_partial_note,
            ) {
                let msg = format_runtime_error(&err);
                notes.push(EvalNote {
                    section: "Errors".to_string(),
                    message: msg.clone(),
                });
                emit_partial_status(&mut relations, &mut notes, "Errors", msg);
                let _ = drain_host_runtime_notes(&mut relations, &mut notes, host);
                return EvalResult {
                    status: EvalStatus::Partial,
                    notes,
                    relations,
                    iterations: total_iterations,
                };
            }
        }
    }

    if drain_host_runtime_notes(&mut relations, &mut notes, host) {
        has_non_fatal_partial_note = true;
    }

    if has_non_fatal_partial_note {
        return EvalResult {
            status: EvalStatus::Partial,
            notes,
            relations,
            iterations: total_iterations,
        };
    }

    emit_ok_status(&mut relations);
    EvalResult {
        status: EvalStatus::Ok,
        notes,
        relations,
        iterations: total_iterations,
    }
}

fn apply_rules_once(
    program: &PlannedProgram,
    rules: &[&PlannedRule],
    context: &mut ExecutionContext,
    relations: &mut BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    notes: &mut Vec<EvalNote>,
    host: &mut impl EngineHostView,
    has_non_fatal_partial_note: &mut bool,
) -> Result<bool, RuntimeError> {
    let mut changed = false;
    for planned_rule in rules {
        let rows = eval_rule(program, planned_rule, context, relations, host)?;
        let head_name = planned_rule.head_predicate().to_string();
        let rel = relations.entry(head_name).or_default();
        let before = rel.len();
        for row in rows {
            rel.insert(row);
        }
        if rel.len() > before {
            changed = true;
        }
        if drain_host_runtime_notes(relations, notes, host) {
            *has_non_fatal_partial_note = true;
        }
    }
    Ok(changed)
}

fn sync_host_control_inputs(
    relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    host: &mut impl EngineHostView,
) -> Result<(), RuntimeError> {
    let depth = scalar_input(relations, "control_max_depth", 32)?.max(0);
    host.set_control_max_depth(depth);
    Ok(())
}

fn format_runtime_error(err: &RuntimeError) -> String {
    format!("runtime error [{}]: {err}", err.code_str())
}

fn drain_host_runtime_notes(
    relations: &mut BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    notes: &mut Vec<EvalNote>,
    host: &mut impl EngineHostView,
) -> bool {
    let mut emitted = false;
    for message in host.take_runtime_notes() {
        emitted = true;
        emit_partial_status(relations, notes, "Notes", message);
    }
    emitted
}

fn inject_required_host_relations(
    relations: &mut BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    host: &mut impl EngineHostView,
) {
    let mut world_stamp = IndexSet::new();
    world_stamp.insert(vec![RuntimeValue::String(host.world_stamp())]);
    relations.insert("world_stamp".to_string(), world_stamp);
}

fn inject_host_extern_relations(
    program: &PlannedProgram,
    predicates: &[String],
    relations: &mut BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    host: &mut impl EngineHostView,
) -> Result<Vec<String>, RuntimeError> {
    let mut notes = Vec::new();
    for predicate in predicates {
        let Some(decl) = program.predicates().get(predicate) else {
            continue;
        };
        let kind = match decl.kind() {
            DeclarationKind::Function => "function",
            DeclarationKind::Relation => "relation",
        };
        let rows = match host.extern_relation_rows(predicate) {
            Ok(Some(rows)) => rows,
            Ok(None) => {
                if !is_engine_managed_extern(predicate) {
                    notes.push(format_extern_rows_none_note(
                        program,
                        kind,
                        predicate,
                        decl.span(),
                    ));
                }
                continue;
            }
            Err(error) => {
                if !is_engine_managed_extern(predicate) {
                    notes.push(format_extern_rows_error_note(
                        program,
                        kind,
                        predicate,
                        decl.span(),
                        &error,
                    ));
                }
                continue;
            }
        };
        let expected_arity = decl.args().len();
        let rel = relations.entry(predicate.clone()).or_default();
        for (row_index, row) in rows.into_iter().enumerate() {
            if row.len() != expected_arity {
                return Err(RuntimeError::ExternRowArity {
                    predicate: predicate.clone(),
                    row_index: row_index + 1,
                    expected: expected_arity,
                    got: row.len(),
                });
            }
            rel.insert(row);
        }
    }
    Ok(notes)
}

#[derive(Debug, Default)]
struct ExternPredicateGroups {
    scalar: Vec<String>,
    non_scalar: Vec<String>,
}

fn collect_extern_predicates(program: &PlannedProgram) -> ExternPredicateGroups {
    let mut groups = ExternPredicateGroups::default();
    for (predicate, decl) in program.predicates() {
        if !decl.attrs().contains(&DeclAttr::Extern) {
            continue;
        }
        if is_scalar_input_predicate(predicate) {
            groups.scalar.push(predicate.clone());
        } else {
            groups.non_scalar.push(predicate.clone());
        }
    }
    groups
}

fn is_scalar_input_predicate(predicate: &str) -> bool {
    matches!(
        predicate,
        "path_limit" | "path_max_depth" | "control_max_depth" | "opt_max_iters"
    )
}

fn is_engine_managed_extern(predicate: &str) -> bool {
    matches!(
        predicate,
        "contains"
            | "starts_with"
            | "fmt"
            | "coalesce"
            | "witness_path"
            | "path_hop"
            | "world_stamp"
    )
}

fn format_extern_rows_none_note(
    program: &PlannedProgram,
    kind: &str,
    predicate: &str,
    decl_span: SrcSpan,
) -> String {
    let condition = format!(
        "returned `Ok(None)` from `EngineHostView::extern_relation_rows`; treating `{predicate}` as an empty relation"
    );
    match extern_goal_reference_summary(program, predicate) {
        Some(references) => {
            let guidance = format!(
                "Return `Ok(Some(vec![]))` for explicit host no-data, or `Ok(Some(rows))` when `{predicate}` should produce data."
            );
            format_extern_rows_referenced_note(kind, predicate, &condition, &references, &guidance)
        }
        None => format_extern_rows_unreferenced_note(
            program,
            kind,
            predicate,
            decl_span,
            &condition,
            "Return `Ok(Some(vec![]))` for explicit host no-data.",
        ),
    }
}

fn format_extern_rows_error_note(
    program: &PlannedProgram,
    kind: &str,
    predicate: &str,
    decl_span: SrcSpan,
    error: &EngineHostError,
) -> String {
    let condition = format!(
        "returned host error from `EngineHostView::extern_relation_rows` ({error}); treating `{predicate}` as an empty relation"
    );
    match extern_goal_reference_summary(program, predicate) {
        Some(references) => {
            let guidance = format!(
                "Resolve the host failure and return `Ok(Some(rows))`, or use `Ok(None)` when `{predicate}` intentionally has no data."
            );
            format_extern_rows_referenced_note(kind, predicate, &condition, &references, &guidance)
        }
        None => format_extern_rows_unreferenced_note(
            program,
            kind,
            predicate,
            decl_span,
            &condition,
            "Return `Ok(Some(vec![]))` for explicit host no-data, and reserve `Err(..)` for real host failures.",
        ),
    }
}

#[derive(Debug)]
struct ExternGoalReferenceSummary {
    count: usize,
    summary: String,
}

fn format_extern_rows_unreferenced_note(
    program: &PlannedProgram,
    kind: &str,
    predicate: &str,
    decl_span: SrcSpan,
    condition: &str,
    guidance: &str,
) -> String {
    let declaration_anchor = declaration_anchor_suffix(program, decl_span);
    format!(
        "extern {kind} `{predicate}` {condition}. `{predicate}` is not referenced by any rule goals in this program.{declaration_anchor} {guidance}"
    )
}

fn format_extern_rows_referenced_note(
    kind: &str,
    predicate: &str,
    condition: &str,
    references: &ExternGoalReferenceSummary,
    guidance: &str,
) -> String {
    format!(
        "extern {kind} `{predicate}` {condition}. `{predicate}` is referenced by {} goal(s). Affected goal/rule sites: {}. {guidance}",
        references.count, references.summary
    )
}

fn extern_goal_reference_summary(
    program: &PlannedProgram,
    predicate: &str,
) -> Option<ExternGoalReferenceSummary> {
    let references = extern_goal_references(program, predicate);
    if references.is_empty() {
        return None;
    }

    let max_refs = 3usize;
    let shown = references
        .iter()
        .take(max_refs)
        .cloned()
        .collect::<Vec<_>>();
    let mut summary = shown.join("; ");
    if references.len() > max_refs {
        summary.push_str(&format!("; (+{} more goals)", references.len() - max_refs));
    }

    Some(ExternGoalReferenceSummary {
        count: references.len(),
        summary,
    })
}

fn declaration_anchor_suffix(program: &PlannedProgram, span: SrcSpan) -> String {
    source_anchor(program, span)
        .map(|anchor| format!(" Declaration anchor: `{anchor}`."))
        .unwrap_or_default()
}

fn extern_goal_references(program: &PlannedProgram, predicate: &str) -> Vec<String> {
    let mut refs = BTreeSet::new();
    for planned_rule in program.planned_rules() {
        let rule_head = format_rule_head(planned_rule);
        for goal in planned_rule.goals() {
            collect_goal_references(program, goal, &rule_head, predicate, &mut refs);
        }
    }
    refs.into_iter().collect()
}

fn collect_goal_references(
    program: &PlannedProgram,
    goal: &Spanned<Goal>,
    rule_head: &str,
    predicate: &str,
    refs: &mut BTreeSet<String>,
) {
    match &goal.value {
        Goal::Atom(atom) => {
            if atom.name.value.as_str() == predicate {
                refs.insert(format_goal_reference(
                    program,
                    format_atom(atom),
                    rule_head,
                    goal.span,
                ));
            }
        }
        Goal::Not(not) => {
            let atom = &not.atom.value;
            if atom.name.value.as_str() == predicate {
                refs.insert(format_goal_reference(
                    program,
                    format!("not {}", format_atom(atom)),
                    rule_head,
                    goal.span,
                ));
            }
        }
        Goal::Constraint(_) => {}
        Goal::Aggregate(a) => {
            for nested in &a.goals {
                collect_goal_references(program, nested, rule_head, predicate, refs);
            }
        }
        Goal::ChooseTopK(c) => {
            for nested in &c.goals {
                collect_goal_references(program, nested, rule_head, predicate, refs);
            }
        }
        Goal::Disjunction(d) => {
            for branch in &d.branches {
                for nested in branch {
                    collect_goal_references(program, nested, rule_head, predicate, refs);
                }
            }
        }
    }
}

fn format_goal_reference(
    program: &PlannedProgram,
    goal_text: String,
    rule_head: &str,
    span: SrcSpan,
) -> String {
    let anchor = source_anchor(program, span)
        .map(|anchor| format!(" @ `{anchor}`"))
        .unwrap_or_default();
    format!("goal `{goal_text}` in rule `{rule_head}`{anchor}")
}

fn goal_anchor_suffix(program: &PlannedProgram, span: SrcSpan) -> String {
    source_anchor(program, span)
        .map(|anchor| format!(" at `{anchor}`"))
        .unwrap_or_default()
}

fn source_anchor(program: &PlannedProgram, span: SrcSpan) -> Option<String> {
    let file = program.source_map().get(span.file)?;
    let start = clamp_to_char_boundary(file.text(), u32::from(span.range.start()) as usize);
    let end = clamp_to_char_boundary(file.text(), u32::from(span.range.end()) as usize);
    let (start_line, start_col) = line_col_for_offset(file.text(), start);
    let (end_line, end_col) = line_col_for_offset(file.text(), end);
    if start_line == end_line && start_col == end_col {
        Some(format!("{}:{start_line}:{start_col}", file.path()))
    } else {
        Some(format!(
            "{}:{start_line}:{start_col}-{end_line}:{end_col}",
            file.path()
        ))
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

fn inject_default_scalar_inputs(relations: &mut BTreeMap<String, IndexSet<Vec<RuntimeValue>>>) {
    ensure_default_input(relations, "path_limit", vec![RuntimeValue::Int(1)]);
    ensure_default_input(relations, "path_max_depth", vec![RuntimeValue::Int(8)]);
    ensure_default_input(relations, "control_max_depth", vec![RuntimeValue::Int(32)]);
    ensure_default_input(relations, "opt_max_iters", vec![RuntimeValue::None]);
}

fn ensure_default_input(
    relations: &mut BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    name: &str,
    row: Vec<RuntimeValue>,
) {
    let rel = relations.entry(name.to_string()).or_default();
    if rel.is_empty() {
        rel.insert(row);
    }
}

fn ensure_engine_output_relations(relations: &mut BTreeMap<String, IndexSet<Vec<RuntimeValue>>>) {
    relations.entry("out_status".to_string()).or_default();
    relations.entry("out_note".to_string()).or_default();
}

fn emit_ok_status(relations: &mut BTreeMap<String, IndexSet<Vec<RuntimeValue>>>) {
    relations
        .entry("out_status".to_string())
        .or_default()
        .insert(vec![RuntimeValue::String("ok".to_string())]);
}

fn emit_partial_status(
    relations: &mut BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    notes: &mut Vec<EvalNote>,
    section: &str,
    message: String,
) {
    relations
        .entry("out_status".to_string())
        .or_default()
        .insert(vec![RuntimeValue::String("partial".to_string())]);
    relations
        .entry("out_note".to_string())
        .or_default()
        .insert(vec![
            RuntimeValue::String(section.to_string()),
            RuntimeValue::String(message.clone()),
        ]);
    if !notes
        .iter()
        .any(|n| n.section == section && n.message == message)
    {
        notes.push(EvalNote {
            section: section.to_string(),
            message,
        });
    }
}

fn effective_max_iters(
    program: &PlannedProgram,
    relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
) -> Result<usize, RuntimeError> {
    let pragma_default = program.pragma_i64("max_iters").unwrap_or(128).max(1);
    match scalar_option_i64_input(relations, "opt_max_iters")? {
        Some(v) => Ok(v.max(1) as usize),
        None => Ok(pragma_default as usize),
    }
}

fn scalar_option_i64_input(
    relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    name: &str,
) -> Result<Option<i64>, RuntimeError> {
    let Some(values) = relations.get(name) else {
        return Ok(None);
    };
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != 1 {
        return Err(RuntimeError::FunctionCardinality {
            predicate: name.to_string(),
            got: values.len(),
            context: format!(
                "input `{name}` expected exactly one optional scalar row, found {} rows ({})",
                values.len(),
                format_indexed_rows(values, 3)
            ),
        });
    }
    let row = values.first().expect("checked non-empty");
    if row.len() != 1 {
        return Err(RuntimeError::TypeMismatchContext {
            context: format!(
                "input `{name}` row 1 expected arity 1 with `option<int>`, found arity {} in row 1 = {}",
                row.len(),
                format_runtime_row(row)
            ),
        });
    }
    match &row[0] {
        RuntimeValue::None => Ok(None),
        RuntimeValue::Some(inner) => match inner.as_ref() {
            RuntimeValue::Int(v) => Ok(Some(*v)),
            _other => Err(RuntimeError::TypeMismatchContext {
                context: format!(
                    "input `{name}` row 1 expected `option<int>`, found {} ({}) in row 1 = {}",
                    stable_type_tag(&row[0], None),
                    format_runtime_value(&row[0]),
                    format_runtime_row(row)
                ),
            }),
        },
        other => Err(RuntimeError::TypeMismatchContext {
            context: format!(
                "input `{name}` row 1 expected `option<int>`, found {} ({}) in row 1 = {}",
                stable_type_tag(other, None),
                format_runtime_value(other),
                format_runtime_row(row)
            ),
        }),
    }
}

fn is_function_predicate(program: &PlannedProgram, name: &str) -> bool {
    program
        .predicate_decl(name)
        .map(|decl| decl.kind() == DeclarationKind::Function)
        .unwrap_or(false)
}

fn function_input_constraints(
    program: &PlannedProgram,
    atom: &raql_syntax::Atom,
    env: &Env,
    selected_mode: Option<usize>,
) -> Result<Vec<(usize, RuntimeValue)>, RuntimeError> {
    let input_positions =
        input_positions(program, atom.name.value.as_str(), atom, env, selected_mode)?;
    let mut constraints = Vec::<(usize, RuntimeValue)>::new();
    for pos in input_positions {
        let value = eval_ground_term(&atom.terms[pos], env)?;
        constraints.push((pos, value));
    }
    Ok(constraints)
}

fn function_match_count(
    atom: &raql_syntax::Atom,
    rel: &IndexSet<Vec<RuntimeValue>>,
    constraints: &[(usize, RuntimeValue)],
) -> usize {
    rel.iter()
        .filter(|tuple| tuple.len() == atom.terms.len())
        .filter(|tuple| {
            constraints
                .iter()
                .all(|(idx, expected)| tuple.get(*idx).is_some_and(|value| value == expected))
        })
        .count()
}

fn input_positions(
    program: &PlannedProgram,
    predicate: &str,
    atom: &raql_syntax::Atom,
    env: &Env,
    selected_mode: Option<usize>,
) -> Result<Vec<usize>, RuntimeError> {
    if let Some(mode_idx) = selected_mode {
        if let Some(modes) = program.modes(predicate) {
            if let Some(mode) = modes.get(mode_idx) {
                let mut positions = Vec::new();
                for (idx, (dir, _)) in mode.args().iter().enumerate() {
                    if matches!(dir, ModeDir::In) {
                        positions.push(idx);
                    }
                }
                return Ok(positions);
            }
        }
    }

    // Fallback for predicates without explicit modes: treat currently-ground terms as inputs.
    let mut positions = Vec::new();
    for (idx, term) in atom.terms.iter().enumerate() {
        if eval_ground_term(term, env).is_ok() {
            positions.push(idx);
        }
    }
    Ok(positions)
}

fn format_function_input_context(
    atom: &raql_syntax::Atom,
    constraints: &[(usize, RuntimeValue)],
) -> String {
    if constraints.is_empty() {
        return format!("goal `{}` with input bindings: <none>", format_atom(atom));
    }
    let joined = constraints
        .iter()
        .map(|(idx, value)| format!("arg{}={value:?}", idx + 1))
        .collect::<Vec<_>>()
        .join(", ");
    format!("goal `{}` with input bindings: {joined}", format_atom(atom))
}

fn format_atom(atom: &raql_syntax::Atom) -> String {
    let terms = atom
        .terms
        .iter()
        .map(format_term)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}({terms})", atom.name.value)
}

fn format_rule_head(rule: &PlannedRule) -> String {
    let terms = rule
        .head_terms()
        .iter()
        .map(format_term)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}({terms})", rule.head_predicate())
}

fn format_term(term: &Spanned<Term>) -> String {
    match &term.value {
        Term::Var(v) => v.to_string(),
        Term::Wildcard => "_".to_string(),
        Term::Int(i) => i.to_string(),
        Term::String(s) => format!("{s:?}"),
        Term::Bool(b) => b.to_string(),
        Term::EnumAtom {
            enum_name,
            variant_name,
        } => format!("{}::{}", enum_name.value, variant_name.value),
        Term::None { turbofish } => match turbofish {
            Some(inner) => format!("none::<{}>", type_ast_tag(&inner.value)),
            None => "none".to_string(),
        },
        Term::Some(inner) => format!("some({})", format_term(inner)),
        Term::List { items, turbofish } => {
            let inner = items.iter().map(format_term).collect::<Vec<_>>().join(", ");
            match turbofish {
                Some(ty) => format!("[{inner}]::<{}>", type_ast_tag(&ty.value)),
                None => format!("[{inner}]"),
            }
        }
    }
}

fn format_expr(expr: &Spanned<Expr>) -> String {
    match &expr.value {
        Expr::Term(term) => format_term(term),
        Expr::UnaryNeg(inner) => format!("-{}", format_expr(inner)),
        Expr::Binary { op, lhs, rhs } => {
            format!(
                "({} {} {})",
                format_expr(lhs),
                format_arith_op(op.value),
                format_expr(rhs)
            )
        }
    }
}

fn format_arith_op(op: raql_syntax::ArithOp) -> &'static str {
    match op {
        raql_syntax::ArithOp::Add => "+",
        raql_syntax::ArithOp::Sub => "-",
        raql_syntax::ArithOp::Mul => "*",
        raql_syntax::ArithOp::Div => "/",
    }
}

fn format_runtime_value(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::Int(i) => i.to_string(),
        RuntimeValue::String(s) => format!("{s:?}"),
        RuntimeValue::Bool(b) => b.to_string(),
        RuntimeValue::Enum { name, variant } => format!("{name}::{variant}"),
        RuntimeValue::Host { kind, id } => format!("{}#{id}", kind.type_tag()),
        RuntimeValue::None => "none".to_string(),
        RuntimeValue::Some(inner) => format!("some({})", format_runtime_value(inner)),
        RuntimeValue::List(values) => format!(
            "[{}]",
            values
                .iter()
                .map(format_runtime_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn format_runtime_row(row: &[RuntimeValue]) -> String {
    format!(
        "({})",
        row.iter()
            .map(format_runtime_value)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn format_indexed_rows(rows: &IndexSet<Vec<RuntimeValue>>, max_rows: usize) -> String {
    if rows.is_empty() {
        return "<none>".to_string();
    }

    let limit = max_rows.max(1);
    let mut rendered = rows
        .iter()
        .take(limit)
        .enumerate()
        .map(|(idx, row)| format!("row {} = {}", idx + 1, format_runtime_row(row)))
        .collect::<Vec<_>>();
    if rows.len() > limit {
        rendered.push(format!("(+{} more rows)", rows.len() - limit));
    }
    rendered.join("; ")
}

fn format_env_bindings(env: &Env) -> String {
    if env.is_empty() {
        return "<none>".to_string();
    }
    env.iter()
        .map(|(name, value)| format!("{name}={}", format_runtime_value(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_arithmetic_bind(bind: &raql_syntax::ArithmeticBindConstraint) -> String {
    format!("{} := {}", bind.target.value, format_expr(&bind.expr))
}

type Env = BTreeMap<String, RuntimeValue>;

fn eval_rule(
    program: &PlannedProgram,
    planned_rule: &PlannedRule,
    context: &mut ExecutionContext,
    relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    host: &mut impl EngineHostView,
) -> Result<Vec<Vec<RuntimeValue>>, RuntimeError> {
    let mut envs = vec![Env::new()];
    for goal_plan in planned_rule.ordered_goals() {
        let goal = planned_rule
            .goal(goal_plan.index())
            .expect("planner invariant: ordered goal index must exist");
        let mut next = Vec::new();
        for env in &envs {
            let mut rows = eval_goal(
                program,
                goal,
                goal_plan.chosen_mode(),
                env,
                planned_rule.var_types(),
                context,
                relations,
                host,
            )?;
            next.append(&mut rows);
        }
        envs = next;
        if envs.is_empty() {
            break;
        }
    }

    let mut out = Vec::new();
    for env in envs {
        let mut row = Vec::new();
        for term in planned_rule.head_terms() {
            row.push(eval_ground_term(term, &env)?);
        }
        out.push(row);
    }
    Ok(out)
}

fn eval_goal(
    program: &PlannedProgram,
    goal: &Spanned<Goal>,
    selected_mode: Option<usize>,
    env: &Env,
    var_types: &BTreeMap<String, CompilerType>,
    context: &mut ExecutionContext,
    relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    host: &mut impl EngineHostView,
) -> Result<Vec<Env>, RuntimeError> {
    match &goal.value {
        Goal::Atom(atom) => {
            if let Some(out) = eval_builtin_atom(atom, env)? {
                return Ok(out);
            }
            let name = atom.name.value.to_string();
            if name == "witness_path" {
                return eval_witness_path_atom(
                    program, goal.span, atom, env, context, relations, host,
                );
            }
            if name == "path_hop" {
                return eval_path_hop_atom(atom, env, context, relations);
            }
            let rel = relations
                .get(&name)
                .ok_or_else(|| RuntimeError::MissingRelation {
                    relation: name.clone(),
                    context: format!(
                        "referenced by goal `{}`{}",
                        format_atom(atom),
                        goal_anchor_suffix(program, goal.span)
                    ),
                })?;
            if is_function_predicate(program, name.as_str()) {
                let constraints = function_input_constraints(program, atom, env, selected_mode)?;
                let cardinality = function_match_count(atom, rel, &constraints);
                if cardinality != 1 {
                    return Err(RuntimeError::FunctionCardinality {
                        predicate: name.clone(),
                        got: cardinality,
                        context: format_function_input_context(atom, &constraints),
                    });
                }
            }
            let mut out = Vec::new();
            for tuple in rel {
                if tuple.len() != atom.terms.len() {
                    continue;
                }
                let mut candidate = env.clone();
                if unify_terms_with_tuple(&atom.terms, tuple, &mut candidate)? {
                    out.push(candidate);
                }
            }
            Ok(out)
        }
        Goal::Not(not) => {
            let atom = &not.atom.value;
            let name = atom.name.value.to_string();
            let rel = relations
                .get(&name)
                .ok_or_else(|| RuntimeError::MissingRelation {
                    relation: name.clone(),
                    context: format!(
                        "referenced by negated goal `not {}`{}",
                        format_atom(atom),
                        goal_anchor_suffix(program, goal.span)
                    ),
                })?;
            let mut exists = false;
            for tuple in rel {
                if tuple.len() != atom.terms.len() {
                    continue;
                }
                let mut candidate = env.clone();
                if unify_terms_with_tuple(&atom.terms, tuple, &mut candidate)? {
                    exists = true;
                    break;
                }
            }
            if exists {
                Ok(Vec::new())
            } else {
                Ok(vec![env.clone()])
            }
        }
        Goal::Constraint(c) => match c {
            Constraint::Relational(r) => match r.op.value {
                RelOp::Eq => {
                    let mut candidate = env.clone();
                    if unify_terms(&r.lhs, &r.rhs, &mut candidate)? {
                        Ok(vec![candidate])
                    } else {
                        Ok(Vec::new())
                    }
                }
                _ => eval_relational_constraint(program, r, env, var_types, host)
                    .map(|ok| if ok { vec![env.clone()] } else { Vec::new() }),
            },
            Constraint::ArithmeticBind(b) => eval_arithmetic_bind_constraint(b, env),
        },
        Goal::Aggregate(a) => eval_aggregate(program, a, env, var_types, context, relations, host),
        Goal::ChooseTopK(c) => {
            eval_choose_topk(program, c, env, var_types, context, relations, host)
        }
        Goal::Disjunction(d) => {
            let mut out = Vec::new();
            for branch in &d.branches {
                let mut branch_envs = vec![env.clone()];
                for g in branch {
                    let mut next = Vec::new();
                    for e in &branch_envs {
                        let mut rows =
                            eval_goal(program, g, None, e, var_types, context, relations, host)?;
                        next.append(&mut rows);
                    }
                    branch_envs = next;
                    if branch_envs.is_empty() {
                        break;
                    }
                }
                out.extend(branch_envs);
            }
            Ok(out)
        }
    }
}

fn eval_builtin_atom(
    atom: &raql_syntax::Atom,
    env: &Env,
) -> Result<Option<Vec<Env>>, RuntimeError> {
    match atom.name.value.as_str() {
        "contains" => {
            if atom.terms.len() != 2 {
                return Ok(Some(Vec::new()));
            }
            let hay = eval_ground_term(&atom.terms[0], env)?;
            let needle = eval_ground_term(&atom.terms[1], env)?;
            let h = match hay {
                RuntimeValue::String(s) => s,
                other => return Err(builtin_type_mismatch("contains", "1", "string", &other)),
            };
            let n = match needle {
                RuntimeValue::String(s) => s,
                other => return Err(builtin_type_mismatch("contains", "2", "string", &other)),
            };
            if h.contains(n.as_str()) {
                Ok(Some(vec![env.clone()]))
            } else {
                Ok(Some(Vec::new()))
            }
        }
        "starts_with" => {
            if atom.terms.len() != 2 {
                return Ok(Some(Vec::new()));
            }
            let value = eval_ground_term(&atom.terms[0], env)?;
            let prefix = eval_ground_term(&atom.terms[1], env)?;
            let s = match value {
                RuntimeValue::String(s) => s,
                other => return Err(builtin_type_mismatch("starts_with", "1", "string", &other)),
            };
            let p = match prefix {
                RuntimeValue::String(s) => s,
                other => return Err(builtin_type_mismatch("starts_with", "2", "string", &other)),
            };
            if s.starts_with(p.as_str()) {
                Ok(Some(vec![env.clone()]))
            } else {
                Ok(Some(Vec::new()))
            }
        }
        "fmt" => {
            if atom.terms.len() != 3 {
                return Ok(Some(Vec::new()));
            }
            let format = eval_ground_term(&atom.terms[0], env)?;
            let args = eval_ground_term(&atom.terms[1], env)?;
            let format = match format {
                RuntimeValue::String(s) => s,
                other => return Err(builtin_type_mismatch("fmt", "1", "string", &other)),
            };
            let args = match args {
                RuntimeValue::List(values) => values,
                other => return Err(builtin_type_mismatch("fmt", "2", "list<string>", &other)),
            };
            let mut string_args = Vec::with_capacity(args.len());
            for (idx, arg) in args.into_iter().enumerate() {
                let s = match arg {
                    RuntimeValue::String(s) => s,
                    other => {
                        let arg_path = format!("2[{}]", idx + 1);
                        return Err(builtin_type_mismatch("fmt", &arg_path, "string", &other));
                    }
                };
                string_args.push(s);
            }
            let out = render_fmt(&format, &string_args);
            let mut bound = env.clone();
            if bind_term_value(&atom.terms[2], &RuntimeValue::String(out), &mut bound)? {
                Ok(Some(vec![bound]))
            } else {
                Ok(Some(Vec::new()))
            }
        }
        "coalesce" => {
            if atom.terms.len() != 3 {
                return Ok(Some(Vec::new()));
            }
            let opt = eval_ground_term(&atom.terms[0], env)?;
            let default = eval_ground_term(&atom.terms[1], env)?;
            let out = match opt {
                RuntimeValue::None => default,
                RuntimeValue::Some(inner) => *inner,
                other => return Err(builtin_type_mismatch("coalesce", "1", "option<_>", &other)),
            };
            let mut bound = env.clone();
            if bind_term_value(&atom.terms[2], &out, &mut bound)? {
                Ok(Some(vec![bound]))
            } else {
                Ok(Some(Vec::new()))
            }
        }
        _ => Ok(None),
    }
}

fn builtin_type_mismatch(
    predicate: &str,
    arg_path: &str,
    expected: &str,
    found: &RuntimeValue,
) -> RuntimeError {
    RuntimeError::TypeMismatchContext {
        context: format!(
            "builtin `{predicate}` argument {arg_path} expected {expected}, found {}",
            stable_type_tag(found, None)
        ),
    }
}

fn render_fmt(format: &str, args: &[String]) -> String {
    let mut out = String::new();
    let mut rest = format;
    let mut idx = 0usize;
    while let Some(pos) = rest.find("{}") {
        out.push_str(&rest[..pos]);
        if let Some(arg) = args.get(idx) {
            out.push_str(arg);
        } else {
            out.push_str("{}");
        }
        idx += 1;
        rest = &rest[pos + 2..];
    }
    out.push_str(rest);
    out
}

fn compiler_type_tag(ty: &CompilerType) -> String {
    match ty {
        CompilerType::Int => "int".to_string(),
        CompilerType::String => "string".to_string(),
        CompilerType::Bool => "bool".to_string(),
        CompilerType::Named(name) => name.clone(),
        CompilerType::Option(inner) => format!("option<{}>", compiler_type_tag(inner)),
        CompilerType::List(inner) => format!("list<{}>", compiler_type_tag(inner)),
        CompilerType::Var(_) => "?".to_string(),
    }
}

fn type_ast_tag(ty: &TypeAst) -> String {
    match ty {
        TypeAst::Int => "int".to_string(),
        TypeAst::String => "string".to_string(),
        TypeAst::Bool => "bool".to_string(),
        TypeAst::Named(name) => name.to_string(),
        TypeAst::Option(inner) => format!("option<{}>", type_ast_tag(&inner.value)),
        TypeAst::List(inner) => format!("list<{}>", type_ast_tag(&inner.value)),
    }
}

fn term_type_tag(
    term: &Spanned<Term>,
    var_types: &BTreeMap<String, CompilerType>,
) -> Option<String> {
    match &term.value {
        Term::Var(name) => var_types.get(name.as_str()).map(compiler_type_tag),
        Term::Wildcard => None,
        Term::Int(_) => Some("int".to_string()),
        Term::String(_) => Some("string".to_string()),
        Term::Bool(_) => Some("bool".to_string()),
        Term::EnumAtom { enum_name, .. } => Some(enum_name.value.to_string()),
        Term::None { turbofish } => turbofish
            .as_ref()
            .map(|inner| format!("option<{}>", type_ast_tag(&inner.value))),
        Term::Some(inner) => {
            let inner_tag = term_type_tag(inner, var_types)?;
            Some(format!("option<{inner_tag}>"))
        }
        Term::List { items, turbofish } => {
            if let Some(inner) = turbofish {
                return Some(format!("list<{}>", type_ast_tag(&inner.value)));
            }
            let first = items.first()?;
            let inner_tag = term_type_tag(first, var_types)?;
            Some(format!("list<{inner_tag}>"))
        }
    }
}

fn generic_inner_type_tag<'a>(tag: &'a str, prefix: &str) -> Option<&'a str> {
    tag.strip_prefix(prefix)?.strip_suffix('>')
}

fn option_inner_type_tag(tag: &str) -> Option<&str> {
    generic_inner_type_tag(tag, "option<")
}

fn list_inner_type_tag(tag: &str) -> Option<&str> {
    generic_inner_type_tag(tag, "list<")
}

fn stable_type_tag(value: &RuntimeValue, explicit_type_tag: Option<&str>) -> String {
    if let Some(tag) = explicit_type_tag {
        return tag.to_string();
    }
    match value {
        RuntimeValue::Int(_) => "int".to_string(),
        RuntimeValue::String(_) => "string".to_string(),
        RuntimeValue::Bool(_) => "bool".to_string(),
        RuntimeValue::Enum { name, .. } => name.clone(),
        RuntimeValue::Host { kind, .. } => kind.type_tag().to_string(),
        RuntimeValue::None => "option<?>".to_string(),
        RuntimeValue::Some(inner) => format!("option<{}>", stable_type_tag(inner, None)),
        RuntimeValue::List(values) => {
            if let Some(first) = values.first() {
                format!("list<{}>", stable_type_tag(first, None))
            } else {
                "list<?>".to_string()
            }
        }
    }
}

fn stable_cmp_enum_variants(
    program: &PlannedProgram,
    enum_name: &str,
    lhs_variant: &str,
    rhs_variant: &str,
) -> CmpOrdering {
    let lhs_ord = program.enum_decl(enum_name).and_then(|decl| {
        decl.variants()
            .iter()
            .position(|variant| variant == lhs_variant)
    });
    let rhs_ord = program.enum_decl(enum_name).and_then(|decl| {
        decl.variants()
            .iter()
            .position(|variant| variant == rhs_variant)
    });
    match (lhs_ord, rhs_ord) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => lhs_variant.cmp(rhs_variant),
    }
}

fn stable_cmp_list(
    program: &PlannedProgram,
    host: &mut impl EngineHostView,
    lhs: &[RuntimeValue],
    rhs: &[RuntimeValue],
    lhs_item_type_tag: Option<&str>,
    rhs_item_type_tag: Option<&str>,
) -> CmpOrdering {
    for (a, b) in lhs.iter().zip(rhs) {
        let cmp = stable_cmp(program, host, a, b, lhs_item_type_tag, rhs_item_type_tag);
        if !cmp.is_eq() {
            return cmp;
        }
    }
    lhs.len().cmp(&rhs.len())
}

fn stable_cmp(
    program: &PlannedProgram,
    host: &mut impl EngineHostView,
    lhs: &RuntimeValue,
    rhs: &RuntimeValue,
    lhs_type_tag: Option<&str>,
    rhs_type_tag: Option<&str>,
) -> CmpOrdering {
    let type_cmp = stable_type_tag(lhs, lhs_type_tag).cmp(&stable_type_tag(rhs, rhs_type_tag));
    if !type_cmp.is_eq() {
        return type_cmp;
    }

    match (lhs, rhs) {
        (RuntimeValue::Int(a), RuntimeValue::Int(b)) => a.cmp(b),
        (RuntimeValue::String(a), RuntimeValue::String(b)) => a.cmp(b),
        (RuntimeValue::Bool(a), RuntimeValue::Bool(b)) => a.cmp(b),
        (
            RuntimeValue::Enum {
                name: lhs_name,
                variant: lhs_variant,
            },
            RuntimeValue::Enum {
                name: rhs_name,
                variant: rhs_variant,
            },
        ) => {
            let name_cmp = lhs_name.cmp(rhs_name);
            if !name_cmp.is_eq() {
                return name_cmp;
            }
            stable_cmp_enum_variants(program, lhs_name, lhs_variant, rhs_variant)
        }
        (RuntimeValue::Host { .. }, RuntimeValue::Host { .. }) => {
            host.stable_key(lhs).cmp(&host.stable_key(rhs))
        }
        (RuntimeValue::None, RuntimeValue::None) => CmpOrdering::Equal,
        (RuntimeValue::None, RuntimeValue::Some(_)) => CmpOrdering::Less,
        (RuntimeValue::Some(_), RuntimeValue::None) => CmpOrdering::Greater,
        (RuntimeValue::Some(a), RuntimeValue::Some(b)) => stable_cmp(
            program,
            host,
            a,
            b,
            lhs_type_tag.and_then(option_inner_type_tag),
            rhs_type_tag.and_then(option_inner_type_tag),
        ),
        (RuntimeValue::List(a), RuntimeValue::List(b)) => stable_cmp_list(
            program,
            host,
            a,
            b,
            lhs_type_tag.and_then(list_inner_type_tag),
            rhs_type_tag.and_then(list_inner_type_tag),
        ),
        _ => CmpOrdering::Equal,
    }
}

fn stable_cmp_witness_hops(
    program: &PlannedProgram,
    host: &mut impl EngineHostView,
    from_a: &RuntimeValue,
    to_a: &RuntimeValue,
    kind_a: &RuntimeValue,
    evidence_a: &RuntimeValue,
    from_b: &RuntimeValue,
    to_b: &RuntimeValue,
    kind_b: &RuntimeValue,
    evidence_b: &RuntimeValue,
) -> CmpOrdering {
    stable_cmp(program, host, from_a, from_b, None, None)
        .then_with(|| stable_cmp(program, host, to_a, to_b, None, None))
        .then_with(|| stable_cmp(program, host, kind_a, kind_b, None, None))
        .then_with(|| stable_cmp(program, host, evidence_a, evidence_b, None, None))
}

fn eval_relational_constraint(
    program: &PlannedProgram,
    r: &raql_syntax::RelationalConstraint,
    env: &Env,
    var_types: &BTreeMap<String, CompilerType>,
    host: &mut impl EngineHostView,
) -> Result<bool, RuntimeError> {
    let lhs = eval_ground_term(&r.lhs, env)?;
    let rhs = eval_ground_term(&r.rhs, env)?;
    let lhs_tag = term_type_tag(&r.lhs, var_types);
    let rhs_tag = term_type_tag(&r.rhs, var_types);
    let shared_tag = lhs_tag.as_deref().or(rhs_tag.as_deref());
    let lhs_cmp_tag = lhs_tag.as_deref().or(shared_tag);
    let rhs_cmp_tag = rhs_tag.as_deref().or(shared_tag);
    let ok = match r.op.value {
        RelOp::Eq => lhs == rhs,
        RelOp::NotEq => lhs != rhs,
        RelOp::Lt => stable_cmp(program, host, &lhs, &rhs, lhs_cmp_tag, rhs_cmp_tag).is_lt(),
        RelOp::LtEq => !stable_cmp(program, host, &lhs, &rhs, lhs_cmp_tag, rhs_cmp_tag).is_gt(),
        RelOp::Gt => stable_cmp(program, host, &lhs, &rhs, lhs_cmp_tag, rhs_cmp_tag).is_gt(),
        RelOp::GtEq => !stable_cmp(program, host, &lhs, &rhs, lhs_cmp_tag, rhs_cmp_tag).is_lt(),
    };
    Ok(ok)
}

fn eval_arithmetic_bind_constraint(
    b: &raql_syntax::ArithmeticBindConstraint,
    env: &Env,
) -> Result<Vec<Env>, RuntimeError> {
    let value = eval_int_expr(&b.expr, env)?;
    if let Some(existing) = env.get(b.target.value.as_str()) {
        match existing {
            RuntimeValue::Int(v) if *v == value => Ok(vec![env.clone()]),
            RuntimeValue::Int(_) => Ok(Vec::new()),
            other => Err(RuntimeError::TypeMismatchContext {
                context: format!(
                    "arithmetic bind `{}` expected target `{}` to be `int`, found {} ({}) in row bindings: {}",
                    format_arithmetic_bind(b),
                    b.target.value,
                    stable_type_tag(other, None),
                    format_runtime_value(other),
                    format_env_bindings(env)
                ),
            }),
        }
    } else {
        let mut next = env.clone();
        next.insert(b.target.value.to_string(), RuntimeValue::Int(value));
        Ok(vec![next])
    }
}

fn eval_aggregate(
    program: &PlannedProgram,
    a: &raql_syntax::AggregateBinder,
    env: &Env,
    var_types: &BTreeMap<String, CompilerType>,
    context: &mut ExecutionContext,
    relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    host: &mut impl EngineHostView,
) -> Result<Vec<Env>, RuntimeError> {
    let mut rows = vec![env.clone()];
    for goal in &a.goals {
        let mut next = Vec::new();
        for e in &rows {
            let mut out = eval_goal(program, goal, None, e, var_types, context, relations, host)?;
            next.append(&mut out);
        }
        rows = next;
    }

    let row_set = rows.into_iter().collect::<BTreeSet<_>>();

    let mut vals = IndexSet::<RuntimeValue>::new();
    if let Some(var) = &a.projection_var {
        for r in &row_set {
            if let Some(v) = r.get(var.value.as_str()) {
                vals.insert(v.clone());
            }
        }
    }
    let projection_tag = a
        .projection_var
        .as_ref()
        .and_then(|var| var_types.get(var.value.as_str()))
        .map(compiler_type_tag);

    let out = match a.name.value {
        raql_syntax::AggregateName::Count | raql_syntax::AggregateName::CountDistinct => {
            if a.projection_var.is_some() {
                RuntimeValue::Int(vals.len() as i64)
            } else {
                RuntimeValue::Int(row_set.len() as i64)
            }
        }
        raql_syntax::AggregateName::Sum => {
            let mut acc: i64 = 0;
            for v in &vals {
                let RuntimeValue::Int(i) = v else {
                    let row_bindings = a
                        .projection_var
                        .as_ref()
                        .and_then(|var| {
                            row_set
                                .iter()
                                .find(|row| row.get(var.value.as_str()).is_some_and(|x| x == v))
                        })
                        .map(format_env_bindings)
                        .unwrap_or_else(|| "<none>".to_string());
                    return Err(RuntimeError::TypeMismatchContext {
                        context: format!(
                            "aggregate `sum` for output `{}` expected projected values from `{}` to be `int`, found {} ({}) in row bindings: {}",
                            a.out.value,
                            a.projection_var
                                .as_ref()
                                .map(|v| v.value.as_str())
                                .unwrap_or("<row>"),
                            stable_type_tag(v, None),
                            format_runtime_value(v),
                            row_bindings
                        ),
                    });
                };
                acc = acc.checked_add(*i).ok_or(RuntimeError::Overflow)?;
            }
            RuntimeValue::Int(acc)
        }
        raql_syntax::AggregateName::Min => {
            let Some(mut best) = vals.iter().next().cloned() else {
                return Ok(Vec::new());
            };
            for value in vals.iter().skip(1) {
                if stable_cmp(
                    program,
                    host,
                    value,
                    &best,
                    projection_tag.as_deref(),
                    projection_tag.as_deref(),
                )
                .is_lt()
                {
                    best = value.clone();
                }
            }
            best
        }
        raql_syntax::AggregateName::Max => {
            let Some(mut best) = vals.iter().next().cloned() else {
                return Ok(Vec::new());
            };
            for value in vals.iter().skip(1) {
                if stable_cmp(
                    program,
                    host,
                    value,
                    &best,
                    projection_tag.as_deref(),
                    projection_tag.as_deref(),
                )
                .is_gt()
                {
                    best = value.clone();
                }
            }
            best
        }
    };

    let mut next = env.clone();
    next.insert(a.out.value.to_string(), out);
    Ok(vec![next])
}

fn eval_choose_topk(
    program: &PlannedProgram,
    c: &raql_syntax::ChooseTopkBinder,
    env: &Env,
    var_types: &BTreeMap<String, CompilerType>,
    context: &mut ExecutionContext,
    relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    host: &mut impl EngineHostView,
) -> Result<Vec<Env>, RuntimeError> {
    let k_value = eval_ground_term(&c.k, env)?;
    let k = match k_value {
        RuntimeValue::Int(i) => {
            if i <= 0 {
                return Err(RuntimeError::TypeMismatchContext {
                    context: format!(
                        "choose_topk `{}` expected `k` term `{}` to evaluate to a positive `int` (> 0), found int ({i}) in row bindings: {}",
                        c.tag.value,
                        format_term(&c.k),
                        format_env_bindings(env)
                    ),
                });
            }
            usize::try_from(i).map_err(|_| RuntimeError::TypeMismatchContext {
                context: format!(
                    "choose_topk `{}` expected `k` term `{}` to fit platform `usize` after evaluating to a positive `int`, found int ({i}) in row bindings: {}",
                    c.tag.value,
                    format_term(&c.k),
                    format_env_bindings(env)
                ),
            })?
        }
        other => {
            return Err(RuntimeError::TypeMismatchContext {
                context: format!(
                    "choose_topk `{}` expected `k` term `{}` to evaluate to `int`, found {} ({}) in row bindings: {}",
                    c.tag.value,
                    format_term(&c.k),
                    stable_type_tag(&other, None),
                    format_runtime_value(&other),
                    format_env_bindings(env)
                ),
            });
        }
    };
    let group = eval_ground_term(&c.group, env)?;

    let mut rows = vec![env.clone()];
    for goal in &c.goals {
        let mut next = Vec::new();
        for e in &rows {
            let mut out = eval_goal(program, goal, None, e, var_types, context, relations, host)?;
            next.append(&mut out);
        }
        rows = next;
    }

    let mut candidates = IndexSet::<(RuntimeValue, RuntimeValue)>::new();
    for row in rows {
        if let (Some(score), Some(item)) = (
            row.get(c.score_var.value.as_str()),
            row.get(c.item_var.value.as_str()),
        ) {
            candidates.insert((score.clone(), item.clone()));
        }
    }

    let mut sorted = candidates.into_iter().collect::<Vec<_>>();
    let score_tag = var_types
        .get(c.score_var.value.as_str())
        .map(compiler_type_tag)
        .unwrap_or_else(|| "int".to_string());
    let item_tag = var_types
        .get(c.item_var.value.as_str())
        .map(compiler_type_tag);
    sorted.sort_by(|(score_a, item_a), (score_b, item_b)| {
        let score_cmp = stable_cmp(
            program,
            host,
            score_b,
            score_a,
            Some(score_tag.as_str()),
            Some(score_tag.as_str()),
        );
        if score_cmp.is_eq() {
            let item_cmp = stable_cmp(
                program,
                host,
                item_a,
                item_b,
                item_tag.as_deref(),
                item_tag.as_deref(),
            );
            if item_cmp.is_eq() {
                stable_cmp(
                    program,
                    host,
                    score_a,
                    score_b,
                    Some(score_tag.as_str()),
                    Some(score_tag.as_str()),
                )
            } else {
                item_cmp
            }
        } else {
            score_cmp
        }
    });

    let mut out = Vec::new();
    for (score, item) in sorted.into_iter().take(k) {
        let mut e = env.clone();
        e.insert(c.score_var.value.to_string(), score);
        e.insert(c.item_var.value.to_string(), item);
        e.insert("__group".to_string(), group.clone());
        out.push(e);
    }
    Ok(out)
}

fn eval_witness_path_atom(
    program: &PlannedProgram,
    goal_span: SrcSpan,
    atom: &raql_syntax::Atom,
    env: &Env,
    context: &mut ExecutionContext,
    relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    host: &mut impl EngineHostView,
) -> Result<Vec<Env>, RuntimeError> {
    if atom.terms.len() != 4 {
        return Err(RuntimeError::WitnessArity {
            predicate: "witness_path".to_string(),
            expected: 4,
            got: atom.terms.len(),
        });
    }
    let graph = eval_ground_term(&atom.terms[0], env)?;
    let from = eval_ground_term(&atom.terms[1], env)?;
    let to = eval_ground_term(&atom.terms[2], env)?;

    let max_depth_raw = scalar_input(relations, "path_max_depth", 8)?;
    if max_depth_raw < 0 {
        return Err(RuntimeError::TypeMismatchContext {
            context: format!(
                "input `path_max_depth` expected `int >= 0` for `witness_path`, found int ({max_depth_raw}); set `path_max_depth` to 0 or greater"
            ),
        });
    }
    let max_depth = usize::try_from(max_depth_raw).map_err(|_| RuntimeError::TypeMismatchContext {
        context: format!(
            "input `path_max_depth` expected `int >= 0` for `witness_path`, found int ({max_depth_raw}) which exceeds platform `usize`; lower `path_max_depth`"
        ),
    })?;
    let path_limit_raw = scalar_input(relations, "path_limit", 1)?;
    if path_limit_raw <= 0 {
        return Err(RuntimeError::TypeMismatchContext {
            context: format!(
                "input `path_limit` expected `int > 0` for `witness_path`, found int ({path_limit_raw}); set `path_limit` to 1 or greater"
            ),
        });
    }
    let path_limit =
        usize::try_from(path_limit_raw).map_err(|_| RuntimeError::TypeMismatchContext {
            context: format!(
                "input `path_limit` expected `int > 0` for `witness_path`, found int ({path_limit_raw}) which exceeds platform `usize`; lower `path_limit`"
            ),
        })?;
    let edges = relations
        .get("graph_edge")
        .ok_or_else(|| RuntimeError::MissingRelation {
            relation: "graph_edge".to_string(),
            context: format!(
                "referenced by builtin `witness_path` call `{}`{}",
                format_atom(atom),
                goal_anchor_suffix(program, goal_span)
            ),
        })?;

    let mut adjacency =
        BTreeMap::<RuntimeValue, Vec<(RuntimeValue, RuntimeValue, RuntimeValue)>>::new();
    for edge in edges {
        if edge.len() != 5 {
            continue;
        }
        if edge[0] != graph {
            continue;
        }
        adjacency.entry(edge[1].clone()).or_default().push((
            edge[2].clone(),
            edge[3].clone(),
            edge[4].clone(),
        ));
    }
    for (from_node, hops) in adjacency.iter_mut() {
        hops.sort_by(|(to_a, k_a, e_a), (to_b, k_b, e_b)| {
            stable_cmp_witness_hops(
                program, host, from_node, to_a, k_a, e_a, from_node, to_b, k_b, e_b,
            )
        });
    }

    let mut queue = vec![(
        from.clone(),
        vec![from.clone()],
        Vec::<(RuntimeValue, RuntimeValue, RuntimeValue, RuntimeValue)>::new(),
    )];
    let mut found = Vec::<Vec<(RuntimeValue, RuntimeValue, RuntimeValue, RuntimeValue)>>::new();
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
                let mut v2 = visited.clone();
                v2.push(next.clone());
                let mut h2 = hops.clone();
                h2.push((node.clone(), next.clone(), kind.clone(), evidence.clone()));
                queue.insert(0, (next.clone(), v2, h2));
            }
        }
    }

    // Persist the found paths for `path_hop` calls within this execution.
    let mut out = Vec::new();
    for hops in found {
        let path_id = RuntimeValue::String(context.alloc_path_id());
        let mut e = env.clone();
        if bind_term_value(&atom.terms[3], &path_id, &mut e)? {
            out.push(e.clone());
            let mut rows = Vec::new();
            for (seq, (from_h, to_h, kind_h, evidence_h)) in hops.into_iter().enumerate() {
                rows.push(vec![
                    path_id.clone(),
                    RuntimeValue::Int(seq as i64),
                    from_h,
                    to_h,
                    kind_h,
                    evidence_h,
                ]);
            }
            if let RuntimeValue::String(id) = path_id {
                context.path_hop_cache.insert(id, rows);
            }
        }
    }
    Ok(out)
}

fn eval_path_hop_atom(
    atom: &raql_syntax::Atom,
    env: &Env,
    context: &ExecutionContext,
    _relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
) -> Result<Vec<Env>, RuntimeError> {
    if atom.terms.len() != 6 {
        return Err(RuntimeError::WitnessArity {
            predicate: "path_hop".to_string(),
            expected: 6,
            got: atom.terms.len(),
        });
    }
    let mut out = Vec::new();
    for (path_id, rows) in &context.path_hop_cache {
        for row in rows {
            if row.len() != 6 {
                return Err(RuntimeError::WitnessPathHopRowArity {
                    path_id: path_id.clone(),
                    expected: 6,
                    got: row.len(),
                });
            }
            let mut e = env.clone();
            let mut ok = true;
            for (t, v) in atom.terms.iter().zip(row) {
                if !bind_term_value(t, v, &mut e)? {
                    ok = false;
                    break;
                }
            }
            if ok {
                out.push(e);
            }
        }
    }
    Ok(out)
}

fn scalar_input(
    relations: &BTreeMap<String, IndexSet<Vec<RuntimeValue>>>,
    name: &str,
    default: i64,
) -> Result<i64, RuntimeError> {
    let Some(values) = relations.get(name) else {
        return Ok(default);
    };
    let Some(first) = values.first() else {
        return Ok(default);
    };
    if values.len() != 1 {
        return Err(RuntimeError::FunctionCardinality {
            predicate: name.to_string(),
            got: values.len(),
            context: format!(
                "input `{name}` expected exactly one scalar row, found {} rows ({})",
                values.len(),
                format_indexed_rows(values, 3)
            ),
        });
    }
    if first.len() != 1 {
        return Err(RuntimeError::TypeMismatchContext {
            context: format!(
                "input `{name}` row 1 expected arity 1 with `int`, found arity {} in row 1 = {}",
                first.len(),
                format_runtime_row(first)
            ),
        });
    }
    match first.first() {
        Some(RuntimeValue::Int(i)) => Ok(*i),
        Some(other) => Err(RuntimeError::TypeMismatchContext {
            context: format!(
                "input `{name}` row 1 expected `int`, found {} ({}) in row 1 = {}",
                stable_type_tag(other, None),
                format_runtime_value(other),
                format_runtime_row(first)
            ),
        }),
        None => unreachable!("checked arity == 1"),
    }
}

fn unify_terms_with_tuple(
    terms: &[Spanned<Term>],
    tuple: &[RuntimeValue],
    env: &mut Env,
) -> Result<bool, RuntimeError> {
    for (term, value) in terms.iter().zip(tuple) {
        if !bind_term_value(term, value, env)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn unify_terms(
    lhs: &Spanned<Term>,
    rhs: &Spanned<Term>,
    env: &mut Env,
) -> Result<bool, RuntimeError> {
    match (&lhs.value, &rhs.value) {
        (Term::Wildcard, _) | (_, Term::Wildcard) => Ok(true),
        (Term::Var(v), _) => bind_var_to_term(v.as_str(), rhs, env),
        (_, Term::Var(v)) => bind_var_to_term(v.as_str(), lhs, env),
        (Term::Int(a), Term::Int(b)) => Ok(a == b),
        (Term::String(a), Term::String(b)) => Ok(a == b),
        (Term::Bool(a), Term::Bool(b)) => Ok(a == b),
        (
            Term::EnumAtom {
                enum_name: a_enum,
                variant_name: a_variant,
            },
            Term::EnumAtom {
                enum_name: b_enum,
                variant_name: b_variant,
            },
        ) => Ok(a_enum.value == b_enum.value && a_variant.value == b_variant.value),
        (Term::None { .. }, Term::None { .. }) => Ok(true),
        (Term::Some(a), Term::Some(b)) => unify_terms(a, b, env),
        (Term::List { items: a, .. }, Term::List { items: b, .. }) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for (a_term, b_term) in a.iter().zip(b) {
                if !unify_terms(a_term, b_term, env)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn bind_var_to_term(var: &str, term: &Spanned<Term>, env: &mut Env) -> Result<bool, RuntimeError> {
    if let Some(existing) = env.get(var).cloned() {
        return bind_term_value(term, &existing, env);
    }

    match &term.value {
        Term::Wildcard => Ok(true),
        Term::Var(other) => {
            if var == other.as_str() {
                Ok(true)
            } else if let Some(existing) = env.get(other.as_str()).cloned() {
                env.insert(var.to_string(), existing);
                Ok(true)
            } else {
                Ok(false)
            }
        }
        _ => match eval_ground_term(term, env) {
            Ok(value) => {
                env.insert(var.to_string(), value);
                Ok(true)
            }
            Err(RuntimeError::UnboundVar(_)) => Ok(false),
            Err(err) => Err(err),
        },
    }
}

fn bind_term_value(
    term: &Spanned<Term>,
    value: &RuntimeValue,
    env: &mut Env,
) -> Result<bool, RuntimeError> {
    match &term.value {
        Term::Var(v) => {
            if let Some(existing) = env.get(v.as_str()) {
                Ok(existing == value)
            } else {
                env.insert(v.to_string(), value.clone());
                Ok(true)
            }
        }
        Term::Wildcard => Ok(true),
        Term::Int(i) => Ok(matches!(value, RuntimeValue::Int(v) if v == i)),
        Term::String(s) => Ok(matches!(value, RuntimeValue::String(v) if v == s)),
        Term::Bool(b) => Ok(matches!(value, RuntimeValue::Bool(v) if v == b)),
        Term::EnumAtom {
            enum_name,
            variant_name,
        } => Ok(matches!(
            value,
            RuntimeValue::Enum { name, variant }
                if name == enum_name.value.as_str() && variant == variant_name.value.as_str()
        )),
        Term::None { .. } => Ok(matches!(value, RuntimeValue::None)),
        Term::Some(inner) => {
            let RuntimeValue::Some(inner_value) = value else {
                return Ok(false);
            };
            bind_term_value(inner, inner_value.as_ref(), env)
        }
        Term::List { items, .. } => {
            let RuntimeValue::List(values) = value else {
                return Ok(false);
            };
            if items.len() != values.len() {
                return Ok(false);
            }
            for (item, value) in items.iter().zip(values) {
                if !bind_term_value(item, value, env)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

fn term_to_ground_value(term: &Spanned<Term>) -> Option<RuntimeValue> {
    match &term.value {
        Term::Int(i) => Some(RuntimeValue::Int(*i)),
        Term::String(s) => Some(RuntimeValue::String(s.to_string())),
        Term::Bool(b) => Some(RuntimeValue::Bool(*b)),
        Term::EnumAtom {
            enum_name,
            variant_name,
        } => Some(RuntimeValue::Enum {
            name: enum_name.value.to_string(),
            variant: variant_name.value.to_string(),
        }),
        Term::None { .. } => Some(RuntimeValue::None),
        Term::Some(inner) => term_to_ground_value(inner).map(|v| RuntimeValue::Some(Box::new(v))),
        Term::List { items, .. } => {
            let mut out = Vec::new();
            for item in items {
                out.push(term_to_ground_value(item)?);
            }
            Some(RuntimeValue::List(out))
        }
        Term::Var(_) | Term::Wildcard => None,
    }
}

fn eval_ground_term(term: &Spanned<Term>, env: &Env) -> Result<RuntimeValue, RuntimeError> {
    match &term.value {
        Term::Var(v) => env
            .get(v.as_str())
            .cloned()
            .ok_or_else(|| RuntimeError::UnboundVar(v.to_string())),
        Term::Wildcard => Err(RuntimeError::TypeMismatchContext {
            context: format!(
                "wildcard `_` cannot be evaluated as a ground term; bind it through a predicate output before using it in an expression (row bindings: {})",
                format_env_bindings(env)
            ),
        }),
        Term::Int(i) => Ok(RuntimeValue::Int(*i)),
        Term::String(s) => Ok(RuntimeValue::String(s.to_string())),
        Term::Bool(b) => Ok(RuntimeValue::Bool(*b)),
        Term::EnumAtom {
            enum_name,
            variant_name,
        } => Ok(RuntimeValue::Enum {
            name: enum_name.value.to_string(),
            variant: variant_name.value.to_string(),
        }),
        Term::None { .. } => Ok(RuntimeValue::None),
        Term::Some(inner) => Ok(RuntimeValue::Some(Box::new(eval_ground_term(inner, env)?))),
        Term::List { items, .. } => {
            let mut vals = Vec::new();
            for i in items {
                vals.push(eval_ground_term(i, env)?);
            }
            Ok(RuntimeValue::List(vals))
        }
    }
}

fn eval_int_expr(expr: &Spanned<Expr>, env: &Env) -> Result<i64, RuntimeError> {
    match &expr.value {
        Expr::Term(t) => match eval_ground_term(t, env)? {
            RuntimeValue::Int(i) => Ok(i),
            other => Err(RuntimeError::TypeMismatchContext {
                context: format!(
                    "arithmetic expression `{}` expected term `{}` to be `int`, found {} ({}) in row bindings: {}",
                    format_expr(expr),
                    format_term(t),
                    stable_type_tag(&other, None),
                    format_runtime_value(&other),
                    format_env_bindings(env)
                ),
            }),
        },
        Expr::UnaryNeg(inner) => eval_int_expr(inner, env)?
            .checked_neg()
            .ok_or(RuntimeError::Overflow),
        Expr::Binary { op, lhs, rhs } => {
            let l = eval_int_expr(lhs, env)?;
            let r = eval_int_expr(rhs, env)?;
            match op.value {
                raql_syntax::ArithOp::Add => l.checked_add(r).ok_or(RuntimeError::Overflow),
                raql_syntax::ArithOp::Sub => l.checked_sub(r).ok_or(RuntimeError::Overflow),
                raql_syntax::ArithOp::Mul => l.checked_mul(r).ok_or(RuntimeError::Overflow),
                raql_syntax::ArithOp::Div => {
                    if r == 0 {
                        Err(RuntimeError::DivisionByZero)
                    } else {
                        l.checked_div(r).ok_or(RuntimeError::Overflow)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
