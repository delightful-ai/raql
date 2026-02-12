use camino::Utf8PathBuf;
use raql_compiler::{plan, resolve, typecheck};
use raql_engine::{EvalStatus, RuntimeValue, execute};
use raql_host::HostRuntime;
use raql_host_ra::RaHostRuntime;
use raql_syntax::{IncludeLoader, parse_program};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> Utf8PathBuf {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("raql workspace root")
        .to_path_buf()
}

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

#[test]
fn executes_raql_against_this_repository_snapshot() {
    let root = temp_dir("snapshot_smoke");
    write_package(
        &root,
        "snapshot_smoke",
        r#"
pub fn extern_relation_rows_for_predicate() {}

pub fn caller() {
    extern_relation_rows_for_predicate();
}
"#,
    );

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    assert!(runtime.analysis_status_ok());
    let stamp = HostRuntime::world_stamp(&runtime)
        .expect("world stamp")
        .as_str()
        .to_string();
    assert!(
        stamp.starts_with("ra-workspace:"),
        "world stamp should encode workspace snapshot identity: {stamp}"
    );

    let src = r#"
.func world_stamp(S: string) extern.
.mode world_stamp(-string).
.decl contains(Haystack: string, Needle: string) extern.
.mode contains(+string, +string).
.decl stamp(S: string).
stamp(S) :- world_stamp(S), contains(S, "raql").
"#;

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let result = execute(&planned, &mut runtime);

    if result.status != EvalStatus::Ok {
        panic!(
            "unexpected status {:?}; notes: {:?}",
            result.status, result.notes
        );
    }
    assert!(
        result
            .relations
            .get("stamp")
            .is_some_and(|rows| { rows.contains(&vec![RuntimeValue::String(stamp.clone())]) })
    );
    assert!(
        result
            .relations
            .get("out_status")
            .is_some_and(|rows| { rows.contains(&vec![RuntimeValue::String("ok".to_string())]) })
    );
}

#[test]
fn executes_stdlib_include_against_repository_snapshot() {
    let repo = repo_root();
    let workspace_root = temp_dir("stdlib_workspace");
    write_package(
        &workspace_root,
        "stdlib_workspace",
        r#"
pub fn extern_relation_rows_for_predicate() {}

pub fn caller() {
    extern_relation_rows_for_predicate();
}
"#,
    );

    let query_dir = temp_dir("stdlib_include");
    let query_path = query_dir.join("query.raql");
    let query = r#"
.include "std.raql".

.decl hit(Name: string).
.decl has_edge() .

hit(Name) :-
  def(D),
  is_fn(D),
  def_name(D, Name),
  contains(Name, "extern_relation_rows_for_predicate").

has_edge() :- call_edge(_, _, _, _).
"#;
    fs::write(query_path.as_std_path(), query).expect("write query");

    let loader = IncludeLoader::new(vec![repo.clone()]);
    let parsed = loader
        .load_program(query_path.as_path())
        .expect("parse+include");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");

    let mut runtime =
        RaHostRuntime::from_workspace_root(workspace_root.as_std_path()).expect("runtime");
    let result = execute(&planned, &mut runtime);

    if result.status != EvalStatus::Ok {
        panic!(
            "unexpected status {:?}; notes: {:?}",
            result.status, result.notes
        );
    }
    assert!(result.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String(
            "extern_relation_rows_for_predicate".to_string(),
        )])
    }));
    assert!(
        result
            .relations
            .get("has_edge")
            .is_some_and(|rows| !rows.is_empty())
    );
}

#[test]
fn derives_cross_file_call_edges_from_workspace_semantics() {
    let root = temp_dir("cross_file_edges");
    let cargo_toml = root.join("Cargo.toml");
    let src = root.join("src");
    fs::create_dir_all(src.as_std_path()).expect("create src");
    let lib = src.join("lib.rs");
    let b = src.join("b.rs");

    fs::write(
        cargo_toml.as_std_path(),
        r#"
[package]
name = "cross_file_edges"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write Cargo.toml");
    fs::write(
        lib.as_std_path(),
        r#"
mod b;

fn caller() {
    b::callee();
}
"#,
    )
    .expect("write lib.rs");
    fs::write(
        b.as_std_path(),
        r#"
fn callee() {}
"#,
    )
    .expect("write src/b.rs");

    let src = r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE }.
.decl def(D: Def) extern.
.mode def(-Def).
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.mode call_edge(-Def, -Def, -Span, -DispatchKind).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.decl hit().

hit() :-
  call_edge(Caller, Callee, _, _),
  def_name(Caller, "caller"),
  def_name(Callee, "callee").
"#;

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = execute(&planned, &mut runtime);

    if result.status != EvalStatus::Ok {
        panic!(
            "unexpected status {:?}; notes: {:?}",
            result.status, result.notes
        );
    }
    assert!(
        result
            .relations
            .get("hit")
            .is_some_and(|rows| !rows.is_empty())
    );
}

#[test]
fn derives_module_scoped_defs_and_edges_from_workspace_semantics() {
    let root = temp_dir("module_scoped_edges");
    let cargo_toml = root.join("Cargo.toml");
    let src = root.join("src");
    fs::create_dir_all(src.as_std_path()).expect("create src");
    let lib = src.join("lib.rs");
    let foo = src.join("foo.rs");

    fs::write(
        cargo_toml.as_std_path(),
        r#"
[package]
name = "module_scoped_edges"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write Cargo.toml");
    fs::write(
        lib.as_std_path(),
        r#"
mod foo;

fn caller() {
    foo::callee();
}
"#,
    )
    .expect("write lib.rs");
    fs::write(
        foo.as_std_path(),
        r#"
pub fn callee() {}
"#,
    )
    .expect("write src/foo.rs");

    let src = r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE }.
.decl def(D: Def) extern.
.mode def(-Def).
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.mode call_edge(-Def, -Def, -Span, -DispatchKind).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.func def_path(D: Def, Path: string) extern.
.mode def_path(+Def, -string).

.decl has_module_path().
.decl has_module_edge().

has_module_path() :-
  def(D),
  def_name(D, "callee"),
  def_path(D, "crate::foo::callee").

has_module_path() :-
  def(D),
  def_name(D, "callee"),
  def_path(D, "crate::callee").

has_module_edge() :-
  call_edge(Caller, Callee, _, _),
  def_path(Caller, "crate::caller"),
  def_path(Callee, "crate::foo::callee").

has_module_edge() :-
  call_edge(Caller, Callee, _, _),
  def_path(Caller, "crate::caller"),
  def_path(Callee, "crate::callee").
"#;

    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");

    let mut runtime = RaHostRuntime::from_workspace_root(root.as_std_path()).expect("runtime");
    let result = execute(&planned, &mut runtime);

    if result.status != EvalStatus::Ok {
        panic!(
            "unexpected status {:?}; notes: {:?}",
            result.status, result.notes
        );
    }
    assert!(
        result
            .relations
            .get("has_module_path")
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        result
            .relations
            .get("has_module_edge")
            .is_some_and(|rows| !rows.is_empty())
    );
}
