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
            not_a_workspace
                .to_str()
                .expect("utf8 non-workspace path"),
        ])
        .output()
        .expect("run raql binary");

    assert!(
        !output.status.success(),
        "strict init failure should exit non-zero; stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to initialize rust-analyzer runtime"),
        "stderr should surface strict runtime init failure; stderr={stderr}"
    );
}
