//! Slice-A truth fixtures (SPEC §16.1) for the definition family:
//! `def_name`, `def_at`, `def_kind`, `def_path`, `def_span`, driven through
//! the catalog operator boundary (`raql_plan::OperatorSet`) exactly as the
//! engine will drive it.
//!
//! Fixture coverage per SPEC §16.1: a macro-expansion site
//! (`make_fn!(made_by_macro)`), a cfg-gated item (`cfg_gated`, off), nested
//! modules, and trait/impl associated items.

mod common;

use common::{Fixture, def_by_path, position_of};
use raql_plan::{OperatorId, OperatorSet, v0_catalog};
use raql_ra::{Def, Projected, Value, project_def};

const WIDGETS_RS: &str = include_str!("fixtures/defs_ws/src/widgets.rs");
const LIB_RS: &str = include_str!("fixtures/defs_ws/src/lib.rs");

/// Seed defs by exact name and return their projected canonical paths,
/// sorted. Every row must echo the name and carry a Def.
fn paths_named(fixture: &Fixture, name: &str) -> Vec<String> {
    let mut ops = fixture.operators();
    let rows = ops
        .invoke(OperatorId::DefsByExactName, &[Value::string(name)])
        .expect("seeding succeeds");
    let mut paths = Vec::new();
    for row in rows {
        let [Value::Def(def), Value::String(echoed)] = row.as_slice() else {
            panic!("def_name row shape: (Def, String), got {row:?}");
        };
        assert_eq!(&**echoed, name);
        paths.push(
            project_def(&fixture.db, &fixture.workspace_root(), *def)
                .path
                .to_string(),
        );
    }
    paths.sort();
    paths
}

fn kind_tag(fixture: &Fixture, def: Def) -> &'static str {
    let mut ops = fixture.operators();
    let rows = ops
        .invoke(OperatorId::KindOfDef, &[Value::Def(def)])
        .expect("def_kind succeeds");
    let [row] = rows.as_slice() else {
        panic!("def_kind is functional: exactly one row, got {rows:?}");
    };
    let [Value::Def(echoed), Value::Enum(tag)] = row.as_slice() else {
        panic!("def_kind row shape: (Def, Enum)");
    };
    assert_eq!(*echoed, def);
    assert_eq!(tag.ty, "DefKind");
    tag.variant
}

fn location_of(fixture: &Fixture, def: Def) -> String {
    let mut ops = fixture.operators();
    let rows = ops
        .invoke(OperatorId::SpanOfDef, &[Value::Def(def)])
        .expect("def_span succeeds");
    let [row] = rows.as_slice() else {
        panic!("def_span: exactly one row, got {rows:?}");
    };
    let [_, Value::FileRange(range)] = row.as_slice() else {
        panic!("def_span row shape: (Def, FileRange)");
    };
    raql_ra::project_file_range(&fixture.db, &fixture.workspace_root(), *range)
        .expect("fixture spans project")
}

#[test]
fn def_name_seeds_exactly() {
    let fixture = Fixture::load("defs_ws");

    // Two same-named fns in different modules: exactly two rows.
    assert_eq!(
        paths_named(&fixture, "duplicate_name"),
        vec![
            "defs_ws::gadgets::duplicate_name".to_owned(),
            "defs_ws::widgets::duplicate_name".to_owned(),
        ],
    );

    // A def produced by macro expansion is seeded like any other.
    assert_eq!(paths_named(&fixture, "made_by_macro"), vec!["defs_ws::made_by_macro".to_owned()]);

    // A cfg'd-off item does not exist in RA's semantic model: absent, not
    // approximated (SPEC §4.3).
    assert_eq!(paths_named(&fixture, "cfg_gated"), Vec::<String>::new());

    // Types, assoc items, variants, and nested-module defs are seedable.
    assert_eq!(paths_named(&fixture, "Widget"), vec!["defs_ws::widgets::Widget".to_owned()]);
    assert_eq!(paths_named(&fixture, "grow"), vec!["defs_ws::widgets::Widget::grow".to_owned()]);
    assert_eq!(
        paths_named(&fixture, "render"),
        vec![
            "defs_ws::widgets::Render::render".to_owned(),
            "defs_ws::widgets::Widget::render".to_owned(),
        ],
    );
    assert_eq!(paths_named(&fixture, "Deep"), vec!["defs_ws::gadgets::inner::Deep".to_owned()]);
    assert_eq!(
        paths_named(&fixture, "Small"),
        vec!["defs_ws::gadgets::Gadget::Small".to_owned()],
    );
}

#[test]
fn def_kind_matrix() {
    let fixture = Fixture::load("defs_ws");
    let cases: &[(&str, &str, &str)] = &[
        ("duplicate_name", "defs_ws::widgets::duplicate_name", "FN"),
        ("grow", "defs_ws::widgets::Widget::grow", "METHOD"),
        ("Widget", "defs_ws::widgets::Widget", "STRUCT"),
        ("Gadget", "defs_ws::gadgets::Gadget", "ENUM"),
        ("Small", "defs_ws::gadgets::Gadget::Small", "VARIANT"),
        ("Render", "defs_ws::widgets::Render", "TRAIT"),
        ("ANSWER", "defs_ws::ANSWER", "CONST"),
        ("DEFAULT_SIZE", "defs_ws::widgets::Widget::DEFAULT_SIZE", "ASSOC_CONST"),
        ("GREETING", "defs_ws::GREETING", "STATIC"),
        ("Meters", "defs_ws::Meters", "TYPE_ALIAS"),
        ("Output", "defs_ws::widgets::Render::Output", "ASSOC_TYPE"),
        ("inner", "defs_ws::gadgets::inner", "MOD"),
    ];
    for (name, path, expected_kind) in cases {
        let def = def_by_path(&fixture, name, path);
        assert_eq!(kind_tag(&fixture, def), *expected_kind, "kind of {path}");
    }
}

#[test]
fn def_path_and_name_echo() {
    let fixture = Fixture::load("defs_ws");
    let mut ops = fixture.operators();

    let def = def_by_path(&fixture, "Deep", "defs_ws::gadgets::inner::Deep");
    let rows = ops.invoke(OperatorId::CanonicalPathOfDef, &[Value::Def(def)]).unwrap();
    assert_eq!(
        rows,
        vec![vec![Value::Def(def), Value::string("defs_ws::gadgets::inner::Deep")]],
    );

    let rows = ops.invoke(OperatorId::NameOfDef, &[Value::Def(def)]).unwrap();
    assert_eq!(rows, vec![vec![Value::Def(def), Value::string("Deep")]]);
}

#[test]
fn def_span_is_macro_aware() {
    let fixture = Fixture::load("defs_ws");

    // A macro-generated def reports the invocation site (primary location,
    // SPEC §6.3), on the `make_fn!(made_by_macro);` line.
    let invocation_line = position_of(LIB_RS, "make_fn!(made_by_macro);", 0).line + 1;
    let def = def_by_path(&fixture, "made_by_macro", "defs_ws::made_by_macro");
    let location = location_of(&fixture, def);
    assert!(
        location.starts_with(&format!("src/lib.rs:{invocation_line}:")),
        "macro-generated def must locate at its invocation site: {location}",
    );

    // An ordinary def spans its item.
    let widget = def_by_path(&fixture, "Widget", "defs_ws::widgets::Widget");
    assert_eq!(location_of(&fixture, widget), "src/widgets.rs:1:1-3:2");
}

#[test]
fn def_at_classifies_positions() {
    let fixture = Fixture::load("defs_ws");
    let mut ops = fixture.operators();
    let file = fixture.file_id("src/widgets.rs");

    let mut defs_at = |pos| {
        let rows = ops
            .invoke(OperatorId::DefAtPosition, &[Value::File(file), Value::Position(pos)])
            .expect("def_at succeeds");
        rows.into_iter()
            .map(|row| match row.as_slice() {
                [Value::File(f), Value::Position(p), Value::Def(def)] => {
                    assert_eq!(*f, file);
                    assert_eq!(*p, pos);
                    project_def(&fixture.db, &fixture.workspace_root(), *def)
                        .path
                        .to_string()
                }
                _ => panic!("def_at row shape: (File, Position, Def)"),
            })
            .collect::<Vec<_>>()
    };

    // `Widget` in `impl Render for Widget` resolves to the struct.
    let pos = position_of(WIDGETS_RS, "impl Render for Widget", 0);
    let widget_use = raql_ra::Position { line: pos.line, col: pos.col + "impl Render for ".len() as u32 };
    assert_eq!(defs_at(widget_use), vec!["defs_ws::widgets::Widget".to_owned()]);

    // `self.size` resolves to the field.
    let pos = position_of(WIDGETS_RS, "self.size += by", 0);
    let field_use = raql_ra::Position { line: pos.line, col: pos.col + "self.".len() as u32 };
    assert_eq!(defs_at(field_use), vec!["defs_ws::widgets::Widget::size".to_owned()]);

    // A local (`by`) is not a definition: absent (SPEC §4.3), not guessed.
    let by_use = raql_ra::Position { line: pos.line, col: pos.col + "self.size += ".len() as u32 };
    assert_eq!(defs_at(by_use), Vec::<String>::new());

    // Whitespace resolves to nothing.
    assert_eq!(defs_at(raql_ra::Position { line: 2, col: 0 }), Vec::<String>::new());
}

#[test]
fn projection_is_exact() {
    let fixture = Fixture::load("defs_ws");
    let widget = def_by_path(&fixture, "Widget", "defs_ws::widgets::Widget");
    let projected = project_def(&fixture.db, &fixture.workspace_root(), widget);
    assert_eq!(projected.kind.tag(), "STRUCT");
    assert_eq!(projected.path, Projected::Text("defs_ws::widgets::Widget".to_owned()));
    assert_eq!(projected.location, Projected::Text("src/widgets.rs:1:1-3:2".to_owned()));
    assert_eq!(projected.handle, Projected::Text("@H:struct:defs_ws::widgets::Widget".to_owned()));
}

/// Registry sanity: every catalog entry's modes are arity-consistent and
/// non-disabled entries declare at least one mode. (Operator dispatch
/// exhaustiveness is enforced by the `OperatorId` enum at compile time.)
#[test]
fn catalog_is_arity_consistent() {
    for predicate in v0_catalog().entries() {
        assert!(
            predicate.is_disabled() || !predicate.modes.is_empty(),
            "`{}` must declare modes or be disabled",
            predicate.name,
        );
        for mode in predicate.modes {
            assert_eq!(
                mode.pattern.len(),
                predicate.arity(),
                "`{}` mode pattern arity",
                predicate.name,
            );
        }
    }
}
