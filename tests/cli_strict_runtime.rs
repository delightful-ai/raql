use std::fs;
use std::process::Command;
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
name = "cli_strict_runtime"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write Cargo.toml");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");
}

#[test]
fn lang_run_surfaces_strict_workspace_init_failures() {
    let work = temp_dir("strict_init_failure");
    let program = work.join("query.raql");
    fs::write(
        &program,
        r#"
.decl ping().
ping().
"#,
    )
    .expect("write query");

    let not_a_workspace = work.join("not_a_workspace");
    fs::create_dir_all(&not_a_workspace).expect("create non-workspace dir");

    let output = Command::new(env!("CARGO_BIN_EXE_raql"))
        .args([
            "lang",
            "run",
            program.to_str().expect("utf8 query path"),
            "--rust-file",
            not_a_workspace.to_str().expect("utf8 non-workspace path"),
        ])
        .output()
        .expect("run raql binary");

    assert!(
        !output.status.success(),
        "strict init failure should exit non-zero; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to initialize rust-analyzer runtime"),
        "stderr should surface strict runtime init failure; stderr={stderr}"
    );
}

#[test]
fn lang_run_surfaces_daemon_side_query_failures() {
    let work = temp_dir("daemon_query_failure");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let program = work.join("bad_query.raql");
    fs::write(&program, ".decl broken(\n").expect("write broken query");

    let output = Command::new(env!("CARGO_BIN_EXE_raql"))
        .args([
            "lang",
            "run",
            program.to_str().expect("utf8 query path"),
            "--rust-file",
            workspace.to_str().expect("utf8 workspace path"),
        ])
        .output()
        .expect("run raql binary");

    assert!(
        !output.status.success(),
        "daemon-side query failures should exit non-zero; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("parse failed"),
        "stderr should surface daemon-side planning failure; stderr={stderr}"
    );
}

#[test]
fn lang_run_surfaces_unsupported_capability_failures() {
    let work = temp_dir("unsupported_capability");
    let workspace = work.join("ws");
    write_workspace(&workspace);

    let program = work.join("unsupported.raql");
    fs::write(
        &program,
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.decl hit().
hit() :- call_edge(_, _, _, _).
"#,
    )
    .expect("write unsupported query");

    let output = Command::new(env!("CARGO_BIN_EXE_raql"))
        .args([
            "lang",
            "run",
            program.to_str().expect("utf8 query path"),
            "--rust-file",
            workspace.to_str().expect("utf8 workspace path"),
        ])
        .output()
        .expect("run raql binary");

    assert!(
        !output.status.success(),
        "unsupported capabilities should exit non-zero; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("call_edge"),
        "stderr should name the unsupported capability; stderr={stderr}"
    );
    assert!(
        stderr.contains("unsupported") || stderr.contains("missing"),
        "stderr should describe explicit capability rejection; stderr={stderr}"
    );
}

#[test]
fn dev_run_direct_is_not_a_supported_cli_command() {
    let work = temp_dir("dev_run_direct_removed");
    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.decl ping().
ping().
"#,
    )
    .expect("write query");

    let output = Command::new(env!("CARGO_BIN_EXE_raql"))
        .args(["dev", "run-direct", query.to_str().expect("utf8 query path")])
        .output()
        .expect("run raql binary");

    assert!(
        !output.status.success(),
        "dev run-direct should be rejected; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unrecognized subcommand"),
        "stderr should show clap rejecting removed command; stderr={stderr}"
    );
    assert!(
        stderr.contains("dev"),
        "stderr should mention the removed top-level command; stderr={stderr}"
    );
}

#[test]
fn lang_run_requires_rust_file_without_advertising_direct_runtime_escape_hatch() {
    let work = temp_dir("lang_run_requires_rust_file");
    let query = work.join("query.raql");
    fs::write(
        &query,
        r#"
.decl ping().
ping().
"#,
    )
    .expect("write query");

    let output = Command::new(env!("CARGO_BIN_EXE_raql"))
        .args(["lang", "run", query.to_str().expect("utf8 query path")])
        .output()
        .expect("run raql binary");

    assert!(
        !output.status.success(),
        "lang run without --rust-file should fail; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("supported `raql lang run` requires `--rust-file`"),
        "stderr should explain the daemon-backed requirement; stderr={stderr}"
    );
    assert!(
        !stderr.contains("run-direct"),
        "stderr should not advertise deleted direct-runtime commands; stderr={stderr}"
    );
}
