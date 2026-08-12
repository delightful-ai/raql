//! Slice-B truth fixtures (SPEC §16.1) for the call family: `callee`,
//! `caller`, `call_edge`, driven through the catalog operator boundary.
//!
//! Fixture coverage per SPEC §16.1: trait dispatch (generic →
//! `THROUGH_TRAIT`), dyn dispatch (`dyn Trait` receiver → `DYN`), a
//! macro-expansion callsite, a cfg-gated caller (absent), a closure-heavy
//! body, and a non-call reference (absent).

mod common;

use common::{Fixture, def_by_path, position_of};
use raql_plan::{OperatorId, OperatorSet};
use raql_ra::{Def, SnapshotOperators, Value, project_def};

const LIB_RS: &str = include_str!("fixtures/calls_ws/src/lib.rs");

/// Invoke a call-family operator and summarize rows as
/// `(peer-projected-path, dispatch-tag)`, sorted. `peer_index` selects the
/// row column holding the non-seed def.
fn edge_summary(
    fixture: &Fixture,
    operator: OperatorId,
    seed: Def,
    peer_index: usize,
) -> Vec<(String, &'static str)> {
    let mut ops = SnapshotOperators::new(&fixture.db);
    let rows = ops
        .invoke(operator, &[Value::Def(seed)])
        .expect("call operator succeeds");
    let mut summary = Vec::new();
    for row in rows {
        assert_eq!(row.len(), 4, "call rows are (Def, Def, Site, Disp)");
        let Value::Def(peer) = row[peer_index] else {
            panic!("peer column must be a Def, got {:?}", row[peer_index]);
        };
        let Value::Enum(tag) = row[3] else {
            panic!("dispatch column must be an Enum, got {:?}", row[3]);
        };
        assert_eq!(tag.ty, "DispatchKind");
        let path = project_def(&fixture.db, &fixture.workspace_root(), peer)
            .path
            .to_string();
        summary.push((path, tag.variant));
    }
    summary.sort();
    summary
}

#[test]
fn callee_classifies_dispatch() {
    let fixture = Fixture::load("calls_ws");

    // Static free-fn call.
    let static_call = def_by_path(&fixture, "static_call", "calls_ws::static_call");
    assert_eq!(
        edge_summary(&fixture, OperatorId::CalleesOfFn, static_call, 1),
        vec![("calls_ws::helper".to_owned(), "DIRECT")],
    );

    // Generic trait-bound receiver: statically resolved trait method.
    let generic_call = def_by_path(&fixture, "generic_call", "calls_ws::generic_call");
    assert_eq!(
        edge_summary(&fixture, OperatorId::CalleesOfFn, generic_call, 1),
        vec![("calls_ws::Greet::greet".to_owned(), "THROUGH_TRAIT")],
    );

    // `dyn Trait` receiver (through the reference autoderef).
    let dyn_call = def_by_path(&fixture, "dyn_call", "calls_ws::dyn_call");
    assert_eq!(
        edge_summary(&fixture, OperatorId::CalleesOfFn, dyn_call, 1),
        vec![("calls_ws::Greet::greet".to_owned(), "DYN")],
    );

    // Inherent method.
    let inherent = def_by_path(&fixture, "inherent_method_call", "calls_ws::inherent_method_call");
    assert_eq!(
        edge_summary(&fixture, OperatorId::CalleesOfFn, inherent, 1),
        vec![("calls_ws::Counter::tick".to_owned(), "DIRECT")],
    );

    // The closure call `f(..)` has no `Def` callee and is absent; the
    // `helper()` argument call is present (SPEC §6.3).
    let closure_using = def_by_path(&fixture, "closure_using", "calls_ws::closure_using");
    assert_eq!(
        edge_summary(&fixture, OperatorId::CalleesOfFn, closure_using, 1),
        vec![("calls_ws::helper".to_owned(), "DIRECT")],
    );

    // Fn-pointer call and fn-as-value reference: no resolvable callee defs.
    let takes_fn_value = def_by_path(&fixture, "takes_fn_value", "calls_ws::takes_fn_value");
    assert_eq!(
        edge_summary(&fixture, OperatorId::CalleesOfFn, takes_fn_value, 1),
        Vec::<(String, &str)>::new(),
    );

    // Known, documented hole (`macro_generated_callsites_absent`): calls
    // that only exist after macro expansion are absent from the raw-AST
    // body walk — same behavior as RA's own `outgoing_calls`.
    for name in ["macro_call", "hidden_macro_call"] {
        let f = def_by_path(&fixture, name, &format!("calls_ws::{name}"));
        assert_eq!(
            edge_summary(&fixture, OperatorId::CalleesOfFn, f, 1),
            Vec::<(String, &str)>::new(),
            "{name} outgoing edges are behind macro expansion",
        );
    }
}

#[test]
fn caller_finds_and_classifies_incoming_edges() {
    let fixture = Fixture::load("calls_ws");

    // Callers of `helper`: the direct call, the closure-body call
    // (attributed to the enclosing named fn), and the macro-expanded call
    // whose callee token sits at the invocation site (`call_with!(helper)`).
    // Absent, honestly (SPEC §4.3): the cfg'd-off `gated_caller`, the
    // non-call reference in `takes_fn_value`, and `hidden_macro_call` —
    // whose callsite token lives in the macro *definition* body, which RA's
    // reference search does not surface
    // (`macro_definition_body_callsites_absent`).
    let helper = def_by_path(&fixture, "helper", "calls_ws::helper");
    assert_eq!(
        edge_summary(&fixture, OperatorId::CallersOfFn, helper, 1),
        vec![
            ("calls_ws::closure_using".to_owned(), "DIRECT"),
            ("calls_ws::macro_call".to_owned(), "DIRECT"),
            ("calls_ws::static_call".to_owned(), "DIRECT"),
        ],
    );

    // Callers of the trait-declared method: both dispatch styles, each
    // classified at its own callsite.
    let greet = def_by_path(&fixture, "greet", "calls_ws::Greet::greet");
    assert_eq!(
        edge_summary(&fixture, OperatorId::CallersOfFn, greet, 1),
        vec![
            ("calls_ws::dyn_call".to_owned(), "DYN"),
            ("calls_ws::generic_call".to_owned(), "THROUGH_TRAIT"),
        ],
    );
}

#[test]
fn call_edge_composes_both_directions() {
    let fixture = Fixture::load("calls_ws");
    let helper = def_by_path(&fixture, "helper", "calls_ws::helper");
    let static_call = def_by_path(&fixture, "static_call", "calls_ws::static_call");

    // Outgoing composition shares the `callee` row shape.
    assert_eq!(
        edge_summary(&fixture, OperatorId::CallEdgesByCaller, static_call, 1),
        vec![("calls_ws::helper".to_owned(), "DIRECT")],
    );

    // Incoming composition reorders `caller` rows into
    // `(Caller, Callee, Site, Disp)` — the peer sits in column 0 and the
    // seed echoes in column 1.
    let mut ops = SnapshotOperators::new(&fixture.db);
    let rows = ops
        .invoke(OperatorId::CallEdgesByCallee, &[Value::Def(helper)])
        .expect("call_edge by callee succeeds");
    for row in &rows {
        assert_eq!(row[1], Value::Def(helper), "callee column echoes the seed");
    }
    assert_eq!(
        edge_summary(&fixture, OperatorId::CallEdgesByCallee, helper, 0),
        vec![
            ("calls_ws::closure_using".to_owned(), "DIRECT"),
            ("calls_ws::macro_call".to_owned(), "DIRECT"),
            ("calls_ws::static_call".to_owned(), "DIRECT"),
        ],
    );
}

#[test]
fn call_sites_project_to_call_lines() {
    let fixture = Fixture::load("calls_ws");
    let static_call = def_by_path(&fixture, "static_call", "calls_ws::static_call");

    let mut ops = SnapshotOperators::new(&fixture.db);
    let rows = ops
        .invoke(OperatorId::CalleesOfFn, &[Value::Def(static_call)])
        .expect("callee succeeds");
    let [row] = rows.as_slice() else {
        panic!("static_call has exactly one callee row, got {rows:?}");
    };
    let Value::FileRange(range) = row[2] else {
        panic!("site column must be a FileRange");
    };
    let location = raql_ra::project_file_range(&fixture.db, &fixture.workspace_root(), range)
        .expect("site projects");
    // The `helper()` call inside `static_call`'s body (1-based line).
    let call_line = position_of(LIB_RS, "    helper()\n}\n\npub fn generic_call", 0).line + 1;
    assert!(
        location.starts_with(&format!("src/lib.rs:{call_line}:")),
        "site must be on the call line: {location}",
    );
}

/// Non-function defs have no call edges: empty, not an error.
#[test]
fn non_function_defs_have_no_call_edges() {
    let fixture = Fixture::load("calls_ws");
    let counter = def_by_path(&fixture, "Counter", "calls_ws::Counter");
    assert_eq!(
        edge_summary(&fixture, OperatorId::CalleesOfFn, counter, 1),
        Vec::<(String, &str)>::new(),
    );
    assert_eq!(
        edge_summary(&fixture, OperatorId::CallersOfFn, counter, 1),
        Vec::<(String, &str)>::new(),
    );
}
