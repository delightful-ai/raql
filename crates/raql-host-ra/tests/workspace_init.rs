use camino::Utf8PathBuf;
use raql_compiler::{plan, resolve, typecheck};
use raql_engine::{EngineHostView, EvalResult, EvalStatus, RuntimeValue, execute};
use raql_host::HostRuntime;
use raql_host_ra::{RaHostInitError, RaHostRuntime};
use raql_syntax::parse_program;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

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

fn run_query(src: &str, runtime: &mut RaHostRuntime) -> EvalResult {
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

    let err = RaHostRuntime::from_workspace_root(root.as_std_path()).expect_err("must fail");
    assert!(matches!(err, RaHostInitError::WorkspaceNotFound { .. }));
}

#[test]
fn init_succeeds_from_manifest_path() {
    let root = temp_dir("manifest_path_init");
    write_package(&root, "manifest_path_init", "pub fn via_manifest() {}\n");
    let manifest = root.join("Cargo.toml");

    let mut runtime = RaHostRuntime::from_manifest_path(manifest.as_std_path()).expect("runtime");
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

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
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

    let runtime_a = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime a");
    let stamp_a = HostRuntime::world_stamp(&runtime_a)
        .expect("world stamp a")
        .as_str()
        .to_string();

    fs::write(
        root.join("src/lib.rs").as_std_path(),
        "pub fn answer() -> i32 { 2 }\n",
    )
    .expect("rewrite lib.rs");

    let runtime_b = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime b");
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

    let runtime_a = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime a");
    let stamp_a = HostRuntime::world_stamp(&runtime_a)
        .expect("world stamp a")
        .as_str()
        .to_string();

    let runtime_b = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime b");
    let stamp_b = HostRuntime::world_stamp(&runtime_b)
        .expect("world stamp b")
        .as_str()
        .to_string();

    assert_eq!(stamp_a, stamp_b);
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

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE }.
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
fn all_predicate_names_still_resolve() {
    let root = temp_dir("all_predicates");
    write_package(&root, "all_predicates", "pub fn f() {}\n");

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");

    let names = [
        "world_stamp",
        "def",
        "search",
        "def_name",
        "def_kind",
        "def_span",
        "def_path",
        "method_of",
        "fn_error_type",
        "fn_return_type",
        "call_edge",
        "field",
        "variant",
        "method",
        "trait_method",
        "implements",
        "from_impl",
        "span_allowed",
        "is_public",
        "in_test",
        "dispatch_str",
        "ty_app",
        "ty_arg",
        "ty_ref",
        "ty_ptr",
        "ty_tuple",
        "ty_slice",
        "ty_param",
        "ty_prim",
        "ty_unknown",
        "node_at",
        "node_kind",
        "node_span",
        "node_parent",
        "enclosing_control",
        "handle",
        "span_key",
        "typeref_id",
        "node_id",
        "call_id",
        "ref_id",
        "impl_id",
        "constructs",
        "propagates",
        "converts",
        "handles",
        "compares",
        "writes",
    ];

    for predicate in names {
        let rows = EngineHostView::extern_relation_rows(&mut runtime, predicate)
            .expect("predicate should resolve");
        assert!(
            rows.is_some(),
            "predicate `{predicate}` should be recognized"
        );
    }
}

#[test]
fn call_edge_cross_file_semantic_resolution() {
    let root = temp_dir("cross_file_semantics");
    write_package(
        &root,
        "cross_file_semantics",
        r#"
mod foo;

fn caller() {
    foo::callee();
}
"#,
    );
    fs::write(
        root.join("src/foo.rs").as_std_path(),
        "pub fn callee() {}\n",
    )
    .expect("write foo.rs");

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");

    let result = run_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE }.
.decl hit().
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.decl def_name(D: Def, Name: string) extern.

hit() :-
  call_edge(Caller, Callee, _, _),
  def_name(Caller, "caller"),
  def_name(Callee, "callee").
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
fn public_and_in_test_markers_match_semantic_visibility_and_test_context() {
    let root = temp_dir("visibility_test_markers");
    write_package(
        &root,
        "visibility_test_markers",
        r#"
pub fn exported() {}
fn hidden() {}

#[cfg(test)]
mod tests {
    pub fn helper() {}

    #[test]
    fn test_case() {
        helper();
    }
}
"#,
    );

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.decl exported_public().
.decl hidden_public().
.decl helper_test().
.decl exported_test().
.decl def(D: Def) extern.
.decl def_name(D: Def, Name: string) extern.
.decl is_public(D: Def) extern.
.decl in_test(D: Def) extern.

exported_public() :- def(D), def_name(D, "exported"), is_public(D).
hidden_public() :- def(D), def_name(D, "hidden"), is_public(D).
helper_test() :- def(D), def_name(D, "helper"), in_test(D).
exported_test() :- def(D), def_name(D, "exported"), in_test(D).
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("exported_public")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("hidden_public")
            .is_some_and(|rows| rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("helper_test")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("exported_test")
            .is_some_and(|rows| rows.is_empty())
    );
}

#[test]
fn span_key_is_workspace_relative_for_local_files() {
    let root = temp_dir("span_key_relative");
    write_package(&root, "span_key_relative", "pub fn answer() -> i32 { 1 }\n");

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let rows = EngineHostView::extern_relation_rows(&mut runtime, "span_key")
        .expect("span_key lookup")
        .expect("span_key rows");

    let mut has_relative = false;
    for row in rows {
        if let Some(RuntimeValue::String(path)) = row.get(1)
            && path == "src/lib.rs"
        {
            has_relative = true;
            break;
        }
    }
    assert!(has_relative, "expected workspace-relative span path");
}
