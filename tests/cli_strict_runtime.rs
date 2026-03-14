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
