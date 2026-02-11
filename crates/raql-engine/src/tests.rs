use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexSet;
use raql_compiler::{plan, resolve, typecheck};
use raql_host::MockHostRuntime;
use raql_syntax::{Constraint, Goal, Stmt, parse_program};

use crate::{
    EngineHostError, EngineHostView, EvalStatus, HostValueKind, RuntimeError, RuntimeValue,
    execute, stable_cmp, term_type_tag,
};

#[derive(Default)]
struct ExternRowsHost {
    world_stamp: String,
    extern_rows: BTreeMap<String, Vec<Vec<RuntimeValue>>>,
    extern_errors: BTreeMap<String, EngineHostError>,
}

impl ExternRowsHost {
    fn with_rows(mut self, predicate: &str, rows: Vec<Vec<RuntimeValue>>) -> Self {
        self.extern_rows.insert(predicate.to_string(), rows);
        self
    }

    fn with_error(mut self, predicate: &str, error: EngineHostError) -> Self {
        self.extern_errors.insert(predicate.to_string(), error);
        self
    }
}

impl EngineHostView for ExternRowsHost {
    fn world_stamp(&mut self) -> String {
        if self.world_stamp.is_empty() {
            "test:extern".to_string()
        } else {
            self.world_stamp.clone()
        }
    }

    fn stable_key(&mut self, value: &RuntimeValue) -> String {
        format!("{value:?}")
    }

    fn extern_relation_rows(
        &mut self,
        predicate: &str,
    ) -> Result<Option<Vec<Vec<RuntimeValue>>>, EngineHostError> {
        if let Some(error) = self.extern_errors.get(predicate) {
            return Err(error.clone());
        }
        Ok(self.extern_rows.get(predicate).cloned())
    }
}

#[derive(Default)]
struct StableKeyFallbackHost {
    extern_rows: BTreeMap<String, Vec<Vec<RuntimeValue>>>,
    fallback_ids: BTreeSet<u64>,
    runtime_notes: BTreeSet<String>,
}

impl StableKeyFallbackHost {
    fn with_rows(mut self, predicate: &str, rows: Vec<Vec<RuntimeValue>>) -> Self {
        self.extern_rows.insert(predicate.to_string(), rows);
        self
    }

    fn with_fallback_id(mut self, id: u64) -> Self {
        self.fallback_ids.insert(id);
        self
    }
}

impl EngineHostView for StableKeyFallbackHost {
    fn world_stamp(&mut self) -> String {
        "test:stable-key".to_string()
    }

    fn stable_key(&mut self, value: &RuntimeValue) -> String {
        match value {
            RuntimeValue::Host { kind, id } if self.fallback_ids.contains(id) => {
                let kind = kind.label();
                self.runtime_notes.insert(format!(
                        "host stable-key fallback for `{kind}` value `{id:#018x}`: injected host lookup failure; using deterministic fallback key."
                    ));
                format!("stable-fallback:{kind}:{id:#018x}")
            }
            RuntimeValue::Host { kind, id } => {
                let kind = kind.label();
                format!("stable:{kind}:{id:#018x}")
            }
            _ => format!("{value:?}"),
        }
    }

    fn take_runtime_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.runtime_notes)
            .into_iter()
            .collect()
    }

    fn extern_relation_rows(
        &mut self,
        predicate: &str,
    ) -> Result<Option<Vec<Vec<RuntimeValue>>>, EngineHostError> {
        Ok(self.extern_rows.get(predicate).cloned())
    }
}

fn planned_src(src: &str) -> raql_compiler::PlannedProgram {
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    plan(typed).expect("plan")
}

fn execute_src(src: &str) -> crate::EvalResult {
    let planned = planned_src(src);
    let mut host = MockHostRuntime::new();
    execute(&planned, &mut host)
}

#[test]
fn runtime_error_codes_are_stable() {
    assert_eq!(RuntimeError::DivisionByZero.code_str(), "RAQL0901");
    assert_eq!(RuntimeError::Overflow.code_str(), "RAQL0902");
    assert_eq!(
        RuntimeError::MissingRelation {
            relation: "missing".to_string(),
            context: "referenced by goal `missing()`".to_string(),
        }
        .code_str(),
        "RAQL0903"
    );
    assert_eq!(
        RuntimeError::UnboundVar("X".to_string()).code_str(),
        "RAQL0904"
    );
    assert_eq!(
        RuntimeError::TypeMismatchContext {
            context: "builtin `contains` argument 1 expected string, found int".to_string(),
        }
        .code_str(),
        "RAQL0905"
    );
    assert_eq!(
        RuntimeError::ExternRowArity {
            predicate: "ext".to_string(),
            row_index: 1,
            expected: 2,
            got: 1,
        }
        .code_str(),
        "RAQL0908"
    );
    assert_eq!(
        RuntimeError::WitnessArity {
            predicate: "path_hop".to_string(),
            expected: 6,
            got: 5,
        }
        .code_str(),
        "RAQL0906"
    );
    assert_eq!(
        RuntimeError::WitnessPathHopRowArity {
            path_id: "path:0".to_string(),
            expected: 6,
            got: 5,
        }
        .code_str(),
        "RAQL0906"
    );
    assert_eq!(
        RuntimeError::FunctionCardinality {
            predicate: "f".to_string(),
            got: 2,
            context: "goal `f(X)` with input bindings: <none>".to_string(),
        }
        .code_str(),
        "RAQL0907"
    );
}

#[test]
fn executes_transitive_closure_style_program() {
    let src = r#"
.decl edge(A: int, B: int) input.
.decl reach(A: int, B: int).
edge(1,2).
edge(2,3).
reach(A,B) :- edge(A,B).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("reach")
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    );
    assert_eq!(
        result
            .relations
            .get("out_status")
            .map(|rows| rows.iter().cloned().collect::<Vec<_>>()),
        Some(vec![vec![RuntimeValue::String("ok".to_string())]])
    );
}

#[test]
fn reports_division_by_zero_as_partial() {
    let src = r#"
.decl p(X: int).
p(X) :- X := 1 / 0.
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| n.section == "Errors"));
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.message.contains("division by zero"))
    );
    assert!(
        result.relations.get("out_status").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::String("partial".to_string())])
        })
    );
    assert!(
        result
            .relations
            .get("out_note")
            .is_some_and(|rows| rows.iter().any(|row| row
                == &vec![
                    RuntimeValue::String("Errors".to_string()),
                    RuntimeValue::String("runtime error [RAQL0901]: division by zero".to_string(),),
                ]))
    );
}

#[test]
fn reports_overflow_as_partial_with_runtime_code_and_output_relations() {
    let src = r#"
.decl seed(X: int) input.
.decl out(X: int).
seed(9223372036854775807).
out(Y) :- seed(X), Y := X + 1.
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0902]")
            && n.message.contains("integer overflow")
    }));
    assert!(
        result.relations.get("out_status").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::String("partial".to_string())])
        })
    );
    assert!(
        result
            .relations
            .get("out_note")
            .is_some_and(|rows| rows.iter().any(|row| row
                == &vec![
                    RuntimeValue::String("Errors".to_string()),
                    RuntimeValue::String("runtime error [RAQL0902]: integer overflow".to_string(),),
                ]))
    );
}

#[test]
fn reports_iteration_limit_as_partial_with_notes_section() {
    let src = r#"
.pragma max_iters = 3.
.decl p(X: int).
p(0).
p(X) :- p(Y), X := Y + 1.
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Notes"
            && n.message
                .contains("fixpoint iteration limit exceeded in SCC `p`")
            && n.message.contains("configured max_iters=3")
            && n.message.contains("triggered at iteration 4")
    }));
    assert!(
        result.relations.get("out_status").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::String("partial".to_string())])
        })
    );
    assert!(result.relations.get("out_note").is_some_and(|rows| {
        rows.iter().any(|row| {
            row.first() == Some(&RuntimeValue::String("Notes".to_string()))
                && row.get(1).is_some_and(|msg| {
                    matches!(
                        msg,
                        RuntimeValue::String(m)
                            if m.contains("fixpoint iteration limit exceeded in SCC `p`")
                                && m.contains("configured max_iters=3")
                                && m.contains("triggered at iteration 4")
                    )
                })
        })
    }));
}

#[test]
fn non_recursive_scc_does_not_trip_iteration_limit() {
    let src = r#"
.pragma max_iters = 1.
.decl edge(A: int, B: int) input.
.decl reach(A: int, B: int).
edge(1,2).
reach(A,B) :- edge(A,B).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result.relations.get("reach").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::Int(1), RuntimeValue::Int(2)])
        })
    );
}

#[test]
fn choose_topk_is_deterministic_for_score_ties() {
    let src = r#"
.decl candidate(G: int, Item: string, Score: int) input.
.decl grp(G: int) input.
.decl top_item(G: int, Score: int, Item: string).
grp(1).
candidate(1, "b", 5).
candidate(1, "a", 5).
candidate(1, "c", 7).
candidate(1, "d", 1).
top_item(G, Score, Item) :-
  grp(G),
  choose_topk("rank", 3, G, Score, Item : candidate(G, Item, Score)).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    let rows = result
        .relations
        .get("top_item")
        .expect("relation exists")
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            vec![
                RuntimeValue::Int(1),
                RuntimeValue::Int(7),
                RuntimeValue::String("c".to_string())
            ],
            vec![
                RuntimeValue::Int(1),
                RuntimeValue::Int(5),
                RuntimeValue::String("a".to_string())
            ],
            vec![
                RuntimeValue::Int(1),
                RuntimeValue::Int(5),
                RuntimeValue::String("b".to_string())
            ],
        ]
    );
}

#[test]
fn relational_order_for_enums_uses_declaration_order() {
    let src = r#"
.type Rank = { Beta, Alpha }.
.decl src(V: Rank) input.
.decl lt(V: Rank).
src(Rank::Alpha).
src(Rank::Beta).
lt(V) :- src(V), V < Rank::Alpha.
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(result.relations.get("lt").is_some_and(|rows| {
        rows.len() == 1
            && rows.contains(&vec![RuntimeValue::Enum {
                name: "Rank".to_string(),
                variant: "Beta".to_string(),
            }])
    }));
}

#[test]
fn aggregate_min_max_for_enums_uses_declaration_order() {
    let src = r#"
.type Rank = { Beta, Alpha }.
.decl src(V: Rank) input.
.decl mn(V: Rank).
.decl mx(V: Rank).
src(Rank::Alpha).
src(Rank::Beta).
mn(V) :- V = min(X : src(X)).
mx(V) :- V = max(X : src(X)).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(result.relations.get("mn").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::Enum {
            name: "Rank".to_string(),
            variant: "Beta".to_string(),
        }])
    }));
    assert!(result.relations.get("mx").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::Enum {
            name: "Rank".to_string(),
            variant: "Alpha".to_string(),
        }])
    }));
}

#[test]
fn choose_topk_ties_use_enum_declaration_order_for_items() {
    let src = r#"
.type Item = { Beta, Alpha, Gamma }.
.decl candidate(G: int, I: Item, Score: int) input.
.decl grp(G: int) input.
.decl top_item(G: int, Score: int, I: Item).
grp(1).
candidate(1, Item::Alpha, 5).
candidate(1, Item::Beta, 5).
candidate(1, Item::Gamma, 7).
top_item(G, Score, I) :-
  grp(G),
  choose_topk("rank", 3, G, Score, I : candidate(G, I, Score)).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    let rows = result
        .relations
        .get("top_item")
        .expect("relation exists")
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            vec![
                RuntimeValue::Int(1),
                RuntimeValue::Int(7),
                RuntimeValue::Enum {
                    name: "Item".to_string(),
                    variant: "Gamma".to_string(),
                },
            ],
            vec![
                RuntimeValue::Int(1),
                RuntimeValue::Int(5),
                RuntimeValue::Enum {
                    name: "Item".to_string(),
                    variant: "Beta".to_string(),
                },
            ],
            vec![
                RuntimeValue::Int(1),
                RuntimeValue::Int(5),
                RuntimeValue::Enum {
                    name: "Item".to_string(),
                    variant: "Alpha".to_string(),
                },
            ],
        ]
    );
}

#[test]
fn stable_cmp_orders_none_by_explicit_option_type_tag() {
    let src = r#"
.decl p().
p().
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let planned = plan(typed).expect("plan");
    let mut host = MockHostRuntime::new();

    let less = stable_cmp(
        &planned,
        &mut host,
        &RuntimeValue::None,
        &RuntimeValue::None,
        Some("option<int>"),
        Some("option<string>"),
    );
    let greater = stable_cmp(
        &planned,
        &mut host,
        &RuntimeValue::None,
        &RuntimeValue::None,
        Some("option<string>"),
        Some("option<int>"),
    );
    assert!(less.is_lt());
    assert!(greater.is_gt());
}

#[test]
fn term_type_tag_uses_turbofish_for_none_and_empty_list() {
    let src = r#"
.decl p().
p() :- none::<int> = none::<int>.
p() :- []::<string> = []::<string>.
"#;
    let parsed = parse_program(src).expect("parse");
    let statements = &parsed.phase().statements;
    let empty = std::collections::BTreeMap::new();

    let rule_none = match &statements[1].value {
        Stmt::Rule(rule) => rule,
        _ => panic!("expected rule"),
    };
    let rel_none = match &rule_none.body[0].value {
        Goal::Constraint(Constraint::Relational(rel)) => rel,
        _ => panic!("expected relational constraint"),
    };
    assert_eq!(
        term_type_tag(&rel_none.lhs, &empty).as_deref(),
        Some("option<int>")
    );

    let rule_list = match &statements[2].value {
        Stmt::Rule(rule) => rule,
        _ => panic!("expected rule"),
    };
    let rel_list = match &rule_list.body[0].value {
        Goal::Constraint(Constraint::Relational(rel)) => rel,
        _ => panic!("expected relational constraint"),
    };
    assert_eq!(
        term_type_tag(&rel_list.lhs, &empty).as_deref(),
        Some("list<string>")
    );
}

#[test]
fn witness_path_hop_order_uses_stable_order() {
    let src = r#"
.type Def = { Start, Beta, Alpha, End }.
.type Span = { S1, S2, S3, S4 }.
.decl graph_edge(Graph: string, From: Def, To: Def, Kind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: string) extern.
.mode witness_path(+string, +Def, +Def, -string).
.decl path_hop(P: string, Seq: int, From: Def, To: Def, Kind: string, Evidence: Span) extern.
.mode path_hop(+string, -int, -Def, -Def, -string, -Span).
.decl first_hop(To: Def).
graph_edge("g", Def::Start, Def::Alpha, "edge", Span::S1).
graph_edge("g", Def::Start, Def::Beta, "edge", Span::S2).
graph_edge("g", Def::Alpha, Def::End, "edge", Span::S3).
graph_edge("g", Def::Beta, Def::End, "edge", Span::S4).
first_hop(To) :-
  witness_path("g", Def::Start, Def::End, P),
  path_hop(P, 0, _, To, _, _).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(result.relations.get("first_hop").is_some_and(|rows| {
        rows.len() == 1
            && rows.contains(&vec![RuntimeValue::Enum {
                name: "Def".to_string(),
                variant: "Beta".to_string(),
            }])
    }));
}

#[test]
fn witness_path_and_path_hop_produce_expected_hops() {
    let src = r#"
.type Def = { D1, D2, D3 }.
.type Span = { E12, E23 }.
.decl graph_edge(Graph: string, From: Def, To: Def, Kind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: string) extern.
.mode witness_path(+string, +Def, +Def, -string).
.decl path_hop(P: string, Seq: int, From: Def, To: Def, Kind: string, Evidence: Span) extern.
.mode path_hop(+string, -int, -Def, -Def, -string, -Span).
.decl hop(Seq: int, From: Def, To: Def, Kind: string, Evidence: Span).
graph_edge("g", Def::D1, Def::D2, "edge", Span::E12).
graph_edge("g", Def::D2, Def::D3, "edge", Span::E23).
hop(Seq, From, To, Kind, Evidence) :-
  witness_path("g", Def::D1, Def::D3, P),
  path_hop(P, Seq, From, To, Kind, Evidence).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    let rows = result
        .relations
        .get("hop")
        .expect("relation exists")
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            vec![
                RuntimeValue::Int(0),
                RuntimeValue::Enum {
                    name: "Def".to_string(),
                    variant: "D1".to_string()
                },
                RuntimeValue::Enum {
                    name: "Def".to_string(),
                    variant: "D2".to_string()
                },
                RuntimeValue::String("edge".to_string()),
                RuntimeValue::Enum {
                    name: "Span".to_string(),
                    variant: "E12".to_string()
                }
            ],
            vec![
                RuntimeValue::Int(1),
                RuntimeValue::Enum {
                    name: "Def".to_string(),
                    variant: "D2".to_string()
                },
                RuntimeValue::Enum {
                    name: "Def".to_string(),
                    variant: "D3".to_string()
                },
                RuntimeValue::String("edge".to_string()),
                RuntimeValue::Enum {
                    name: "Span".to_string(),
                    variant: "E23".to_string()
                }
            ],
        ]
    );
}

#[test]
fn witness_path_hop_sequences_are_zero_based_and_gapless_per_path() {
    let src = r#"
.type Def = { Start, A, B, End }.
.type Span = { SA, AE, SB, BE }.
.func path_max_depth(N: int) input.
.mode path_max_depth(-int).
.func path_limit(N: int) input.
.mode path_limit(-int).
.decl graph_edge(Graph: string, From: Def, To: Def, Kind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: string) extern.
.mode witness_path(+string, +Def, +Def, -string).
.decl path_hop(P: string, Seq: int, From: Def, To: Def, Kind: string, Evidence: Span) extern.
.mode path_hop(+string, -int, -Def, -Def, -string, -Span).
.decl hop_seq(P: string, Seq: int).
path_max_depth(8).
path_limit(2).
graph_edge("g", Def::Start, Def::A, "edge", Span::SA).
graph_edge("g", Def::A, Def::End, "edge", Span::AE).
graph_edge("g", Def::Start, Def::B, "edge", Span::SB).
graph_edge("g", Def::B, Def::End, "edge", Span::BE).
hop_seq(P, Seq) :-
  witness_path("g", Def::Start, Def::End, P),
  path_hop(P, Seq, _, _, _, _).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    let rows = result
        .relations
        .get("hop_seq")
        .expect("relation exists")
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let mut by_path = BTreeMap::<String, Vec<i64>>::new();
    for row in rows {
        assert_eq!(row.len(), 2, "expected hop_seq row arity 2");
        let path_id = match row[0].clone() {
            RuntimeValue::String(path_id) => path_id,
            other => panic!("expected string path id, found {other:?}"),
        };
        let seq = match row[1] {
            RuntimeValue::Int(seq) => seq,
            ref other => panic!("expected int sequence, found {other:?}"),
        };
        by_path.entry(path_id).or_default().push(seq);
    }
    assert_eq!(by_path.len(), 2, "expected two witness paths");
    for seqs in by_path.values_mut() {
        seqs.sort_unstable();
        assert_eq!(seqs.as_slice(), [0, 1]);
    }
}

#[test]
fn opt_max_iters_some_overrides_pragma() {
    let src = r#"
.pragma max_iters = 50.
.func opt_max_iters(N: option<int>) input.
opt_max_iters(some(2)).
.decl p(X: int).
p(0).
p(X) :- p(Y), X := Y + 1.
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Notes"
            && n.message.contains("fixpoint iteration limit exceeded")
            && n.message.contains("configured max_iters=2")
            && n.message.contains("triggered at iteration 3")
    }));
}

#[test]
fn injects_default_control_max_depth_input() {
    let src = r#"
.decl control_max_depth(N: int) input.
.decl observed(N: int).
observed(N) :- control_max_depth(N).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("observed")
            .is_some_and(|rows| rows.contains(&vec![RuntimeValue::Int(32)]))
    );
}

#[test]
fn world_stamp_is_injected_from_host_runtime() {
    let src = r#"
.func world_stamp(S: string) extern.
.mode world_stamp(-string).
.decl stamp(S: string).
stamp(S) :- world_stamp(S).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("type");
    let planned = plan(typed).expect("plan");
    let mut host = MockHostRuntime::new().with_world_stamp("cfg:test");
    let result = execute(&planned, &mut host);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(result.relations.get("stamp").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("cfg:test".to_string())])
    }));
    assert!(result.relations.get("world_stamp").is_some_and(|rows| {
        rows.len() == 1 && rows.contains(&vec![RuntimeValue::String("cfg:test".to_string())])
    }));
}

#[test]
fn function_cardinality_violation_for_input_function_halts_run() {
    let src = r#"
.func single_value(S: string) input.
.mode single_value(-string).
single_value("a").
single_value("b").
.decl p() .
p() :- single_value(_).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0907]")
            && n.message
                .contains("functional predicate `single_value` cardinality violation")
            && n.message
                .contains("goal `single_value(_)` with input bindings: <none>")
    }));
}

#[test]
fn function_cardinality_for_opt_max_iters_includes_input_context() {
    let src = r#"
.func opt_max_iters(N: option<int>) input.
opt_max_iters(some(1)).
opt_max_iters(some(2)).
.decl p().
p().
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0907]")
            && n.message
                .contains("input `opt_max_iters` expected exactly one optional scalar row")
            && n.message.contains("row 1 = (some(1))")
            && n.message.contains("row 2 = (some(2))")
    }));
}

#[test]
fn scalar_input_type_mismatch_includes_input_row_context() {
    let mut rows = IndexSet::new();
    rows.insert(vec![RuntimeValue::String("deep".to_string())]);
    let mut relations = BTreeMap::new();
    relations.insert("control_max_depth".to_string(), rows);

    let err = super::scalar_input(&relations, "control_max_depth", 32).expect_err("type mismatch");
    assert_eq!(err.code_str(), "RAQL0905");
    assert!(
        err.to_string()
            .contains("input `control_max_depth` row 1 expected `int`, found string (\"deep\")")
    );
    assert!(err.to_string().contains("row 1 = (\"deep\")"));
}

#[test]
fn optional_scalar_input_type_mismatch_includes_input_row_context() {
    let mut rows = IndexSet::new();
    rows.insert(vec![RuntimeValue::Int(1)]);
    let mut relations = BTreeMap::new();
    relations.insert("opt_max_iters".to_string(), rows);

    let err =
        super::scalar_option_i64_input(&relations, "opt_max_iters").expect_err("type mismatch");
    assert_eq!(err.code_str(), "RAQL0905");
    assert!(
        err.to_string()
            .contains("input `opt_max_iters` row 1 expected `option<int>`, found int (1)")
    );
    assert!(err.to_string().contains("row 1 = (1)"));
}

#[test]
fn scalar_input_arity_mismatch_includes_input_row_context() {
    let mut rows = IndexSet::new();
    rows.insert(vec![RuntimeValue::Int(16), RuntimeValue::Int(32)]);
    let mut relations = BTreeMap::new();
    relations.insert("control_max_depth".to_string(), rows);

    let err = super::scalar_input(&relations, "control_max_depth", 32).expect_err("arity");
    assert_eq!(err.code_str(), "RAQL0905");
    assert!(
        err.to_string()
            .contains("input `control_max_depth` row 1 expected arity 1 with `int`, found arity 2")
    );
    assert!(err.to_string().contains("row 1 = (16, 32)"));
}

#[test]
fn optional_scalar_input_arity_mismatch_includes_input_row_context() {
    let mut rows = IndexSet::new();
    rows.insert(vec![RuntimeValue::None, RuntimeValue::None]);
    let mut relations = BTreeMap::new();
    relations.insert("opt_max_iters".to_string(), rows);

    let err = super::scalar_option_i64_input(&relations, "opt_max_iters").expect_err("arity");
    assert_eq!(err.code_str(), "RAQL0905");
    assert!(err.to_string().contains(
        "input `opt_max_iters` row 1 expected arity 1 with `option<int>`, found arity 2"
    ));
    assert!(err.to_string().contains("row 1 = (none, none)"));
}

#[test]
fn arithmetic_bind_type_mismatch_includes_target_and_row_context() {
    let parsed = parse_program(
        r#"
.decl p().
p() :- X := 1.
"#,
    )
    .expect("parse");
    let rule = match &parsed.phase().statements[1].value {
        Stmt::Rule(rule) => rule,
        _ => panic!("expected rule"),
    };
    let bind = match &rule.body[0].value {
        Goal::Constraint(Constraint::ArithmeticBind(bind)) => bind,
        _ => panic!("expected arithmetic bind"),
    };

    let mut env = BTreeMap::new();
    env.insert("X".to_string(), RuntimeValue::String("bad".to_string()));

    let err = super::eval_arithmetic_bind_constraint(bind, &env).expect_err("type mismatch");
    assert_eq!(err.code_str(), "RAQL0905");
    assert!(
        err.to_string()
            .contains("arithmetic bind `X := 1` expected target `X` to be `int`")
    );
    assert!(err.to_string().contains("row bindings: X=\"bad\""));
}

#[test]
fn eval_int_expr_type_mismatch_includes_expression_context() {
    let parsed = parse_program(
        r#"
.decl p().
p() :- X := Y.
"#,
    )
    .expect("parse");
    let rule = match &parsed.phase().statements[1].value {
        Stmt::Rule(rule) => rule,
        _ => panic!("expected rule"),
    };
    let bind = match &rule.body[0].value {
        Goal::Constraint(Constraint::ArithmeticBind(bind)) => bind,
        _ => panic!("expected arithmetic bind"),
    };

    let mut env = BTreeMap::new();
    env.insert("Y".to_string(), RuntimeValue::String("oops".to_string()));

    let err = super::eval_int_expr(&bind.expr, &env).expect_err("type mismatch");
    assert_eq!(err.code_str(), "RAQL0905");
    assert!(
        err.to_string()
            .contains("arithmetic expression `Y` expected term `Y` to be `int`")
    );
    assert!(err.to_string().contains("found string (\"oops\")"));
    assert!(err.to_string().contains("row bindings: Y=\"oops\""));
}

#[test]
fn eval_ground_term_wildcard_reports_contextual_type_mismatch() {
    let parsed = parse_program(
        r#"
.decl p().
p() :- q(_).
"#,
    )
    .expect("parse");
    let rule = match &parsed.phase().statements[1].value {
        Stmt::Rule(rule) => rule,
        _ => panic!("expected rule"),
    };
    let atom = match &rule.body[0].value {
        Goal::Atom(atom) => atom,
        _ => panic!("expected atom"),
    };
    let env = BTreeMap::new();

    let err = super::eval_ground_term(&atom.terms[0], &env).expect_err("wildcard mismatch");
    assert_eq!(err.code_str(), "RAQL0905");
    assert!(
        err.to_string()
            .contains("wildcard `_` cannot be evaluated as a ground term")
    );
    assert!(err.to_string().contains("row bindings: <none>"));
}

#[test]
fn missing_relation_for_witness_path_includes_reference_context() {
    let src = r#"
.type Def = { A, B }.
.decl witness_path(Graph: string, From: Def, To: Def, P: string) extern.
.mode witness_path(+string, +Def, +Def, -string).
.decl out().
out() :- witness_path("g", Def::A, Def::B, _).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0903]")
            && n.message.contains("missing relation `graph_edge`")
            && n.message
                .contains("referenced by builtin `witness_path` call")
            && n.message.contains("witness_path(\"g\", Def::A, Def::B, _)")
            && n.message.contains("<memory>:")
    }));
}

#[test]
fn builtin_type_mismatch_contains_reports_predicate_and_argument() {
    let src = r#"
.decl contains(Haystack: int, Needle: string) extern.
.decl out().
out() :- contains(1, "x").
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0905]")
            && n.message
                .contains("builtin `contains` argument 1 expected string, found int")
    }));
}

#[test]
fn builtin_type_mismatch_fmt_reports_nested_argument_path() {
    let src = r#"
.decl fmt(Format: string, Args: list<int>, Out: string) extern.
.decl out().
out() :- fmt("{}", [1], _).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0905]")
            && n.message
                .contains("builtin `fmt` argument 2[1] expected string, found int")
    }));
}

#[test]
fn path_hop_arity_errors_are_specific() {
    let parsed = parse_program(
        r#"
.decl p().
p() :- path_hop(P).
"#,
    )
    .expect("parse");
    let rule = match &parsed.phase().statements[1].value {
        Stmt::Rule(rule) => rule,
        _ => panic!("expected rule"),
    };
    let atom = match &rule.body[0].value {
        Goal::Atom(atom) => atom,
        _ => panic!("expected atom"),
    };
    let env = BTreeMap::new();
    let context = super::ExecutionContext::default();
    let relations = BTreeMap::new();
    let err =
        super::eval_path_hop_atom(atom, &env, &context, &relations).expect_err("arity mismatch");
    assert_eq!(err.code_str(), "RAQL0906");
    assert_eq!(
        err.to_string(),
        "witness predicate `path_hop` arity mismatch: expected 6, got 1"
    );
}

#[test]
fn witness_path_hop_error_messages_distinguish_failure_causes() {
    let arity = RuntimeError::WitnessArity {
        predicate: "path_hop".to_string(),
        expected: 6,
        got: 5,
    };
    let row = RuntimeError::WitnessPathHopRowArity {
        path_id: "path:bad".to_string(),
        expected: 6,
        got: 4,
    };
    assert_eq!(
        arity.to_string(),
        "witness predicate `path_hop` arity mismatch: expected 6, got 5"
    );
    assert_eq!(
        row.to_string(),
        "witness predicate `path_hop` cached row for path `path:bad` has arity 4, expected 6"
    );
}

#[test]
fn malformed_extern_row_arity_is_reported() {
    let src = r#"
.decl ext(A: int, B: int) extern.
.mode ext(-int, -int).
.decl out(A: int).
out(A) :- ext(A, _).
"#;
    let planned = planned_src(src);
    let mut host = ExternRowsHost::default().with_rows("ext", vec![vec![RuntimeValue::Int(1)]]);
    let result = execute(&planned, &mut host);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0908]")
            && n.message
                .contains("extern relation `ext` row 1 has arity 1, expected 2")
            && n.message.contains("host `extern_relation_rows`")
    }));
}

#[test]
fn host_scalar_overrides_for_witness_path_do_not_collide_with_defaults() {
    let src = r#"
.type Def = { A, B, C }.
.type Span = { S1, S2 }.
.func path_max_depth(N: int) extern.
.mode path_max_depth(-int).
.func path_limit(N: int) extern.
.mode path_limit(-int).
.decl graph_edge(Graph: string, From: Def, To: Def, Kind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: string) extern.
.mode witness_path(+string, +Def, +Def, -string).
.decl out(P: string).
graph_edge("g", Def::A, Def::B, "edge", Span::S1).
graph_edge("g", Def::B, Def::C, "edge", Span::S2).
out(P) :- witness_path("g", Def::A, Def::C, P).
"#;
    let planned = planned_src(src);
    let mut host = ExternRowsHost::default()
        .with_rows("path_max_depth", vec![vec![RuntimeValue::Int(1)]])
        .with_rows("path_limit", vec![vec![RuntimeValue::Int(5)]]);
    let result = execute(&planned, &mut host);

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("out")
            .is_some_and(|rows| rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("path_max_depth")
            .is_some_and(|rows| { rows.len() == 1 && rows.contains(&vec![RuntimeValue::Int(1)]) })
    );
    assert!(
        result
            .relations
            .get("path_limit")
            .is_some_and(|rows| { rows.len() == 1 && rows.contains(&vec![RuntimeValue::Int(5)]) })
    );
}

#[test]
fn missing_host_extern_rows_are_reported_as_actionable_partial_note() {
    let src = r#"
.decl ext(A: int) extern.
.mode ext(-int).
.decl out().
out() :- ext(_).
"#;
    let planned = planned_src(src);
    let mut host = ExternRowsHost::default();
    let result = execute(&planned, &mut host);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Notes"
            && n.message
                .contains("extern relation `ext` returned `Ok(None)`")
            && n.message.contains("EngineHostView::extern_relation_rows")
            && n.message.contains("treating `ext` as an empty relation")
            && n.message.contains("`ext` is referenced by 1 goal(s)")
            && n.message.contains("goal `ext(_)` in rule `out()`")
            && n.message.contains("<memory>:")
            && n.message.contains("Ok(Some(vec![]))")
    }));
    assert!(
        result.relations.get("out_status").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::String("partial".to_string())])
        })
    );
}

#[test]
fn missing_host_extern_rows_for_unused_predicate_note_mentions_no_usage() {
    let src = r#"
.decl ext(A: int) extern.
.mode ext(-int).
.decl out().
out().
"#;
    let planned = planned_src(src);
    let mut host = ExternRowsHost::default();
    let result = execute(&planned, &mut host);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Notes"
            && n.message
                .contains("extern relation `ext` returned `Ok(None)`")
            && n.message.contains("is not referenced by any rule goals")
            && n.message.contains("Declaration anchor:")
            && n.message.contains("<memory>:")
            && n.message.contains("Ok(Some(vec![]))")
    }));
}

#[test]
fn host_extern_rows_errors_are_distinct_from_ok_none_no_data() {
    let src = r#"
.decl ext(A: int) extern.
.mode ext(-int).
.decl out().
out() :- ext(_).
"#;
    let planned = planned_src(src);
    let mut host = ExternRowsHost::default().with_error(
        "ext",
        EngineHostError::new("extern_rows(ext)", "backend timeout"),
    );
    let result = execute(&planned, &mut host);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Notes"
            && n.message
                .contains("extern relation `ext` returned host error")
            && n.message
                .contains("host `extern_rows(ext)` failed: backend timeout")
            && n.message.contains("goal `ext(_)` in rule `out()`")
            && n.message.contains("<memory>:")
            && n.message.contains("Ok(None)")
    }));
    assert!(
        result.relations.get("out_status").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::String("partial".to_string())])
        })
    );
}

#[test]
fn stable_key_fallback_notes_mark_execution_partial() {
    let src = r#"
.decl src(V: Def) extern.
.mode src(-Def).
.decl mn(V: Def).
mn(V) :- V = min(X : src(X)).
"#;
    let planned = planned_src(src);
    let value_a = RuntimeValue::Host {
        kind: HostValueKind::Def,
        id: 0x71,
    };
    let value_b = RuntimeValue::Host {
        kind: HostValueKind::Def,
        id: 0x72,
    };
    let mut host = StableKeyFallbackHost::default()
        .with_rows("src", vec![vec![value_a], vec![value_b]])
        .with_fallback_id(0x71);
    let result = execute(&planned, &mut host);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Notes"
            && n.message
                .contains("host stable-key fallback for `Def` value `0x0000000000000071`")
            && n.message.contains("using deterministic fallback key")
    }));
    assert!(
        result.relations.get("out_status").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::String("partial".to_string())])
        })
    );
}

#[test]
fn choose_topk_k_type_mismatch_reports_tag_term_and_row_context() {
    let src = r#"
.decl kcfg(K: int) extern.
.mode kcfg(-int).
.decl candidate(G: int, Item: string, Score: int) input.
.decl grp(G: int) input.
.decl top_item(G: int, Score: int, Item: string).
grp(1).
candidate(1, "a", 7).
top_item(G, Score, Item) :-
  grp(G),
  kcfg(K),
  choose_topk("rank", K, G, Score, Item : candidate(G, Item, Score)).
"#;
    let planned = planned_src(src);
    let mut host = ExternRowsHost::default()
        .with_rows("kcfg", vec![vec![RuntimeValue::String("x".to_string())]]);
    let result = execute(&planned, &mut host);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
            n.section == "Errors"
                && n.message.contains("[RAQL0905]")
                && n.message.contains(
                    "choose_topk `rank` expected `k` term `K` to evaluate to `int`, found string (\"x\")"
                )
                && n.message.contains("row bindings:")
                && n.message.contains("K=\"x\"")
        }));
}

#[test]
fn choose_topk_k_negative_reports_actionable_runtime_context() {
    let src = r#"
.decl kcfg(K: int) extern.
.mode kcfg(-int).
.decl candidate(G: int, Item: string, Score: int) input.
.decl grp(G: int) input.
.decl top_item(G: int, Score: int, Item: string).
grp(1).
candidate(1, "a", 7).
top_item(G, Score, Item) :-
  grp(G),
  kcfg(K),
  choose_topk("rank", K, G, Score, Item : candidate(G, Item, Score)).
"#;
    let planned = planned_src(src);
    let mut host = ExternRowsHost::default().with_rows("kcfg", vec![vec![RuntimeValue::Int(-1)]]);
    let result = execute(&planned, &mut host);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
            n.section == "Errors"
                && n.message.contains("[RAQL0905]")
                && n.message.contains(
                    "choose_topk `rank` expected `k` term `K` to evaluate to a positive `int` (> 0), found int (-1)"
                )
                && n.message.contains("row bindings:")
                && n.message.contains("K=-1")
        }));
}

#[test]
fn choose_topk_k_zero_reports_actionable_runtime_context() {
    let src = r#"
.decl kcfg(K: int) extern.
.mode kcfg(-int).
.decl candidate(G: int, Item: string, Score: int) input.
.decl grp(G: int) input.
.decl top_item(G: int, Score: int, Item: string).
grp(1).
candidate(1, "a", 7).
top_item(G, Score, Item) :-
  grp(G),
  kcfg(K),
  choose_topk("rank", K, G, Score, Item : candidate(G, Item, Score)).
"#;
    let planned = planned_src(src);
    let mut host = ExternRowsHost::default().with_rows("kcfg", vec![vec![RuntimeValue::Int(0)]]);
    let result = execute(&planned, &mut host);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
            n.section == "Errors"
                && n.message.contains("[RAQL0905]")
                && n.message.contains(
                    "choose_topk `rank` expected `k` term `K` to evaluate to a positive `int` (> 0), found int (0)"
                )
                && n.message.contains("row bindings:")
                && n.message.contains("K=0")
        }));
}

#[test]
fn witness_path_negative_max_depth_reports_actionable_runtime_context() {
    let src = r#"
.type Def = { A, B }.
.type Span = { S }.
.func path_max_depth(N: int) input.
.mode path_max_depth(-int).
.func path_limit(N: int) input.
.mode path_limit(-int).
.decl graph_edge(Graph: string, From: Def, To: Def, Kind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: string) extern.
.mode witness_path(+string, +Def, +Def, -string).
.decl out().
path_max_depth(-1).
path_limit(1).
graph_edge("g", Def::A, Def::B, "edge", Span::S).
out() :- witness_path("g", Def::A, Def::B, _).
"#;
    let result = execute_src(src);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0905]")
            && n.message.contains(
                "input `path_max_depth` expected `int >= 0` for `witness_path`, found int (-1)",
            )
            && n.message.contains("set `path_max_depth` to 0 or greater")
    }));
}

#[test]
fn witness_path_non_positive_limit_reports_actionable_runtime_context() {
    let src = r#"
.type Def = { A, B }.
.type Span = { S }.
.func path_max_depth(N: int) input.
.mode path_max_depth(-int).
.func path_limit(N: int) input.
.mode path_limit(-int).
.decl graph_edge(Graph: string, From: Def, To: Def, Kind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: string) extern.
.mode witness_path(+string, +Def, +Def, -string).
.decl out().
path_max_depth(8).
path_limit(0).
graph_edge("g", Def::A, Def::B, "edge", Span::S).
out() :- witness_path("g", Def::A, Def::B, _).
"#;
    let result = execute_src(src);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0905]")
            && n.message
                .contains("input `path_limit` expected `int > 0` for `witness_path`, found int (0)")
            && n.message.contains("set `path_limit` to 1 or greater")
    }));
}

#[test]
fn witness_path_scalar_input_cardinality_violation_reports_actionable_runtime_context() {
    let src = r#"
.type Def = { A, B }.
.type Span = { S }.
.func path_max_depth(N: int) input.
.mode path_max_depth(-int).
.func path_limit(N: int) input.
.mode path_limit(-int).
.decl graph_edge(Graph: string, From: Def, To: Def, Kind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: string) extern.
.mode witness_path(+string, +Def, +Def, -string).
.decl out().
path_max_depth(4).
path_max_depth(8).
path_limit(1).
graph_edge("g", Def::A, Def::B, "edge", Span::S).
out() :- witness_path("g", Def::A, Def::B, _).
"#;
    let result = execute_src(src);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0907]")
            && n.message
                .contains("input `path_max_depth` expected exactly one scalar row, found 2 rows")
            && n.message.contains("row 1 =")
            && n.message.contains("row 2 =")
    }));
}

#[test]
fn witness_path_limit_cardinality_violation_reports_actionable_runtime_context() {
    let src = r#"
.type Def = { A, B }.
.type Span = { S }.
.func path_max_depth(N: int) input.
.mode path_max_depth(-int).
.func path_limit(N: int) input.
.mode path_limit(-int).
.decl graph_edge(Graph: string, From: Def, To: Def, Kind: string, Evidence: Span) input.
.decl witness_path(Graph: string, From: Def, To: Def, P: string) extern.
.mode witness_path(+string, +Def, +Def, -string).
.decl out().
path_max_depth(8).
path_limit(1).
path_limit(2).
graph_edge("g", Def::A, Def::B, "edge", Span::S).
out() :- witness_path("g", Def::A, Def::B, _).
"#;
    let result = execute_src(src);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
        n.section == "Errors"
            && n.message.contains("[RAQL0907]")
            && n.message
                .contains("input `path_limit` expected exactly one scalar row, found 2 rows")
            && n.message.contains("row 1 =")
            && n.message.contains("row 2 =")
    }));
}

#[test]
fn aggregate_sum_type_mismatch_reports_projection_context() {
    let src = r#"
.decl src(V: int) extern.
.mode src(-int).
.decl total(N: int).
total(N) :- N = sum(V : src(V)).
"#;
    let planned = planned_src(src);
    let mut host = ExternRowsHost::default()
        .with_rows("src", vec![vec![RuntimeValue::String("oops".to_string())]]);
    let result = execute(&planned, &mut host);

    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|n| {
            n.section == "Errors"
                && n.message.contains("[RAQL0905]")
                && n.message.contains(
                    "aggregate `sum` for output `N` expected projected values from `V` to be `int`, found string (\"oops\")"
                )
                && n.message.contains("row bindings: V=\"oops\"")
        }));
}

#[test]
fn builtin_helpers_execute_without_host_relations() {
    let src = r#"
.decl doc(Opt: option<string>) input.
.decl contains(Haystack: string, Needle: string) extern.
.decl starts_with(S: string, Prefix: string) extern.
.decl fmt(Format: string, Args: list<string>, Out: string) extern.
.func coalesce(Opt: option<string>, Default: string, Out: string) extern.
.decl out(S: string).
doc(none).
out(S) :-
  doc(Opt),
  coalesce(Opt, "fallback", C),
  fmt("pre-{}-suf", [C], S),
  starts_with(S, "pre"),
  contains(S, "fallback").
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(result.relations.get("out").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("pre-fallback-suf".to_string())])
    }));
}

#[test]
fn aggregate_empty_input_semantics_match_spec() {
    let src = r#"
.decl src(V: int) input.
.decl c(N: int).
.decl s(N: int).
.decl mn(N: int).
.decl mx(N: int).
c(N) :- N = count(V : src(V)).
s(N) :- N = sum(V : src(V)).
mn(N) :- N = min(V : src(V)).
mx(N) :- N = max(V : src(V)).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("c")
            .is_some_and(|rows| rows.contains(&vec![RuntimeValue::Int(0)]))
    );
    assert!(
        result
            .relations
            .get("s")
            .is_some_and(|rows| rows.contains(&vec![RuntimeValue::Int(0)]))
    );
    assert_eq!(result.relations.get("mn").map(|r| r.len()), Some(0));
    assert_eq!(result.relations.get("mx").map(|r| r.len()), Some(0));
}

#[test]
fn aggregate_count_uses_set_semantics_for_rows_and_projection() {
    let src = r#"
.decl pair(X: int, Y: int) input.
.decl c_proj(N: int).
.decl c_rows(N: int).
pair(1, 10).
c_proj(N) :- N = count(V : (pair(_, V); pair(_, V))).
c_rows(N) :- N = count((pair(X, Y); pair(X, Y))).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("c_proj")
            .is_some_and(|rows| rows.contains(&vec![RuntimeValue::Int(1)]))
    );
    assert!(
        result
            .relations
            .get("c_rows")
            .is_some_and(|rows| rows.contains(&vec![RuntimeValue::Int(1)]))
    );
}

#[test]
fn atom_matching_binds_nested_some_and_list_variables() {
    let src = r#"
.decl src_opt(V: option<int>) input.
.decl src_list(V: list<int>) input.
.decl out_opt(X: int).
.decl out_list(A: int, B: int).
src_opt(some(7)).
src_list([1,2]).
out_opt(X) :- src_opt(some(X)).
out_list(A, B) :- src_list([A, B]).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("out_opt")
            .is_some_and(|rows| rows.contains(&vec![RuntimeValue::Int(7)]))
    );
    assert!(
        result.relations.get("out_list").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::Int(1), RuntimeValue::Int(2)])
        })
    );
}

#[test]
fn equality_constraint_unifies_structural_terms_and_binds_nested_vars() {
    let src = r#"
.decl src(V: option<int>) input.
.decl out(X: int).
src(some(9)).
out(X) :- src(V), V = some(X).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("out")
            .is_some_and(|rows| rows.contains(&vec![RuntimeValue::Int(9)]))
    );
}

#[test]
fn equality_constraint_can_bind_after_planner_defers_until_ground() {
    let src = r#"
.decl p(X: int) input.
.decl q(X: int).
p(1).
q(X) :- X = Y, p(Y).
"#;
    let result = execute_src(src);
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("q")
            .is_some_and(|rows| rows.contains(&vec![RuntimeValue::Int(1)]))
    );
}
