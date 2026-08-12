//! SPEC §6.4 feasibility-spike gates G1–G6.
//!
//! Each test loads the `spike_ws` fixture through `load_workspace_into_db`
//! (the same entry point the future `raql-server` uses) and drives the
//! `raql_callees` tracked query on a real `RootDatabase`.
//!
//! The tests share one process-global execution counter in `raql-ra`, so they
//! serialize on a mutex to keep counter deltas exact.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, mpsc};

use base_db::salsa::Cancelled;
use common::{Fixture, find_adt, find_fn};
use hir::db::HirDatabase;
use ide_db::RootDatabase;
use raql_ra::{DispatchKind, raql_callees, raql_callees_execution_count};

static GATE_LOCK: Mutex<()> = Mutex::new(());

fn gate_lock() -> MutexGuard<'static, ()> {
    GATE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

const LIB_RS: &str = include_str!("fixtures/spike_ws/src/lib.rs");
const EXTRA_RS: &str = include_str!("fixtures/spike_ws/src/extra.rs");

fn callee_summary(db: &RootDatabase, f: hir::Function) -> Vec<(String, DispatchKind)> {
    let sites = raql_callees(db, f);
    hir::attach_db(db, || {
        let mut summary: Vec<_> = sites
            .iter()
            .map(|site| (site.peer.name(db).as_str().to_owned(), site.dispatch))
            .collect();
        summary.sort();
        summary
    })
}

/// G1: the tracked query compiles against the pinned RA rev and answers on a
/// database constructed via `load_workspace_into_db`.
#[test]
fn g1_loads_and_answers() {
    let _guard = gate_lock();
    let ws = Fixture::load("spike_ws");
    let alpha = find_fn(&ws.db, "alpha");
    assert_eq!(
        callee_summary(&ws.db, alpha),
        vec![
            ("beta".to_owned(), DispatchKind::Direct),
            ("bump".to_owned(), DispatchKind::Direct),
            ("gamma".to_owned(), DispatchKind::Direct),
        ],
    );
}

/// G2: two consecutive calls with the same `hir::Function` on the same
/// revision execute the body once.
#[test]
fn g2_memoizes() {
    let _guard = gate_lock();
    let ws = Fixture::load("spike_ws");
    let alpha = find_fn(&ws.db, "alpha");
    let before = raql_callees_execution_count();
    let first = raql_callees(&ws.db, alpha);
    assert_eq!(raql_callees_execution_count(), before + 1);
    let second = raql_callees(&ws.db, alpha);
    assert_eq!(raql_callees_execution_count(), before + 1, "second call must be memoized");
    assert_eq!(first, second);
}

/// G3: an edit to an unrelated file does not recompute the query; an edit to
/// the function's own body does.
#[test]
fn g3_invalidates_precisely() {
    let _guard = gate_lock();
    let mut ws = Fixture::load("spike_ws");
    let alpha = find_fn(&ws.db, "alpha");
    let baseline = callee_summary(&ws.db, alpha);
    let after_first = raql_callees_execution_count();

    // Unrelated edit: a body-only change in another file.
    let unrelated = EXTRA_RS.replace("41", "43");
    assert_ne!(unrelated, EXTRA_RS);
    ws.apply_edit("src/extra.rs", &unrelated);
    assert_eq!(callee_summary(&ws.db, alpha), baseline);
    assert_eq!(
        raql_callees_execution_count(),
        after_first,
        "edit to an unrelated file must not recompute raql_callees",
    );

    // Related edit: alpha's own body.
    let related = LIB_RS.replace("let from_beta = beta();", "let from_beta = beta() + 10;");
    assert_ne!(related, LIB_RS);
    ws.apply_edit("src/lib.rs", &related);
    assert_eq!(callee_summary(&ws.db, alpha), baseline, "callee set is unchanged by this edit");
    assert_eq!(
        raql_callees_execution_count(),
        after_first + 1,
        "edit to the function's body must recompute raql_callees",
    );
}

/// G4: `apply_change` during an in-flight evaluation unwinds with Salsa
/// cancellation; catching at the request boundary and retrying succeeds.
#[test]
fn g4_cancels_and_retries() {
    let _guard = gate_lock();
    let mut ws = Fixture::load("spike_ws");
    let alpha = find_fn(&ws.db, "alpha");

    let snapshot = ws.db.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        Cancelled::catch(|| {
            let mut iterations: u64 = 0;
            loop {
                // Every salsa fetch (memo hits included) checks the
                // cancellation flag, so this loop unwinds as soon as the
                // write below starts.
                let _ = raql_callees(&snapshot, alpha);
                iterations += 1;
                if iterations == 1 {
                    started_tx.send(()).expect("main thread is waiting");
                }
            }
        })
    });

    started_rx.recv().expect("worker started");
    let edit = EXTRA_RS.replace("41", "45");
    ws.apply_edit("src/extra.rs", &edit);

    let result: Result<(), Cancelled> = worker.join().expect("worker must not panic");
    assert!(result.is_err(), "in-flight evaluation must unwind with Cancelled");

    // Retry on a fresh snapshot succeeds.
    let retry = ws.db.clone();
    assert_eq!(callee_summary(&retry, alpha).len(), 3);
}

/// G5: no shadow state in the slice, and `attach_db` stays behind its one
/// audited choke point (`snapshot.rs`). The workspace-level grep gate is in
/// the commit evidence; this keeps the crate-local part executable over
/// every library source file.
#[test]
fn g5_no_shadow_state() {
    let sources = [
        ("lib.rs", include_str!("../src/lib.rs")),
        ("calls.rs", include_str!("../src/calls.rs")),
        ("crate_defs.rs", include_str!("../src/crate_defs.rs")),
        ("def.rs", include_str!("../src/def.rs")),
        ("value.rs", include_str!("../src/value.rs")),
        ("snapshot.rs", include_str!("../src/snapshot.rs")),
        ("operators.rs", include_str!("../src/operators.rs")),
        ("operators/def_name.rs", include_str!("../src/operators/def_name.rs")),
        ("operators/def_meta.rs", include_str!("../src/operators/def_meta.rs")),
        ("operators/def_at.rs", include_str!("../src/operators/def_at.rs")),
        ("operators/calls.rs", include_str!("../src/operators/calls.rs")),
        ("operators/scans.rs", include_str!("../src/operators/scans.rs")),
        ("operators/span.rs", include_str!("../src/operators/span.rs")),
        ("projection.rs", include_str!("../src/projection.rs")),
    ];
    for (name, src) in sources {
        for banned in [
            "mtime",
            "SystemTime",
            "fingerprint",
            "content_revision",
            "Mutex",
            "RwLock",
            "OnceLock",
            "LazyLock",
        ] {
            assert!(
                !src.contains(banned),
                "raql-ra source must not hold shadow state (`{banned}` in {name})",
            );
        }
        if name != "snapshot.rs" {
            assert!(
                !src.contains("attach_db"),
                "`attach_db` must only appear in snapshot.rs (found in {name})",
            );
        }
    }
}

static ADT_PROBE_EXECUTIONS: AtomicU64 = AtomicU64::new(0);

/// Test-local tracked query keyed on `hir::Adt`, for the G6 ADT-handle leg.
/// Lives here (not in the crate) so the production extension stays exactly
/// one tracked query, per SPEC §6.4.
fn adt_field_count(db: &dyn HirDatabase, adt: hir::Adt) -> usize {
    #[salsa::interned]
    struct InternedAdt {
        #[returns(copy)]
        adt: hir::Adt,
    }

    #[salsa::tracked]
    fn adt_field_count<'db>(db: &'db dyn HirDatabase, key: InternedAdt<'db>) -> usize {
        ADT_PROBE_EXECUTIONS.fetch_add(1, Ordering::Relaxed);
        hir::attach_db(db, || match key.adt(db) {
            hir::Adt::Struct(strukt) => strukt.fields(db).len(),
            hir::Adt::Enum(enum_) => enum_.variants(db).len(),
            hir::Adt::Union(union_) => union_.fields(db).len(),
        })
    }

    *adt_field_count(db, InternedAdt::new(db, adt))
}

/// G6: `hir::Function` and an ADT handle work as tracked-query keys across
/// revisions where the item persists (the re-resolved handle compares equal
/// and the memo table keyed on it behaves).
#[test]
fn g6_handles_are_stable_keys() {
    let _guard = gate_lock();
    let mut ws = Fixture::load("spike_ws");

    // Function leg.
    let alpha_before = find_fn(&ws.db, "alpha");
    let baseline = callee_summary(&ws.db, alpha_before);
    let related = LIB_RS.replace("let from_beta = beta();", "let from_beta = beta() + 20;");
    ws.apply_edit("src/lib.rs", &related);
    let alpha_after = find_fn(&ws.db, "alpha");
    assert_eq!(
        alpha_before, alpha_after,
        "hir::Function handle must persist across a body edit",
    );
    assert_eq!(callee_summary(&ws.db, alpha_after), baseline);

    // ADT leg.
    let widget_before = find_adt(&ws.db, "Widget");
    let probes_start = ADT_PROBE_EXECUTIONS.load(Ordering::Relaxed);
    assert_eq!(adt_field_count(&ws.db, widget_before), 1);
    assert_eq!(adt_field_count(&ws.db, widget_before), 1);
    assert_eq!(
        ADT_PROBE_EXECUTIONS.load(Ordering::Relaxed),
        probes_start + 1,
        "adt probe must memoize on the same revision",
    );

    let with_field = related.replace(
        "pub struct Widget {\n    pub count: u32,\n}",
        "pub struct Widget {\n    pub count: u32,\n    pub label: u32,\n}",
    );
    assert_ne!(with_field, related);
    ws.apply_edit("src/lib.rs", &with_field);

    let widget_after = find_adt(&ws.db, "Widget");
    assert_eq!(
        widget_before, widget_after,
        "hir::Adt handle must persist across a field addition",
    );
    assert_eq!(adt_field_count(&ws.db, widget_after), 2);
    assert_eq!(ADT_PROBE_EXECUTIONS.load(Ordering::Relaxed), probes_start + 2);
}
