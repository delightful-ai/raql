use camino::Utf8PathBuf;
use raql_host_ra::legacy::LegacyRaHostRuntime;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_workspace_root(label: &str) -> Utf8PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "raql_host_ra_legacy_surface_{label}_{}_{}",
        std::process::id(),
        stamp
    ));
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "legacy_surface_{stamp}"
version = "0.0.0"
edition = "2021"
"#
        ),
    )
    .expect("write Cargo.toml");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");
    Utf8PathBuf::from_path_buf(root).expect("utf8 path")
}

#[test]
fn eager_runtime_is_only_accessed_via_legacy_namespace() {
    let root = temp_workspace_root("namespace");
    let runtime = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    assert!(runtime.analysis_status_ok(), "legacy runtime should still work for quarantined dev/test coverage");
}
