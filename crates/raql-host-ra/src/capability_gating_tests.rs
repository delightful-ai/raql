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
fn unknown_extern_capabilities_fail_explicitly() {
    let root = temp_workspace_root("unsupported");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.decl unsupported_predicate(D: Def) extern.
.decl hit().
hit() :- unsupported_predicate(_).
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
        .expect_err("unknown extern predicates should be rejected");
    assert!(
        err.to_string().contains("unsupported_predicate"),
        "error={err}"
    );
}

#[test]
fn workspace_service_rejects_unknown_capabilities_during_run() {
    let root = temp_workspace_root("run_rejects_unsupported");
    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.decl unsupported_predicate(D: Def) extern.
.decl hit().
hit() :- unsupported_predicate(_).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let err = service
        .run_planned(&planned)
        .expect_err("WorkspaceService should reject unknown capabilities before execution");
    assert!(
        err.to_string().contains("unsupported_predicate"),
        "error={err}"
    );
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
fn search_capability_is_rejected_on_the_daemon_runtime() {
    let root = temp_workspace_root("search_rejected");
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
    let missing =
        MissingCapabilitiesError::from_required_and_supported(required, service.supported_capabilities())
            .expect_err("search should stay disabled until rebuilt from RA-native truth");
    assert!(
        missing.to_string().contains("search"),
        "missing capabilities should include search; err={missing}"
    );
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

#[test]
fn call_graph_capabilities_are_supported_on_the_daemon_runtime() {
    let root = temp_workspace_root("call_graph_supported");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.func dispatch_str(Dispatch: DispatchKind, Label: string) extern.
.decl hit().
hit() :- call_edge(_, _, _, Dispatch), dispatch_str(Dispatch, _).
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
        .expect("call-graph capabilities should be supported in the daemon runtime");
}

#[test]
fn syntax_control_capabilities_are_supported_on_the_daemon_runtime() {
    let root = temp_workspace_root("syntax_control_supported");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.type NodeKind = { IF, MATCH, WHILE, FOR, LOOP, BLOCK, TRY, ARM, OTHER }.
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.func node_at(S: Span, N: option<Node>) extern.
.func node_kind(N: Node, K: NodeKind) extern.
.func node_span(N: Node, S: Span) extern.
.func node_parent(N: Node, P: option<Node>) extern.
.decl enclosing_control(S: Span, K: NodeKind, ControlS: Span, Dist: int) extern.
.func node_id(N: Node, H: string) extern.
.decl hit().
hit() :-
  call_edge(_, _, Site, _),
  node_at(Site, some(Node)),
  node_kind(Node, _),
  node_span(Node, _),
  node_parent(Node, _),
  enclosing_control(Site, _, _, _),
  node_id(Node, _).
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
        .expect("syntax/control capabilities should be supported in the daemon runtime");
}

#[test]
fn approximate_reference_capabilities_are_rejected_on_the_daemon_runtime() {
    let root = temp_workspace_root("reference_events_rejected");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.decl compares(Type: Def, Site: Span, Op: string, Fn: Def) extern.
.decl writes(Subject: Def, Site: Span, Fn: Def) extern.
.decl ref_id(R: Ref, H: string) extern.
.mode ref_id(-Ref, -string).
.decl hit().
hit() :- compares(_, _, _, _), writes(_, _, _), ref_id(_, _).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let required = required_extern_capabilities(&planned)
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    let err =
        MissingCapabilitiesError::from_required_and_supported(required, service.supported_capabilities())
            .expect_err("approximate reference capabilities should be disabled until RA-native");
    let message = err.to_string();
    assert!(message.contains("compares"), "error={message}");
    assert!(message.contains("writes"), "error={message}");
    assert!(message.contains("ref_id"), "error={message}");
}

#[test]
fn error_flow_capabilities_are_rejected_on_the_daemon_runtime() {
    let root = temp_workspace_root("error_flow_rejected");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.decl constructs(ErrType: Def, Variant: string, Site: Span, Fn: Def) extern.
.decl propagates(ErrType: Def, Site: Span, Fn: Def) extern.
.decl converts(SrcErr: Def, DstErr: Def, Site: Span, Fn: Def) extern.
.decl handles(ErrType: Def, Variant: option<string>, Site: Span, Fn: Def) extern.
.decl hit().
hit() :-
  constructs(_, _, _, _),
  propagates(_, _, _),
  converts(_, _, _, _),
  handles(_, _, _, _).
"#;
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    let planned = plan(typed).expect("plan");
    let required = required_extern_capabilities(&planned)
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    let err =
        MissingCapabilitiesError::from_required_and_supported(required, service.supported_capabilities())
            .expect_err("approximate error-flow capabilities should be disabled until RA-native");
    let message = err.to_string();
    assert!(message.contains("constructs"), "error={message}");
    assert!(message.contains("propagates"), "error={message}");
    assert!(message.contains("converts"), "error={message}");
    assert!(message.contains("handles"), "error={message}");
}

#[test]
fn stable_handle_capabilities_are_supported_on_the_daemon_runtime() {
    let root = temp_workspace_root("stable_handles_supported");
    let service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let src = r#"
.decl call_id(C: Call, H: string) extern.
.mode call_id(-Call, -string).
.decl impl_id(I: Impl, H: string) extern.
.mode impl_id(-Impl, -string).
.decl hit().
hit() :- call_id(_, _), impl_id(_, _).
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
        .expect("stable-handle capabilities should be supported in the daemon runtime");
}
