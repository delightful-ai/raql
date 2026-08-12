//! Engine semantics over the full pipeline: programs compile through
//! resolve → typecheck → plan (catalog externs, demand planning) and
//! execute against a canned [`OperatorSet`]. The value type is a test
//! double; `Sym` stands in for opaque semantic handles (defs, spans) —
//! equality and hashing only, exactly like RA handles.

use std::collections::{BTreeMap, HashMap};

use raql_plan::{EngineValue, OperatorId, OperatorSet};

use crate::{EvalStatus, execute};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TestValue {
    Int(i64),
    Str(String),
    Bool(bool),
    Enum(String, String),
    Opt(Option<Box<TestValue>>),
    List(Vec<TestValue>),
    /// An opaque handle: no ordering, no projection.
    Sym(&'static str),
}

use TestValue::{Int, Sym};

fn s(text: &str) -> TestValue {
    TestValue::Str(text.to_string())
}

impl EngineValue for TestValue {
    fn int(value: i64) -> Self {
        Int(value)
    }

    fn string(value: &str) -> Self {
        s(value)
    }

    fn boolean(value: bool) -> Self {
        TestValue::Bool(value)
    }

    fn enum_tag(ty: &str, variant: &str) -> Self {
        TestValue::Enum(ty.to_string(), variant.to_string())
    }

    fn none() -> Self {
        TestValue::Opt(None)
    }

    fn some(inner: Self) -> Self {
        TestValue::Opt(Some(Box::new(inner)))
    }

    fn list(items: Vec<Self>) -> Self {
        TestValue::List(items)
    }

    fn as_int(&self) -> Option<i64> {
        match self {
            Int(value) => Some(*value),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            TestValue::Str(value) => Some(value),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            TestValue::Bool(value) => Some(*value),
            _ => None,
        }
    }

    fn as_enum(&self) -> Option<(&str, &str)> {
        match self {
            TestValue::Enum(ty, variant) => Some((ty, variant)),
            _ => None,
        }
    }

    fn as_option(&self) -> Option<Option<&Self>> {
        match self {
            TestValue::Opt(inner) => Some(inner.as_deref()),
            _ => None,
        }
    }

    fn as_list(&self) -> Option<&[Self]> {
        match self {
            TestValue::List(items) => Some(items),
            _ => None,
        }
    }

    fn plain_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Int(a), Int(b)) => Some(a.cmp(b)),
            (TestValue::Str(a), TestValue::Str(b)) => Some(a.cmp(b)),
            (TestValue::Bool(a), TestValue::Bool(b)) => Some(a.cmp(b)),
            (TestValue::Enum(a, av), TestValue::Enum(b, bv)) => {
                Some(a.cmp(b).then_with(|| av.cmp(bv)))
            }
            _ => None,
        }
    }

    fn type_tag(&self) -> String {
        match self {
            Int(_) => "int".to_string(),
            TestValue::Str(_) => "string".to_string(),
            TestValue::Bool(_) => "bool".to_string(),
            TestValue::Enum(ty, _) => ty.clone(),
            TestValue::Opt(_) => "option<_>".to_string(),
            TestValue::List(_) => "list<_>".to_string(),
            Sym(_) => "Sym".to_string(),
        }
    }
}

/// Canned operator extents: each operator filters its full extent by the
/// mode's bound positions, mirroring the catalog contract (bound inputs in
/// declaration order, full tuples out).
#[derive(Default)]
struct TestOps {
    extents: HashMap<OperatorId, (Vec<usize>, Vec<Vec<TestValue>>)>,
    calls: HashMap<OperatorId, usize>,
}

impl TestOps {
    fn with(mut self, operator: OperatorId, bound: &[usize], rows: Vec<Vec<TestValue>>) -> Self {
        self.extents.insert(operator, (bound.to_vec(), rows));
        self
    }

    fn calls(&self, operator: OperatorId) -> usize {
        self.calls.get(&operator).copied().unwrap_or(0)
    }
}

impl OperatorSet for TestOps {
    type Value = TestValue;
    type Error = String;

    fn invoke(
        &mut self,
        operator: OperatorId,
        inputs: &[TestValue],
    ) -> Result<Vec<Vec<TestValue>>, String> {
        *self.calls.entry(operator).or_default() += 1;
        let (bound, extent) = self
            .extents
            .get(&operator)
            .ok_or_else(|| format!("no canned extent for {}", operator.name()))?;
        assert_eq!(bound.len(), inputs.len(), "operator input arity");
        Ok(extent
            .iter()
            .filter(|row| bound.iter().zip(inputs).all(|(position, input)| &row[*position] == input))
            .cloned()
            .collect())
    }
}

fn compile(source: &str) -> raql_compiler::PlannedProgram {
    let parsed = raql_syntax::parse_program(source).expect("parse");
    let resolved = raql_compiler::resolve(parsed).expect("resolve");
    let typed = raql_compiler::typecheck(resolved).expect("typecheck");
    raql_compiler::plan(typed).unwrap_or_else(|diags| panic!("plan: {diags:#?}"))
}

fn run(source: &str, ops: &mut TestOps) -> crate::EvalResult<TestValue> {
    run_with_inputs(source, ops, &BTreeMap::new())
}

fn run_with_inputs(
    source: &str,
    ops: &mut TestOps,
    inputs: &BTreeMap<String, Vec<Vec<TestValue>>>,
) -> crate::EvalResult<TestValue> {
    execute(&compile(source), inputs, ops)
}

fn rows(result: &crate::EvalResult<TestValue>, relation: &str) -> Vec<Vec<TestValue>> {
    result
        .relations
        .get(relation)
        .unwrap_or_else(|| panic!("relation `{relation}` in {:?}", result.relations.keys()))
        .iter()
        .cloned()
        .collect()
}

fn sorted(mut rows: Vec<Vec<TestValue>>) -> Vec<Vec<TestValue>> {
    rows.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
    rows
}

// ---------------------------------------------------------------------
// Core row semantics: facts, joins, recursion, negation
// ---------------------------------------------------------------------

#[test]
fn facts_join_through_derived_rules() {
    let source = r#"
.decl edge(A: int, B: int).
edge(1, 2).
edge(2, 3).
.decl two_hop(A: int, C: int) output.
two_hop(A, C) :- edge(A, B), edge(B, C).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(result.status, EvalStatus::Ok);
    assert_eq!(rows(&result, "two_hop"), vec![vec![Int(1), Int(3)]]);
}

#[test]
fn input_relations_come_from_the_request() {
    let source = r#"
.decl seed(X: int) input.
.decl out(X: int) output.
out(X) :- seed(X).
"#;
    let mut inputs = BTreeMap::new();
    inputs.insert("seed".to_string(), vec![vec![Int(7)], vec![Int(9)]]);
    let result = run_with_inputs(source, &mut TestOps::default(), &inputs);
    assert_eq!(sorted(rows(&result, "out")), vec![vec![Int(7)], vec![Int(9)]]);
}

#[test]
fn recursion_reaches_the_fixpoint() {
    let source = r#"
.decl edge(A: int, B: int).
edge(1, 2).
edge(2, 3).
edge(3, 4).
.decl reach(A: int, B: int) output.
reach(A, B) :- edge(A, B).
reach(A, C) :- edge(A, B), reach(B, C).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(result.status, EvalStatus::Ok);
    assert_eq!(rows(&result, "reach").len(), 6);
}

#[test]
fn recursion_hits_the_iteration_cap_honestly() {
    // Successor-style growth with no natural bound below the cap: the cap
    // fires, the result degrades to Partial, and the note says so.
    let source = r#"
.pragma max_iters = 4.
.decl n(X: int) output.
n(0).
n(Y) :- n(X), X < 100, Y := X + 1.
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|note| note.message.contains("RAQL0910")), "{:?}", result.notes);
}

#[test]
fn negation_is_stratified_and_seeded() {
    let source = r#"
.decl candidate(X: int).
candidate(1).
candidate(2).
.decl blocked(X: int).
blocked(2).
.decl allowed(X: int) output.
allowed(X) :- candidate(X), not blocked(X).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(rows(&result, "allowed"), vec![vec![Int(1)]]);
}

#[test]
fn disjunction_flattens_into_a_union() {
    let source = r#"
.decl a(X: int).
a(1).
.decl b(X: int).
b(2).
.decl either(X: int) output.
either(X) :- (a(X) ; b(X)).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(sorted(rows(&result, "either")), vec![vec![Int(1)], vec![Int(2)]]);
}

// ---------------------------------------------------------------------
// Constraints and engine builtins
// ---------------------------------------------------------------------

#[test]
fn constraints_bind_compare_and_compute() {
    let source = r#"
.decl item(X: int).
item(2).
item(5).
.decl out(X: int, Doubled: int) output.
out(X, D) :- item(X), X >= 3, D := X * 2 + 1.
.decl aliased(Y: int) output.
aliased(Y) :- item(X), Y = X.
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(rows(&result, "out"), vec![vec![Int(5), Int(11)]]);
    assert_eq!(sorted(rows(&result, "aliased")), vec![vec![Int(2)], vec![Int(5)]]);
}

#[test]
fn division_by_zero_degrades_to_partial() {
    let source = r#"
.decl item(X: int).
item(0).
.decl out(Y: int) output.
out(Y) :- item(X), Y := 1 / X.
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(result.notes.iter().any(|note| note.message.contains("RAQL0901")));
    assert_eq!(rows(&result, "out"), Vec::<Vec<TestValue>>::new());
}

#[test]
fn string_builtins_filter_and_format() {
    let source = r#"
.decl name(N: string).
name("alpha").
name("beta").
.decl hit(N: string) output.
hit(N) :- name(N), starts_with(N, "al").
.decl misses(N: string) output.
misses(N) :- name(N), not contains(N, "et").
.decl fmt(F: string, Args: list<string>, Out: string) extern.
.decl msg(M: string) output.
msg(M) :- name(N), starts_with(N, "al"), fmt("hello {}", [N], M).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(rows(&result, "hit"), vec![vec![s("alpha")]]);
    assert_eq!(rows(&result, "misses"), vec![vec![s("alpha")]]);
    assert_eq!(rows(&result, "msg"), vec![vec![s("hello alpha")]]);
}

#[test]
fn coalesce_and_options_round_trip() {
    let source = r#"
.decl raw(V: option<int>).
raw(none).
raw(some(3)).
.func coalesce(Opt: option<int>, Default: int, Out: int) extern.
.decl out(V: int) output.
out(V) :- raw(O), coalesce(O, 0, V).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(result.status, EvalStatus::Ok, "{:?}", result.notes);
    assert_eq!(sorted(rows(&result, "out")), vec![vec![Int(0)], vec![Int(3)]]);
}

// ---------------------------------------------------------------------
// Aggregates and choose_topk
// ---------------------------------------------------------------------

#[test]
fn aggregates_group_per_correlated_binding() {
    let source = r#"
.decl sale(Shop: string, Item: string, Price: int).
sale("north", "apple", 3).
sale("north", "pear", 5).
sale("south", "apple", 2).
.decl report(Shop: string, Items: int, Total: int, Cheapest: int) output.
report(Shop, Items, Total, Cheapest) :-
  sale(Shop, _, _),
  Items = count(I : sale(Shop, I, _)),
  Total = sum(P : sale(Shop, _, P)),
  Cheapest = min(P : sale(Shop, _, P)).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(result.status, EvalStatus::Ok, "{:?}", result.notes);
    assert_eq!(
        sorted(rows(&result, "report")),
        vec![
            vec![s("north"), Int(2), Int(8), Int(3)],
            vec![s("south"), Int(1), Int(2), Int(2)],
        ],
    );
}

#[test]
fn min_over_an_empty_extent_fails_the_goal() {
    let source = r#"
.decl seed(X: int).
seed(1).
.decl empty(X: int).
.decl out(M: int) output.
out(M) :- seed(_), M = min(V : empty(V)).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(result.status, EvalStatus::Ok);
    assert_eq!(rows(&result, "out"), Vec::<Vec<TestValue>>::new());
}

#[test]
fn count_over_an_empty_extent_binds_zero() {
    let source = r#"
.decl seed(X: int).
seed(1).
.decl empty(X: int).
.decl out(N: int) output.
out(N) :- seed(_), N = count(V : empty(V)).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(rows(&result, "out"), vec![vec![Int(0)]]);
}

#[test]
fn choose_topk_selects_by_score_descending() {
    let source = r#"
.decl scored(Item: string, Score: int).
scored("low", 1).
scored("mid", 5).
scored("high", 9).
.decl top(Item: string, Score: int) output.
top(I, S) :- choose_topk("t", 2, 1, Score, Item : scored(Item, Score)), I = Item, S = Score.
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(
        sorted(rows(&result, "top")),
        vec![vec![s("high"), Int(9)], vec![s("mid"), Int(5)]],
    );
}

// ---------------------------------------------------------------------
// Witness paths
// ---------------------------------------------------------------------

#[test]
fn witness_path_walks_derived_graph_edges() {
    let source = r#"
.decl graph_edge(Graph: string, From: Def, To: Def, EdgeKind: string, Evidence: Span).
.decl link(F: Def, T: Def, S: Span).
.decl hops(Seq: int, From: Def, To: Def) output.
graph_edge("g", F, T, "link", S) :- link(F, T, S).
path_limit(1).
.decl path_limit(N: int).
.decl start(D: Def) input.
.decl goal(D: Def) input.
hops(Seq, From, To) :-
  start(A), goal(B),
  witness_path("g", A, B, P),
  path_hop(P, Seq, From, To, _, _).
"#;
    let mut inputs = BTreeMap::new();
    inputs.insert("start".to_string(), vec![vec![Sym("a")]]);
    inputs.insert("goal".to_string(), vec![vec![Sym("c")]]);
    let mut ops = TestOps::default();
    // link is a plain derived relation populated by facts — but facts
    // cannot carry Sym handles, so feed it as an input relation instead.
    let source = source.replace(".decl link(F: Def, T: Def, S: Span).", ".decl link(F: Def, T: Def, S: Span) input.");
    inputs.insert(
        "link".to_string(),
        vec![
            vec![Sym("a"), Sym("b"), Sym("s1")],
            vec![Sym("b"), Sym("c"), Sym("s2")],
        ],
    );
    let result = run_with_inputs(&source, &mut ops, &inputs);
    assert_eq!(result.status, EvalStatus::Ok, "{:?}", result.notes);
    assert_eq!(
        sorted(rows(&result, "hops")),
        vec![
            vec![Int(0), Sym("a"), Sym("b")],
            vec![Int(1), Sym("b"), Sym("c")],
        ],
    );
}

// ---------------------------------------------------------------------
// Catalog operators: invocation, modes, demand memoization
// ---------------------------------------------------------------------

fn def_world() -> TestOps {
    let defs = [(Sym("f1"), "alpha", "FN"),
        (Sym("f2"), "beta", "FN"),
        (Sym("s1"), "alpha", "STRUCT")];
    TestOps::default()
        .with(
            OperatorId::NameOfDef,
            &[0],
            defs.iter().map(|(d, n, _)| vec![d.clone(), s(n)]).collect(),
        )
        .with(
            OperatorId::DefsByExactName,
            &[1],
            defs.iter().map(|(d, n, _)| vec![d.clone(), s(n)]).collect(),
        )
        .with(
            OperatorId::KindOfDef,
            &[0],
            defs.iter()
                .map(|(d, _, k)| vec![d.clone(), TestValue::enum_tag("DefKind", k)])
                .collect(),
        )
        .with(OperatorId::DefsScan, &[], defs.iter().map(|(d, _, _)| vec![d.clone()]).collect())
}

#[test]
fn extern_goals_invoke_the_catalog_operator_for_their_mode() {
    let source = r#"
.type DefKind = { FN, METHOD, STRUCT }.
.decl hit(D: Def) output.
hit(D) :- def_name(D, "alpha"), def_kind(D, DefKind::FN).
"#;
    let mut ops = def_world();
    let result = run(source, &mut ops);
    assert_eq!(result.status, EvalStatus::Ok, "{:?}", result.notes);
    assert_eq!(rows(&result, "hit"), vec![vec![Sym("f1")]]);
    // The name seed ran (not the scan), and the kind filter ran per
    // candidate def.
    assert_eq!(ops.calls(OperatorId::DefsByExactName), 1);
    assert_eq!(ops.calls(OperatorId::DefsScan), 0);
    assert_eq!(ops.calls(OperatorId::KindOfDef), 2);
}

#[test]
fn declared_scans_enumerate() {
    let source = r#"
.decl all_names(N: string) output.
all_names(N) :- def(D), def_name(D, N).
"#;
    let mut ops = def_world();
    let result = run(source, &mut ops);
    // Set semantics: two defs named "alpha" project to one row.
    assert_eq!(sorted(rows(&result, "all_names")), vec![vec![s("alpha")], vec![s("beta")]]);
    assert_eq!(ops.calls(OperatorId::DefsScan), 1);
}

#[test]
fn demanded_specializations_memoize_per_seed() {
    // is_fn is demanded once per distinct def; the second rule referencing
    // it must reuse the memoized specialization rows.
    let source = r#"
.decl seed(D: Def) input.
.type DefKind = { FN, METHOD, STRUCT }.
.decl is_fn(D: Def).
.mode is_fn(+Def).
is_fn(D) :- def_kind(D, DefKind::FN).
.decl once(D: Def) output.
once(D) :- seed(D), is_fn(D).
.decl twice(D: Def) output.
twice(D) :- seed(D), is_fn(D), is_fn(D).
"#;
    let mut inputs = BTreeMap::new();
    inputs.insert("seed".to_string(), vec![vec![Sym("f1")]]);
    let mut ops = def_world();
    let result = run_with_inputs(source, &mut ops, &inputs);
    assert_eq!(result.status, EvalStatus::Ok, "{:?}", result.notes);
    assert_eq!(rows(&result, "once"), vec![vec![Sym("f1")]]);
    assert_eq!(rows(&result, "twice"), vec![vec![Sym("f1")]]);
    // One def, one demanded (is_fn, (+), [f1]) specialization: one
    // def_kind invocation despite three call sites.
    assert_eq!(ops.calls(OperatorId::KindOfDef), 1);
}

#[test]
fn operator_errors_are_errors_not_empty_relations() {
    let source = r#"
.decl hit(D: Def) output.
hit(D) :- def(D).
"#;
    // No canned extent for DefsScan.
    let mut ops = TestOps::default();
    let result = run(source, &mut ops);
    assert_eq!(result.status, EvalStatus::Partial);
    assert!(
        result.notes.iter().any(|note| note.message.contains("RAQL0909")),
        "{:?}",
        result.notes,
    );
}

#[test]
fn out_status_reflects_the_evaluation() {
    let source = r#"
.decl ok(X: int) output.
ok(1).
"#;
    let result = run(source, &mut TestOps::default());
    assert_eq!(rows(&result, "out_status"), vec![vec![s("ok")]]);
    assert_eq!(rows(&result, "ok"), vec![vec![Int(1)]]);
}
