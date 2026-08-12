//! Dev-only P2 latency probe (SPEC §15): exact-name seed (`def_name(-,+)`,
//! C2) followed by the C0/C1 projections (`def_kind`, `def_path`,
//! `def_span`, `handle`, `span_key`) for every seeded def, driven through
//! the catalog operator boundary exactly as the engine will drive it.
//!
//! Quarantined direct-runtime path (AGENTS.md): measurement only, never
//! public CLI behavior. Timing claims require a release build:
//!
//! ```sh
//! cargo run --release -p raql-ra --example p2_probe -- <workspace-root> <def-name>
//! ```
//!
//! Reports the cold first iteration separately, then p50/p95/max over the
//! warm iterations. Target (SPEC §15): ≤ 100ms p95 warm.

use std::path::Path;
use std::time::{Duration, Instant};

use ide_db::RootDatabase;
use load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_into_db};
use project_model::{CargoConfig, ProjectManifest, ProjectWorkspace, RustLibSource};
use raql_plan::{OperatorId, OperatorSet};
use raql_ra::{SnapshotOperators, Value};

const WARM_ITERATIONS: usize = 100;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(root), Some(def_name)) = (args.next(), args.next()) else {
        eprintln!("usage: p2_probe <workspace-root> <def-name>");
        std::process::exit(2);
    };

    let load_started = Instant::now();
    let root = Path::new(&root).canonicalize().expect("workspace root exists");
    let abs_root = vfs::AbsPathBuf::assert_utf8(root.clone());
    let manifest = ProjectManifest::discover_single(&abs_root).expect("workspace manifest");
    let cargo_config = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        all_targets: true,
        set_test: true,
        no_deps: false,
        ..CargoConfig::default()
    };
    let mut workspace =
        ProjectWorkspace::load(manifest, &cargo_config, &|_| {}).expect("workspace loads");
    let build_scripts = workspace
        .run_build_scripts(&cargo_config, &|_| {})
        .expect("build scripts run");
    workspace.set_build_scripts(build_scripts);
    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: true,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
        num_worker_threads: std::thread::available_parallelism().map_or(1, usize::from),
        proc_macro_processes: 1,
    };
    let mut db = RootDatabase::new(None);
    let (_vfs, _proc_macro_server) =
        load_workspace_into_db(workspace, &cargo_config.extra_env, &load_config, &mut db)
            .expect("load_workspace_into_db succeeds");
    println!("p2-probe load {}ms", load_started.elapsed().as_millis());

    let mut timings = Vec::with_capacity(1 + WARM_ITERATIONS);
    let mut seeded = 0usize;
    let mut projected_rows = 0usize;
    for _ in 0..(1 + WARM_ITERATIONS) {
        let started = Instant::now();
        let (defs, rows) = run_p2(&db, &root, &def_name);
        timings.push(started.elapsed());
        (seeded, projected_rows) = (defs, rows);
    }

    if seeded == 0 {
        eprintln!("p2-probe: no defs named `{def_name}` — pick a real seed");
        std::process::exit(1);
    }
    let cold = timings[0];
    let mut warm: Vec<Duration> = timings[1..].to_vec();
    warm.sort();
    println!(
        "p2-probe seed `{def_name}` -> {seeded} defs, {projected_rows} projection rows/iter",
    );
    println!("p2-probe cold {}ms", cold.as_millis());
    println!(
        "p2-probe warm p50 {}µs, p95 {}µs, max {}µs ({} iterations)",
        warm[warm.len() / 2].as_micros(),
        warm[warm.len() * 95 / 100].as_micros(),
        warm[warm.len() - 1].as_micros(),
        warm.len(),
    );
}

/// One P2 request: seed by exact name, then run every keyed projection on
/// every seeded def (and `span_key` on each def's span).
fn run_p2(db: &RootDatabase, workspace_root: &Path, name: &str) -> (usize, usize) {
    let mut ops = SnapshotOperators::new(db, workspace_root);
    let seeds = ops
        .invoke(OperatorId::DefsByExactName, &[Value::string(name)])
        .expect("seed succeeds");
    let mut rows = 0usize;
    for row in &seeds {
        let def = row[0].clone();
        for operator in [
            OperatorId::KindOfDef,
            OperatorId::CanonicalPathOfDef,
            OperatorId::HandleOfDef,
        ] {
            rows += ops.invoke(operator, &[def.clone()]).expect("projection succeeds").len();
        }
        let spans = ops.invoke(OperatorId::SpanOfDef, &[def]).expect("def_span succeeds");
        rows += spans.len();
        for span_row in spans {
            let span = span_row[1].clone();
            rows += ops
                .invoke(OperatorId::SpanKeyOfSpan, &[span])
                .expect("span_key succeeds")
                .len();
        }
    }
    (seeds.len(), rows)
}
