//! Tests for the surviving session path: load a real Cargo workspace,
//! keep it in sync, run a planned program through `raql-ra` operators,
//! and project the result (SPEC §13.1).
//!
//! Semantic truth per predicate belongs to `raql-ra`'s own suites; what is
//! proved here is the seam this crate owns — that a program compiled from
//! the catalog reaches RA over a real workspace and comes back as
//! projected rows, and that source edits are visible on the next run.

use camino::Utf8PathBuf;
use raql_compiler::{plan, required_extern_capabilities, resolve, typecheck};
use raql_engine::EvalStatus;
use raql_host::{CapabilityId, MissingCapabilitiesError};
use raql_syntax::parse_program;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::RaHostInitError;
use crate::projection::{ProjectedRunResult, ProjectedValue};
use crate::workspace_service::WorkspaceService;

fn temp_workspace_root(label: &str) -> Utf8PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "raql_host_ra_workspace_service_{label}_{}_{}",
        std::process::id(),
        stamp
    ));
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "workspace_service_{stamp}"
version = "0.0.0"
edition = "2021"
"#
        ),
    )
    .expect("write Cargo.toml");
    Utf8PathBuf::from_path_buf(root).expect("utf8 path")
}

fn plan_query(src: &str) -> raql_compiler::PlannedProgram {
    let parsed = parse_program(src).expect("parse");
    let resolved = resolve(parsed).expect("resolve");
    let typed = typecheck(resolved).expect("typecheck");
    plan(typed).expect("plan")
}

/// The strings of a one-column relation, in extent order.
fn column(result: &ProjectedRunResult, relation: &str) -> Vec<String> {
    result
        .relations
        .get(relation)
        .map(|rows| {
            rows.iter()
                .map(|row| match row.as_slice() {
                    [ProjectedValue::String(text)] => text.clone(),
                    other => panic!("expected a one-string row in `{relation}`; row={other:?}"),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn workspace_service_init_fails_without_workspace_manifest() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "raql_host_ra_workspace_service_init_missing_{}_{}",
        std::process::id(),
        stamp
    ));
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("standalone.rs"), "fn main() {}\n").expect("write standalone.rs");

    let err = WorkspaceService::from_workspace_root(root.as_path()).expect_err("must fail");
    let RaHostInitError::WorkspaceNotFound { details, .. } = err else {
        panic!("expected workspace-not-found error");
    };
    assert!(
        !details.trim().is_empty(),
        "workspace-not-found error should include cause details"
    );
}

#[test]
fn workspace_service_runs_a_seeded_query_end_to_end() {
    let root = temp_workspace_root("seeded_end_to_end");
    fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha() {}\npub fn beta() { alpha(); }\n",
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_manifest_path(root.join("Cargo.toml").as_std_path())
        .expect("service from manifest path");
    let planned = plan_query(
        r#"
.decl target(D: Def).
target(D) :- def_name(D, "beta").

.decl beta_path(Path: string) output.
beta_path(Path) :- target(D), def_path(D, Path).

.decl beta_def(D: Def) output.
beta_def(D) :- target(D).
"#,
    );
    let result = service.run_planned(&planned).expect("run seeded query");

    assert_eq!(result.status, EvalStatus::Ok, "notes={:?}", result.notes);
    assert_eq!(
        column(&result, "out_status"),
        vec!["ok".to_string()],
        "notes={:?}",
        result.notes
    );

    let paths = column(&result, "beta_path");
    assert!(
        paths.iter().any(|path| path.ends_with("::beta")),
        "expected a canonical path ending in `::beta`; paths={paths:?}"
    );

    // A `Def` reaches the boundary as its §13.1 projection: kind, canonical
    // path, workspace-relative location.
    let defs = column(&result, "beta_def");
    assert_eq!(defs.len(), 1, "defs={defs:?}");
    let def = &defs[0];
    assert!(def.starts_with("FN "), "def={def}");
    assert!(def.contains("::beta @ src/lib.rs:2:"), "def={def}");
}

#[test]
fn workspace_service_answers_bound_call_queries() {
    let root = temp_workspace_root("bound_calls");
    fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha() {}\npub fn beta() { alpha(); }\n",
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_manifest_path(root.join("Cargo.toml").as_std_path())
        .expect("service from manifest path");
    let planned = plan_query(
        r#"
.decl target(D: Def).
target(D) :- def_name(D, "alpha").

.decl alpha_callers(Path: string) output.
alpha_callers(Path) :- target(D), caller(D, C, _, _), def_path(C, Path).

.decl beta_callees(Path: string) output.
beta_callees(Path) :-
  def_name(B, "beta"),
  callee(B, K, _, _),
  def_path(K, Path).
"#,
    );
    let result = service.run_planned(&planned).expect("run call query");

    assert_eq!(result.status, EvalStatus::Ok, "notes={:?}", result.notes);
    let callers = column(&result, "alpha_callers");
    assert!(
        callers.iter().any(|path| path.ends_with("::beta")),
        "expected `beta` among alpha's callers; callers={callers:?}"
    );
    let callees = column(&result, "beta_callees");
    assert!(
        callees.iter().any(|path| path.ends_with("::alpha")),
        "expected `alpha` among beta's callees; callees={callees:?}"
    );
}

#[test]
fn workspace_service_sees_incremental_rust_file_edits() {
    let root = temp_workspace_root("incremental");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_manifest_path(root.join("Cargo.toml").as_std_path())
        .expect("service from manifest path");
    let initial_epoch = service.workspace_epoch();

    let alpha_query = plan_query(
        r#"
.decl hit(Path: string) output.
hit(Path) :- def_name(D, "alpha"), def_path(D, Path).
"#,
    );
    let omega_query = plan_query(
        r#"
.decl hit(Path: string) output.
hit(Path) :- def_name(D, "omega"), def_path(D, Path).
"#,
    );

    let first = service.run_planned(&alpha_query).expect("first run");
    assert!(!column(&first, "hit").is_empty(), "expected `alpha` before the edit");

    fs::write(root.join("src/lib.rs"), "pub fn omega() {}\n").expect("rewrite lib.rs");

    let second = service.run_planned(&omega_query).expect("second run");
    assert!(
        !column(&second, "hit").is_empty(),
        "the rewritten source should be visible on the next run"
    );
    let stale = service.run_planned(&alpha_query).expect("stale run");
    assert!(
        column(&stale, "hit").is_empty(),
        "the removed definition should be gone; rows={:?}",
        stale.relations.get("hit")
    );

    assert_eq!(
        service.workspace_epoch(),
        initial_epoch,
        "same-file Rust edits should remain incremental rather than forcing a full reload"
    );
    assert!(
        service.content_revision() > 0,
        "content revision should advance after a source edit"
    );
}

#[test]
fn supported_capabilities_cover_what_planned_programs_require() {
    let root = temp_workspace_root("capability_gate");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");
    let service = WorkspaceService::from_manifest_path(root.join("Cargo.toml").as_std_path())
        .expect("service from manifest path");

    // One goal per catalog family the host claims to implement, so the
    // daemon's gate is proved against the operator set rather than against
    // a hand-maintained list.
    let planned = plan_query(
        r#"
.type DefKind = { FN, METHOD, STRUCT, ENUM, UNION, TRAIT, MOD, IMPL, TYPE_ALIAS,
  CONST, STATIC, FIELD, VARIANT, ASSOC_TYPE, ASSOC_CONST, MACRO, OTHER }.
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.

.decl report(Name: string, Path: string, Handle: string, Key: string) output.
report(Name, Path, Handle, Key) :-
  fn_def(F),
  def(F),
  def_name(F, Name),
  def_kind(F, DefKind::FN),
  is_public(F),
  not in_test(F),
  def_path(F, Path),
  handle(F, Handle),
  def_span(F, S),
  span_allowed(S),
  span_key(S, Key, _, _, _, _),
  call_edge(F, _, Site, DispatchKind::DIRECT),
  span_allowed(Site),
  caller(F, _, _, _),
  callee(F, _, _, _).
"#,
    );

    let required = required_extern_capabilities(&planned);
    assert!(
        required.len() >= 12,
        "the gate probe should exercise most of the catalog; required={required:?}"
    );
    MissingCapabilitiesError::from_required_and_supported(
        required.iter().cloned().map(CapabilityId::from),
        service.supported_capabilities(),
    )
    .expect("every catalog predicate the host implements must be a supported capability");
}
