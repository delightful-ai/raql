use camino::Utf8PathBuf;
use raql_compiler::{plan, required_extern_capabilities, resolve, typecheck};
use raql_host::MissingCapabilitiesError;
use raql_syntax::{parse_program, parse_program_from_file};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::workspace_service::WorkspaceService;

fn temp_workspace_root(label: &str) -> Utf8PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "raql_host_ra_capability_{label}_{}_{}",
        std::process::id(),
        stamp
    ));
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "capability_{stamp}"
version = "0.0.0"
edition = "2021"
"#
        ),
    )
    .expect("write Cargo.toml");
    fs::write(root.join("src/lib.rs"), "pub fn caller() {}\n").expect("write lib.rs");
    Utf8PathBuf::from_path_buf(root).expect("utf8 path")
}

#[test]
fn unsupported_stdlib_capabilities_fail_explicitly() {
    let root = temp_workspace_root("unsupported");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.decl hit().
hit() :- call_edge(_, _, _, _).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let required = required_extern_capabilities(&planned)
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    let err = MissingCapabilitiesError::from_required_and_supported(required, service.supported_capabilities())
        .expect_err("call_edge should be unsupported in the day-one RA core");
    assert!(err.to_string().contains("call_edge"), "error={err}");
}

#[test]
fn workspace_service_rejects_unsupported_capabilities_during_run() {
    let root = temp_workspace_root("run_rejects_unsupported");
    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.decl hit().
hit() :- call_edge(_, _, _, _).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let err = service
        .run_planned(&planned)
        .expect_err("WorkspaceService should reject unsupported capabilities before execution");
    assert!(err.to_string().contains("call_edge"), "error={err}");
}

#[test]
fn include_only_entrypoints_do_not_pull_capabilities_from_unreachable_included_rules() {
    let root = temp_workspace_root("include_only_entrypoint");
    let query_dir = root.join("queries");
    fs::create_dir_all(&query_dir).expect("create query dir");
    fs::write(
        query_dir.join("lib.raql"),
        r#"
.decl seed().
.decl hit().
hit() :- seed().

.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.decl unused().
unused() :- call_edge(_, _, _, _).
"#,
    )
    .expect("write lib.raql");
    let entry = query_dir.join("entry.raql");
    fs::write(
        &entry,
        r#"
.include "lib.raql".
seed().
"#,
    )
    .expect("write entry.raql");

    let parsed = parse_program_from_file(entry.as_path(), &[query_dir.clone()]).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let required = required_extern_capabilities(&planned);
    assert!(
        required.is_empty(),
        "include-only entrypoint should not inherit unreachable included capabilities; required={required:?}"
    );
}

#[test]
fn search_capability_is_supported_on_the_daemon_runtime() {
    let root = temp_workspace_root("search_supported");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.decl search(Q: string, D: Def, Score: int) extern.
.decl hit(D: Def, Score: int).
hit(D, Score) :- search("caller", D, Score).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let required = required_extern_capabilities(&planned)
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    MissingCapabilitiesError::from_required_and_supported(required, service.supported_capabilities())
        .expect("search should be supported in the daemon runtime");
}

#[test]
fn structure_and_trait_capabilities_are_supported_on_the_daemon_runtime() {
    let root = temp_workspace_root("structure_trait_supported");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.decl field(Owner: Def, Name: string, Ty: TypeRef) extern.
.decl variant(Enum: Def, Name: string, VariantDef: Def) extern.
.decl method(Owner: Def, Method: Def) extern.
.decl trait_method(Owner: Def, Method: Def) extern.
.decl implements(Type: Def, Trait: Def, ImplDef: Def) extern.
.decl from_impl(Src: Def, Dst: Def, ImplDef: Def) extern.
.decl hit().
hit() :-
  field(_, _, _),
  variant(_, _, _),
  method(_, _),
  trait_method(_, _),
  implements(_, _, _),
  from_impl(_, _, _).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let required = required_extern_capabilities(&planned)
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    MissingCapabilitiesError::from_required_and_supported(required, service.supported_capabilities())
        .expect("structure/trait capabilities should be supported in the daemon runtime");
}

#[test]
fn type_surface_capabilities_are_supported_on_the_daemon_runtime() {
    let root = temp_workspace_root("type_surface_supported");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.type Mutability = { IMM, MUT }.
.func fn_error_type(F: Def, Err: option<Def>) extern.
.func fn_return_type(F: Def, TR: TypeRef) extern.
.decl ty_app(TR: TypeRef, Head: Def) extern.
.decl ty_arg(TR: TypeRef, Index: int, Arg: TypeRef) extern.
.decl ty_ref(TR: TypeRef, Mut: Mutability, Inner: TypeRef) extern.
.decl ty_ptr(TR: TypeRef, Mut: Mutability, Inner: TypeRef) extern.
.decl ty_tuple(TR: TypeRef, Index: int, Elem: TypeRef) extern.
.decl ty_slice(TR: TypeRef, Elem: TypeRef) extern.
.decl ty_param(TR: TypeRef, Param: Def) extern.
.decl ty_prim(TR: TypeRef, Name: string) extern.
.decl ty_unknown(TR: TypeRef) extern.
.func typeref_id(TR: TypeRef, H: string) extern.
.decl hit().
hit() :-
  fn_error_type(_, _),
  fn_return_type(_, _),
  ty_app(_, _),
  ty_arg(_, _, _),
  ty_ref(_, _, _),
  ty_ptr(_, _, _),
  ty_tuple(_, _, _),
  ty_slice(_, _),
  ty_param(_, _),
  ty_prim(_, _),
  ty_unknown(_),
  typeref_id(_, _).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let required = required_extern_capabilities(&planned)
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    MissingCapabilitiesError::from_required_and_supported(required, service.supported_capabilities())
        .expect("type-surface capabilities should be supported in the daemon runtime");
}
