use camino::Utf8PathBuf;
use raql_compiler::{plan, resolve, typecheck};
use raql_engine::{EvalResult, EvalStatus, execute};
use raql_host::HostRuntime;
use raql_host_ra::legacy::LegacyRaHostRuntime;
use raql_host_ra::RaHostInitError;
use raql_syntax::parse_program;
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// TODO(ra-daemon-cutover): split this file into daemon-backed/runtime-service
// coverage vs explicit eager direct-runtime coverage, then delete the direct
// runtime half as the daemon-backed surface absorbs the remaining cases.

fn temp_dir(label: &str) -> Utf8PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "raql_host_ra_{label}_{}_{}",
        std::process::id(),
        stamp
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    Utf8PathBuf::from_path_buf(dir).expect("utf8 temp path")
}

fn write_package(root: &Utf8PathBuf, name: &str, lib_src: &str) {
    let cargo_toml = root.join("Cargo.toml");
    let src = root.join("src");
    fs::create_dir_all(src.as_std_path()).expect("create src");
    fs::write(
        cargo_toml.as_std_path(),
        format!(
            r#"
[package]
name = "{name}"
version = "0.0.0"
edition = "2021"
"#
        ),
    )
    .expect("write Cargo.toml");
    fs::write(src.join("lib.rs").as_std_path(), lib_src).expect("write lib.rs");
}

fn run_query(src: &str, runtime: &mut LegacyRaHostRuntime) -> EvalResult {
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    execute(&planned, runtime)
}

#[test]
fn init_fails_without_workspace_manifest() {
    let root = temp_dir("no_manifest");
    fs::write(root.join("standalone.rs").as_std_path(), "fn main() {}\n").expect("write rs");

    let err = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect_err("must fail");
    let RaHostInitError::WorkspaceNotFound { details, .. } = err else {
        panic!("expected workspace-not-found error");
    };
    assert!(
        !details.trim().is_empty(),
        "workspace-not-found error should include cause details"
    );
}

#[test]
fn init_succeeds_from_manifest_path() {
    let root = temp_dir("manifest_path_init");
    write_package(&root, "manifest_path_init", "pub fn via_manifest() {}\n");
    let manifest = root.join("Cargo.toml");

    let mut runtime = LegacyRaHostRuntime::from_manifest_path(manifest.as_std_path()).expect("runtime");
    assert!(runtime.analysis_status_ok());
    let stamp = HostRuntime::world_stamp(&runtime)
        .expect("world stamp")
        .as_str()
        .to_string();
    assert!(stamp.starts_with("ra-workspace:"));

    let result = run_query(
        r#"
.decl hit().
.decl def(D: Def) extern.
.decl def_name(D: Def, Name: string) extern.

hit() :- def(D), def_name(D, "via_manifest").
"#,
        &mut runtime,
    );
    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("hit")
            .is_some_and(|rows| !rows.is_empty())
    );
}

#[test]
fn workspace_init_uses_load_cargo_full_fidelity() {
    let root = temp_dir("full_fidelity");
    write_package(
        &root,
        "full_fidelity",
        r#"
include!(concat!(env!("OUT_DIR"), "/generated.rs"));

pub fn caller() {
    generated_fn();
}
"#,
    );

    fs::write(
        root.join("build.rs").as_std_path(),
        r#"
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out.join("generated.rs"), "pub fn generated_fn() {}\n").expect("write generated");
}
"#,
    )
    .expect("write build.rs");

    let mut runtime = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.decl generated().
.decl def(D: Def) extern.
.decl def_name(D: Def, Name: string) extern.

generated() :- def(D), def_name(D, "generated_fn").
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("generated")
            .is_some_and(|rows| !rows.is_empty()),
        "generated build.rs symbol should be visible"
    );
}

#[test]
fn world_stamp_changes_when_local_file_content_changes() {
    let root = temp_dir("world_stamp");
    write_package(&root, "world_stamp", "pub fn answer() -> i32 { 1 }\n");

    let runtime_a = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime a");
    let stamp_a = HostRuntime::world_stamp(&runtime_a)
        .expect("world stamp a")
        .as_str()
        .to_string();

    fs::write(
        root.join("src/lib.rs").as_std_path(),
        "pub fn answer() -> i32 { 2 }\n",
    )
    .expect("rewrite lib.rs");

    let runtime_b = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime b");
    let stamp_b = HostRuntime::world_stamp(&runtime_b)
        .expect("world stamp b")
        .as_str()
        .to_string();

    assert_ne!(stamp_a, stamp_b);
}

#[test]
fn world_stamp_changes_when_manifest_configuration_changes() {
    let root = temp_dir("world_stamp_manifest_cfg");
    write_package(
        &root,
        "world_stamp_manifest_cfg",
        "pub fn answer() -> i32 { 1 }\n",
    );

    let runtime_a = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime a");
    let stamp_a = HostRuntime::world_stamp(&runtime_a)
        .expect("world stamp a")
        .as_str()
        .to_string();

    let manifest = root.join("Cargo.toml");
    let mut manifest_text =
        fs::read_to_string(manifest.as_std_path()).expect("read Cargo.toml before update");
    manifest_text.push_str(
        r#"
[features]
default = []
extra = []
"#,
    );
    fs::write(manifest.as_std_path(), manifest_text).expect("rewrite Cargo.toml with features");

    let runtime_b = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime b");
    let stamp_b = HostRuntime::world_stamp(&runtime_b)
        .expect("world stamp b")
        .as_str()
        .to_string();

    assert_ne!(stamp_a, stamp_b);
}

#[test]
fn world_stamp_is_stable_when_workspace_is_unchanged() {
    let root = temp_dir("world_stamp_stable");
    write_package(
        &root,
        "world_stamp_stable",
        "pub fn answer() -> i32 { 1 }\n",
    );

    let runtime_a = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime a");
    let stamp_a = HostRuntime::world_stamp(&runtime_a)
        .expect("world stamp a")
        .as_str()
        .to_string();

    let runtime_b = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime b");
    let stamp_b = HostRuntime::world_stamp(&runtime_b)
        .expect("world stamp b")
        .as_str()
        .to_string();

    assert_eq!(stamp_a, stamp_b);
}

#[test]
fn runtime_reload_now_refreshes_snapshot_after_file_change() {
    let root = temp_dir("runtime_reload_now");
    write_package(&root, "runtime_reload_now", "pub fn before_reload() {}\n");

    let mut runtime = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let stamp_before = HostRuntime::world_stamp(&runtime)
        .expect("world stamp before")
        .as_str()
        .to_string();

    let query = r#"
.decl hit().
.decl def(D: Def) extern.
.decl def_name(D: Def, Name: string) extern.

hit() :- def(D), def_name(D, "after_reload").
"#;
    let before = run_query(query, &mut runtime);
    assert_eq!(before.status, EvalStatus::Ok);
    assert!(
        before
            .relations
            .get("hit")
            .is_none_or(|rows| rows.is_empty()),
        "before reload, after_reload should not be visible"
    );

    std::thread::sleep(Duration::from_millis(20));
    fs::write(
        root.join("src/lib.rs").as_std_path(),
        "pub fn before_reload() {}\npub fn after_reload() {}\n",
    )
    .expect("rewrite lib.rs");
    runtime.reload_now().expect("reload runtime snapshot");

    let stamp_after = HostRuntime::world_stamp(&runtime)
        .expect("world stamp after")
        .as_str()
        .to_string();
    assert_ne!(stamp_before, stamp_after);

    let after = run_query(query, &mut runtime);
    assert_eq!(after.status, EvalStatus::Ok);
    assert!(
        after
            .relations
            .get("hit")
            .is_some_and(|rows| !rows.is_empty()),
        "after reload, new function should be visible"
    );
}

#[test]
fn reload_now_propagates_workspace_load_errors() {
    let root = temp_dir("runtime_reload_error");
    write_package(&root, "runtime_reload_error", "pub fn marker() {}\n");

    let mut runtime = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    fs::remove_file(root.join("Cargo.toml").as_std_path()).expect("remove manifest");

    let err = runtime.reload_now().expect_err("reload should fail");
    let RaHostInitError::WorkspaceNotFound { details, .. } = err else {
        panic!("expected workspace-not-found error");
    };
    assert!(
        !details.trim().is_empty(),
        "workspace-not-found reload error should include cause details"
    );
}

#[test]
fn workspace_init_loads_all_workspace_members() {
    let root = temp_dir("workspace_members");

    fs::write(
        root.join("Cargo.toml").as_std_path(),
        r#"
[workspace]
members = ["corelib", "app"]
"#,
    )
    .expect("write workspace Cargo.toml");

    let core_dir = root.join("corelib");
    fs::create_dir_all(core_dir.join("src").as_std_path()).expect("create corelib src");
    fs::write(
        core_dir.join("Cargo.toml").as_std_path(),
        r#"
[package]
name = "corelib"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write corelib Cargo.toml");
    fs::write(
        core_dir.join("src/lib.rs").as_std_path(),
        "pub fn helper() {}\n",
    )
    .expect("write corelib lib.rs");

    let app_dir = root.join("app");
    fs::create_dir_all(app_dir.join("src").as_std_path()).expect("create app src");
    fs::write(
        app_dir.join("Cargo.toml").as_std_path(),
        r#"
[package]
name = "app"
version = "0.0.0"
edition = "2021"

[dependencies]
corelib = { path = "../corelib" }
"#,
    )
    .expect("write app Cargo.toml");
    fs::write(
        app_dir.join("src/lib.rs").as_std_path(),
        r#"
pub fn caller() {
    corelib::helper();
}
"#,
    )
    .expect("write app lib.rs");

    let mut runtime = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl member_defs().
.decl member_edge().
.decl def(D: Def) extern.
.decl def_name(D: Def, Name: string) extern.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.

member_defs() :-
  def(D1),
  def_name(D1, "caller"),
  def(D2),
  def_name(D2, "helper").

member_edge() :-
  call_edge(Caller, Callee, _, _),
  def_name(Caller, "caller"),
  def_name(Callee, "helper").
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("member_defs")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("member_edge")
            .is_some_and(|rows| !rows.is_empty())
    );
}

#[test]
fn dependency_library_source_roots_are_included_in_snapshot() {
    let dep_root = temp_dir("dep_library_source");
    write_package(
        &dep_root,
        "dep_library_source",
        r#"
pub fn helper() {
    deep();
}

fn deep() {}
"#,
    );

    let root = temp_dir("dep_library_app");
    fs::create_dir_all(root.join("src").as_std_path()).expect("create app src");
    fs::write(
        root.join("Cargo.toml").as_std_path(),
        format!(
            r#"
[package]
name = "dep_library_app"
version = "0.0.0"
edition = "2021"

[dependencies]
dep_library_source = {{ path = "{}" }}
"#,
            dep_root.as_std_path().display()
        ),
    )
    .expect("write app Cargo.toml");
    fs::write(
        root.join("src/lib.rs").as_std_path(),
        r#"
pub fn caller() {
    dep_library_source::helper();
}
"#,
    )
    .expect("write app lib.rs");

    let mut runtime = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.decl def_name(D: Def, Name: string) extern.
.decl app_to_dep().
.decl dep_internal().

app_to_dep() :-
  call_edge(Caller, Callee, _, _),
  def_name(Caller, "caller"),
  def_name(Callee, "helper").

dep_internal() :-
  call_edge(Caller, Callee, _, _),
  def_name(Caller, "helper"),
  def_name(Callee, "deep").
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("app_to_dep")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("dep_internal")
            .is_some_and(|rows| !rows.is_empty())
    );
}

#[test]
fn registry_like_dependency_roots_are_excluded_from_snapshot() {
    let dep_root = temp_dir("dep_registry_like")
        .join("registry")
        .join("src")
        .join("fake-index")
        .join("registry_like_dep");
    fs::create_dir_all(dep_root.join("src").as_std_path()).expect("create dep src");
    fs::write(
        dep_root.join("Cargo.toml").as_std_path(),
        r#"
[package]
name = "registry_like_dep"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write dep Cargo.toml");
    fs::write(
        dep_root.join("src/lib.rs").as_std_path(),
        "pub fn helper() {}\n",
    )
    .expect("write dep lib.rs");

    let root = temp_dir("registry_like_dep_app");
    fs::create_dir_all(root.join("src").as_std_path()).expect("create app src");
    fs::write(
        root.join("Cargo.toml").as_std_path(),
        format!(
            r#"
[package]
name = "registry_like_dep_app"
version = "0.0.0"
edition = "2021"

[dependencies]
registry_like_dep = {{ path = "{}" }}
"#,
            dep_root.as_std_path().display()
        ),
    )
    .expect("write app Cargo.toml");
    fs::write(
        root.join("src/lib.rs").as_std_path(),
        r#"
pub fn caller() {
    registry_like_dep::helper();
}
"#,
    )
    .expect("write app lib.rs");

    let mut runtime = LegacyRaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.decl def(D: Def) extern.
.decl def_name(D: Def, Name: string) extern.
.decl local_visible().
.decl dep_visible().

local_visible() :-
  def(D),
  def_name(D, "caller").

dep_visible() :-
  def(D),
  def_name(D, "helper").
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("local_visible")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("dep_visible")
            .is_none_or(|rows| rows.is_empty()),
        "registry-like dependency roots must not be snapshot-expanded"
    );
}
