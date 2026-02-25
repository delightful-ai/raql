use camino::Utf8PathBuf;
use raql_compiler::{plan, resolve, typecheck};
use raql_engine::{EngineHostView, EvalResult, EvalStatus, RuntimeValue, execute};
use raql_host::HostRuntime;
use raql_host_ra::{
    RaHostInitError, RaHostRuntime, RuntimeScalarOptions, ScalarInputKey, ScalarValue, StableId,
};
use raql_syntax::parse_program;
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
fn world_stamp_changes_when_manifest_configuration_changes() {
    let root = temp_dir("world_stamp_manifest_cfg");
    write_package(
        &root,
        "world_stamp_manifest_cfg",
        "pub fn answer() -> i32 { 1 }\n",
    );

    let runtime_a = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime a");
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
fn runtime_reload_now_refreshes_snapshot_after_file_change() {
    let root = temp_dir("runtime_reload_now");
    write_package(&root, "runtime_reload_now", "pub fn before_reload() {}\n");

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
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

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
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
fn runtime_reload_preserves_runtime_scalar_configuration() {
    let root = temp_dir("runtime_reload_preserves_cfg");
    write_package(
        &root,
        "runtime_reload_preserves_cfg",
        "pub fn preserved() {}\n",
    );

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let options = RuntimeScalarOptions::default()
        .with_path_limit(17)
        .with_path_max_depth(9)
        .with_max_iters(321);
    runtime.set_runtime_scalar_options(options);
    let scalar_key = ScalarInputKey::new(StableId::new(7), "engine.path.limit");
    runtime.insert_scalar_input(scalar_key.clone(), ScalarValue::I64(42));
    let override_id = StableId::new(0xfeed_face);
    runtime.insert_stable_id_override("cfg", "engine.path.limit", override_id);

    std::thread::sleep(Duration::from_millis(20));
    fs::write(
        root.join("src/lib.rs").as_std_path(),
        "pub fn preserved() {}\npub fn changed() {}\n",
    )
    .expect("rewrite lib.rs");
    runtime.reload_now().expect("reload runtime");

    let observed_options = HostRuntime::runtime_scalar_options(&runtime).expect("options");
    assert_eq!(observed_options.path_limit(), Some(17));
    assert_eq!(observed_options.path_max_depth(), Some(9));
    assert_eq!(observed_options.max_iters(), Some(321));
    assert_eq!(
        HostRuntime::scalar_input(&runtime, &scalar_key).expect("scalar input"),
        Some(ScalarValue::I64(42))
    );
    assert_eq!(
        HostRuntime::stable_id(&runtime, "cfg", "engine.path.limit").expect("stable id override"),
        override_id
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

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
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

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
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
fn error_flow_predicates_are_semantically_populated() {
    let root = temp_dir("error_flow_semantics");
    write_package(
        &root,
        "error_flow_semantics",
        r#"
#[derive(Debug, PartialEq)]
pub enum InnerErr {
    Bad,
}

#[derive(Debug, PartialEq)]
pub enum OuterErr {
    Wrapped(InnerErr),
}

impl From<InnerErr> for OuterErr {
    fn from(value: InnerErr) -> Self {
        OuterErr::Wrapped(value)
    }
}

fn produce_inner(flag: bool) -> Result<(), InnerErr> {
    if flag {
        Ok(())
    } else {
        Err(InnerErr::Bad)
    }
}

pub fn demo(flag: bool) -> Result<(), OuterErr> {
    let mut tracked = OuterErr::Wrapped(InnerErr::Bad);
    tracked = OuterErr::Wrapped(InnerErr::Bad);
    let _is_same = tracked == OuterErr::Wrapped(InnerErr::Bad);

    match &tracked {
        OuterErr::Wrapped(_) => {}
    }

    produce_inner(flag)?;
    Ok(())
}

pub async fn async_demo(flag: bool) -> Result<(), OuterErr> {
    produce_inner(flag)?;
    Ok(())
}
"#,
    );

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.decl constructs(ErrType: Def, Variant: string, Site: Span, Fn: Def) extern.
.decl propagates(ErrType: Def, Site: Span, Fn: Def) extern.
.decl converts(Src: Def, Dst: Def, Site: Span, Fn: Def) extern.
.decl handles(ErrType: Def, Variant: option<string>, Site: Span, Fn: Def) extern.
.decl compares(Subject: Def, Site: Span, Op: string, Fn: Def) extern.
.decl writes(Subject: Def, Site: Span, Fn: Def) extern.
.decl def_name(D: Def, Name: string) extern.

.decl has_construct().
.decl has_unit_construct().
.decl has_propagate().
.decl has_async_propagate().
.decl has_convert().
.decl has_handle().
.decl has_compare().
.decl has_write().

has_construct() :-
  constructs(_, "Wrapped", _, Fn),
  def_name(Fn, "demo").

has_unit_construct() :-
  constructs(_, "Bad", _, Fn),
  def_name(Fn, "produce_inner").

has_propagate() :-
  propagates(_, _, Fn),
  def_name(Fn, "demo").

has_async_propagate() :-
  propagates(_, _, Fn),
  def_name(Fn, "async_demo").

has_convert() :-
  converts(Src, Dst, _, Fn),
  def_name(Fn, "demo"),
  Src != Dst.

has_handle() :-
  handles(_, _, _, Fn),
  def_name(Fn, "demo").

has_compare() :-
  compares(_, _, "==", Fn),
  def_name(Fn, "demo").

has_write() :-
  writes(_, _, Fn),
  def_name(Fn, "demo").
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("has_construct")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("has_unit_construct")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("has_propagate")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("has_async_propagate")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("has_convert")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("has_handle")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("has_compare")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("has_write")
            .is_some_and(|rows| !rows.is_empty())
    );
}

#[test]
fn fn_error_type_and_ty_arg_handle_result_aliases_and_generics() {
    let root = temp_dir("result_alias_generics");
    write_package(
        &root,
        "result_alias_generics",
        r#"
#[derive(Debug)]
pub struct Payload<T>(pub T);

#[derive(Debug)]
pub enum OuterErr {
    Boom,
}

pub type AppResult<T> = Result<T, OuterErr>;
pub type NestedErr = OuterErr;

pub fn via_alias() -> AppResult<()> {
    Err(OuterErr::Boom)
}

pub fn via_nested_alias() -> Result<Payload<u8>, NestedErr> {
    Err(OuterErr::Boom)
}

pub fn direct_result() -> Result<(), OuterErr> {
    Err(OuterErr::Boom)
}
"#,
    );

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.decl def(D: Def) extern.
.decl def_name(D: Def, Name: string) extern.
.decl fn_error_type(Fn: Def, Err: option<Def>) extern.
.decl fn_return_type(Fn: Def, Ret: TypeRef) extern.
.decl ty_arg(T: TypeRef, Ix: int, Arg: TypeRef) extern.

.decl direct_error_ok().
.decl alias_error_ok().
.decl nested_error_ok().
.decl return_arg_ok().

direct_error_ok() :-
  def(F),
  def_name(F, "direct_result"),
  fn_error_type(F, some(E)),
  def_name(E, "OuterErr").

alias_error_ok() :-
  def(F),
  def_name(F, "via_alias"),
  fn_error_type(F, some(E)),
  def_name(E, "OuterErr").

nested_error_ok() :-
  def(F),
  def_name(F, "via_nested_alias"),
  fn_error_type(F, some(E)),
  def_name(E, "OuterErr").

return_arg_ok() :-
  def(F),
  def_name(F, "via_nested_alias"),
  fn_return_type(F, Ret),
  ty_arg(Ret, 1, _).
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("direct_error_ok")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("alias_error_ok")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("nested_error_ok")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("return_arg_ok")
            .is_some_and(|rows| !rows.is_empty())
    );
}

#[test]
fn field_types_are_semantically_populated() {
    let root = temp_dir("field_types_semantic");
    write_package(
        &root,
        "field_types_semantic",
        r#"
pub struct Record {
    pub count: u32,
    pub enabled: bool,
}
"#,
    );

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");

    let result = run_query(
        r#"
.decl def(D: Def) extern.
.decl def_name(D: Def, Name: string) extern.
.decl field(Owner: Def, Name: string, Ty: TypeRef) extern.
.decl ty_prim(T: TypeRef, Name: string) extern.

.decl count_field_typed().
.decl enabled_field_typed().

count_field_typed() :-
  def(Owner),
  def_name(Owner, "Record"),
  field(Owner, "count", Ty),
  ty_prim(Ty, "u32").

enabled_field_typed() :-
  def(Owner),
  def_name(Owner, "Record"),
  field(Owner, "enabled", Ty),
  ty_prim(Ty, "bool").
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("count_field_typed")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("enabled_field_typed")
            .is_some_and(|rows| !rows.is_empty())
    );
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
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
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
fn call_edge_direct_rows_are_not_duplicated() {
    let root = temp_dir("call_edge_direct_dedup");
    write_package(
        &root,
        "call_edge_direct_dedup",
        r#"
fn callee() {}

fn caller() {
    callee();
}
"#,
    );

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.decl def_name(D: Def, Name: string) extern.
.decl direct_edge(Site: Span, Dispatch: DispatchKind).

direct_edge(Site, Dispatch) :-
  call_edge(Caller, Callee, Site, Dispatch),
  def_name(Caller, "caller"),
  def_name(Callee, "callee").
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    let rows = result
        .relations
        .get("direct_edge")
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        1,
        "direct caller->callee edge should appear exactly once"
    );
    assert!(rows.iter().any(|row| {
        matches!(
            row.as_slice(),
            [
                RuntimeValue::Host { .. },
                RuntimeValue::Enum { name, variant }
            ] if name == "DispatchKind" && variant == "DIRECT"
        )
    }));
}

#[test]
fn usages_populate_ref_ids_and_search_hits() {
    let root = temp_dir("usage_refs_and_search");
    write_package(
        &root,
        "usage_refs_and_search",
        r#"
fn callee() {}

fn caller() {
    callee();
}
"#,
    );

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let ref_rows = EngineHostView::extern_relation_rows(&mut runtime, "ref_id")
        .expect("ref_id rows")
        .expect("ref_id predicate");
    assert!(
        !ref_rows.is_empty(),
        "usages pass should populate reference IDs"
    );

    let search_rows = EngineHostView::extern_relation_rows(&mut runtime, "search")
        .expect("search rows")
        .expect("search predicate");
    assert!(
        search_rows.iter().any(|row| {
            matches!(
                row.as_slice(),
                [RuntimeValue::String(key), _, RuntimeValue::Int(score)]
                    if key == "callee" && *score == 115
            )
        }),
        "search rows should include usage-backed hit with usage score"
    );
}

#[test]
fn unresolved_calls_are_omitted_in_strict_mode() {
    let root = temp_dir("unresolved_call_omitted");
    write_package(
        &root,
        "unresolved_call_omitted",
        r#"
mod b;

fn caller() {
    b::callee();
}
"#,
    );
    fs::write(root.join("src/b.rs").as_std_path(), "fn callee() {}\n").expect("write b.rs");

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.decl def_name(D: Def, Name: string) extern.
.decl unresolved_edge().

unresolved_edge() :-
  call_edge(Caller, _, _, _),
  def_name(Caller, "caller").
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("unresolved_edge")
            .map_or(true, |rows| rows.is_empty())
    );
}

#[test]
fn call_edge_classifies_semantic_callable_dispatch_kinds() {
    let root = temp_dir("call_dispatch_semantics");
    write_package(
        &root,
        "call_dispatch_semantics",
        r#"
fn target() {}

fn direct_caller() {
    target();
}

fn fn_pointer_caller() {
    let fp: fn() = target;
    fp();
}

fn closure_immediate_caller() {
    (|| target())();
}

fn closure_binding_caller() {
    let bound = || target();
    bound();
}
"#,
    );

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = run_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.decl def_name(D: Def, Name: string) extern.

.decl direct_edge().
.decl fn_pointer_edge().
.decl closure_immediate_edge().
.decl closure_bound_edge().

direct_edge() :-
  call_edge(Caller, Callee, _, DIRECT),
  def_name(Caller, "direct_caller"),
  def_name(Callee, "target").

fn_pointer_edge() :-
  call_edge(Caller, _, _, FN_POINTER),
  def_name(Caller, "fn_pointer_caller").

closure_immediate_edge() :-
  call_edge(Caller, _, _, CLOSURE),
  def_name(Caller, "closure_immediate_caller").

closure_bound_edge() :-
  call_edge(Caller, _, _, THROUGH_TRAIT),
  def_name(Caller, "closure_binding_caller").
"#,
        &mut runtime,
    );

    assert_eq!(result.status, EvalStatus::Ok);
    assert!(
        result
            .relations
            .get("direct_edge")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("fn_pointer_edge")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("closure_immediate_edge")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("closure_bound_edge")
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
