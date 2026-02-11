use raql_syntax::{
    AggregateName, ArithOp, Constraint, DeclarationKind, Diagnostic, Directive, Expr, Goal, Stmt,
    Term, parse_program,
};

fn parse_errors(src: &str) -> Vec<Diagnostic> {
    parse_program(src).expect_err("program should fail parsing")
}

fn parse_error_messages(src: &str) -> Vec<String> {
    parse_errors(src)
        .into_iter()
        .map(|diag| diag.message)
        .collect()
}

fn assert_has_message(messages: &[String], needle: &str) {
    assert!(
        messages.iter().any(|message| message.contains(needle)),
        "expected diagnostic containing `{needle}`, got: {messages:#?}",
    );
}

fn find_diag<'a>(diagnostics: &'a [Diagnostic], needle: &str) -> &'a Diagnostic {
    diagnostics
        .iter()
        .find(|diag| diag.message.contains(needle))
        .unwrap_or_else(|| panic!("expected diagnostic containing `{needle}`"))
}

#[test]
fn parses_representative_v01_surface() {
    let src = r#"
.include "std.raql".
.type RenderMode = { DOC_SIG, ITEM }.
.mode walk(+Def, -int, ?TypeRef).
.pragma max_iter = 10.
.decl edge(A: int, B: int) input.
.func doc(D: Def, V: option<string>) extern.

edge(1, 2).
ranked(A, none::<int>, [A, 2]::<int>) :-
  edge(A, B),
  X := B + 1 * (B - 1),
  A != X,
  (edge(A, B) ; edge(B, A)),
  N = count(V : edge(A, V)),
  M = count(edge(A, B)),
  choose_topk("tag", 3, A, Score, Item : edge(A, Item), Score := Item + 1).
"#;

    let program = parse_program(src).expect("program should parse");
    assert!(program.sources().file_count() == 1);

    let statements = &program.phase().statements;
    assert_eq!(statements.len(), 8);

    match &statements[0].value {
        Stmt::Directive(Directive::Include(include)) => {
            assert_eq!(include.path.value.as_str(), "std.raql")
        }
        other => panic!("unexpected stmt: {other:?}"),
    }

    match &statements[4].value {
        Stmt::Declaration(decl) => {
            assert_eq!(decl.kind, DeclarationKind::Relation);
            assert_eq!(decl.name.value.as_str(), "edge");
            assert_eq!(decl.args.len(), 2);
        }
        other => panic!("unexpected stmt: {other:?}"),
    }

    let Stmt::Rule(rule) = &statements[7].value else {
        panic!("expected rule statement");
    };
    assert_eq!(rule.body.len(), 7);

    match &rule.head.value.terms[1].value {
        Term::None { turbofish } => {
            assert!(turbofish.is_some());
        }
        other => panic!("expected none::<...> term, got {other:?}"),
    }

    match &rule.head.value.terms[2].value {
        Term::List { items, turbofish } => {
            assert_eq!(items.len(), 2);
            assert!(turbofish.is_some());
        }
        other => panic!("expected list term, got {other:?}"),
    }

    match &rule.body[1].value {
        Goal::Constraint(Constraint::ArithmeticBind(bind)) => match &bind.expr.value {
            Expr::Binary { op, .. } => assert_eq!(op.value, ArithOp::Add),
            other => panic!("expected additive expr, got {other:?}"),
        },
        other => panic!("expected arithmetic bind goal, got {other:?}"),
    }

    match &rule.body[4].value {
        Goal::Aggregate(agg) => {
            assert_eq!(agg.name.value, AggregateName::Count);
            assert!(agg.projection_var.is_some());
            assert_eq!(agg.goals.len(), 1);
        }
        other => panic!("expected aggregate goal, got {other:?}"),
    }

    match &rule.body[6].value {
        Goal::ChooseTopK(choose) => {
            assert_eq!(choose.tag.value.as_str(), "tag");
            assert_eq!(choose.goals.len(), 2);
        }
        other => panic!("expected choose_topk goal, got {other:?}"),
    }
}

#[test]
fn reports_malformed_program() {
    let src = r#"
.decl p(X: int) input.
p(X) :- X := 1 + .
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "expected term; accepted forms:");
    assert_has_message(&messages, "`none`/`some(...)`");
}

#[test]
fn reports_unexpected_character_with_accepted_token_guidance() {
    let src = r#"
.decl p(X: int) input.
p(@).
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "unexpected character `@`");
    assert_has_message(&messages, "accepted tokens begin with");
}

#[test]
fn reports_single_quote_string_misuse_with_double_quote_example() {
    let src = r#"
.decl txt(X: string) input.
txt('oops').
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "unexpected character `'`");
    assert_has_message(
        &messages,
        "RAQL strings must use double quotes, not single quotes",
    );
    assert_has_message(&messages, "replace `'value'` with `\"value\"`");
}

#[test]
fn reports_include_path_string_literal_error_with_example_and_hint() {
    let src = r#"
.include std.raql.
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "expected include path string literal");
    assert_has_message(&messages, ".include \"file.raql\".");
    assert_has_message(
        &messages,
        "wrap the include path in double quotes before the closing `.`",
    );
}

#[test]
fn parse_label_reports_expected_and_actual_for_include_path_error() {
    let src = r#"
.include std.raql.
"#;

    let diagnostics = parse_errors(src);
    let diag = find_diag(&diagnostics, "expected include path string literal");
    let primary = diag.primary.as_ref().expect("primary label");
    assert!(
        primary
            .message
            .contains("expected include path string literal")
    );
    assert!(primary.message.contains("found identifier `std`"));
    assert_ne!(primary.message, "here");
}

#[test]
fn parse_label_reports_end_of_input_for_missing_statement_terminator() {
    let src = r#"
.decl p(X: int) input
"#;

    let diagnostics = parse_errors(src);
    let diag = find_diag(&diagnostics, "expected `.` after declaration");
    let primary = diag.primary.as_ref().expect("primary label");
    assert!(primary.message.contains("expected `.` after declaration"));
    assert!(primary.message.contains("found end of input"));
}

#[test]
fn reports_unterminated_block_comment_with_closure_hint() {
    let src = "/* missing end";

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "unterminated block comment");
    assert_has_message(&messages, "add a closing `*/`");
}

#[test]
fn reports_unterminated_string_with_closure_hint() {
    let src = r#"
.decl txt(X: string) input.
txt("oops
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "unterminated string literal");
    assert_has_message(&messages, "add a closing `\"`");
}

#[test]
fn handles_nested_comments_and_escapes() {
    let src = r#"
/* outer
   /* inner */
*/
.decl txt(X: string) input.
txt("a\n\"b\"").
"#;

    let program = parse_program(src).expect("program should parse");
    assert_eq!(program.phase().statements.len(), 2);
}

#[test]
fn parses_witness_path_path_hop_and_seq_output_surface() {
    let src = r#"
.decl graph_edge(Graph: string, From: int, To: int, Kind: string, Evidence: string) input.
.decl out_span_frag(Path: string, Seq: int, Kind: string, Text: string) output.
.decl witness(Graph: string, From: int, To: int, Path: string).
.decl hop(Path: string, Seq: int, From: int, To: int, Kind: string, Evidence: string).
witness("g", 1, 3, P) :- witness_path("g", 1, 3, P).
hop(P, Seq, From, To, Kind, Evidence) :- path_hop(P, Seq, From, To, Kind, Evidence).
"#;

    let program = parse_program(src).expect("program should parse");
    let statements = &program.phase().statements;
    assert_eq!(statements.len(), 6);

    let Stmt::Rule(witness_rule) = &statements[4].value else {
        panic!("expected witness rule");
    };
    assert_eq!(witness_rule.body.len(), 1);
    match &witness_rule.body[0].value {
        Goal::Atom(atom) => assert_eq!(atom.name.value.as_str(), "witness_path"),
        other => panic!("expected witness_path atom, got {other:?}"),
    }

    let Stmt::Rule(hop_rule) = &statements[5].value else {
        panic!("expected hop rule");
    };
    assert_eq!(hop_rule.body.len(), 1);
    match &hop_rule.body[0].value {
        Goal::Atom(atom) => assert_eq!(atom.name.value.as_str(), "path_hop"),
        other => panic!("expected path_hop atom, got {other:?}"),
    }
}

#[test]
fn reports_variable_name_rule_with_uppercase_or_underscore_examples() {
    let src = r#"
.decl edge(A: int, B: int) input.
.decl ranked(A: int) output.
ranked(A) :- choose_topk("tag", 3, A, score, Item : edge(A, Item)).
"#;

    let messages = parse_error_messages(src);
    assert_has_message(
        &messages,
        "must start with `_` or an uppercase letter, e.g. `X` or `_Tmp`",
    );
    assert_has_message(&messages, "found `score`");
}

#[test]
fn reports_option_type_constructor_error_with_example() {
    let src = r#"
.decl p(X: option) input.
"#;

    let messages = parse_error_messages(src);
    assert_has_message(
        &messages,
        "expected `<...>` after `option` type constructor",
    );
    assert_has_message(&messages, "example: `option<int>`");
}

#[test]
fn reports_option_type_missing_closing_angle_with_example() {
    let src = r#"
.decl p(X: option<int) input.
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "expected `>` to close `option<...>`");
    assert_has_message(&messages, "example: `option<int>`");
}

#[test]
fn reports_list_type_constructor_error_with_example() {
    let src = r#"
.decl p(X: list) input.
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "expected `<...>` after `list` type constructor");
    assert_has_message(&messages, "example: `list<int>`");
}

#[test]
fn reports_list_type_missing_closing_angle_with_example() {
    let src = r#"
.decl p(X: list<int) input.
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "expected `>` to close `list<...>`");
    assert_has_message(&messages, "example: `list<int>`");
}

#[test]
fn reports_choose_topk_empty_goals_with_item_colon_example() {
    let src = r#"
.decl edge(A: int, B: int) input.
.decl ranked(A: int) output.
ranked(A) :- choose_topk("tag", 3, A, Score, Item : ).
"#;

    let messages = parse_error_messages(src);
    assert_has_message(
        &messages,
        "expected at least one choose_topk candidate goal after `Item :`",
    );
    assert_has_message(
        &messages,
        "choose_topk(\"tag\", 3, Group, Score, Item : edge(Group, Item))",
    );
}

#[test]
fn reports_trailing_comma_in_goal_list_with_context() {
    let src = r#"
.decl p(X: int) input.
.decl q(X: int) input.
p(X) :- q(X), .
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "trailing `,` in rule body");
    assert_has_message(&messages, "remove the trailing comma or add another goal");
}

#[test]
fn reports_disjunction_semicolon_requirement_with_example() {
    let src = r#"
.decl edge(A: int, B: int) input.
.decl p(A: int) output.
p(A) :- (edge(A, B)).
"#;

    let messages = parse_error_messages(src);
    assert_has_message(
        &messages,
        "disjunction group must contain `;` between branches",
    );
    assert_has_message(&messages, "(edge(A, B) ; edge(B, A))");
}

#[test]
fn reports_unsupported_escape_with_valid_escape_list() {
    let src = r#"
.decl txt(X: string) input.
txt("bad\q").
"#;

    let messages = parse_error_messages(src);
    assert_has_message(
        &messages,
        "valid escapes are `\\\\`, `\\\"`, `\\n`, `\\t`, and `\\r`",
    );
}

#[test]
fn reports_missing_comma_between_atom_arguments_with_example() {
    let src = r#"
.decl p(A: int, B: int) input.
p(1 2).
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "expected `,` between atom arguments");
    assert_has_message(&messages, "example: `edge(A, B)`");
}

#[test]
fn reports_missing_comma_between_list_items_with_example() {
    let src = r#"
.decl p(X: list<int>) input.
p([1 2]).
"#;

    let messages = parse_error_messages(src);
    assert_has_message(&messages, "expected `,` between list items");
    assert_has_message(&messages, "example: `[A, B]`");
}

#[test]
fn recovery_reports_skipped_region_with_likely_missing_dot_hint() {
    let src = r#"
.decl p(A: int, B: int) input.
.decl q(A: int) input.
p(A B).
q(A).
"#;

    let diagnostics = parse_errors(src);
    let recovery = diagnostics
        .iter()
        .find(|diag| diag.message.contains("parser recovery skipped"))
        .expect("missing parser recovery diagnostic");
    assert!(
        recovery
            .message
            .contains("likely cause: missing `.` after the previous statement")
    );
    assert!(
        recovery
            .notes
            .iter()
            .any(|note| note.contains("missing `.`"))
    );
    let primary = recovery.primary.as_ref().expect("recovery primary label");
    let start = src.find("B).").expect("start of skipped region") as u32;
    let end = start + "B)".len() as u32;
    assert_eq!(u32::from(primary.span.range.start()), start);
    assert_eq!(u32::from(primary.span.range.end()), end);
}

#[test]
fn option_missing_type_args_points_at_option_constructor() {
    let src = r#"
.decl p(X: option) input.
"#;

    let diagnostics = parse_errors(src);
    let diag = diagnostics
        .iter()
        .find(|diag| diag.message.contains("expected `<...>` after `option`"))
        .expect("missing option constructor diagnostic");
    let primary = diag.primary.as_ref().expect("primary label");
    let start = src.find("option").expect("option token") as u32;
    let end = start + "option".len() as u32;
    assert_eq!(u32::from(primary.span.range.start()), start);
    assert_eq!(u32::from(primary.span.range.end()), end);
}

#[test]
fn list_missing_closing_angle_points_at_open_generic_range() {
    let src = r#"
.decl p(X: list<int) input.
"#;

    let diagnostics = parse_errors(src);
    let diag = diagnostics
        .iter()
        .find(|diag| diag.message.contains("expected `>` to close `list<...>`"))
        .expect("missing list closing angle diagnostic");
    let primary = diag.primary.as_ref().expect("primary label");
    let start = src.find("list").expect("list token") as u32;
    let inner_start = src.find("int)").expect("inner type token") as u32;
    let end = inner_start + "int".len() as u32;
    assert_eq!(u32::from(primary.span.range.start()), start);
    assert_eq!(u32::from(primary.span.range.end()), end);
}
