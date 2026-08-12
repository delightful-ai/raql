//! Dev-only timing probe for the SPEC §6.4 spike: cold/warm `raql_callees`
//! timings on a real workspace (first data points of SPEC §15).
//!
//! This is a quarantined direct-runtime path (AGENTS.md): it exists to
//! measure the spike query and must never be wired into public CLI behavior.
//! Timing claims require a release build:
//!
//! ```sh
//! cargo run --release -p raql-ra --example spike_timings -- <workspace-root> <fn-name>
//! ```

use std::path::Path;
use std::time::Instant;

use ide_db::RootDatabase;
use load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_into_db};
use project_model::{CargoConfig, ProjectManifest, ProjectWorkspace, RustLibSource};
use raql_ra::raql_callees;
use vfs::AbsPathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(root), Some(fn_name)) = (args.next(), args.next()) else {
        eprintln!("usage: spike_timings <workspace-root> <fn-name>");
        std::process::exit(2);
    };

    let load_started = Instant::now();
    let root = AbsPathBuf::assert_utf8(
        Path::new(&root).canonicalize().expect("workspace root exists"),
    );
    let manifest = ProjectManifest::discover_single(&root).expect("workspace manifest");
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
    println!("spike-timing load {}ms", load_started.elapsed().as_millis());

    let resolve_started = Instant::now();
    let target = hir::attach_db(&db, || {
        hir::Crate::all(&db)
            .into_iter()
            .filter(|krate| krate.origin(&db).is_local())
            .flat_map(|krate| krate.modules(&db))
            .flat_map(|module| module.declarations(&db))
            .find_map(|decl| match decl {
                hir::ModuleDef::Function(f) if f.name(&db).as_str() == fn_name => Some(f),
                _ => None,
            })
    });
    let Some(target) = target else {
        eprintln!("function `{fn_name}` not found in local crates");
        std::process::exit(1);
    };
    println!("spike-timing resolve {}ms", resolve_started.elapsed().as_millis());

    let cold_started = Instant::now();
    let sites = raql_callees(&db, target);
    println!(
        "spike-timing cold {}ms ({} callsites)",
        cold_started.elapsed().as_millis(),
        sites.len(),
    );

    let mut warm = Vec::with_capacity(100);
    for _ in 0..100 {
        let warm_started = Instant::now();
        let _ = raql_callees(&db, target);
        warm.push(warm_started.elapsed());
    }
    warm.sort();
    println!(
        "spike-timing warm p50 {}µs, max {}µs (100 memoized calls)",
        warm[warm.len() / 2].as_micros(),
        warm[warm.len() - 1].as_micros(),
    );
}
