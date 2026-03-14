use camino::Utf8PathBuf;
use raql_compiler::{plan, resolve, typecheck};
use raql_engine::{EvalStatus, RuntimeValue, execute};
use raql_host::{DefId, NodeId, SpanCoord, SpanId, SpanKey, TypeRefId};
use raql_host_ra::legacy::LegacyRaHostRuntime;
use raql_host_ra::{GenericArg, NodeKind, TypeShape};
use raql_ir::StableId;
use raql_syntax::parse_program;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

// TODO(ra-daemon-cutover): migrate this file off the eager direct runtime and
// onto explicit daemon-backed or host-fixture coverage, then remove the legacy
// runtime dependency entirely.

fn temp_workspace_root(label: &str) -> Utf8PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "raql_host_ra_extern_dispatch_{label}_{}_{}",
        std::process::id(),
        stamp
    ));
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"
[package]
name = "extern_dispatch_{stamp}"
version = "0.0.0"
edition = "2021"
"#
        ),
    )
    .expect("write Cargo.toml");
    fs::write(root.join("src/lib.rs"), "pub fn fixture_seed() {}\n").expect("write lib.rs");
    Utf8PathBuf::from_path_buf(root).expect("utf8 path")
}

#[test]
fn engine_dispatches_required_ra_extern_predicates() {
    let root = temp_workspace_root("extern_rows");
    let mut runtime = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");

    let tr = TypeRefId::new(StableId::new(0x100));
    let arg = TypeRefId::new(StableId::new(0x101));
    let head = DefId::new(StableId::new(0x200));
    runtime.insert_type(
        tr,
        TypeShape::App {
            head,
            args: vec![GenericArg::Type(arg)],
        },
    );
    runtime.insert_type(arg, TypeShape::Prim("i32".to_string()));
    runtime.insert_handle(head, "def://Result");

    let span = SpanId::new(StableId::new(0x300));
    runtime.insert_span(
        span,
        SpanKey::new(
            "crates/raql-host-ra/src/lib.rs",
            SpanCoord::new(1, 0),
            SpanCoord::new(1, 8),
        ),
    );
    let node = NodeId::new(StableId::new(0x301));
    runtime.insert_node(node, NodeKind::If, span, None);

    let src = r#"
.type NodeKind = { IF, MATCH, WHILE, FOR, LOOP, BLOCK, TRY, ARM, OTHER }.
.decl ty_app(TR: TypeRef, Head: Def) extern.
.decl ty_arg(TR: TypeRef, Index: int, Arg: TypeRef) extern.
.decl ty_prim(TR: TypeRef, Name: string) extern.
.decl handle(D: Def, H: string) extern.
.decl node_kind(N: Node, K: NodeKind) extern.
.decl node_span(N: Node, S: Span) extern.
.decl node_at(S: Span, N: option<Node>) extern.
.decl span_key(S: Span, RelPath: string, L0: int, C0: int, L1: int, C1: int) extern.

.decl type_row(I: int, Name: string, Handle: string).
.decl node_row(RelPath: string, Kind: NodeKind).

type_row(I, Name, Handle) :-
  ty_app(TR, D),
  ty_arg(TR, I, Arg),
  ty_prim(Arg, Name),
  handle(D, Handle).

node_row(RelPath, Kind) :-
  node_kind(N, Kind),
  node_span(N, S),
  node_at(S, some(N)),
  span_key(S, RelPath, _L0, _C0, _L1, _C1).
"#;

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let result = execute(&planned, &mut runtime);

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(result.relations.get("type_row").is_some_and(|rows| {
        rows.contains(&vec![
            RuntimeValue::Int(0),
            RuntimeValue::String("i32".to_string()),
            RuntimeValue::String("def://Result".to_string()),
        ])
    }));
    assert!(result.relations.get("node_row").is_some_and(|rows| {
        rows.contains(&vec![
            RuntimeValue::String("crates/raql-host-ra/src/lib.rs".to_string()),
            RuntimeValue::Enum {
                name: "NodeKind".to_string(),
                variant: "IF".to_string(),
            },
        ])
    }));
}

fn seeded_runtime_for_enclosing_control() -> LegacyRaHostRuntime {
    let root = temp_workspace_root("enclosing_control");
    let mut runtime = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");

    let query_span = SpanId::new(StableId::new(0x500));
    let expr_span = SpanId::new(StableId::new(0x501));
    let block_span = SpanId::new(StableId::new(0x502));
    let if_span = SpanId::new(StableId::new(0x503));

    runtime.insert_span(
        query_span,
        SpanKey::new(
            "crates/raql-host-ra/src/lib.rs",
            SpanCoord::new(12, 7),
            SpanCoord::new(12, 8),
        ),
    );
    runtime.insert_span(
        expr_span,
        SpanKey::new(
            "crates/raql-host-ra/src/lib.rs",
            SpanCoord::new(12, 6),
            SpanCoord::new(12, 9),
        ),
    );
    runtime.insert_span(
        block_span,
        SpanKey::new(
            "crates/raql-host-ra/src/lib.rs",
            SpanCoord::new(12, 4),
            SpanCoord::new(14, 1),
        ),
    );
    runtime.insert_span(
        if_span,
        SpanKey::new(
            "crates/raql-host-ra/src/lib.rs",
            SpanCoord::new(10, 0),
            SpanCoord::new(16, 1),
        ),
    );

    let if_node = NodeId::new(StableId::new(0x510));
    let block_node = NodeId::new(StableId::new(0x511));
    let expr_node = NodeId::new(StableId::new(0x512));

    runtime.insert_node(if_node, NodeKind::If, if_span, None);
    runtime.insert_node(block_node, NodeKind::Block, block_span, Some(if_node));
    runtime.insert_node(expr_node, NodeKind::Expr, expr_span, Some(block_node));

    runtime
}

#[test]
fn engine_executes_enclosing_control_with_default_depth() {
    let mut runtime = seeded_runtime_for_enclosing_control();
    let src = r#"
.type NodeKind = { IF, MATCH, WHILE, FOR, LOOP, BLOCK, TRY, ARM, OTHER }.
.decl span_key(S: Span, RelPath: string, L0: int, C0: int, L1: int, C1: int) extern.
.decl enclosing_control(S: Span, K: NodeKind, ControlS: Span, Dist: int) extern.
.decl hit(Dist: int).

hit(Dist) :-
  span_key(S, "crates/raql-host-ra/src/lib.rs", 12, 7, 12, 8),
  enclosing_control(S, NodeKind::IF, _ControlS, Dist).
"#;

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let result = execute(&planned, &mut runtime);

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("hit")
            .is_some_and(|rows| { rows.contains(&vec![RuntimeValue::Int(2)]) })
    );
}

#[test]
fn engine_executes_enclosing_control_with_overridden_depth_limit() {
    let mut runtime = seeded_runtime_for_enclosing_control();
    let src = r#"
.type NodeKind = { IF, MATCH, WHILE, FOR, LOOP, BLOCK, TRY, ARM, OTHER }.
.decl span_key(S: Span, RelPath: string, L0: int, C0: int, L1: int, C1: int) extern.
.decl enclosing_control(S: Span, K: NodeKind, ControlS: Span, Dist: int) extern.
.decl control_max_depth(N: int) input.
.decl hit(Dist: int).

control_max_depth(1).
hit(Dist) :-
  span_key(S, "crates/raql-host-ra/src/lib.rs", 12, 7, 12, 8),
  enclosing_control(S, NodeKind::IF, _ControlS, Dist).
"#;

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let result = execute(&planned, &mut runtime);

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("hit")
            .is_some_and(|rows| rows.is_empty())
    );
}
