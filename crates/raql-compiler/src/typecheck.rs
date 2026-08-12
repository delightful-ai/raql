//! Type checking: unification, schema inference, and the reserved-contract
//! validators.

use std::collections::BTreeMap;

use raql_syntax::{
    AggregateName, Constraint, DeclarationKind, Expr, Goal, RelOp, Rule, Spanned, SrcSpan, Term,
};

use crate::diagnostics::{
    CompilerDiagnostic, DiagBundle, enrich_include_stack_context, format_span_brief,
};
use crate::plan::goals_mention_var;
use crate::program::{
    CompilerType, EnumDecl, PredicateDecl, PredicateSchemaOrigin, PredicateUsage,
    PredicateUsageKind, ResolvedProgram, TypedProgram, TypedRule, format_mode_signature,
    format_type_name, parse_type_ast,
};

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
