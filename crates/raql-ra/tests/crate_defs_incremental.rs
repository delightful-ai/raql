//! SPEC §16.3 incrementality assertions for the tracked `raql_crate_defs`
//! query (the G2/G3 shape): memoized within a revision, *not* invalidated
//! by body edits (its dependencies are the def maps and item lists, not
//! bodies), recomputed precisely when the item set changes.
//!
//! Lives in its own integration-test binary so the process-global
//! execution counter sees no traffic from other tests.

mod common;

use common::Fixture;
use raql_plan::{OperatorId, OperatorSet};
use raql_ra::{Value, raql_crate_defs_execution_count};

const GADGETS_RS: &str = include_str!("fixtures/defs_ws/src/gadgets.rs");

fn def_count(fixture: &Fixture) -> usize {
    fixture
        .operators()
        .invoke(OperatorId::DefsScan, &[])
        .expect("def scan succeeds")
        .len()
}

#[test]
fn crate_defs_memoizes_and_invalidates_precisely() {
    let mut fixture = Fixture::load("defs_ws");

    // First scan executes the query once per local crate (`defs_ws` and
    // its path dependency `dep_lib`).
    let before = raql_crate_defs_execution_count();
    let baseline = def_count(&fixture);
    assert_eq!(raql_crate_defs_execution_count(), before + 2);

    // Same revision: memoized.
    assert_eq!(def_count(&fixture), baseline);
    assert_eq!(raql_crate_defs_execution_count(), before + 2);

    // A body-only edit changes no def maps: revalidated, not re-executed.
    let body_edit = GADGETS_RS.replace("    2\n", "    2 + 40\n");
    assert_ne!(body_edit, GADGETS_RS);
    fixture.apply_edit("src/gadgets.rs", &body_edit);
    assert_eq!(def_count(&fixture), baseline);
    assert_eq!(
        raql_crate_defs_execution_count(),
        before + 2,
        "a body edit must not recompute raql_crate_defs",
    );

    // Adding an item recomputes the edited crate — and only it: the
    // dependency crate's memo survives.
    let item_edit = format!("{body_edit}\npub fn freshly_added() -> u32 {{ 8 }}\n");
    fixture.apply_edit("src/gadgets.rs", &item_edit);
    assert_eq!(def_count(&fixture), baseline + 1);
    assert_eq!(raql_crate_defs_execution_count(), before + 3);

    // The new def is real: seedable by name through the catalog boundary.
    let rows = fixture
        .operators()
        .invoke(OperatorId::DefsByExactName, &[Value::string("freshly_added")])
        .expect("seeding succeeds");
    assert_eq!(rows.len(), 1);
}
