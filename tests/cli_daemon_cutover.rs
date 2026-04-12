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
.func def_name(D: Def, Name: string) extern.
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
.func def_name(D: Def, Name: string) extern.
.func def_span(D: Def, S: Span) extern.
.decl span_allowed(S: Span) extern.
.func span_key(S: Span, RelPath: string, L0: int, C0: int, L1: int, C1: int) extern.
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
fn lang_run_supports_structure_and_trait_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_structure_trait");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
pub trait Greeter {
    fn greet(&self);
}

pub struct Person {
    pub name: String,
    age: u32,
}

pub enum Choice {
    First,
    Second,
}

pub struct NameError;
pub struct AgeError;

impl Greeter for Person {
    fn greet(&self) {}
}

impl From<NameError> for AgeError {
    fn from(_: NameError) -> Self {
        AgeError
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
.decl field_hit(Name: string).
.decl field_type_hit(Name: string, TyName: string).
.decl variant_hit(Name: string).
.decl method_hit(Name: string).
.decl trait_method_hit(Name: string).
.decl impl_hit(Name: string).
.decl from_hit(SrcName: string, DstName: string).
field_hit(Name) :- def(Person), def_name(Person, "Person"), field(Person, Name, _).
field_type_hit(Name, TyName) :-
  def(Person),
  def_name(Person, "Person"),
  field(Person, Name, Ty),
  ty_app(Ty, Head),
  def_name(Head, TyName).
field_type_hit(Name, TyName) :-
  def(Person),
  def_name(Person, "Person"),
  field(Person, Name, Ty),
  ty_prim(Ty, TyName).
variant_hit(Name) :- def(Choice), def_name(Choice, "Choice"), variant(Choice, Name, _).
method_hit(Name) :- def(Person), def_name(Person, "Person"), method(Person, Method), def_name(Method, Name).
trait_method_hit(Name) :- def(Greeter), def_name(Greeter, "Greeter"), trait_method(Greeter, Method), def_name(Method, Name).
impl_hit(Name) :- def(Person), def_name(Person, "Person"), implements(Person, Trait, _), def_name(Trait, Name).
from_hit(SrcName, DstName) :- def(Src), def_name(Src, "NameError"), from_impl(Src, Dst, _), def_name(Src, SrcName), def_name(Dst, DstName).
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
        "daemon-backed structure query should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("field_hit(\"name\")"), "stdout={stdout}");
    assert!(stdout.contains("field_type_hit(\"name\", \"String\")"), "stdout={stdout}");
    assert!(stdout.contains("field_type_hit(\"age\", \"u32\")"), "stdout={stdout}");
    assert!(stdout.contains("variant_hit(\"First\")"), "stdout={stdout}");
    assert!(stdout.contains("method_hit(\"greet\")"), "stdout={stdout}");
    assert!(stdout.contains("trait_method_hit(\"greet\")"), "stdout={stdout}");
    assert!(stdout.contains("impl_hit(\"Greeter\")"), "stdout={stdout}");
    assert!(
        stdout.contains("from_hit(\"NameError\", \"AgeError\")"),
        "stdout={stdout}"
    );
}

#[test]
fn lang_run_supports_type_surface_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_type_surface");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
pub struct Wrapper<T>(pub T);
pub struct Item;
pub struct Oops;

pub fn make_wrapper(item: Item) -> Wrapper<Item> { Wrapper(item) }
pub fn borrow_item(item: &mut Item) -> &mut Item { item }
pub fn raw_item(item: *const Item) -> *const Item { item }
pub fn tuple_item() -> (Item, i32) { (Item, 1) }
pub fn slice_item(items: &[Item]) -> &[Item] { items }
pub fn generic_item<T>(value: T) -> T { value }
pub fn parse_item() -> Result<Item, Oops> { Err(Oops) }
pub fn opaque_array() -> [i32; 4] { [0; 4] }
"#,
    )
    .expect("write lib.rs");

    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.include "std.raql".
.decl app_hit(Path: string).
.decl arg_hit(Path: string).
.decl ref_hit(Name: string).
.decl ptr_hit(Name: string).
.decl tuple_prim_hit(Name: string).
.decl slice_hit(Name: string).
.decl param_hit(Name: string).
.decl error_hit(Name: string).
.decl unknown_hit(Key: string).
app_hit(Path) :- def(F), def_name(F, "make_wrapper"), fn_return_type(F, TR), type_head_path(TR, Path).
arg_hit(Path) :- def(F), def_name(F, "make_wrapper"), fn_return_type(F, TR), ty_arg(TR, 0, Arg), type_head_path(Arg, Path).
ref_hit(Name) :- def(F), def_name(F, "borrow_item"), fn_return_type(F, TR), ty_ref(TR, Mutability::MUT, Inner), type_head_def(Inner, Head), def_name(Head, Name).
ptr_hit(Name) :- def(F), def_name(F, "raw_item"), fn_return_type(F, TR), ty_ptr(TR, Mutability::IMM, Inner), type_head_def(Inner, Head), def_name(Head, Name).
tuple_prim_hit(Name) :- def(F), def_name(F, "tuple_item"), fn_return_type(F, TR), ty_tuple(TR, 1, Elem), ty_prim(Elem, Name).
slice_hit(Name) :- def(F), def_name(F, "slice_item"), fn_return_type(F, TR), ty_ref(TR, Mutability::IMM, RefInner), ty_slice(RefInner, Elem), type_head_def(Elem, Head), def_name(Head, Name).
param_hit(Name) :- def(F), def_name(F, "generic_item"), fn_return_type(F, TR), ty_param(TR, Param), def_name(Param, Name).
error_hit(Name) :- def(F), def_name(F, "parse_item"), fn_error_type(F, some(Err)), def_name(Err, Name).
unknown_hit(Key) :- def(F), def_name(F, "opaque_array"), fn_return_type(F, TR), ty_unknown(TR), typeref_id(TR, Key).
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
        "daemon-backed type query should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("app_hit(\"Wrapper\")"), "stdout={stdout}");
    assert!(stdout.contains("arg_hit(\"Item\")"), "stdout={stdout}");
    assert!(stdout.contains("ref_hit(\"Item\")"), "stdout={stdout}");
    assert!(stdout.contains("ptr_hit(\"Item\")"), "stdout={stdout}");
    assert!(stdout.contains("tuple_prim_hit(\"i32\")"), "stdout={stdout}");
    assert!(stdout.contains("slice_hit(\"Item\")"), "stdout={stdout}");
    assert!(stdout.contains("param_hit(\"T\")"), "stdout={stdout}");
    assert!(stdout.contains("error_hit(\"Oops\")"), "stdout={stdout}");
    assert!(stdout.contains("unknown_hit("), "stdout={stdout}");
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
    assert!(stdout.contains("closure_hit(\"closure\")"), "stdout={stdout}");
    assert!(
        stdout.contains("fn_pointer_hit(\"fn_pointer\")"),
        "stdout={stdout}"
    );
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
fn lang_run_supports_syntax_control_queries_on_the_daemon_path() {
    let work = temp_dir("daemon_syntax_control");
    let workspace = work.join("ws");
    write_workspace(&workspace);
    fs::write(
        workspace.join("src/lib.rs"),
        r#"
pub fn direct_target() {}

pub fn syntax_demo(flag: bool) {
    if flag {
        direct_target();
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
.decl node_hit().
.decl if_control_hit().
node_hit() :-
  def(F),
  def_name(F, "syntax_demo"),
  call_edge(F, _, Site, _),
  node_at(Site, some(Node)),
  node_kind(Node, NodeKind::OTHER),
  node_span(Node, _),
  node_parent(Node, some(_)),
  node_id(Node, _).
if_control_hit() :-
  def(F),
  def_name(F, "syntax_demo"),
  call_edge(F, _, Site, _),
  enclosing_control(Site, NodeKind::IF, _, _).
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
        "daemon-backed syntax/control query should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("node_hit()."), "stdout={stdout}");
    assert!(stdout.contains("if_control_hit()."), "stdout={stdout}");
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
  handles(E, some(Variant), _, F),
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
fn lang_run_supports_stable_handle_queries_on_the_daemon_path() {
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
    let mut slot = left;
    slot.greet();
    if slot == right {
        slot = right;
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
.decl call_key(H: string).
.decl impl_key(H: string).
call_key(H) :- call_id(_, H).
impl_key(H) :- impl_id(_, H).
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
        "daemon-backed stable-handle query should succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("call_key(\"call:src/lib.rs:"), "stdout={stdout}");
    assert!(stdout.contains("impl_key(\"impl:src/lib.rs:"), "stdout={stdout}");
}
