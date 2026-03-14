use camino::Utf8PathBuf;
use raql_compiler::{plan, resolve, typecheck};
use raql_engine::RuntimeValue;
use raql_host_ra::WorkspaceService;
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

#[test]
fn workspace_service_tracks_incremental_rust_file_edits() {
    let root = temp_workspace_root("incremental");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
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
