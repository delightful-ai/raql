use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("raql_cli_{label}_{}_{}", std::process::id(), stamp));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn raql_bin() -> std::path::PathBuf {
    let cargo_bin = std::path::PathBuf::from(env!("CARGO_BIN_EXE_raql"));
    cargo_bin
        .parent()
        .and_then(|dir| dir.parent())
        .map(|dir| dir.join("raql"))
        .filter(|path| path.is_file())
        .unwrap_or(cargo_bin)
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
.decl hit(Name: string) output.
hit(Name) :- def(D), is_fn(D), def_name(D, Name), contains(Name, "alpha").
"#,
    )
    .expect("write query");

    let run = || {
        Command::new(raql_bin())
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

    let second = run();
    assert!(
        second.status.success(),
        "second daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
}

#[test]
fn lang_run_supports_exact_name_only_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_exact_name_only");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.decl hit(Name: string).
hit(Name) :- def_name(_, "alpha"), Name = "alpha".
"#,
    )
    .expect("write query");

    let run = || {
        Command::new(raql_bin())
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
        "exact-name daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("daemon: cold"), "stdout={first_stdout}");
    assert!(first_stdout.contains("alpha"), "stdout={first_stdout}");

    let second = run();
    assert!(
        second.status.success(),
        "warm exact-name daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains("daemon: warm"), "stdout={second_stdout}");
    assert!(second_stdout.contains("alpha"), "stdout={second_stdout}");
}

#[test]
fn lang_run_supports_stdlib_exact_name_seed_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_stdlib_exact_name_seed");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl hit(Name: string).
hit(Name) :- def_name(T, "alpha"), is_fn(T), def_name(T, Name).
"#,
    )
    .expect("write query");

    let run = || {
        Command::new(raql_bin())
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
        "stdlib exact-name seed daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("daemon: cold"), "stdout={first_stdout}");
    assert!(first_stdout.contains("alpha"), "stdout={first_stdout}");

    let second = run();
    assert!(
        second.status.success(),
        "warm stdlib exact-name seed daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains("daemon: warm"), "stdout={second_stdout}");
    assert!(second_stdout.contains("alpha"), "stdout={second_stdout}");
}

#[test]
fn lang_run_supports_stdlib_exact_name_visibility_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_stdlib_exact_name_visibility");
    let workspace = work.join("ws");
    fs::create_dir_all(workspace.join("src")).expect("create src");
    fs::write(
        workspace.join("Cargo.toml"),
        r#"[package]
name = "cli_daemon_visibility"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write Cargo.toml");
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
pub fn alpha() {}
fn hidden() {}
#[cfg(test)]
mod tests {
    pub fn helper() {}
}
"#,
    )
    .expect("write lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl public_hit(Name: string).
.decl hidden_hit(Name: string).
public_hit(Name) :- def_name(T, "alpha"), is_public(T), def_name(T, Name).
hidden_hit(Name) :- def_name(T, "hidden"), is_public(T), def_name(T, Name).
"#,
    )
    .expect("write query");

    let run = || {
        Command::new(raql_bin())
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
        "stdlib exact-name visibility daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("daemon: cold"), "stdout={first_stdout}");
    assert!(first_stdout.contains("public_hit(\"alpha\")"), "stdout={first_stdout}");
    assert!(!first_stdout.contains("hidden_hit(\"hidden\")"), "stdout={first_stdout}");

    let second = run();
    assert!(
        second.status.success(),
        "warm stdlib exact-name visibility daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains("daemon: warm"), "stdout={second_stdout}");
    assert!(second_stdout.contains("public_hit(\"alpha\")"), "stdout={second_stdout}");
    assert!(!second_stdout.contains("hidden_hit(\"hidden\")"), "stdout={second_stdout}");
}

#[test]
fn lang_run_supports_lookup_seeded_span_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_lookup_seeded_span");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.decl hit(RelPath: string).
hit(RelPath) :-
  def_name(D, "alpha"),
  def_span(D, S),
  span_allowed(S),
  span_key(S, RelPath, _L0, _C0, _L1, _C1).
"#,
    )
    .expect("write query");

    let run = || {
        Command::new(raql_bin())
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
        "lookup-seeded span daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("src/lib.rs"), "stdout={first_stdout}");
    assert!(first_stdout.contains("daemon: cold"), "stdout={first_stdout}");

    let second = run();
    assert!(
        second.status.success(),
        "warm lookup-seeded span daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains("src/lib.rs"), "stdout={second_stdout}");
    assert!(second_stdout.contains("daemon: warm"), "stdout={second_stdout}");
}

#[test]
fn lang_run_supports_lookup_seeded_handle_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_lookup_seeded_handle");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.decl hit(H: string).
hit(H) :- def_name(D, "alpha"), handle(D, H).
"#,
    )
    .expect("write query");

    let run = || {
        Command::new(raql_bin())
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
        "lookup-seeded handle daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(
        first_stdout.contains("def://alpha") || first_stdout.contains("::alpha"),
        "stdout={first_stdout}"
    );
    assert!(first_stdout.contains("daemon: cold"), "stdout={first_stdout}");

    let second = run();
    assert!(
        second.status.success(),
        "warm lookup-seeded handle daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        second_stdout.contains("def://alpha") || second_stdout.contains("::alpha"),
        "stdout={second_stdout}"
    );
    assert!(second_stdout.contains("daemon: warm"), "stdout={second_stdout}");
}

#[test]
fn lang_run_supports_stdlib_exact_name_seed_queries_with_duplicate_names_on_the_daemon_path() {
    let work = temp_dir("daemon_stdlib_exact_name_duplicates");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
pub mod first {
    pub fn load_and_plan() {}
}

pub mod second {
    pub fn load_and_plan() {}
}
"#,
    )
    .expect("write lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl hit(Path: string) output.
hit(Path) :- def_name(T, "load_and_plan"), is_fn(T), def_path(T, Path).
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        output.status.success(),
        "duplicate exact-name daemon-backed run should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("RAQL0907"),
        "reverse exact-name lookup should not trip function-cardinality on duplicate names; stdout={stdout}"
    );
    assert!(
        stdout.contains("cli_daemon_cutover::first::load_and_plan"),
        "stdout={stdout}"
    );
    assert!(
        stdout.contains("cli_daemon_cutover::second::load_and_plan"),
        "stdout={stdout}"
    );
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
        Command::new(raql_bin())
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
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "alpha").
"#,
    )
    .expect("write dir_a helper");

    fs::write(dir_b.join("query.raql"), ".include \"helper.raql\".\n").expect("write dir_b query");
    fs::write(
        dir_b.join("helper.raql"),
        r#"
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "beta").
"#,
    )
    .expect("write dir_b helper");

    let run = |cwd: &std::path::Path| {
        Command::new(raql_bin())
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
        Command::new(raql_bin())
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

#[test]
fn lang_run_respawns_cleanly_after_daemon_idle_shutdown() {
    let work = temp_dir("idle_shutdown");
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
        Command::new(raql_bin())
            .env("RAQL_DAEMON_IDLE_TIMEOUT_MS", "200")
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

    thread::sleep(Duration::from_millis(500));

    let second = run();
    assert!(
        second.status.success(),
        "daemon-backed run after idle timeout should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        second_stdout.contains("daemon: cold"),
        "daemon should have idled out and respawned cleanly; stdout={second_stdout}"
    );
}

#[test]
fn lang_run_reports_executed_workspace_state_after_sync_and_reload() {
    let work = temp_dir("session_state");
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
        Command::new(raql_bin())
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
    assert!(first_stdout.contains("workspace_epoch: 0"), "stdout={first_stdout}");
    assert!(first_stdout.contains("content_revision: 0"), "stdout={first_stdout}");

    fs::write(workspace.join("src/lib.rs"), "pub fn alpha() {}\npub fn omega() {}\n").expect("rewrite lib.rs");

    let second = run();
    assert!(
        second.status.success(),
        "second daemon-backed run should succeed after source edit; stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains("daemon: warm"), "stdout={second_stdout}");
    assert!(second_stdout.contains("workspace_epoch: 0"), "stdout={second_stdout}");
    assert!(
        second_stdout.contains("content_revision: 1"),
        "session metadata should reflect the incrementally synced revision; stdout={second_stdout}"
    );

    let manifest = workspace.join("Cargo.toml");
    let mut manifest_text = fs::read_to_string(&manifest).expect("read Cargo.toml");
    manifest_text.push_str(
        r#"
[features]
extra = []
"#,
    );
    fs::write(&manifest, manifest_text).expect("rewrite Cargo.toml");

    let third = run();
    assert!(
        third.status.success(),
        "third daemon-backed run should succeed after manifest change; stdout={} stderr={}",
        String::from_utf8_lossy(&third.stdout),
        String::from_utf8_lossy(&third.stderr)
    );
    let third_stdout = String::from_utf8_lossy(&third.stdout);
    assert!(third_stdout.contains("daemon: warm"), "stdout={third_stdout}");
    assert!(
        third_stdout.contains("workspace_epoch: 1"),
        "session metadata should reflect the reloaded workspace epoch; stdout={third_stdout}"
    );
    assert!(
        third_stdout.contains("content_revision: 2"),
        "session metadata should reflect the executed post-reload revision; stdout={third_stdout}"
    );
}

#[test]
fn lang_run_rejects_semantic_search_on_the_daemon_path() {
    let work = temp_dir("daemon_search");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        "pub fn alpha_search_marker() {}\npub fn omega_search_marker() {}\n",
    )
    .expect("write lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl hit(Name: string).
hit(Name) :- search("alpha", D, _Score), def_name(D, Name).
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        !output.status.success(),
        "daemon-backed search query should be rejected until rebuilt from RA-native truth; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("search"), "stderr={stderr}");
}

#[test]
fn lang_run_rejects_structure_and_trait_queries_until_their_families_land() {
    // The v1 structure/trait family (`field`, `variant`, `method`,
    // `trait_method`, `implements`, `from_impl`) enters the catalog only
    // with its operators and proof matrix (SPEC §8.6). Until then the
    // predicates do not exist, and the refusal happens at compile time.
    let work = temp_dir("daemon_structure_trait");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl field_hit(Name: string).
field_hit(Name) :- def_name(Person, "Person"), field(Person, Name, _).
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        !output.status.success(),
        "structure/trait queries must refuse until the family lands; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("field"), "stderr={stderr}");
}

#[test]
fn lang_run_rejects_type_surface_queries_until_their_families_land() {
    // The type-navigation family (`fn_return_type`, `ty_*`) is not in the
    // v0 catalog (SPEC §8.6); it returns with operators + proof matrix.
    let work = temp_dir("daemon_type_surface");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl app_hit(F: Def).
app_hit(F) :- def_name(F, "make_wrapper"), fn_return_type(F, _).
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        !output.status.success(),
        "type-surface queries must refuse until the family lands; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fn_return_type"), "stderr={stderr}");
}

#[test]
fn lang_run_supports_call_graph_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_call_graph");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
pub fn direct_target() {}
pub fn direct_caller() { direct_target(); }

pub trait Greeter {
    fn greet(&self);
}

pub struct Person;

impl Greeter for Person {
    fn greet(&self) {}
}

pub fn trait_caller(person: Person) { person.greet(); }
pub fn dyn_caller(greeter: &dyn Greeter) { greeter.greet(); }
pub fn closure_caller() { let closure = || direct_target(); closure(); }
pub fn fn_pointer_caller(fp: fn()) { fp(); }
"#,
    )
    .expect("write lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl direct_hit(Target: string, Label: string).
.decl trait_hit(Label: string).
.decl dyn_hit(Label: string).
.decl closure_hit(Label: string).
.decl fn_pointer_hit(Label: string).
direct_hit(Target, Label) :- def(Caller), def_name(Caller, "direct_caller"), call_edge(Caller, Callee, _, Dispatch), def_name(Callee, Target), dispatch_str(Dispatch, Label).
trait_hit(Label) :- def(Caller), def_name(Caller, "trait_caller"), call_edge(Caller, _, _, Dispatch), dispatch_str(Dispatch, Label).
dyn_hit(Label) :- def(Caller), def_name(Caller, "dyn_caller"), call_edge(Caller, _, _, Dispatch), dispatch_str(Dispatch, Label).
closure_hit(Label) :- def(Caller), def_name(Caller, "closure_caller"), call_edge(Caller, _, _, Dispatch), dispatch_str(Dispatch, Label).
fn_pointer_hit(Label) :- def(Caller), def_name(Caller, "fn_pointer_caller"), call_edge(Caller, _, _, Dispatch), dispatch_str(Dispatch, Label).
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        output.status.success(),
        "daemon-backed call-graph query should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("direct_hit(\"direct_target\", \"direct\")"),
        "stdout={stdout}"
    );
    assert!(stdout.contains("trait_hit(\"through_trait\")"), "stdout={stdout}");
    assert!(stdout.contains("dyn_hit(\"dyn\")"), "stdout={stdout}");
    // Honest absences (SPEC §6.3, §8.6 caveats): the `closure()` and
    // `fp()` callsites have no resolvable `Def` callee and are absent —
    // the closure body's `direct_target()` call is attributed to the
    // enclosing named function instead.
    assert!(stdout.contains("closure_hit(\"direct\")"), "stdout={stdout}");
    assert!(!stdout.contains("closure_hit(\"closure\")"), "stdout={stdout}");
    assert!(!stdout.contains("fn_pointer_hit("), "stdout={stdout}");
}

#[test]
fn lang_run_supports_stdlib_caller_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_stdlib_caller");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
pub fn direct_target() {}
pub fn direct_caller() { direct_target(); }
"#,
    )
    .expect("write lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl caller_hit(Name: string).
.decl caller_path_hit().
caller_hit(Name) :-
  def_name(Target, "direct_target"),
  is_fn(Target),
  caller(Target, Caller, _, DispatchKind::DIRECT),
  def_name(Caller, Name).
caller_path_hit() :-
  def_name(Target, "direct_target"),
  is_fn(Target),
  caller(Target, Caller, _, DispatchKind::DIRECT),
  def_path(Caller, Path),
  contains(Path, "direct_caller").
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        output.status.success(),
        "daemon-backed stdlib caller query should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("caller_hit(\"direct_caller\")"), "stdout={stdout}");
    assert!(stdout.contains("caller_path_hit()"), "stdout={stdout}");
}

#[test]
fn lang_run_rejects_syntax_control_queries_until_their_families_land() {
    // The node/control family (`node_*`, `enclosing_control`) is not in
    // the v0 catalog (SPEC §8.6); it returns with operators + proof
    // matrix.
    let work = temp_dir("daemon_syntax_control");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl if_control_hit().
if_control_hit() :-
  def_name(F, "syntax_demo"),
  call_edge(F, _, Site, _),
  enclosing_control(Site, "IF", _, _).
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        !output.status.success(),
        "syntax/control queries must refuse until the family lands; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("enclosing_control"), "stderr={stderr}");
}

#[test]
fn lang_run_rejects_approximate_reference_event_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_reference_events");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Counter(pub i32);

pub fn event_demo(left: Counter, right: Counter) {
    let mut slot = left;
    if left == right {
        slot = right;
    }
    let _ = slot;
}
"#,
    )
    .expect("write lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl compare_hit(Op: string).
.decl write_hit().
compare_hit(Op) :-
  def(T),
  def_name(T, "Counter"),
  compares(T, _, Op, F),
  def_name(F, "event_demo").
write_hit() :-
  def(T),
  def_name(T, "Counter"),
  writes(T, _, F),
  def_name(F, "event_demo").
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        !output.status.success(),
        "approximate reference-event query should be rejected; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("compares") || stderr.contains("writes"),
        "stderr={stderr}"
    );
}

#[test]
fn lang_run_rejects_error_flow_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_error_flow");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
#[derive(Debug)]
pub enum ParseErr {
    Bad(String),
    Empty,
}

#[derive(Debug)]
pub enum AppErr {
    Parse(ParseErr),
}

impl From<ParseErr> for AppErr {
    fn from(err: ParseErr) -> Self {
        AppErr::Parse(err)
    }
}

pub fn parse(flag: bool) -> Result<(), ParseErr> {
    if flag {
        Err(ParseErr::Bad("bad".to_string()))
    } else {
        Err(ParseErr::Empty)
    }
}

pub fn propagate_parse(flag: bool) -> Result<(), AppErr> {
    parse(flag)?;
    Ok(())
}

pub fn handle_parse(flag: bool) -> Result<(), AppErr> {
    match parse(flag) {
        Err(ParseErr::Bad(_)) => Ok(()),
        Err(_) => Ok(()),
        Ok(()) => Ok(()),
    }
}
"#,
    )
    .expect("write lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl construct_hit(Variant: string).
.decl propagate_hit().
.decl convert_hit().
.decl handle_hit(Variant: string).
construct_hit(Variant) :-
  def(E),
  def_name(E, "ParseErr"),
  constructs(E, Variant, _, F),
  def_name(F, "parse").
propagate_hit() :-
  def(E),
  def_name(E, "AppErr"),
  propagates(E, _, F),
  def_name(F, "propagate_parse").
convert_hit() :-
  def(Src),
  def_name(Src, "ParseErr"),
  def(Dst),
  def_name(Dst, "AppErr"),
  converts(Src, Dst, _, F),
  def_name(F, "propagate_parse").
handle_hit(Variant) :-
  def(E),
  def_name(E, "ParseErr"),
  handles(E, Variant, _, F),
  def_name(F, "handle_parse").
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        !output.status.success(),
        "approximate error-flow query should be rejected on the daemon path; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("constructs")
            || stderr.contains("propagates")
            || stderr.contains("converts")
            || stderr.contains("handles"),
        "stderr={stderr}"
    );
}

#[test]
fn lang_run_supports_handle_queries_on_the_daemon_path() {
    // The old `call_id`/`ref_id`/`impl_id` zero-bound key enumerations are
    // deleted from the language (SPEC §8.6); `handle(D, H)` projects the
    // §13.2 selector grammar instead.
    let work = temp_dir("daemon_stable_handles");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Person(pub i32);

pub trait Greeter {
    fn greet(&self);
}

impl Greeter for Person {
    fn greet(&self) {}
}

pub fn invoke(left: Person, right: Person) {
    let _ = (left, right);
}
"#,
    )
    .expect("write lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl fn_key(H: string).
.decl impl_key(H: string).
fn_key(H) :- def_name(D, "invoke"), is_fn(D), handle(D, H).
impl_key(H) :- def(D), def_kind(D, DefKind::IMPL), handle(D, H).
"#,
    )
    .expect("write query");

    let output = Command::new(raql_bin())
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
        .expect("run raql");

    assert!(
        output.status.success(),
        "daemon-backed handle query should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("fn_key(\"@H:fn:cli_daemon_cutover::invoke\")"),
        "stdout={stdout}"
    );
    // Four derive impls + the explicit `Greeter` impl share Person's
    // path, so every impl handle is ordinal-qualified (SPEC §13.2).
    assert!(
        stdout.contains("impl_key(\"@H:impl:cli_daemon_cutover::Person#"),
        "stdout={stdout}"
    );
}
