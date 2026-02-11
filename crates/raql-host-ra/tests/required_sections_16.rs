use raql_engine::{EngineHostView, HostValueKind, RuntimeValue};
use raql_host::HostRuntime;
use raql_host_ra::{
    DefId, DeterministicRaHost, GenericArg, Mutability, NodeId, NodeKind, RaHostError, SpanCoord,
    SpanId, SpanKey, StableId, TypeRefId, TypeShape,
};
use span::{EditionedFileId, TextRange, TextSize};
use vfs::FileId;

fn def(raw: u64) -> DefId {
    DefId::new(StableId::new(raw))
}

fn type_ref(raw: u64) -> TypeRefId {
    TypeRefId::new(StableId::new(raw))
}

fn node(raw: u64) -> NodeId {
    NodeId::new(StableId::new(raw))
}

fn span(raw: u64) -> SpanId {
    SpanId::new(StableId::new(raw))
}

fn key(path: &str, l0: u32, c0: u32, l1: u32, c1: u32) -> SpanKey {
    SpanKey::new(path, SpanCoord::new(l0, c0), SpanCoord::new(l1, c1))
}

fn editioned_file_id(raw_file_id: u32) -> EditionedFileId {
    EditionedFileId::current_edition(FileId::from_raw(raw_file_id))
}

#[test]
fn ty_app_and_ty_arg_are_zero_based_and_type_only() {
    let mut host = DeterministicRaHost::new();
    let tr = type_ref(0x10);
    let arg_ok = type_ref(0x11);
    let arg_err = type_ref(0x12);
    let head = def(0x20);

    host.insert_type(
        tr,
        TypeShape::App {
            head,
            args: vec![
                GenericArg::Lifetime,
                GenericArg::Type(arg_ok),
                GenericArg::Const,
                GenericArg::Type(arg_err),
            ],
        },
    );

    assert_eq!(host.ty_app(tr), Some(head));
    assert_eq!(host.ty_args(tr), vec![(0, arg_ok), (1, arg_err)]);
    assert_eq!(host.ty_arg(tr, 0), Some(arg_ok));
    assert_eq!(host.ty_arg(tr, 1), Some(arg_err));
    assert_eq!(host.ty_arg(tr, 2), None);
}

#[test]
fn wrappers_are_exposed_with_stable_indexing() {
    let mut host = DeterministicRaHost::new();

    let inner = type_ref(0x30);
    let ref_tr = type_ref(0x31);
    let ptr_tr = type_ref(0x32);
    let tuple_tr = type_ref(0x33);
    let slice_tr = type_ref(0x34);
    let tuple_b = type_ref(0x35);

    host.insert_type(
        ref_tr,
        TypeShape::Ref {
            mutability: Mutability::Mut,
            inner,
        },
    );
    host.insert_type(
        ptr_tr,
        TypeShape::Ptr {
            mutability: Mutability::Shared,
            inner,
        },
    );
    host.insert_type(tuple_tr, TypeShape::Tuple(vec![inner, tuple_b]));
    host.insert_type(slice_tr, TypeShape::Slice(inner));

    assert_eq!(host.ty_ref(ref_tr), Some((Mutability::Mut, inner)));
    assert_eq!(host.ty_ptr(ptr_tr), Some((Mutability::Shared, inner)));
    assert_eq!(host.ty_tuples(tuple_tr), vec![(0, inner), (1, tuple_b)]);
    assert_eq!(host.ty_tuple(tuple_tr, 0), Some(inner));
    assert_eq!(host.ty_tuple(tuple_tr, 1), Some(tuple_b));
    assert_eq!(host.ty_tuple(tuple_tr, 2), None);
    assert_eq!(host.ty_slice(slice_tr), Some(inner));
}

#[test]
fn parameters_primitives_and_unknown_are_exposed() {
    let mut host = DeterministicRaHost::new();

    let param_tr = type_ref(0x40);
    let prim_tr = type_ref(0x41);
    let unknown_tr = type_ref(0x42);

    let param_def = def(0x55);
    host.insert_type(param_tr, TypeShape::Param(param_def));
    host.insert_type(prim_tr, TypeShape::Prim("i32".into()));
    host.insert_type(unknown_tr, TypeShape::Unknown);

    assert_eq!(host.ty_param(param_tr), Some(param_def));
    assert_eq!(host.ty_prim(prim_tr), Some("i32"));
    assert!(host.ty_unknown(unknown_tr));
    assert!(!host.ty_unknown(param_tr));
}

#[test]
fn stable_order_ids_are_deterministic_and_overridable() {
    let a = DeterministicRaHost::new();
    let b = DeterministicRaHost::new();

    let tr = type_ref(0x77);
    let n = node(0x88);

    assert_eq!(a.typeref_id(tr), b.typeref_id(tr));
    assert_eq!(a.node_id(n), b.node_id(n));
    assert_eq!(a.typeref_id(tr).as_str(), "typeref_id:0x0000000000000077");
    assert_eq!(a.node_id(n).as_str(), "node_id:0x0000000000000088");

    let mut c = DeterministicRaHost::new();
    c.insert_typeref_id(tr, "typeref://result");
    c.insert_node_id(n, "node://stmt");

    assert_eq!(c.typeref_id(tr).as_str(), "typeref://result");
    assert_eq!(c.node_id(n).as_str(), "node://stmt");
}

#[test]
fn stable_key_fallbacks_are_visible_and_deterministic() {
    let mut host_a = DeterministicRaHost::new();
    let mut host_b = DeterministicRaHost::new();
    let def = def(0x700);
    let span = span(0x701);
    let def_value = RuntimeValue::Host {
        kind: HostValueKind::Def,
        id: def.stable_id().as_u64(),
    };
    let span_value = RuntimeValue::Host {
        kind: HostValueKind::Span,
        id: span.stable_id().as_u64(),
    };

    let def_key_a = EngineHostView::stable_key(&mut host_a, &def_value);
    let span_key_a = EngineHostView::stable_key(&mut host_a, &span_value);
    let def_key_b = EngineHostView::stable_key(&mut host_b, &def_value);
    let span_key_b = EngineHostView::stable_key(&mut host_b, &span_value);

    assert_eq!(def_key_a, def_key_b);
    assert_eq!(span_key_a, span_key_b);
    assert_eq!(def_key_a, "ra-host:fallback:handle:0x0000000000000700");
    assert!(span_key_a.starts_with("ra/fallback/0000000000000701.rs:"));
}

#[test]
fn span_key_relation_exposes_fallback_rows_for_unregistered_node_spans() {
    let mut host = DeterministicRaHost::new();
    let orphan_span = span(0x710);
    host.insert_node(node(0x711), NodeKind::Expr, orphan_span, None);

    let fallback = host.span_key(orphan_span).expect("fallback span key");
    let rows = EngineHostView::extern_relation_rows(&mut host, "span_key")
        .expect("span key lookup should succeed")
        .expect("span key rows");

    assert!(fallback.rel_path().contains("/fallback/"));
    assert_eq!(
        rows,
        vec![vec![
            RuntimeValue::Host {
                kind: HostValueKind::Span,
                id: orphan_span.stable_id().as_u64(),
            },
            RuntimeValue::String(fallback.rel_path().to_string()),
            RuntimeValue::Int(fallback.start().line() as i64),
            RuntimeValue::Int(fallback.start().column() as i64),
            RuntimeValue::Int(fallback.end().line() as i64),
            RuntimeValue::Int(fallback.end().column() as i64),
        ]]
    );
}

#[test]
fn node_at_picks_the_most_specific_containing_node() {
    let mut host = DeterministicRaHost::new();

    let query = span(0x200);
    let outer_span = span(0x201);
    let inner_span = span(0x202);
    let miss = span(0x203);

    host.insert_span(query, key("src/main.rs", 1, 12, 1, 13));
    host.insert_span(outer_span, key("src/main.rs", 1, 0, 1, 40));
    host.insert_span(inner_span, key("src/main.rs", 1, 10, 1, 20));
    host.insert_span(miss, key("src/main.rs", 1, 41, 1, 45));

    let outer = node(0x210);
    let inner = node(0x211);
    host.insert_node(outer, NodeKind::Block, outer_span, None);
    host.insert_node(inner, NodeKind::Expr, inner_span, Some(outer));

    assert_eq!(host.node_at(query), Some(inner));
    assert_eq!(host.node_at(miss), None);
}

#[test]
fn node_at_tie_breaks_with_stable_order_node_id() {
    let mut host = DeterministicRaHost::new();

    let query = span(0x300);
    let shared = span(0x301);
    host.insert_span(query, key("src/lib.rs", 7, 3, 7, 4));
    host.insert_span(shared, key("src/lib.rs", 7, 0, 7, 10));

    let node_a = node(0x310);
    let node_b = node(0x311);

    host.insert_node(node_a, NodeKind::Expr, shared, None);
    host.insert_node(node_b, NodeKind::Expr, shared, None);
    host.insert_node_id(node_a, "node://b");
    host.insert_node_id(node_b, "node://a");

    assert_eq!(host.node_at(query), Some(node_b));
}

#[test]
fn node_attributes_are_functional_and_optional_parent_is_supported() {
    let mut host = DeterministicRaHost::new();

    let root = node(0x410);
    let child = node(0x411);
    let root_span = span(0x420);
    let child_span = span(0x421);

    host.insert_span(root_span, key("src/lib.rs", 1, 0, 20, 0));
    host.insert_span(child_span, key("src/lib.rs", 5, 2, 5, 8));
    host.insert_node(root, NodeKind::Item, root_span, None);
    host.insert_node(child, NodeKind::Expr, child_span, Some(root));

    assert_eq!(host.node_kind(root), Some(NodeKind::Item));
    assert_eq!(host.node_span(child), Some(child_span));
    assert_eq!(host.node_parent(root), Some(None));
    assert_eq!(host.node_parent(child), Some(Some(root)));
    assert_eq!(host.node_parent(node(0x499)), None);
}

#[test]
fn enclosing_control_returns_first_control_ancestor_with_distance() {
    let mut host = DeterministicRaHost::new();

    let query = span(0x500);
    let expr_span = span(0x501);
    let block_span = span(0x502);
    let if_span = span(0x503);
    let while_span = span(0x504);

    host.insert_span(query, key("src/main.rs", 12, 7, 12, 8));
    host.insert_span(expr_span, key("src/main.rs", 12, 6, 12, 9));
    host.insert_span(block_span, key("src/main.rs", 12, 4, 14, 1));
    host.insert_span(if_span, key("src/main.rs", 10, 0, 16, 1));
    host.insert_span(while_span, key("src/main.rs", 8, 0, 20, 1));

    let while_node = node(0x510);
    let if_node = node(0x511);
    let block_node = node(0x512);
    let expr_node = node(0x513);

    host.insert_node(while_node, NodeKind::While, while_span, None);
    host.insert_node(if_node, NodeKind::If, if_span, Some(while_node));
    host.insert_node(block_node, NodeKind::Block, block_span, Some(if_node));
    host.insert_node(expr_node, NodeKind::Expr, expr_span, Some(block_node));

    let control = host
        .enclosing_control(query)
        .expect("should find the nearest control ancestor");
    assert_eq!(control.kind, NodeKind::If);
    assert_eq!(control.span, if_span);
    assert_eq!(control.distance, 2);
}

#[test]
fn enclosing_control_respects_default_and_overridden_max_depth() {
    let mut host = DeterministicRaHost::new();

    let query = span(0x600);
    let expr_span = span(0x601);
    let block_span = span(0x602);
    let if_span = span(0x603);

    host.insert_span(query, key("src/lib.rs", 30, 5, 30, 6));
    host.insert_span(expr_span, key("src/lib.rs", 30, 4, 30, 7));
    host.insert_span(block_span, key("src/lib.rs", 30, 2, 31, 1));
    host.insert_span(if_span, key("src/lib.rs", 28, 0, 34, 1));

    let if_node = node(0x610);
    let block_node = node(0x611);
    let expr_node = node(0x612);

    host.insert_node(if_node, NodeKind::If, if_span, None);
    host.insert_node(block_node, NodeKind::Block, block_span, Some(if_node));
    host.insert_node(expr_node, NodeKind::Expr, expr_span, Some(block_node));

    assert_eq!(host.control_max_depth(), 32);
    assert!(host.enclosing_control(query).is_some());

    host.set_control_max_depth(1);
    assert_eq!(host.enclosing_control(query), None);

    let deep_override = host.enclosing_control_with_depth(query, 2);
    assert!(deep_override.is_some());
}

#[test]
fn invalid_span_range_includes_source_path_context() {
    let mut host = DeterministicRaHost::new();
    let err = host
        .intern_span_from_text(
            editioned_file_id(0),
            "src/failing.rs",
            "abc",
            TextRange::new(TextSize::from(0), TextSize::from(8)),
        )
        .expect_err("range past end should fail");

    match &err {
        RaHostError::InvalidSpanRange {
            rel_path,
            start,
            end,
            len,
        } => {
            assert_eq!(rel_path.as_ref(), "src/failing.rs");
            assert_eq!((*start, *end, *len), (0, 8, 3));
        }
    }
    assert!(err.to_string().contains("src/failing.rs"));
}
