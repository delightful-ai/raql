use std::fs;
use std::process::Command;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("raql_cli_{label}_{}_{}", std::process::id(), stamp));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn write_workspace(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "cli_daemon_cutover"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write Cargo.toml");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn alpha() {}
pub fn beta() { alpha(); }
"#,
    )
    .expect("write lib.rs");
}

#[test]
fn lang_run_is_daemon_backed_and_reuses_warm_session() {
    let work = temp_dir("daemon_cutover");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl hit(Name: string).
hit(Name) :- def(D), is_fn(D), def_name(D, Name), contains(Name, "alpha").
"#,
    )
    .expect("write query");

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_raql"))
            .args([
                "lang",
                "run",
                query.to_str().expect("utf8 query"),
                "--rust-file",
                workspace.to_str().expect("utf8 workspace"),
                "--include-dir",
                env!("CARGO_MANIFEST_DIR"),
            ])
            .output()
            .expect("run raql")
    };

    let first = run();
    assert!(
        first.status.success(),
        "first daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("daemon: cold"), "stdout={first_stdout}");
    assert!(first_stdout.contains("alpha"), "stdout={first_stdout}");

    let second = run();
    assert!(
        second.status.success(),
        "second daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains("daemon: warm"), "stdout={second_stdout}");
}

#[test]
fn concurrent_cold_starts_share_one_daemon_startup() {
    let work = temp_dir("daemon_concurrent_cold_start");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl hit(Name: string).
hit(Name) :- def(D), is_fn(D), def_name(D, Name), contains(Name, "alpha").
"#,
    )
    .expect("write query");

    let run = |query: std::path::PathBuf, workspace: std::path::PathBuf| {
        Command::new(env!("CARGO_BIN_EXE_raql"))
            .args([
                "lang",
                "run",
                query.to_str().expect("utf8 query"),
                "--rust-file",
                workspace.to_str().expect("utf8 workspace"),
                "--include-dir",
                env!("CARGO_MANIFEST_DIR"),
            ])
            .output()
            .expect("run raql")
    };

    let first_query = query.clone();
    let first_workspace = workspace.clone();
    let second_query = query.clone();
    let second_workspace = workspace.clone();
    let first = thread::spawn(move || run(first_query, first_workspace));
    let second = thread::spawn(move || run(second_query, second_workspace));
    let first = first.join().expect("first join");
    let second = second.join().expect("second join");

    assert!(
        first.status.success(),
        "first concurrent run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "second concurrent run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout)
    );
    assert!(combined.contains("daemon: cold"), "stdout={combined}");
    assert!(combined.contains("daemon: warm"), "stdout={combined}");
}

#[test]
fn lang_run_resolves_relative_query_and_include_paths_per_invocation() {
    let work = temp_dir("relative_paths");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
pub fn alpha() {}
pub fn beta() {}
"#,
    )
    .expect("write lib.rs");

    let dir_a = work.join("dir_a");
    let dir_b = work.join("dir_b");
    fs::create_dir_all(&dir_a).expect("create dir_a");
    fs::create_dir_all(&dir_b).expect("create dir_b");

    fs::write(dir_a.join("query.raql"), ".include \"helper.raql\".\n").expect("write dir_a query");
    fs::write(
        dir_a.join("helper.raql"),
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "alpha").
"#,
    )
    .expect("write dir_a helper");

    fs::write(dir_b.join("query.raql"), ".include \"helper.raql\".\n").expect("write dir_b query");
    fs::write(
        dir_b.join("helper.raql"),
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "beta").
"#,
    )
    .expect("write dir_b helper");

    let run = |cwd: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_raql"))
            .current_dir(cwd)
            .args([
                "lang",
                "run",
                "query.raql",
                "--rust-file",
                workspace.to_str().expect("utf8 workspace"),
                "--include-dir",
                ".",
            ])
            .output()
            .expect("run raql")
    };

    let first = run(&dir_a);
    assert!(
        first.status.success(),
        "first relative-path run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("daemon: cold"), "stdout={first_stdout}");
    assert!(first_stdout.contains("alpha"), "stdout={first_stdout}");

    let second = run(&dir_b);
    assert!(
        second.status.success(),
        "second relative-path run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains("daemon: warm"), "stdout={second_stdout}");
    assert!(second_stdout.contains("beta"), "stdout={second_stdout}");
}

#[test]
fn lang_run_refreshes_visible_defs_after_incremental_source_edits() {
    let work = temp_dir("incremental_refresh");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(workspace.join("src/lib.rs"), "pub fn alpha_only_marker() {}\n").expect("write initial lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl visible(Name: string).
visible(Name) :- def(D), is_fn(D), def_name(D, Name).
"#,
    )
    .expect("write query");

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_raql"))
            .args([
                "lang",
                "run",
                query.to_str().expect("utf8 query"),
                "--rust-file",
                workspace.to_str().expect("utf8 workspace"),
                "--include-dir",
                env!("CARGO_MANIFEST_DIR"),
            ])
            .output()
            .expect("run raql")
    };

    let first = run();
    assert!(
        first.status.success(),
        "first daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("daemon: cold"), "stdout={first_stdout}");
    assert!(first_stdout.contains("alpha_only_marker"), "stdout={first_stdout}");

    fs::write(workspace.join("src/lib.rs"), "pub fn omega_only_marker() {}\n").expect("rewrite lib.rs");

    let second = run();
    assert!(
        second.status.success(),
        "second daemon-backed run should succeed after source edit; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains("daemon: warm"), "stdout={second_stdout}");
    assert!(second_stdout.contains("omega_only_marker"), "stdout={second_stdout}");
    assert!(
        !second_stdout.contains("alpha_only_marker"),
        "warm daemon run should not retain stale defs after incremental edit; stdout={second_stdout}"
    );
}
