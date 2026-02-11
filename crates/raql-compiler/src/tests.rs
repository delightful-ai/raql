use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use camino::Utf8PathBuf;
use raql_syntax::{Directive, Goal, Stmt, parse_program, parse_program_from_file};

use crate::{CompilerType, plan, resolve, typecheck};

fn has_code(diags: &[crate::CompilerDiagnostic], code: &str) -> bool {
    diags.iter().any(|d| d.code_str() == code)
}

fn write_include_chain_fixture(
    leaf_source: &str,
) -> (Utf8PathBuf, [Utf8PathBuf; 3], std::path::PathBuf) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be monotonic")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("raql-include-stack-{unique}"));
    fs::create_dir_all(&temp_dir).expect("create test temp dir");
    let root = temp_dir.join("root.raql");
    let middle = temp_dir.join("middle.raql");
    let leaf = temp_dir.join("leaf.raql");
    fs::write(&root, ".include \"middle.raql\".\n").expect("write root");
    fs::write(&middle, ".include \"leaf.raql\".\n").expect("write middle");
    fs::write(&leaf, leaf_source).expect("write leaf");

    let expected_root =
        Utf8PathBuf::from_path_buf(fs::canonicalize(&root).expect("canonical root"))
            .expect("utf-8 root canonical path");
    let expected_middle =
        Utf8PathBuf::from_path_buf(fs::canonicalize(&middle).expect("canonical middle"))
            .expect("utf-8 middle canonical path");
    let expected_leaf =
        Utf8PathBuf::from_path_buf(fs::canonicalize(&leaf).expect("canonical leaf"))
            .expect("utf-8 leaf canonical path");
    let root_utf8 = Utf8PathBuf::from_path_buf(root).expect("utf-8 root path");
    (
        root_utf8,
        [expected_root, expected_middle, expected_leaf],
        temp_dir,
    )
}

#[test]
fn compiler_diagnostic_carries_include_stack_for_included_file_error() {
    let (root_utf8, expected_stack, temp_dir) = write_include_chain_fixture("w([]).\n");
    let parsed = parse_program_from_file(root_utf8.as_path(), &[]).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0202")
        .expect("expected ambiguous empty-list diagnostic");

    assert_eq!(diag.include_stack(), expected_stack.as_slice());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn resolve_diagnostic_carries_include_stack_for_included_file_error() {
    let (root_utf8, expected_stack, temp_dir) =
        write_include_chain_fixture(".decl dup(X: int).\n.decl dup(X: int).\n");
    let parsed = parse_program_from_file(root_utf8.as_path(), &[]).expect("parse");
    let err = resolve(parsed).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0101")
        .expect("expected duplicate declaration diagnostic");

    assert_eq!(diag.include_stack(), expected_stack.as_slice());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn plan_diagnostic_carries_include_stack_for_included_file_error() {
    let (root_utf8, expected_stack, temp_dir) = write_include_chain_fixture(
        ".decl edge(A: int, B: int) input.\n.mode edge(+int, -int).\n.decl blocked(B: int).\nblocked(B) :- edge(A, B).\n",
    );
    let parsed = parse_program_from_file(root_utf8.as_path(), &[]).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0301")
        .expect("expected planner stuck diagnostic");

    assert_eq!(diag.include_stack(), expected_stack.as_slice());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn pipeline_handles_simple_program() {
    let src = r#"
.decl edge(A: int, B: int) input.
.decl reach(A: int, B: int).
reach(A, B) :- edge(A, B).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let planned = plan(typed).expect("plan");
    assert_eq!(planned.planned_rules().len(), 1);
}

#[test]
fn rejects_variable_fact() {
    let src = r#"
.decl p(X: int) input.
p(X).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(err.iter().any(|d| d.code_str() == "RAQL0402"));
}

#[test]
fn catches_ambiguous_none() {
    let src = r#"
w(none).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0201")
        .expect("must contain RAQL0201");
    assert!(diag.message.contains("`none`"));
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|h| h.contains("option type is already known"))
    );
}

#[test]
fn catches_ambiguous_empty_list() {
    let src = r#"
w([]).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0202")
        .expect("must contain RAQL0202");
    assert!(diag.message.contains("`[]`"));
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|h| h.contains("element type is already known"))
    );
}

#[test]
fn unknown_body_only_predicate_reports_raql0100() {
    let src = r#"
.decl seed(X: int) input.
.decl out(X: int).
out(X) :- missing(X), seed(X).
"#;
    let parsed_for_spans = parse_program(src).expect("parse");
    let missing_span = match &parsed_for_spans.phase().statements[2].value {
        Stmt::Rule(rule) => match &rule.body[0].value {
            Goal::Atom(atom) => atom.name.span,
            _ => panic!("expected first goal to be an atom"),
        },
        _ => panic!("expected rule"),
    };

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0100" && d.span() == Some(missing_span))
        .expect("must contain usage-anchored RAQL0100 for unknown body predicate");
    assert!(diag.message.contains("unknown predicate `missing`"));
    assert!(diag.message.contains("rule body atom"));
}

#[test]
fn infers_schema_for_undeclared_rule_head_predicate() {
    let src = r#"
.decl seed(X: int) input.
derived(X) :- seed(X).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck should infer derived predicate");
    let derived = typed
        .predicates
        .get("derived")
        .expect("derived predicate should exist");
    assert!(derived.inferred);
    assert_eq!(derived.args, vec![CompilerType::Int]);
}

#[test]
fn reports_unknown_mode_predicate_with_exact_code() {
    let src = r#"
.mode missing(+int).
.decl p(X: int) input.
p(1).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0103"));
}

#[test]
fn mode_arity_mismatch_points_to_mode_span_with_contextual_message() {
    let src = r#"
.decl edge(A: int, B: int) input.
.mode edge(+int).
"#;
    let parsed_for_spans = parse_program(src).expect("parse");
    let decl_span = match &parsed_for_spans.phase().statements[0].value {
        Stmt::Declaration(decl) => decl.name.span,
        _ => panic!("expected declaration"),
    };
    let mode_span = match &parsed_for_spans.phase().statements[1].value {
        Stmt::Directive(Directive::Mode(mode)) => mode.predicate.span,
        _ => panic!("expected mode directive"),
    };

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0204")
        .expect("must contain RAQL0204");

    assert_eq!(diag.span(), Some(mode_span));
    assert!(diag.message.contains("mode `(+int)`"));
    assert!(diag.message.contains("expects 2"));
    assert!(diag.help.as_deref().is_some_and(|h| {
        h.contains(&super::format_span_brief(
            parsed_for_spans.sources(),
            decl_span,
        ))
    }));
}

#[test]
fn mode_type_mismatch_reports_argument_direction_and_types() {
    let src = r#"
.decl edge(A: int, B: int) input.
.mode edge(+string, -int).
"#;
    let parsed_for_spans = parse_program(src).expect("parse");
    let mode_span = match &parsed_for_spans.phase().statements[1].value {
        Stmt::Directive(Directive::Mode(mode)) => mode.predicate.span,
        _ => panic!("expected mode directive"),
    };

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0204")
        .expect("must contain RAQL0204");

    assert_eq!(diag.span(), Some(mode_span));
    assert!(diag.message.contains("argument 1 (+ input)"));
    assert!(diag.message.contains("expects `int`"));
    assert!(diag.message.contains("provides `string`"));
}

#[test]
fn planner_reports_stuck_mode_with_context_and_exact_code() {
    let src = r#"
.decl edge(A: int, B: int) input.
.mode edge(+int, -int).
.decl blocked(B: int).
blocked(B) :- edge(A, B).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail planning");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0301")
        .expect("must contain RAQL0301");
    assert!(diag.message.contains("first blocked goal"));
    assert!(diag.message.contains("`edge(A, B)`"));
    assert!(
        diag.message
            .contains("goal kind `atom`: `edge/2` cannot run")
    );
    assert!(diag.message.contains("mode 1 `(+int, -int)`"));
    assert!(diag.message.contains("missing or ungrounded inputs:"));
    assert!(diag.message.contains("arg 1 `A` (+ input `int`)"));
    assert!(diag.message.contains("missing grounded vars: A"));
    assert!(diag.message.contains("current variable context:"));
    assert!(diag.message.contains("bound vars: none"));
    assert!(diag.message.contains("grounded vars: none"));
    assert!(diag.span().is_some());
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|h| h.contains("to make `edge(A, B)` runnable"))
    );
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|h| h.contains("selected mode: mode 1 `(+int, -int)`"))
    );
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|h| h.contains("bind required inputs earlier or add mode declarations"))
    );
}

#[test]
fn planner_reports_current_bound_and_grounded_context_when_stuck() {
    let src = r#"
.decl src(A: int) input.
.mode src(-int).
.decl edge(A: int, B: int) input.
.mode edge(+int, +int).
.decl blocked(B: int).
blocked(B) :- src(A), edge(A, B).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail planning");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0301")
        .expect("must contain RAQL0301");

    assert!(diag.message.contains("`edge(A, B)`"));
    assert!(diag.message.contains("missing or ungrounded inputs:"));
    assert!(diag.message.contains("arg 2 `B` (+ input `int`)"));
    assert!(diag.message.contains("missing grounded vars: B"));
    assert!(diag.message.contains("current variable context:"));
    assert!(diag.message.contains("bound vars: A"));
    assert!(diag.message.contains("grounded vars: A"));
}

#[test]
fn stratification_rejects_negation_cycle_with_cycle_context_and_exact_code() {
    let src = r#"
.decl seed(X: int) input.
.decl p(X: int).
.decl q(X: int).
p(X) :- seed(X), not q(X).
q(X) :- seed(X), not p(X).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail stratification");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0401")
        .expect("must contain RAQL0401");
    assert!(diag.message.contains("non-stratifiable cycle detected"));
    assert!(diag.message.contains("non-positive edge kind is negative"));
    assert!(diag.message.contains("`p` -negative-> `q`"));
    assert!(diag.message.contains("`q` -negative-> `p`"));
    assert!(diag.span().is_some());
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|h| h.contains("negated goal"))
    );
}

#[test]
fn stratification_cycle_context_uses_per_edge_instance_span() {
    let src = r#"
.decl seed(X: int) input.
.decl p(X: int).
.decl q(X: int).
p(X) :- seed(X), not q(X).
p(X) :- seed(X), not q(X).
q(X) :- seed(X), not p(X).
"#;
    let parsed_for_spans = parse_program(src).expect("parse");
    let mut p_not_q_spans = Vec::new();
    for stmt in &parsed_for_spans.phase().statements {
        if let Stmt::Rule(rule) = &stmt.value {
            if rule.head.value.name.value.as_str() != "p" {
                continue;
            }
            for goal in &rule.body {
                if let Goal::Not(not) = &goal.value {
                    if not.atom.value.name.value.as_str() == "q" {
                        p_not_q_spans.push(goal.span);
                    }
                }
            }
        }
    }
    assert_eq!(p_not_q_spans.len(), 2);
    let second_p_not_q_span = p_not_q_spans[1];

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail stratification");
    let cycle_diags = err
        .iter()
        .filter(|d| {
            d.code_str() == "RAQL0401"
                && d.message.contains("non-stratifiable cycle detected")
                && d.help
                    .as_deref()
                    .is_some_and(|h| h.contains("negated goal `not q`"))
        })
        .collect::<Vec<_>>();

    assert!(
        cycle_diags
            .iter()
            .any(|diag| diag.span() == Some(second_p_not_q_span)),
        "expected one cycle diagnostic to point at the second `not q` goal span"
    );
}

#[test]
fn arity_mismatch_points_to_usage_span_and_references_declaration_span() {
    let src = r#"
.decl p(X: int).
p(1, 2).
"#;
    let parsed_for_spans = parse_program(src).expect("parse");
    let decl_span = match &parsed_for_spans.phase().statements[0].value {
        Stmt::Declaration(decl) => decl.name.span,
        _ => panic!("expected declaration"),
    };
    let usage_span = match &parsed_for_spans.phase().statements[1].value {
        Stmt::Fact(fact) => fact.atom.value.name.span,
        _ => panic!("expected fact"),
    };

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0203" && d.span() == Some(usage_span))
        .expect("must contain usage-anchored RAQL0203");
    let usage_location = super::format_span_brief(parsed_for_spans.sources(), usage_span);
    let decl_location = super::format_span_brief(parsed_for_spans.sources(), decl_span);

    assert_eq!(diag.span(), Some(usage_span));
    assert!(diag.message.contains("used with 2 argument(s)"));
    assert!(diag.message.contains(&usage_location));
    assert!(!diag.message.contains("bytes"));
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|h| h.contains(&decl_location))
    );
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|h| h.contains("1 argument(s)") && h.contains("declaration"))
    );
}

#[test]
fn inferred_schema_arity_mismatch_references_inferred_origin_usage() {
    let src = r#"
.decl p(X: int).
p(1, 2).
"#;
    let parsed = parse_program(src).expect("parse");
    let mut resolved = resolve(parsed).expect("resolve");
    let decl = resolved
        .predicates
        .get_mut("p")
        .expect("declared predicate should exist");
    decl.inferred = true;
    decl.inferred_from = Some(super::PredicateUsageKind::Fact);

    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0203")
        .expect("must contain RAQL0203");
    assert!(diag.message.contains("the fact at"));
    assert!(!diag.message.contains("declaration expects"));
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|h| h.contains("edit the fact at"))
    );
}

#[test]
fn arity_diagnostics_report_multiple_conflicts_without_overwrite_loss() {
    let src = r#"
.decl edge(A: int, B: int) input.
.decl out(A: int).
out(A) :- edge(A), edge(A, 1, 2).
"#;
    let parsed_for_spans = parse_program(src).expect("parse");
    let (first_usage_span, second_usage_span) = match &parsed_for_spans.phase().statements[2].value
    {
        Stmt::Rule(rule) => {
            let first = match &rule.body[0].value {
                Goal::Atom(atom) => atom.name.span,
                _ => panic!("expected first goal atom"),
            };
            let second = match &rule.body[1].value {
                Goal::Atom(atom) => atom.name.span,
                _ => panic!("expected second goal atom"),
            };
            (first, second)
        }
        _ => panic!("expected rule"),
    };

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let arity_diags = err
        .iter()
        .filter(|d| d.code_str() == "RAQL0203")
        .collect::<Vec<_>>();

    assert_eq!(arity_diags.len(), 2);
    assert!(
        arity_diags
            .iter()
            .any(|d| d.span() == Some(first_usage_span))
    );
    assert!(
        arity_diags
            .iter()
            .any(|d| d.span() == Some(second_usage_span))
    );
    assert!(
        arity_diags
            .iter()
            .all(|d| d.message.contains("rule body atom"))
    );
}

#[test]
fn rejects_reserved_out_status_output_declaration() {
    let src = r#"
.decl out_status(Status: string) output.
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0404"));
}

#[test]
fn rejects_reserved_out_status_fact() {
    let src = r#"
.decl out_status(Status: string).
out_status("ok").
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0404"));
}

#[test]
fn rejects_reserved_out_status_rule_head() {
    let src = r#"
.decl seed(X: int) input.
seed(1).
out_status("ok") :- seed(X).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail");
    assert!(has_code(&err, "RAQL0404"));
}

#[test]
fn rejects_invalid_graph_edge_schema() {
    let src = r#"
.decl graph_edge(Graph: int, From: Def, To: Def, EdgeKind: string, Evidence: Span).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0405"));
}

#[test]
fn rejects_invalid_graph_edge_endpoint_or_evidence_types() {
    let src = r#"
.decl graph_edge(Graph: string, From: int, To: int, EdgeKind: string, Evidence: string).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0405"));
}

#[test]
fn accepts_required_graph_edge_schema() {
    let src = r#"
.decl graph_edge(Graph: string, From: Def, To: Def, EdgeKind: string, Evidence: Span).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck should succeed");
    assert!(typed.predicates.contains_key("graph_edge"));
}

#[test]
fn rejects_unrestricted_negation_variable() {
    let src = r#"
.decl q(X: int) input.
.decl p(X: int).
p(X) :- not q(X).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail");
    assert!(has_code(&err, "RAQL0406"));
}

#[test]
fn rejects_unrestricted_non_binding_constraint_variable() {
    let src = r#"
.decl p(X: int).
p(X) :- X != 1.
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail");
    assert!(has_code(&err, "RAQL0406"));
}

#[test]
fn rejects_sum_with_non_int_projection() {
    let src = r#"
.decl s(V: string) input.
.decl p(N: int).
p(N) :- N = sum(V : s(V)).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0200")
        .expect("must contain RAQL0200");
    assert!(diag.message.contains("cannot unify `string` with `int`"));
}

#[test]
fn rejects_min_without_projection_form() {
    let src = r#"
.decl s(V: int) input.
.decl p(M: int).
p(M) :- M = min(s(V)).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0206"));
}

#[test]
fn aggregate_projection_variable_must_appear_in_goals() {
    let src = r#"
.decl src(V: int) input.
.decl out(N: int).
out(N) :- N = count(X : src(V)).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0206"));
}

#[test]
fn choose_topk_score_and_item_must_appear_in_goals() {
    let src = r#"
.decl src(I: int) input.
.decl out(I: int).
out(I) :- choose_topk("t", 1, 1, Score, Item : src(1)), I = Item.
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0206"));
}

#[test]
fn is_orderable_recurses_for_option_and_list() {
    assert!(!CompilerType::Option(Box::new(CompilerType::Var(0))).is_orderable());
    assert!(!CompilerType::List(Box::new(CompilerType::Var(1))).is_orderable());
    assert!(CompilerType::Option(Box::new(CompilerType::Int)).is_orderable());
    assert!(CompilerType::List(Box::new(CompilerType::Named("Def".to_string()))).is_orderable());
}

#[test]
fn relational_order_operator_requires_orderable_type() {
    let src = r#"
.decl src(X: int) input.
.decl bad(X: int).
bad(X) :- src(X), none < none.
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0206"));
}

#[test]
fn choose_topk_item_must_be_orderable() {
    let src = r#"
.decl src(I: int) input.
.decl out(S: int).
out(S) :- choose_topk("tag", 1, 1, S, I : src(_), I = none, S := 1).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    assert!(has_code(&err, "RAQL0206"));
}

#[test]
fn occurs_check_rejects_recursive_unification() {
    let src = r#"
p(X) :- X = some(X).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let err = typecheck(resolved).expect_err("must fail");
    let diag = err
        .iter()
        .find(|d| d.code_str() == "RAQL0205")
        .expect("must contain RAQL0205");
    assert!(diag.message.contains("occurs inside recursive term"));
    assert!(diag.message.contains("variable `_t"));
}

#[test]
fn aggregate_recursion_cycle_is_rejected() {
    let src = r#"
.decl p(N: int).
p(N) :- N = count(p(_)).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail");
    assert!(has_code(&err, "RAQL0401"));
}

#[test]
fn aggregate_is_forbidden_inside_indirect_recursive_scc() {
    let src = r#"
.decl base(V: int) input.
.decl p(X: int).
.decl q(X: int).
p(X) :- q(X).
q(X) :- p(X), N = count(V : base(V)), X = N.
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail");
    assert!(has_code(&err, "RAQL0401"));
}

#[test]
fn choose_topk_is_forbidden_inside_indirect_recursive_scc() {
    let src = r#"
.decl base(I: int) input.
.decl p(X: int).
.decl q(X: int).
p(X) :- q(X).
q(X) :- p(X), choose_topk("tag", 1, 1, S, I : base(I), S := I), X = I.
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail");
    assert!(has_code(&err, "RAQL0401"));
}

#[test]
fn witness_path_is_forbidden_inside_recursive_scc() {
    let src = r#"
.decl graph_edge(Graph: string, From: Def, To: Def, EdgeKind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: Path) extern.
.mode witness_path(+string, +Def, +Def, -Path).
.decl p(X: Def).
.decl q(X: Def).
p(X) :- q(X).
q(X) :- p(X), witness_path("g", X, X, _).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail");
    assert!(has_code(&err, "RAQL0401"));
}

#[test]
fn witness_path_induces_selection_dependency_on_graph_edge() {
    let src = r#"
.decl graph_edge(Graph: string, From: Def, To: Def, EdgeKind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: Path) extern.
.mode witness_path(+string, +Def, +Def, -Path).
.decl seed(D: Def) input.
.decl hop(P: Path).
hop(P) :- seed(D), witness_path("g", D, D, P).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let planned = plan(typed).expect("plan");
    let hop_stratum = planned.strata.get("hop").copied().unwrap_or_default();
    let edge_stratum = planned
        .strata
        .get("graph_edge")
        .copied()
        .unwrap_or_default();
    assert!(hop_stratum > edge_stratum);
}

#[test]
fn path_hop_induces_selection_dependency_on_graph_edge() {
    let src = r#"
.decl graph_edge(Graph: string, From: Def, To: Def, EdgeKind: string, Evidence: Span) input.
.decl path_hop(P: Path, Seq: int, From: Def, To: Def, Kind: string, Evidence: Span) extern.
.mode path_hop(+Path, -int, -Def, -Def, -string, -Span).
.decl seed_path(P: Path) input.
.decl hops(Seq: int).
hops(Seq) :- seed_path(P), path_hop(P, Seq, _, _, _, _).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let planned = plan(typed).expect("plan");
    let hops_stratum = planned.strata.get("hops").copied().unwrap_or_default();
    let edge_stratum = planned
        .strata
        .get("graph_edge")
        .copied()
        .unwrap_or_default();
    assert!(hops_stratum > edge_stratum);
}

#[test]
fn choose_topk_requires_ground_group() {
    let src = r#"
.decl item(I: int) input.
.decl top(G: int, I: int).
item(1).
top(G, I) :- choose_topk("tag", 1, G, S, I : item(I), S := I).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let err = plan(typed).expect_err("must fail");
    assert!(has_code(&err, "RAQL0301") || has_code(&err, "RAQL0406"));
}

#[test]
fn planner_defers_eq_until_one_side_is_ground() {
    let src = r#"
.decl p(X: int) input.
.decl q(X: int).
q(X) :- X = Y, p(Y).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let planned = plan(typed).expect("plan");
    let order = planned.planned_rules()[0]
        .ordered_goals()
        .iter()
        .map(|g| g.index())
        .collect::<Vec<_>>();
    assert_eq!(order, vec![1, 0]);
}
