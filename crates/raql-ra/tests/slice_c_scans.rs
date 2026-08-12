//! Slice-C truth fixtures (SPEC §16.1) for the step-3 remainder: the
//! `def`/`fn_def`/`def_name`/`call_edge` scans (SPEC §8.5) and the
//! `is_public`/`in_test`/`span_allowed`/`handle`/`span_key` predicates,
//! driven through the catalog operator boundary.
//!
//! The `defs_ws` fixture carries a non-member path dependency (`dep_lib`).
//! RA's local/library partition (`CrateOrigin::is_local`, source-root
//! `is_library`) treats path dependencies as local, editable code — so
//! `dep_lib` is inside both the scan domain and the request scope, and the
//! tests pin that down. Registry/git/sysroot dependencies are the library
//! side; this fixture graph deliberately has none (`sysroot: None`), so
//! the library-side negatives live with the probe runs, not here.

mod common;

use common::{Fixture, def_by_path, def_by_scan_path, find_fn, position_of};
use raql_plan::{OperatorId, OperatorSet};
use raql_ra::{Def, Projected, Value, project_def, project_file_range};

const WIDGETS_RS: &str = include_str!("fixtures/defs_ws/src/widgets.rs");

/// Run a no-input scan and hand back its raw rows.
fn scan(fixture: &Fixture, operator: OperatorId) -> Vec<Vec<Value>> {
    fixture.operators().invoke(operator, &[]).expect("scan succeeds")
}

/// Every def of a single-column scan, projected as `KIND path`; impls and
/// other pathless defs are returned separately (they have handles or
/// nothing, never invented paths).
fn def_scan_summary(fixture: &Fixture) -> (Vec<String>, Vec<Def>, Vec<Def>) {
    let mut pathed = Vec::new();
    let mut impls = Vec::new();
    let mut pathless = Vec::new();
    for row in scan(fixture, OperatorId::DefsScan) {
        let [Value::Def(def)] = row.as_slice() else {
            panic!("def row shape: (Def), got {row:?}");
        };
        let projected = project_def(&fixture.db, &fixture.workspace_root(), *def);
        match (&projected.path, def) {
            (_, Def::Impl(_)) => impls.push(*def),
            (Projected::Text(path), _) => pathed.push(format!("{} {path}", projected.kind.tag())),
            (Projected::Unprojectable(_), _) => pathless.push(*def),
        }
    }
    pathed.sort();
    (pathed, impls, pathless)
}

fn filter_passes(fixture: &Fixture, operator: OperatorId, def: Def) -> bool {
    let rows = fixture
        .operators()
        .invoke(operator, &[Value::Def(def)])
        .expect("filter succeeds");
    match rows.as_slice() {
        [] => false,
        [row] => {
            assert_eq!(row.as_slice(), [Value::Def(def)], "filter rows echo the def");
            true
        }
        _ => panic!("filters are functional: at most one row, got {rows:?}"),
    }
}

fn handle_of(fixture: &Fixture, def: Def) -> Option<String> {
    let rows = fixture
        .operators()
        .invoke(OperatorId::HandleOfDef, &[Value::Def(def)])
        .expect("handle succeeds");
    match rows.as_slice() {
        [] => None,
        [row] => match row.as_slice() {
            [Value::Def(echoed), Value::String(handle)] => {
                assert_eq!(echoed, &def);
                Some(handle.to_string())
            }
            _ => panic!("handle row shape: (Def, String)"),
        },
        _ => panic!("handle is functional: at most one row, got {rows:?}"),
    }
}

fn span_of(fixture: &Fixture, def: Def) -> ide_db::FileRange {
    let rows = fixture
        .operators()
        .invoke(OperatorId::SpanOfDef, &[Value::Def(def)])
        .expect("def_span succeeds");
    let [row] = rows.as_slice() else {
        panic!("def_span: exactly one row, got {rows:?}");
    };
    let [_, Value::FileRange(range)] = row.as_slice() else {
        panic!("def_span row shape: (Def, FileRange)");
    };
    *range
}

#[test]
fn def_scan_enumerates_the_module_tree_exactly() {
    let fixture = Fixture::load("defs_ws");
    let (pathed, impls, pathless) = def_scan_summary(&fixture);

    assert_eq!(
        pathed,
        vec![
            "ASSOC_CONST defs_ws::widgets::Widget::DEFAULT_SIZE".to_owned(),
            "ASSOC_TYPE defs_ws::widgets::Render::Output".to_owned(),
            "ASSOC_TYPE defs_ws::widgets::Widget::Output".to_owned(),
            "CONST defs_ws::ANSWER".to_owned(),
            "ENUM defs_ws::gadgets::Gadget".to_owned(),
            "FIELD defs_ws::gadgets::Gadget::Large::watts".to_owned(),
            "FIELD defs_ws::widgets::Widget::size".to_owned(),
            "FN defs_ws::crate_helper".to_owned(),
            "FN defs_ws::gadgets::duplicate_name".to_owned(),
            "FN defs_ws::made_by_macro".to_owned(),
            "FN defs_ws::private_helper".to_owned(),
            "FN defs_ws::standalone_test".to_owned(),
            "FN defs_ws::tests::helper_in_tests".to_owned(),
            "FN defs_ws::widgets::duplicate_name".to_owned(),
            "FN dep_lib::dep_fn".to_owned(),
            "MACRO defs_ws::make_fn".to_owned(),
            "METHOD defs_ws::gadgets::inner::Deep::poke".to_owned(),
            "METHOD defs_ws::widgets::Render::render".to_owned(),
            "METHOD defs_ws::widgets::Widget::grow".to_owned(),
            "METHOD defs_ws::widgets::Widget::render".to_owned(),
            "MOD defs_ws::gadgets".to_owned(),
            "MOD defs_ws::gadgets::inner".to_owned(),
            "MOD defs_ws::tests".to_owned(),
            "MOD defs_ws::widgets".to_owned(),
            "STATIC defs_ws::GREETING".to_owned(),
            "STRUCT defs_ws::gadgets::inner::Deep".to_owned(),
            "STRUCT defs_ws::widgets::Widget".to_owned(),
            "TRAIT defs_ws::widgets::Render".to_owned(),
            "TYPE_ALIAS defs_ws::Meters".to_owned(),
            "VARIANT defs_ws::gadgets::Gadget::Large".to_owned(),
            "VARIANT defs_ws::gadgets::Gadget::Small".to_owned(),
        ],
    );

    // Three impls (two on Widget, one on Deep); the only other pathless
    // defs are the two crate root modules.
    assert_eq!(impls.len(), 3);
    assert_eq!(pathless.len(), 2);
    assert!(
        pathless.iter().all(|def| matches!(def, Def::Module(_))),
        "pathless defs are the crate root modules",
    );
}

#[test]
fn fn_def_scan_filters_to_functions() {
    let fixture = Fixture::load("defs_ws");
    let mut paths: Vec<String> = scan(&fixture, OperatorId::FnDefsScan)
        .into_iter()
        .map(|row| {
            let [Value::Def(def)] = row.as_slice() else {
                panic!("fn_def row shape: (Def)");
            };
            assert!(matches!(def, Def::Function(_)));
            project_def(&fixture.db, &fixture.workspace_root(), *def).path.to_string()
        })
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            "defs_ws::crate_helper".to_owned(),
            "defs_ws::gadgets::duplicate_name".to_owned(),
            "defs_ws::gadgets::inner::Deep::poke".to_owned(),
            "defs_ws::made_by_macro".to_owned(),
            "defs_ws::private_helper".to_owned(),
            "defs_ws::standalone_test".to_owned(),
            "defs_ws::tests::helper_in_tests".to_owned(),
            "defs_ws::widgets::Render::render".to_owned(),
            "defs_ws::widgets::Widget::grow".to_owned(),
            "defs_ws::widgets::Widget::render".to_owned(),
            "defs_ws::widgets::duplicate_name".to_owned(),
            // The path dependency is local code in RA's model.
            "dep_lib::dep_fn".to_owned(),
        ],
    );
}

#[test]
fn def_names_scan_is_enumeration_times_name() {
    let fixture = Fixture::load("defs_ws");
    let (pathed, _, _) = def_scan_summary(&fixture);

    let rows = scan(&fixture, OperatorId::DefNamesScan);
    for row in &rows {
        let [Value::Def(_), Value::String(_)] = row.as_slice() else {
            panic!("def_name scan row shape: (Def, String), got {row:?}");
        };
    }
    // Exactly the named defs: every pathed def is named, and nothing else
    // is (the three impls and the crate root module have no name).
    assert_eq!(rows.len(), pathed.len());
    let duplicate_rows = rows
        .iter()
        .filter(|row| matches!(&row[1], Value::String(name) if &**name == "duplicate_name"))
        .count();
    assert_eq!(duplicate_rows, 2);
}

#[test]
fn call_edge_scan_composes_over_the_outgoing_direction() {
    let fixture = Fixture::load("calls_ws");
    let mut edges: Vec<(String, String, &str)> = scan(&fixture, OperatorId::CallEdgesScan)
        .into_iter()
        .map(|row| {
            let [Value::Def(caller), Value::Def(callee), Value::FileRange(_), Value::Enum(tag)] =
                row.as_slice()
            else {
                panic!("call_edge row shape: (Def, Def, Site, Disp), got {row:?}");
            };
            assert_eq!(tag.ty, "DispatchKind");
            let path = |def: &Def| {
                project_def(&fixture.db, &fixture.workspace_root(), *def).path.to_string()
            };
            (path(caller), path(callee), tag.variant)
        })
        .collect();
    edges.sort();

    // The §8.5 rewrite inherits `callee`'s caveats: the macro-generated
    // callsites (`macro_call`, `hidden_macro_call`) and the fn-pointer call
    // are absent; the cfg'd-off caller does not exist.
    assert_eq!(
        edges,
        vec![
            ("calls_ws::closure_using".to_owned(), "calls_ws::helper".to_owned(), "DIRECT"),
            ("calls_ws::dyn_call".to_owned(), "calls_ws::Greet::greet".to_owned(), "DYN"),
            (
                "calls_ws::generic_call".to_owned(),
                "calls_ws::Greet::greet".to_owned(),
                "THROUGH_TRAIT",
            ),
            (
                "calls_ws::inherent_method_call".to_owned(),
                "calls_ws::Counter::tick".to_owned(),
                "DIRECT",
            ),
            ("calls_ws::static_call".to_owned(), "calls_ws::helper".to_owned(), "DIRECT"),
        ],
    );
}

#[test]
fn is_public_is_declared_visibility_only() {
    let fixture = Fixture::load("defs_ws");
    let cases: &[(&str, &str, bool)] = &[
        ("Widget", "defs_ws::widgets::Widget", true),
        ("helper_in_tests", "defs_ws::tests::helper_in_tests", true),
        // `pub(crate)` and private are not public.
        ("crate_helper", "defs_ws::crate_helper", false),
        ("private_helper", "defs_ws::private_helper", false),
    ];
    for (name, path, expected) in cases {
        let def = def_by_path(&fixture, name, path);
        assert_eq!(
            filter_passes(&fixture, OperatorId::IsPublicFilter, def),
            *expected,
            "is_public({path})",
        );
    }

    // Fields are not seedable by name (`fields_not_in_symbol_index`) but
    // are enumerable, and their own visibility counts.
    let size = def_by_scan_path(&fixture, "defs_ws::widgets::Widget::size");
    assert!(filter_passes(&fixture, OperatorId::IsPublicFilter, size));
}

#[test]
fn in_test_matches_test_fns_and_tests_modules() {
    let fixture = Fixture::load("defs_ws");
    let cases: &[(&str, &str, bool)] = &[
        // `#[test]` function outside any tests module.
        ("standalone_test", "defs_ws::standalone_test", true),
        // Plain fn under a module named `tests`, and the module itself.
        ("helper_in_tests", "defs_ws::tests::helper_in_tests", true),
        ("tests", "defs_ws::tests", true),
        ("Widget", "defs_ws::widgets::Widget", false),
        ("crate_helper", "defs_ws::crate_helper", false),
    ];
    for (name, path, expected) in cases {
        let def = def_by_path(&fixture, name, path);
        assert_eq!(
            filter_passes(&fixture, OperatorId::InTestFilter, def),
            *expected,
            "in_test({path})",
        );
    }
}

#[test]
fn handles_follow_the_grammar_with_impl_ordinals() {
    let fixture = Fixture::load("defs_ws");

    // Ordinary defs: `@H:kind:canonical-path`, no ordinal.
    for (name, path, expected) in [
        ("Widget", "defs_ws::widgets::Widget", "@H:struct:defs_ws::widgets::Widget"),
        ("grow", "defs_ws::widgets::Widget::grow", "@H:fn:defs_ws::widgets::Widget::grow"),
        ("Small", "defs_ws::gadgets::Gadget::Small", "@H:variant:defs_ws::gadgets::Gadget::Small"),
    ] {
        let def = def_by_path(&fixture, name, path);
        assert_eq!(handle_of(&fixture, def).as_deref(), Some(expected));
        // The predicate and the output boundary share one projection.
        assert_eq!(
            project_def(&fixture.db, &fixture.workspace_root(), def).handle.to_string(),
            expected,
        );
    }

    // Impls: self-type path, ordinal-qualified only when shared, in source
    // order. Widget has two impls (inherent first in the file), Deep one.
    let (_, impls, _) = def_scan_summary(&fixture);
    let mut impl_handles: Vec<(String, String)> = impls
        .iter()
        .map(|impl_def| {
            let handle = handle_of(&fixture, *impl_def).expect("ADT impls have handles");
            let location = project_def(&fixture.db, &fixture.workspace_root(), *impl_def)
                .location
                .to_string();
            (handle, location)
        })
        .collect();
    impl_handles.sort();

    let inherent_line = position_of(WIDGETS_RS, "impl Widget {", 0).line + 1;
    let trait_line = position_of(WIDGETS_RS, "impl Render for Widget", 0).line + 1;
    assert_eq!(impl_handles.len(), 3);
    assert_eq!(impl_handles[0].0, "@H:impl:defs_ws::gadgets::inner::Deep");
    assert_eq!(impl_handles[1].0, "@H:impl:defs_ws::widgets::Widget#0");
    assert!(
        impl_handles[1].1.starts_with(&format!("src/widgets.rs:{inherent_line}:")),
        "ordinal 0 is the first impl in source order: {}",
        impl_handles[1].1,
    );
    assert_eq!(impl_handles[2].0, "@H:impl:defs_ws::widgets::Widget#1");
    assert!(impl_handles[2].1.starts_with(&format!("src/widgets.rs:{trait_line}:")));
}

#[test]
fn span_key_projects_workspace_relative_zero_based() {
    let fixture = Fixture::load("defs_ws");
    let widget = def_by_path(&fixture, "Widget", "defs_ws::widgets::Widget");
    let range = span_of(&fixture, widget);

    let rows = fixture
        .operators()
        .invoke(OperatorId::SpanKeyOfSpan, &[Value::FileRange(range)])
        .expect("span_key succeeds");
    let [row] = rows.as_slice() else {
        panic!("span_key: exactly one row, got {rows:?}");
    };
    // Widget spans widgets.rs lines 1-3 (1-based) — zero-based here; the
    // human rendering of the same span stays 1-based.
    assert_eq!(
        row.as_slice(),
        [
            Value::FileRange(range),
            Value::string("src/widgets.rs"),
            Value::Int(0),
            Value::Int(0),
            Value::Int(2),
            Value::Int(1),
        ],
    );
    assert_eq!(
        project_file_range(&fixture.db, &fixture.workspace_root(), range).as_deref(),
        Some("src/widgets.rs:1:1-3:2"),
    );
}

#[test]
fn span_allowed_scopes_to_local_roots() {
    let fixture = Fixture::load("defs_ws");

    // A workspace span passes and echoes.
    let widget = def_by_path(&fixture, "Widget", "defs_ws::widgets::Widget");
    let range = span_of(&fixture, widget);
    let rows = fixture
        .operators()
        .invoke(OperatorId::SpanAllowedFilter, &[Value::FileRange(range)])
        .expect("span_allowed succeeds");
    assert_eq!(rows, vec![vec![Value::FileRange(range)]]);

    // A path dependency is local, editable code in RA's partition — in
    // scope. (Library roots — registry deps, sysroot — are the out-of-scope
    // side; this graph has none, deliberately.)
    let dep_fn = find_fn(&fixture.db, "dep_fn");
    let dep_range = span_of(&fixture, Def::Function(dep_fn));
    let rows = fixture
        .operators()
        .invoke(OperatorId::SpanAllowedFilter, &[Value::FileRange(dep_range)])
        .expect("span_allowed succeeds");
    assert_eq!(rows, vec![vec![Value::FileRange(dep_range)]]);
}
