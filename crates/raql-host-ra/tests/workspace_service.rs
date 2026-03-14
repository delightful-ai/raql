use camino::Utf8PathBuf;
use raql_compiler::{plan, resolve, typecheck};
use raql_engine::RuntimeValue;
use raql_host_ra::{RaHostInitError, WorkspaceService};
use raql_syntax::parse_program;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

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

fn query_world_stamp(service: &mut WorkspaceService) -> String {
    let planned = plan_query(
        r#"
.func world_stamp(Stamp: string) extern.
.decl stamp(Stamp: string).
stamp(Stamp) :- world_stamp(Stamp).
"#,
    );
    let result = service.run_planned(&planned).expect("run world_stamp query");
    result
        .relations
        .get("stamp")
        .and_then(|rows| rows.first())
        .and_then(|row| row.first())
        .and_then(|value| match value {
            RuntimeValue::String(text) => Some(text.clone()),
            _ => None,
        })
        .expect("world_stamp row")
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
fn workspace_service_init_succeeds_from_manifest_path() {
    let root = temp_workspace_root("manifest_path_init");
    fs::write(root.join("src/lib.rs"), "pub fn via_manifest() {}\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_manifest_path(root.join("Cargo.toml").as_std_path())
        .expect("service from manifest path");
    let stamp = query_world_stamp(&mut service);
    assert!(stamp.starts_with("ra-workspace:"), "stamp={stamp}");

    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl hit().
hit() :- def(D), def_name(D, "via_manifest").
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(
        result
            .relations
            .get("hit")
            .is_some_and(|rows| !rows.is_empty())
    );
}

#[test]
fn workspace_service_tracks_incremental_rust_file_edits() {
    let root = temp_workspace_root("incremental");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");

    let mut service =
        WorkspaceService::from_manifest_path(root.join("Cargo.toml").as_std_path()).expect("service");
    let initial_epoch = service.workspace_epoch();
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "alpha").
"#,
    );
    let first = service.run_planned(&planned).expect("first run");
    assert!(first.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("alpha".to_string())])
    }));

    fs::write(root.join("src/lib.rs"), "pub fn omega() {}\n").expect("rewrite lib.rs");
    let second_planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "omega").
"#,
    );
    let second = service.run_planned(&second_planned).expect("second run");
    assert!(second.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("omega".to_string())])
    }));
    assert_eq!(
        service.workspace_epoch(),
        initial_epoch,
        "same-file Rust edits should remain incremental rather than forcing a full reload"
    );
    assert!(service.content_revision() > 0, "content revision should advance after source edit");
}

#[test]
fn workspace_service_world_stamp_changes_when_local_file_content_changes() {
    let root = temp_workspace_root("world_stamp_local_change");
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }\n").expect("write lib.rs");

    let mut service =
        WorkspaceService::from_manifest_path(root.join("Cargo.toml").as_std_path()).expect("service");
    let before = query_world_stamp(&mut service);

    fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 2 }\n").expect("rewrite lib.rs");
    let after = query_world_stamp(&mut service);

    assert_ne!(before, after);
}

#[test]
fn workspace_service_world_stamp_changes_when_manifest_configuration_changes() {
    let root = temp_workspace_root("world_stamp_manifest_change");
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let epoch_before = service.workspace_epoch();
    let stamp_before = query_world_stamp(&mut service);

    let manifest = root.join("Cargo.toml");
    let mut manifest_text = fs::read_to_string(manifest.as_std_path()).expect("read Cargo.toml");
    manifest_text.push_str(
        r#"
[features]
default = []
extra = []
"#,
    );
    fs::write(manifest.as_std_path(), manifest_text).expect("rewrite Cargo.toml");

    let stamp_after = query_world_stamp(&mut service);
    assert_ne!(stamp_before, stamp_after);
    assert!(
        service.workspace_epoch() > epoch_before,
        "manifest changes should force a workspace reload"
    );
}

#[test]
fn workspace_service_world_stamp_is_stable_when_workspace_is_unchanged() {
    let root = temp_workspace_root("world_stamp_stable");
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let first = query_world_stamp(&mut service);
    let second = query_world_stamp(&mut service);

    assert_eq!(first, second);
}

#[test]
fn workspace_service_reports_manifest_removal_on_next_run() {
    let root = temp_workspace_root("manifest_removal");
    fs::write(root.join("src/lib.rs"), "pub fn marker() {}\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    fs::remove_file(root.join("Cargo.toml").as_std_path()).expect("remove Cargo.toml");

    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.decl hit().
hit() :- def(D).
"#,
    );
    let err = service.run_planned(&planned).expect_err("run should fail");
    assert!(
        err.to_string().contains("workspace"),
        "expected workspace load failure after manifest removal; err={err}"
    );
}

#[test]
fn workspace_service_from_member_manifest_sees_sibling_workspace_members() {
    let root = temp_workspace_root("member_manifest_scope");
    fs::write(
        root.join("Cargo.toml"),
        r#"[workspace]
members = ["member_a", "member_b"]
resolver = "2"
"#,
    )
    .expect("write workspace Cargo.toml");

    let member_a = root.join("member_a");
    let member_b = root.join("member_b");
    fs::create_dir_all(member_a.join("src")).expect("create member_a src");
    fs::create_dir_all(member_b.join("src")).expect("create member_b src");
    fs::write(
        member_a.join("Cargo.toml"),
        r#"[package]
name = "member_a"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write member_a Cargo.toml");
    fs::write(
        member_b.join("Cargo.toml"),
        r#"[package]
name = "member_b"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write member_b Cargo.toml");
    fs::write(member_a.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write member_a lib.rs");
    fs::write(member_b.join("src/lib.rs"), "pub fn beta() {}\n").expect("write member_b lib.rs");

    let mut service = WorkspaceService::from_manifest_path(member_a.join("Cargo.toml").as_std_path())
        .expect("service from member manifest");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "beta").
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("beta".to_string())])
    }));
}

#[test]
fn workspace_service_from_workspace_root_sees_all_workspace_members() {
    let root = temp_workspace_root("workspace_root_scope");
    fs::write(
        root.join("Cargo.toml"),
        r#"[workspace]
members = ["corelib", "app"]
resolver = "2"
"#,
    )
    .expect("write workspace Cargo.toml");

    let core_dir = root.join("corelib");
    let app_dir = root.join("app");
    fs::create_dir_all(core_dir.join("src")).expect("create corelib src");
    fs::create_dir_all(app_dir.join("src")).expect("create app src");
    fs::write(
        core_dir.join("Cargo.toml"),
        r#"[package]
name = "corelib"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write corelib Cargo.toml");
    fs::write(core_dir.join("src/lib.rs"), "pub fn helper() {}\n").expect("write corelib lib.rs");
    fs::write(
        app_dir.join("Cargo.toml"),
        r#"[package]
name = "app"
version = "0.0.0"
edition = "2021"

[dependencies]
corelib = { path = "../corelib" }
"#,
    )
    .expect("write app Cargo.toml");
    fs::write(
        app_dir.join("src/lib.rs"),
        r#"
pub fn caller() {
    corelib::helper();
}
"#,
    )
    .expect("write app lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl visible(Name: string).
visible(Name) :- def(D), def_name(D, Name).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("visible").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("caller".to_string())])
            && rows.contains(&vec![RuntimeValue::String("helper".to_string())])
    }));
}

#[test]
fn workspace_service_includes_path_dependency_defs() {
    let dep_root = temp_workspace_root("path_dep_source");
    fs::write(
        dep_root.join("Cargo.toml"),
        r#"[package]
name = "path_dep_source"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write dep Cargo.toml");
    fs::write(
        dep_root.join("src/lib.rs"),
        r#"
pub fn helper() {
    deep();
}

fn deep() {}
"#,
    )
    .expect("write dep lib.rs");

    let root = temp_workspace_root("path_dep_app");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "path_dep_app"
version = "0.0.0"
edition = "2021"

[dependencies]
path_dep_source = {{ path = "{}" }}
"#,
            dep_root.as_std_path().display()
        ),
    )
    .expect("write app Cargo.toml");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn caller() {
    path_dep_source::helper();
}
"#,
    )
    .expect("write app lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl visible(Name: string).
visible(Name) :- def(D), def_name(D, Name).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    let observed = result.relations.get("visible").cloned().unwrap_or_default();
    assert!(
        observed.contains(&vec![RuntimeValue::String("caller".to_string())])
            && observed.contains(&vec![RuntimeValue::String("helper".to_string())])
            && observed.contains(&vec![RuntimeValue::String("deep".to_string())]),
        "expected caller/helper/deep in path-dependency scope; observed={observed:?}"
    );
}

#[test]
fn workspace_service_excludes_registry_like_dependency_defs() {
    let dep_root = temp_workspace_root("registry_like_dep")
        .join("registry")
        .join("src")
        .join("fake-index")
        .join("registry_like_dep");
    fs::create_dir_all(dep_root.join("src")).expect("create dep src");
    fs::write(
        dep_root.join("Cargo.toml"),
        r#"[package]
name = "registry_like_dep"
version = "0.0.0"
edition = "2021"
"#,
    )
    .expect("write dep Cargo.toml");
    fs::write(dep_root.join("src/lib.rs"), "pub fn helper() {}\n").expect("write dep lib.rs");

    let root = temp_workspace_root("registry_like_dep_app");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
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
        root.join("src/lib.rs"),
        r#"
pub fn caller() {
    registry_like_dep::helper();
}
"#,
    )
    .expect("write app lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl visible(Name: string).
visible(Name) :- def(D), def_name(D, Name).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    let observed = result.relations.get("visible").cloned().unwrap_or_default();
    assert!(
        observed.contains(&vec![RuntimeValue::String("caller".to_string())])
            && !observed.contains(&vec![RuntimeValue::String("helper".to_string())]),
        "expected registry-like dep helper to stay hidden; observed={observed:?}"
    );
}

#[test]
fn workspace_service_surfaces_generated_build_symbols() {
    let root = temp_workspace_root("generated_symbols");
    fs::write(
        root.join("src/lib.rs"),
        r#"
include!(concat!(env!("OUT_DIR"), "/generated.rs"));

pub fn caller() {
    generated_fn();
}
"#,
    )
    .expect("write lib.rs");
    fs::write(
        root.join("build.rs"),
        r#"
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out.join("generated.rs"), "pub fn generated_fn() {}\n").expect("write generated");
}
"#,
    )
    .expect("write build.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl generated(Name: string).
generated(Name) :- def(D), def_name(D, Name), contains(Name, "generated_fn").
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("generated").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("generated_fn".to_string())])
    }));
}

#[test]
fn workspace_service_exposes_world_stamp_on_supported_runs() {
    let root = temp_workspace_root("world_stamp_smoke");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.func world_stamp(Stamp: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl stamp(Stamp: string).
stamp(Stamp) :- world_stamp(Stamp), contains(Stamp, "ra-workspace:").
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("stamp").is_some_and(|rows| {
        rows.iter().any(|row| {
            row.first().is_some_and(|value| match value {
                RuntimeValue::String(text) => text.starts_with("ra-workspace:"),
                _ => false,
            })
        })
    }));
}

#[test]
fn workspace_service_reports_supported_def_paths() {
    let root = temp_workspace_root("def_path_smoke");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.func def_path(D: Def, Path: string) extern.
.decl hit(Path: string).
hit(Path) :- def(D), def_name(D, "alpha"), def_path(D, Path).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    let observed = result
        .relations
        .get("hit")
        .cloned()
        .unwrap_or_default();
    assert!(
        observed.iter().any(|row| {
            row.first().is_some_and(|value| match value {
                RuntimeValue::String(path) => path.ends_with("alpha"),
                _ => false,
            })
        }),
        "expected a supported def_path row for alpha; observed={observed:?}"
    );
}

#[test]
fn workspace_service_marks_public_and_test_defs() {
    let root = temp_workspace_root("visibility_test_markers");
    fs::write(
        root.join("src/lib.rs"),
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
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl is_public(D: Def) extern.
.decl in_test(D: Def) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl exported_public(Name: string).
.decl hidden_public(Name: string).
.decl helper_test(Name: string).
.decl exported_test(Name: string).
exported_public(Name) :- def(D), def_name(D, Name), is_public(D).
hidden_public(Name) :- def(D), def_name(D, Name), is_public(D), contains(Name, "hidden").
helper_test(Name) :- def(D), def_name(D, Name), in_test(D), contains(Name, "helper").
exported_test(Name) :- def(D), def_name(D, Name), in_test(D), contains(Name, "exported").
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(
        result
            .relations
            .get("exported_public")
            .is_some_and(|rows| {
                rows.contains(&vec![RuntimeValue::String("exported".to_string())])
            })
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
            .is_some_and(|rows| {
                rows.contains(&vec![RuntimeValue::String("helper".to_string())])
            })
    );
    assert!(
        result
            .relations
            .get("exported_test")
            .is_some_and(|rows| rows.is_empty())
    );
}

#[test]
fn workspace_service_reports_workspace_relative_span_keys() {
    let root = temp_workspace_root("span_key_relative");
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.func def_span(D: Def, S: Span) extern.
.func span_key(S: Span, RelPath: string, L0: int, C0: int, L1: int, C1: int) extern.
.decl hit(RelPath: string).
hit(RelPath) :-
  def(D),
  def_name(D, "answer"),
  def_span(D, S),
  span_key(S, RelPath, _L0, _C0, _L1, _C1).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("src/lib.rs".to_string())])
    }));
}
