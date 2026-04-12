use camino::Utf8PathBuf;
use raql_compiler::{plan, resolve, typecheck};
use raql_engine::{EvalStatus, RuntimeValue};
use raql_host::{
    DefId, ExternLookupHostValue, ExternLookupHostValueKind, ExternLookupRequest,
    ExternLookupShape, ExternLookupValue,
};
use raql_syntax::parse_program;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate as raql_host_ra;
use crate::RaHostInitError;
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
    assert!(
        !second.relations.get("hit").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::String("alpha".to_string())])
        }),
        "incremental refresh should drop stale defs after source rewrite"
    );

    let stale_planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "alpha").
"#,
    );
    let stale = service.run_planned(&stale_planned).expect("stale query run");
    assert!(
        stale.relations.get("hit").is_none_or(|rows| rows.is_empty()),
        "stale alpha defs should disappear after incremental rewrite; rows={:?}",
        stale.relations.get("hit")
    );
    assert_eq!(
        service.workspace_epoch(),
        initial_epoch,
        "same-file Rust edits should remain incremental rather than forcing a full reload"
    );
    assert!(service.content_revision() > 0, "content revision should advance after source edit");
}

#[test]
fn workspace_service_preserves_core_host_on_incremental_syntax_edits() {
    let root = temp_workspace_root("incremental_core_host_overlay");
    fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha() {}\npub fn beta() { alpha(); }\n",
    )
    .expect("write lib.rs");

    let mut service =
        WorkspaceService::from_manifest_path(root.join("Cargo.toml").as_std_path()).expect("service");
    let beta_query = plan_query(
        r#"
.decl def_span(D: Def, Span: Span) extern.
.decl node_at(Query: Span, Node: Node) extern.
.func def_name(D: Def, Name: string) extern.
.decl hit(Name: string).
hit(Name) :- def_name(D, "beta"), def_name(D, Name), def_span(D, Span), node_at(Span, _).
"#,
    );
    let first = service.run_planned(&beta_query).expect("first run");
    assert!(first.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("beta".to_string())])
    }));
    assert!(service.has_core_host(), "core host should be populated after call-graph query");

    fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha() {}\npub fn gamma() { alpha(); }\n",
    )
    .expect("rewrite lib.rs");
    service.sync().expect("sync after edit");
    assert!(
        service.has_core_host(),
        "ordinary rust edits should preserve core host via changed-file overlay"
    );

    let gamma_query = plan_query(
        r#"
.decl def_span(D: Def, Span: Span) extern.
.decl node_at(Query: Span, Node: Node) extern.
.func def_name(D: Def, Name: string) extern.
.decl hit(Name: string).
hit(Name) :- def_name(D, "gamma"), def_name(D, Name), def_span(D, Span), node_at(Span, _).
"#,
    );
    let second = service.run_planned(&gamma_query).expect("second run");
    assert!(second.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("gamma".to_string())])
    }));
    assert!(
        !second.relations.get("hit").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::String("beta".to_string())])
        }),
        "stale syntax rows should be removed after overlay refresh"
    );
}

#[test]
fn workspace_service_tracks_incremental_rust_file_edits_after_watcher_settles() {
    let root = temp_workspace_root("incremental_after_watcher_ready");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");

    let mut service =
        WorkspaceService::from_manifest_path(root.join("Cargo.toml").as_std_path()).expect("service");
    std::thread::sleep(Duration::from_millis(150));

    let initial_epoch = service.workspace_epoch();
    fs::write(root.join("src/lib.rs"), "pub fn omega() {}\n").expect("rewrite lib.rs");

    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "omega").
"#,
    );
    let result = service.run_planned(&planned).expect("watcher-ready run");
    assert!(result.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("omega".to_string())])
    }));
    assert_eq!(
        service.workspace_epoch(),
        initial_epoch,
        "same-file Rust edits should stay incremental after watcher settles"
    );
}

#[test]
fn workspace_service_excludes_import_and_alias_symbols_from_def_surface() {
    let root = temp_workspace_root("def_surface_excludes_aliases");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub mod inner {
    pub fn beta() {}
}

pub fn alpha() {}
pub use inner::beta as beta_alias;
use inner::beta as local_beta_alias;
"#,
    )
    .expect("write lib.rs");

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
    let rows = result.relations.get("visible").expect("visible rows");
    assert!(rows.contains(&vec![RuntimeValue::String("alpha".to_string())]));
    assert!(rows.contains(&vec![RuntimeValue::String("beta".to_string())]));
    assert!(
        !rows.contains(&vec![RuntimeValue::String("beta_alias".to_string())]),
        "re-export alias should not become a def row; rows={rows:?}"
    );
    assert!(
        !rows.contains(&vec![RuntimeValue::String("local_beta_alias".to_string())]),
        "local import alias should not become a def row; rows={rows:?}"
    );
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
fn workspace_service_ignores_unrelated_root_subtree_file_changes() {
    let root = temp_workspace_root("ignore_unrelated_root_subtree");
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }\n").expect("write lib.rs");
    let unrelated_dir = root.join("tmp/unrelated");
    fs::create_dir_all(unrelated_dir.as_std_path()).expect("create unrelated dir");
    let unrelated_file = unrelated_dir.join("ghost.rs");
    fs::write(unrelated_file.as_std_path(), "pub fn ghost() {}\n").expect("write unrelated file");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let before = query_world_stamp(&mut service);

    std::thread::sleep(Duration::from_millis(20));
    fs::write(unrelated_file.as_std_path(), "pub fn ghost() { let _ = 1; }\n")
        .expect("rewrite unrelated file");

    let after = query_world_stamp(&mut service);
    assert_eq!(
        before, after,
        "unrelated root-subtree files outside RA/VFS workspace truth must not perturb world stamp"
    );
}

#[test]
fn workspace_service_rejects_semantic_search_queries() {
    let root = temp_workspace_root("semantic_search");
    fs::write(root.join("src/lib.rs"), "pub fn alpha_marker() {}\npub fn omega_marker() {}\n")
        .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl search(Q: string, D: Def, Score: int) extern.
.func def_name(D: Def, Name: string) extern.
.decl hit(Name: string).
hit(Name) :- search("alpha", D, _Score), def_name(D, Name).
"#,
    );
    let err = service
        .run_planned(&planned)
        .expect_err("search should be rejected until rebuilt from RA-native symbol search");
    let msg = err.to_string();
    assert!(msg.contains("search"), "error should mention search; err={msg}");
}

#[test]
fn workspace_service_supports_structure_and_trait_rows() {
    let root = temp_workspace_root("structure_trait_rows");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub trait Greeter {
    fn greet(&self);
}

pub struct Person {
    pub name: String,
    age: u32,
}

pub enum Choice {
    First,
    Second,
}

pub struct NameError;
pub struct AgeError;

impl Greeter for Person {
    fn greet(&self) {}
}

impl From<NameError> for AgeError {
    fn from(_: NameError) -> Self {
        AgeError
    }
}
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.type DefKind = {
  FN, METHOD,
  STRUCT, ENUM, UNION, TRAIT,
  MOD, IMPL, TYPE_ALIAS,
  CONST, STATIC,
  FIELD, VARIANT,
  ASSOC_TYPE, ASSOC_CONST,
  MACRO, OTHER
}.
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl def(D: Def) extern.
.mode def(-Def).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.decl field(Owner: Def, Name: string, Ty: TypeRef) extern.
.mode field(+Def, -string, -TypeRef).
.decl ty_app(TR: TypeRef, Head: Def) extern.
.mode ty_app(+TypeRef, -Def).
.decl ty_prim(TR: TypeRef, Name: string) extern.
.mode ty_prim(+TypeRef, -string).
.decl variant(Enum: Def, Name: string, VariantDef: Def) extern.
.mode variant(+Def, -string, -Def).
.decl method(Owner: Def, Method: Def) extern.
.mode method(+Def, -Def).
.decl trait_method(Owner: Def, Method: Def) extern.
.mode trait_method(+Def, -Def).
.decl implements(Type: Def, Trait: Def, ImplDef: Def) extern.
.mode implements(+Def, -Def, -Def).
.decl from_impl(Src: Def, Dst: Def, ImplDef: Def) extern.
.mode from_impl(+Def, -Def, -Def).
.decl field_hit(Name: string).
.decl field_type_hit(Name: string, TyName: string).
.decl variant_hit(Name: string).
.decl method_hit(Name: string).
.decl trait_method_hit(Name: string).
.decl impl_hit(Name: string).
.decl from_hit(SrcName: string, DstName: string).
field_hit(Name) :- def(Person), def_name(Person, "Person"), field(Person, Name, _).
field_type_hit(Name, TyName) :-
  def(Person),
  def_name(Person, "Person"),
  field(Person, Name, Ty),
  ty_app(Ty, Head),
  def_name(Head, TyName).
field_type_hit(Name, TyName) :-
  def(Person),
  def_name(Person, "Person"),
  field(Person, Name, Ty),
  ty_prim(Ty, TyName).
variant_hit(Name) :- def(Choice), def_name(Choice, "Choice"), variant(Choice, Name, _).
method_hit(Name) :- def(Person), def_name(Person, "Person"), method(Person, Method), def_name(Method, Name).
trait_method_hit(Name) :- def(Greeter), def_name(Greeter, "Greeter"), trait_method(Greeter, Method), def_name(Method, Name).
impl_hit(Name) :- def(Person), def_name(Person, "Person"), implements(Person, Trait, _), def_name(Trait, Name).
from_hit(SrcName, DstName) :- def(Src), def_name(Src, "NameError"), from_impl(Src, Dst, _), def_name(Src, SrcName), def_name(Dst, DstName).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("field_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("name".to_string())])
    }));
    assert!(result.relations.get("field_type_hit").is_some_and(|rows| {
        rows.contains(&vec![
            RuntimeValue::String("name".to_string()),
            RuntimeValue::String("String".to_string()),
        ])
    }));
    assert!(result.relations.get("field_type_hit").is_some_and(|rows| {
        rows.contains(&vec![
            RuntimeValue::String("age".to_string()),
            RuntimeValue::String("u32".to_string()),
        ])
    }));
    assert!(result.relations.get("variant_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("First".to_string())])
    }));
    assert!(result.relations.get("method_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("greet".to_string())])
    }));
    assert!(result.relations.get("trait_method_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("greet".to_string())])
    }));
    assert!(result.relations.get("impl_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("Greeter".to_string())])
    }));
    assert!(result.relations.get("from_hit").is_some_and(|rows| {
        rows.contains(&vec![
            RuntimeValue::String("NameError".to_string()),
            RuntimeValue::String("AgeError".to_string()),
        ])
    }));
}

#[test]
fn workspace_service_supports_type_surface_rows() {
    let root = temp_workspace_root("type_surface_rows");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub struct Wrapper<T>(pub T);
pub struct Item;
pub struct Oops;

pub fn make_wrapper(item: Item) -> Wrapper<Item> { Wrapper(item) }
pub fn borrow_item(item: &mut Item) -> &mut Item { item }
pub fn raw_item(item: *const Item) -> *const Item { item }
pub fn tuple_item() -> (Item, i32) { (Item, 1) }
pub fn slice_item(items: &[Item]) -> &[Item] { items }
pub fn generic_item<T>(value: T) -> T { value }
pub fn parse_item() -> Result<Item, Oops> { Err(Oops) }
pub fn opaque_array() -> [i32; 4] { [0; 4] }
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.type Mutability = { IMM, MUT }.
.decl def(D: Def) extern.
.mode def(-Def).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.func def_path(D: Def, Path: string) extern.
.mode def_path(+Def, -string).
.func fn_error_type(F: Def, Err: option<Def>) extern.
.mode fn_error_type(+Def, -option<Def>).
.func fn_return_type(F: Def, TR: TypeRef) extern.
.mode fn_return_type(+Def, -TypeRef).
.decl ty_app(TR: TypeRef, Head: Def) extern.
.mode ty_app(+TypeRef, -Def).
.decl ty_arg(TR: TypeRef, Index: int, Arg: TypeRef) extern.
.mode ty_arg(+TypeRef, -int, -TypeRef).
.decl ty_ref(TR: TypeRef, Mut: Mutability, Inner: TypeRef) extern.
.mode ty_ref(+TypeRef, -Mutability, -TypeRef).
.decl ty_ptr(TR: TypeRef, Mut: Mutability, Inner: TypeRef) extern.
.mode ty_ptr(+TypeRef, -Mutability, -TypeRef).
.decl ty_tuple(TR: TypeRef, Index: int, Elem: TypeRef) extern.
.mode ty_tuple(+TypeRef, -int, -TypeRef).
.decl ty_slice(TR: TypeRef, Elem: TypeRef) extern.
.mode ty_slice(+TypeRef, -TypeRef).
.decl ty_param(TR: TypeRef, Param: Def) extern.
.mode ty_param(+TypeRef, -Def).
.decl ty_prim(TR: TypeRef, Name: string) extern.
.mode ty_prim(+TypeRef, -string).
.decl ty_unknown(TR: TypeRef) extern.
.mode ty_unknown(+TypeRef).
.func typeref_id(TR: TypeRef, H: string) extern.
.mode typeref_id(+TypeRef, -string).
.decl app_hit(Path: string).
.decl arg_hit(Path: string).
.decl ref_hit(Name: string).
.decl ptr_hit(Name: string).
.decl tuple_prim_hit(Name: string).
.decl slice_hit(Name: string).
.decl param_hit(Name: string).
.decl error_hit(Name: string).
.decl unknown_hit(Key: string).
.decl return_key_hit(Key: string).
app_hit(Path) :- def(F), def_name(F, "make_wrapper"), fn_return_type(F, TR), ty_app(TR, Head), def_path(Head, Path).
arg_hit(Path) :- def(F), def_name(F, "make_wrapper"), fn_return_type(F, TR), ty_arg(TR, 0, Arg), ty_app(Arg, Head), def_path(Head, Path).
ref_hit(Name) :- def(F), def_name(F, "borrow_item"), fn_return_type(F, TR), ty_ref(TR, Mutability::MUT, Inner), ty_app(Inner, Head), def_name(Head, Name).
ptr_hit(Name) :- def(F), def_name(F, "raw_item"), fn_return_type(F, TR), ty_ptr(TR, Mutability::IMM, Inner), ty_app(Inner, Head), def_name(Head, Name).
tuple_prim_hit(Name) :- def(F), def_name(F, "tuple_item"), fn_return_type(F, TR), ty_tuple(TR, 1, Elem), ty_prim(Elem, Name).
slice_hit(Name) :- def(F), def_name(F, "slice_item"), fn_return_type(F, TR), ty_ref(TR, Mutability::IMM, RefInner), ty_slice(RefInner, Elem), ty_app(Elem, Head), def_name(Head, Name).
param_hit(Name) :- def(F), def_name(F, "generic_item"), fn_return_type(F, TR), ty_param(TR, Param), def_name(Param, Name).
error_hit(Name) :- def(F), def_name(F, "parse_item"), fn_error_type(F, some(Err)), def_name(Err, Name).
unknown_hit(Key) :- def(F), def_name(F, "opaque_array"), fn_return_type(F, TR), ty_unknown(TR), typeref_id(TR, Key).
return_key_hit(Key) :- def(F), def_name(F, "make_wrapper"), fn_return_type(F, TR), typeref_id(TR, Key).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("app_hit").is_some_and(|rows| {
        rows.iter().any(|row| row.first().is_some_and(|value| {
            matches!(value, RuntimeValue::String(path) if path == "Wrapper" || path.ends_with("::Wrapper"))
        }))
    }));
    assert!(result.relations.get("arg_hit").is_some_and(|rows| {
        rows.iter().any(|row| row.first().is_some_and(|value| {
            matches!(value, RuntimeValue::String(path) if path == "Item" || path.ends_with("::Item"))
        }))
    }));
    assert!(result.relations.get("ref_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("Item".to_string())])
    }));
    assert!(result.relations.get("ptr_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("Item".to_string())])
    }));
    assert!(result.relations.get("tuple_prim_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("i32".to_string())])
    }));
    assert!(result.relations.get("slice_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("Item".to_string())])
    }));
    assert!(result.relations.get("param_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("T".to_string())])
    }));
    assert!(result.relations.get("error_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("Oops".to_string())])
    }));
    assert!(result.relations.get("unknown_hit").is_some_and(|rows| !rows.is_empty()));
    assert!(result.relations.get("return_key_hit").is_some_and(|rows| !rows.is_empty()));
}

#[test]
fn workspace_service_supports_call_graph_rows() {
    let root = temp_workspace_root("call_graph_rows");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn direct_target() {}
pub fn direct_caller() { direct_target(); }

pub trait Greeter {
    fn greet(&self);
}

pub struct Person;

impl Greeter for Person {
    fn greet(&self) {}
}

pub fn trait_caller(person: Person) { person.greet(); }
pub fn dyn_caller(greeter: &dyn Greeter) { greeter.greet(); }
pub fn closure_caller() { let closure = || direct_target(); closure(); }
pub fn fn_pointer_caller(fp: fn()) { fp(); }
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl def(D: Def) extern.
.mode def(-Def).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.mode call_edge(+Def, -Def, -Span, -DispatchKind).
.func dispatch_str(Dispatch: DispatchKind, Label: string) extern.
.mode dispatch_str(+DispatchKind, -string).
.decl direct_hit(Target: string, Label: string).
.decl trait_hit(Label: string).
.decl dyn_hit(Label: string).
.decl closure_hit(Label: string).
.decl fn_pointer_hit(Label: string).
direct_hit(Target, Label) :- def(Caller), def_name(Caller, "direct_caller"), call_edge(Caller, Callee, _, Dispatch), def_name(Callee, Target), dispatch_str(Dispatch, Label).
trait_hit(Label) :- def(Caller), def_name(Caller, "trait_caller"), call_edge(Caller, _, _, Dispatch), dispatch_str(Dispatch, Label).
dyn_hit(Label) :- def(Caller), def_name(Caller, "dyn_caller"), call_edge(Caller, _, _, Dispatch), dispatch_str(Dispatch, Label).
closure_hit(Label) :- def(Caller), def_name(Caller, "closure_caller"), call_edge(Caller, _, _, Dispatch), dispatch_str(Dispatch, Label).
fn_pointer_hit(Label) :- def(Caller), def_name(Caller, "fn_pointer_caller"), call_edge(Caller, _, _, Dispatch), dispatch_str(Dispatch, Label).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("direct_hit").is_some_and(|rows| {
        rows.contains(&vec![
            RuntimeValue::String("direct_target".to_string()),
            RuntimeValue::String("direct".to_string()),
        ])
    }));
    assert!(result.relations.get("trait_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("through_trait".to_string())])
    }));
    assert!(result.relations.get("dyn_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("dyn".to_string())])
    }));
    assert!(result.relations.get("closure_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("closure".to_string())])
    }));
    assert!(result.relations.get("fn_pointer_hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("fn_pointer".to_string())])
    }));
}

#[test]
fn workspace_service_call_graph_ignores_nested_item_bodies() {
    let root = temp_workspace_root("call_graph_nested_items");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn target() {}

pub fn outer() {
    fn inner() { target(); }
    inner();
}
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl def(D: Def) extern.
.mode def(-Def).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.mode call_edge(+Def, -Def, -Span, -DispatchKind).
.decl outer_hit().
outer_hit() :- def(Caller), def_name(Caller, "outer"), call_edge(Caller, Callee, _, _), def_name(Callee, "target").
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(
        result.relations.get("outer_hit").is_none_or(|rows| rows.is_empty()),
        "outer should not inherit call edges from nested item bodies; rows={:?}",
        result.relations.get("outer_hit")
    );
}

#[test]
fn workspace_service_supports_syntax_control_rows() {
    let root = temp_workspace_root("syntax_control_rows");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn direct_target() {}

pub fn syntax_demo(flag: bool) {
    if flag {
        direct_target();
    }
}
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.type NodeKind = { IF, MATCH, WHILE, FOR, LOOP, BLOCK, TRY, ARM, OTHER }.
.decl def(D: Def) extern.
.mode def(-Def).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.mode call_edge(+Def, -Def, -Span, -DispatchKind).
.func node_at(S: Span, N: option<Node>) extern.
.mode node_at(+Span, -option<Node>).
.func node_kind(N: Node, K: NodeKind) extern.
.mode node_kind(+Node, -NodeKind).
.func node_span(N: Node, S: Span) extern.
.mode node_span(+Node, -Span).
.func node_parent(N: Node, P: option<Node>) extern.
.mode node_parent(+Node, -option<Node>).
.decl enclosing_control(S: Span, K: NodeKind, ControlS: Span, Dist: int) extern.
.mode enclosing_control(+Span, -NodeKind, -Span, -int).
.func node_id(N: Node, H: string) extern.
.mode node_id(+Node, -string).
.decl node_hit().
.decl if_control_hit().
node_hit() :-
  def(F),
  def_name(F, "syntax_demo"),
  call_edge(F, _, Site, _),
  node_at(Site, some(Node)),
  node_kind(Node, NodeKind::OTHER),
  node_span(Node, _),
  node_parent(Node, some(_)),
  node_id(Node, _).
if_control_hit() :-
  def(F),
  def_name(F, "syntax_demo"),
  call_edge(F, _, Site, _),
  enclosing_control(Site, NodeKind::IF, _, _).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("node_hit").is_some_and(|rows| !rows.is_empty()));
    assert!(result
        .relations
        .get("if_control_hit")
        .is_some_and(|rows| !rows.is_empty()));
}

#[test]
fn workspace_service_rejects_approximate_reference_queries() {
    let root = temp_workspace_root("reference_event_rows");
    fs::write(
        root.join("src/lib.rs"),
        r#"
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Counter(pub i32);

pub fn event_demo(left: Counter, right: Counter) {
    let mut slot = left;
    if left == right {
        slot = right;
    }
    let _ = slot;
}
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.mode def(-Def).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.decl compares(Type: Def, Site: Span, Op: string, Fn: Def) extern.
.mode compares(+Def, -Span, -string, -Def).
.decl writes(Subject: Def, Site: Span, Fn: Def) extern.
.mode writes(+Def, -Span, -Def).
.decl ref_id(R: Ref, H: string) extern.
.mode ref_id(-Ref, -string).
.decl compare_hit(Op: string).
.decl write_hit().
.decl ref_key(H: string).
compare_hit(Op) :-
  def(T),
  def_name(T, "Counter"),
  compares(T, _, Op, F),
  def_name(F, "event_demo").
write_hit() :-
  def(T),
  def_name(T, "Counter"),
  writes(T, _, F),
  def_name(F, "event_demo").
ref_key(H) :- ref_id(_, H).
"#,
    );
    let err = service
        .run_planned(&planned)
        .expect_err("approximate reference capabilities should be rejected");
    let message = err.to_string();
    assert!(message.contains("compares"), "error={message}");
    assert!(message.contains("writes"), "error={message}");
    assert!(message.contains("ref_id"), "error={message}");
}

#[test]
fn workspace_service_rejects_error_flow_queries() {
    let root = temp_workspace_root("error_flow_rejected");
    fs::write(
        root.join("src/lib.rs"),
        r#"
#[derive(Debug)]
pub enum ParseErr {
    Bad(String),
    Empty,
}

#[derive(Debug)]
pub enum AppErr {
    Parse(ParseErr),
}

impl From<ParseErr> for AppErr {
    fn from(err: ParseErr) -> Self {
        AppErr::Parse(err)
    }
}

pub fn parse(flag: bool) -> Result<(), ParseErr> {
    if flag {
        Err(ParseErr::Bad("bad".to_string()))
    } else {
        Err(ParseErr::Empty)
    }
}

pub fn propagate_parse(flag: bool) -> Result<(), AppErr> {
    parse(flag)?;
    Ok(())
}

pub fn handle_parse(flag: bool) -> Result<(), AppErr> {
    match parse(flag) {
        Err(ParseErr::Bad(_)) => Ok(()),
        Err(_) => Ok(()),
        Ok(()) => Ok(()),
    }
}
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.mode def(-Def).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.decl constructs(ErrType: Def, Variant: string, Site: Span, Fn: Def) extern.
.mode constructs(+Def, -string, -Span, -Def).
.decl propagates(ErrType: Def, Site: Span, Fn: Def) extern.
.mode propagates(+Def, -Span, -Def).
.decl converts(SrcErr: Def, DstErr: Def, Site: Span, Fn: Def) extern.
.mode converts(+Def, +Def, -Span, -Def).
.decl handles(ErrType: Def, Variant: option<string>, Site: Span, Fn: Def) extern.
.mode handles(+Def, -option<string>, -Span, -Def).
.decl construct_hit(Variant: string).
.decl propagate_hit().
.decl convert_hit().
.decl handle_hit(Variant: string).
construct_hit(Variant) :-
  def(E),
  def_name(E, "ParseErr"),
  constructs(E, Variant, _, F),
  def_name(F, "parse").
propagate_hit() :-
  def(E),
  def_name(E, "AppErr"),
  propagates(E, _, F),
  def_name(F, "propagate_parse").
convert_hit() :-
  def(Src),
  def_name(Src, "ParseErr"),
  def(Dst),
  def_name(Dst, "AppErr"),
  converts(Src, Dst, _, F),
  def_name(F, "propagate_parse").
handle_hit(Variant) :-
  def(E),
  def_name(E, "ParseErr"),
  handles(E, some(Variant), _, F),
  def_name(F, "handle_parse").
"#,
    );
    let err = service
        .run_planned(&planned)
        .expect_err("approximate error-flow queries should be rejected until RA-native");
    let message = err.to_string();
    assert!(message.contains("constructs"), "error={message}");
    assert!(message.contains("propagates"), "error={message}");
    assert!(message.contains("converts"), "error={message}");
    assert!(message.contains("handles"), "error={message}");
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
    let canonical_root = fs::canonicalize(root.as_std_path()).expect("canonical workspace root");
    assert_eq!(
        raql_host_ra::daemon::resolve_workspace_root(member_a.join("Cargo.toml").as_std_path())
            .expect("resolved workspace root"),
        canonical_root,
    );
    assert_eq!(service.workspace_root(), canonical_root.as_path());
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.func def_span(D: Def, S: Span) extern.
.func span_key(S: Span, RelPath: string, L0: int, C0: int, L1: int, C1: int) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl hit(Name: string).
.decl beta_span(RelPath: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "beta").
beta_span(RelPath) :-
  def(D),
  def_name(D, Name),
  contains(Name, "beta"),
  def_span(D, S),
  span_key(S, RelPath, _L0, _C0, _L1, _C1).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(result.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("beta".to_string())])
    }));
    assert!(result.relations.get("beta_span").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("member_b/src/lib.rs".to_string())])
    }));
}

#[test]
fn workspace_service_from_member_manifest_detects_new_sibling_member_files() {
    let root = temp_workspace_root("member_manifest_new_file");
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
    let initial_epoch = service.workspace_epoch();
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl hit(Name: string).
hit(Name) :- def(D), def_name(D, Name), contains(Name, "gamma").
"#,
    );
    let before = service.run_planned(&planned).expect("run before new file");
    assert!(
        before.relations.get("hit").is_none_or(|rows| rows.is_empty()),
        "gamma should not exist before the new sibling file lands; rows={:?}",
        before.relations.get("hit")
    );

    fs::write(member_b.join("src/lib.rs"), "pub mod extra;\npub fn beta() {}\n")
        .expect("rewrite member_b lib.rs");
    fs::write(member_b.join("src/extra.rs"), "pub fn gamma() {}\n").expect("write member_b extra.rs");

    let after = service.run_planned(&planned).expect("run after new file");
    assert!(after.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("gamma".to_string())])
    }));
    assert!(
        service.workspace_epoch() > initial_epoch,
        "new sibling-member files should trigger a workspace reload"
    );
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
fn workspace_service_includes_registry_like_path_dependency_defs() {
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
            && observed.contains(&vec![RuntimeValue::String("helper".to_string())]),
        "path dependencies loaded by rust-analyzer should stay visible even when their path looks registry-like; observed={observed:?}"
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
fn workspace_service_refreshes_generated_build_symbols_when_build_inputs_change() {
    let root = temp_workspace_root("generated_refresh");
    fs::write(
        root.join("src/lib.rs"),
        r#"
include!(concat!(env!("OUT_DIR"), "/generated.rs"));

pub fn caller() {
    generated_alpha();
}
"#,
    )
    .expect("write lib.rs");
    fs::write(root.join("schema.txt"), "generated_alpha\n").expect("write schema");
    fs::write(
        root.join("build.rs"),
        r#"
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=schema.txt");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let symbol = fs::read_to_string("schema.txt").expect("read schema");
    let symbol = symbol.trim();
    fs::write(out.join("generated.rs"), format!("pub fn {symbol}() {{}}\n")).expect("write generated");
}
"#,
    )
    .expect("write build.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let initial_epoch = service.workspace_epoch();
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl generated(Name: string).
generated(Name) :- def(D), def_name(D, Name), contains(Name, "generated_").
"#,
    );
    let first = service.run_planned(&planned).expect("first run");
    assert!(first.relations.get("generated").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("generated_alpha".to_string())])
    }));

    fs::write(root.join("schema.txt"), "generated_omega\n").expect("rewrite schema");

    let second = service.run_planned(&planned).expect("second run");
    let observed = second
        .relations
        .get("generated")
        .cloned()
        .unwrap_or_default();
    assert!(
        observed.contains(&vec![RuntimeValue::String("generated_omega".to_string())]),
        "expected refreshed generated symbol after build input change; observed={observed:?}"
    );
    assert!(
        !observed.contains(&vec![RuntimeValue::String("generated_alpha".to_string())]),
        "stale generated symbol should disappear after build input change; observed={observed:?}"
    );
    assert!(
        service.workspace_epoch() > initial_epoch,
        "build input changes should force a workspace reload"
    );
}

#[test]
fn workspace_service_refreshes_generated_build_symbols_when_build_rs_changes() {
    let root = temp_workspace_root("generated_refresh_build_rs");
    fs::write(
        root.join("src/lib.rs"),
        r#"
include!(concat!(env!("OUT_DIR"), "/generated.rs"));
"#,
    )
    .expect("write lib.rs");
    fs::write(
        root.join("build.rs"),
        r#"
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out.join("generated.rs"), "pub fn generated_alpha() {}\n").expect("write generated");
}
"#,
    )
    .expect("write build.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let initial_epoch = service.workspace_epoch();
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl generated(Name: string).
generated(Name) :- def(D), def_name(D, Name), contains(Name, "generated_").
"#,
    );
    let first = service.run_planned(&planned).expect("first run");
    assert!(first.relations.get("generated").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("generated_alpha".to_string())])
    }));

    fs::write(
        root.join("build.rs"),
        r#"
use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out.join("generated.rs"), "pub fn generated_omega() {}\n").expect("write generated");
}
"#,
    )
    .expect("rewrite build.rs");

    let second = service.run_planned(&planned).expect("second run");
    let observed = second
        .relations
        .get("generated")
        .cloned()
        .unwrap_or_default();
    assert!(
        observed.contains(&vec![RuntimeValue::String("generated_omega".to_string())]),
        "expected generated symbol refresh after build.rs edit; observed={observed:?}"
    );
    assert!(
        !observed.contains(&vec![RuntimeValue::String("generated_alpha".to_string())]),
        "stale generated symbol should disappear after build.rs edit; observed={observed:?}"
    );
    assert!(
        service.workspace_epoch() > initial_epoch,
        "build.rs edits should force a workspace reload"
    );
}

#[test]
fn workspace_service_refreshes_generated_build_symbols_when_tracked_rust_inputs_change() {
    let root = temp_workspace_root("generated_refresh_tracked_rust");
    fs::write(
        root.join("src/lib.rs"),
        r#"
include!(concat!(env!("OUT_DIR"), "/generated.rs"));
"#,
    )
    .expect("write lib.rs");
    fs::write(root.join("src/input.rs"), "generated_alpha\n").expect("write input");
    fs::write(
        root.join("build.rs"),
        r#"
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src/input.rs");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let symbol = fs::read_to_string("src/input.rs").expect("read input");
    let symbol = symbol.trim();
    fs::write(out.join("generated.rs"), format!("pub fn {symbol}() {{}}\n")).expect("write generated");
}
"#,
    )
    .expect("write build.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let initial_epoch = service.workspace_epoch();
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl contains(Haystack: string, Needle: string) extern.
.decl generated(Name: string).
generated(Name) :- def(D), def_name(D, Name), contains(Name, "generated_").
"#,
    );
    let first = service.run_planned(&planned).expect("first run");
    assert!(first.relations.get("generated").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("generated_alpha".to_string())])
    }));

    fs::write(root.join("src/input.rs"), "generated_omega\n").expect("rewrite input");

    let second = service.run_planned(&planned).expect("second run");
    let observed = second
        .relations
        .get("generated")
        .cloned()
        .unwrap_or_default();
    assert!(
        observed.contains(&vec![RuntimeValue::String("generated_omega".to_string())]),
        "expected generated symbol refresh after tracked Rust input edit; observed={observed:?}"
    );
    assert!(
        !observed.contains(&vec![RuntimeValue::String("generated_alpha".to_string())]),
        "stale generated symbol should disappear after tracked Rust input edit; observed={observed:?}"
    );
    assert!(
        service.workspace_epoch() > initial_epoch,
        "tracked Rust rerun inputs should force a workspace reload"
    );
}

#[test]
fn workspace_service_ignores_unrelated_files_for_explicit_build_script_inputs() {
    let root = temp_workspace_root("generated_explicit_inputs");
    fs::write(
        root.join("src/lib.rs"),
        r#"
include!(concat!(env!("OUT_DIR"), "/generated.rs"));
"#,
    )
    .expect("write lib.rs");
    fs::write(root.join("schema.txt"), "generated_alpha\n").expect("write schema");
    fs::write(
        root.join("build.rs"),
        r#"
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=schema.txt");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let symbol = fs::read_to_string("schema.txt").expect("read schema");
    let symbol = symbol.trim();
    fs::write(out.join("generated.rs"), format!("pub fn {symbol}() {{}}\n")).expect("write generated");
}
"#,
    )
    .expect("write build.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let initial_epoch = service.workspace_epoch();
    let before = query_world_stamp(&mut service);

    fs::write(root.join("README.md"), "notes\n").expect("write unrelated file");

    let after = query_world_stamp(&mut service);
    assert_eq!(
        before, after,
        "unrelated files should not invalidate a build script with explicit rerun-if-changed inputs"
    );
    assert_eq!(
        service.workspace_epoch(),
        initial_epoch,
        "unrelated files should not force a workspace reload when build inputs are explicit"
    );
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
        "expected a supported def_path row for alpha; status={:?} notes={:?} def={:?} def_name={:?} def_path={:?} observed={observed:?}",
        result.status,
        result.notes,
        result.relations.get("def"),
        result.relations.get("def_name"),
        result.relations.get("def_path"),
    );
}

#[test]
fn workspace_service_reports_supported_def_paths_for_exact_name_seeded_structs() {
    let root = temp_workspace_root("def_path_struct_exact_seed");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub struct Person {
    pub name: String,
}
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.func def_name(D: Def, Name: string) extern.
.func def_path(D: Def, Path: string) extern.
.decl hit(Path: string).
hit(Path) :- def_name(D, "Person"), def_path(D, Path).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    let observed = result.relations.get("hit").cloned().unwrap_or_default();
    assert!(
        observed.iter().any(|row| {
            row.first().is_some_and(|value| match value {
                RuntimeValue::String(path) => path.ends_with("Person"),
                _ => false,
            })
        }),
        "expected a supported exact-name-seeded def_path row for Person; status={:?} notes={:?} def_name={:?} def_path={:?} observed={observed:?}",
        result.status,
        result.notes,
        result.relations.get("def_name"),
        result.relations.get("def_path"),
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
fn workspace_service_supports_unbound_public_def_lookup_rows() {
    let root = temp_workspace_root("visibility_lookup_only");
    fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\nfn beta() {}\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl is_public(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl hit(Name: string).
hit(Name) :- is_public(D), def_name(D, Name).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(matches!(result.status, EvalStatus::Ok), "{result:?}");
    assert!(result.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("alpha".to_string())])
    }));
    assert!(
        !result.relations.get("hit").is_some_and(|rows| {
            rows.contains(&vec![RuntimeValue::String("beta".to_string())])
        }),
        "unbound visibility lookup should enumerate only public defs"
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

#[test]
fn workspace_service_supports_lookup_seeded_def_spans_and_span_keys() {
    let root = temp_workspace_root("lookup_seeded_def_span");
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }\n").expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.type DefKind = { FN }.
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.func def_kind(D: Def, K: DefKind) extern.
.func def_span(D: Def, S: Span) extern.
.decl span_allowed(S: Span) extern.
.func span_key(S: Span, RelPath: string, L0: int, C0: int, L1: int, C1: int) extern.
.decl hit(RelPath: string).
hit(RelPath) :-
  def_name(D, "answer"),
  def(D),
  def_kind(D, DefKind::FN),
  def_span(D, S),
  span_allowed(S),
  span_key(S, RelPath, _L0, _C0, _L1, _C1).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert!(matches!(result.status, EvalStatus::Ok), "{result:?}");
    assert!(result.relations.get("hit").is_some_and(|rows| {
        rows.contains(&vec![RuntimeValue::String("src/lib.rs".to_string())])
    }));
}

#[test]
fn workspace_service_ignores_unloaded_root_subtree_changes() {
    let root = temp_workspace_root("ignore_unloaded_subtree");
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }\n").expect("write lib.rs");
    let stray_dir = root.join("tmp").join("scratch");
    fs::create_dir_all(&stray_dir).expect("create stray dir");
    let stray = stray_dir.join("ghost.rs");
    fs::write(&stray, "pub fn ghost() {}\n").expect("write stray file");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let before = query_world_stamp(&mut service);

    std::thread::sleep(Duration::from_millis(20));
    fs::write(&stray, "pub fn ghost() { let _ = 1; }\n").expect("rewrite stray file");

    let after = query_world_stamp(&mut service);
    assert_eq!(
        before, after,
        "editing an unloaded subtree should not invalidate the live workspace"
    );
}

#[test]
fn workspace_service_looks_up_call_edges_for_bound_callee() {
    let root = temp_workspace_root("lookup_seeded_callers");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn direct_target() {}
pub fn direct_caller() { direct_target(); }
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let target_rows = service
        .extern_lookup_rows(&ExternLookupRequest::new(
            "def_name",
            ExternLookupShape::FunctionExactBindings,
            2,
            [1],
            vec![ExternLookupValue::String("direct_target".into())],
        ))
        .expect("lookup def_name")
        .expect("def_name rows");
    let target = target_rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| match value {
            ExternLookupValue::Host(host) if host.kind() == ExternLookupHostValueKind::Def => {
                Some(DefId::new(host.stable_id()))
            }
            _ => None,
        })
        .expect("target def");

    let call_rows = service
        .extern_lookup_rows(&ExternLookupRequest::new(
            "call_edge",
            ExternLookupShape::RelationExactBindings,
            4,
            [1],
            vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                target.stable_id(),
            ))],
        ))
        .expect("lookup call_edge")
        .expect("call_edge rows");

    let caller = call_rows
        .iter()
        .find_map(|row| match (row.first(), row.get(3)) {
            (
                Some(ExternLookupValue::Host(host)),
                Some(ExternLookupValue::Enum { name, variant }),
            ) if host.kind() == ExternLookupHostValueKind::Def
                && name.as_ref() == "DispatchKind"
                && variant.as_ref() == "DIRECT" =>
            {
                Some(DefId::new(host.stable_id()))
            }
            _ => None,
        })
        .expect("direct caller row");

    let caller_name_rows = service
        .extern_lookup_rows(&ExternLookupRequest::new(
            "def_name",
            ExternLookupShape::FunctionExactBindings,
            2,
            [0],
            vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                caller.stable_id(),
            ))],
        ))
        .expect("lookup caller name")
        .expect("caller name rows");

    assert!(caller_name_rows.iter().any(|row| {
        row.get(1)
            .is_some_and(|value| matches!(value, ExternLookupValue::String(name) if name.as_ref() == "direct_caller"))
    }));
}

#[test]
fn workspace_service_looks_up_call_edges_for_bound_caller() {
    let root = temp_workspace_root("lookup_seeded_callees");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn direct_target() {}
pub fn direct_caller() { direct_target(); }
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let caller_rows = service
        .extern_lookup_rows(&ExternLookupRequest::new(
            "def_name",
            ExternLookupShape::FunctionExactBindings,
            2,
            [1],
            vec![ExternLookupValue::String("direct_caller".into())],
        ))
        .expect("lookup caller")
        .expect("caller rows");
    let caller = caller_rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| match value {
            ExternLookupValue::Host(host) if host.kind() == ExternLookupHostValueKind::Def => {
                Some(DefId::new(host.stable_id()))
            }
            _ => None,
        })
        .expect("caller def");

    let call_rows = service
        .extern_lookup_rows(&ExternLookupRequest::new(
            "call_edge",
            ExternLookupShape::RelationExactBindings,
            4,
            [0],
            vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                caller.stable_id(),
            ))],
        ))
        .expect("lookup call_edge")
        .expect("call_edge rows");

    let callee = call_rows
        .iter()
        .find_map(|row| match (row.get(1), row.get(3)) {
            (
                Some(ExternLookupValue::Host(host)),
                Some(ExternLookupValue::Enum { name, variant }),
            ) if host.kind() == ExternLookupHostValueKind::Def
                && name.as_ref() == "DispatchKind"
                && variant.as_ref() == "DIRECT" =>
            {
                Some(DefId::new(host.stable_id()))
            }
            _ => None,
        })
        .expect("callee def");

    let callee_name_rows = service
        .extern_lookup_rows(&ExternLookupRequest::new(
            "def_name",
            ExternLookupShape::FunctionExactBindings,
            2,
            [0],
            vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                callee.stable_id(),
            ))],
        ))
        .expect("lookup callee name")
        .expect("callee name rows");

    assert!(callee_name_rows.iter().any(|row| {
        row.get(1)
            .is_some_and(|value| matches!(value, ExternLookupValue::String(name) if name.as_ref() == "direct_target"))
    }));
}

#[test]
fn workspace_service_looks_up_call_edges_for_bound_callee_through_alias_reference() {
    let root = temp_workspace_root("lookup_seeded_callers_alias");
    fs::write(
        root.join("src/lib.rs"),
        r#"
mod callee {
    pub fn direct_target() {}
}

use callee::direct_target as alias_target;

pub fn direct_caller() { alias_target(); }
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let target_rows = service
        .extern_lookup_rows(&ExternLookupRequest::new(
            "def_name",
            ExternLookupShape::FunctionExactBindings,
            2,
            [1],
            vec![ExternLookupValue::String("direct_target".into())],
        ))
        .expect("lookup def_name")
        .expect("def_name rows");
    let target = target_rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| match value {
            ExternLookupValue::Host(host) if host.kind() == ExternLookupHostValueKind::Def => {
                Some(DefId::new(host.stable_id()))
            }
            _ => None,
        })
        .expect("target def");

    let call_rows = service
        .extern_lookup_rows(&ExternLookupRequest::new(
            "call_edge",
            ExternLookupShape::RelationExactBindings,
            4,
            [1],
            vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                target.stable_id(),
            ))],
        ))
        .expect("lookup call_edge")
        .expect("call_edge rows");

    let caller = call_rows
        .iter()
        .find_map(|row| match (row.first(), row.get(3)) {
            (
                Some(ExternLookupValue::Host(host)),
                Some(ExternLookupValue::Enum { name, variant }),
            ) if host.kind() == ExternLookupHostValueKind::Def
                && name.as_ref() == "DispatchKind"
                && variant.as_ref() == "DIRECT" =>
            {
                Some(DefId::new(host.stable_id()))
            }
            _ => None,
        })
        .expect("direct caller row");

    let caller_name_rows = service
        .extern_lookup_rows(&ExternLookupRequest::new(
            "def_name",
            ExternLookupShape::FunctionExactBindings,
            2,
            [0],
            vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                caller.stable_id(),
            ))],
        ))
        .expect("lookup caller name")
        .expect("caller name rows");

    assert!(caller_name_rows.iter().any(|row| {
        row.get(1)
            .is_some_and(|value| matches!(value, ExternLookupValue::String(name) if name.as_ref() == "direct_caller"))
    }));
}

#[test]
fn workspace_service_semantic_prewarm_runs_without_error() {
    let root = temp_workspace_root("semantic_prewarm");
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn direct_target() {}
pub fn direct_caller() { direct_target(); }
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    service.prewarm_semantics().expect("prewarm semantics");

    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl hit(Name: string) output.
hit(Name) :- def_name(D, "direct_target"), def_name(D, Name).
"#,
    );
    let result = service.run_planned(&planned).expect("run query after prewarm");
    assert_eq!(result.status, raql_engine::EvalStatus::Ok);
}

#[test]
fn workspace_service_supports_stable_handle_rows() {
    let root = temp_workspace_root("stable_handle_rows");
    fs::write(
        root.join("src/lib.rs"),
        r#"
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Person(pub i32);

pub trait Greeter {
    fn greet(&self);
}

impl Greeter for Person {
    fn greet(&self) {}
}

pub fn invoke(left: Person, right: Person) {
    let mut slot = left;
    slot.greet();
    if slot == right {
        slot = right;
    }
}
"#,
    )
    .expect("write lib.rs");

    let mut service = WorkspaceService::from_workspace_root(root.as_std_path()).expect("service");
    let planned = plan_query(
        r#"
.decl call_id(C: Call, H: string) extern.
.mode call_id(-Call, -string).
.decl impl_id(I: Impl, H: string) extern.
.mode impl_id(-Impl, -string).
.decl call_key(H: string).
.decl impl_key(H: string).
call_key(H) :- call_id(_, H).
impl_key(H) :- impl_id(_, H).
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    let call_keys = result.relations.get("call_key").cloned().unwrap_or_default();
    let impl_keys = result.relations.get("impl_key").cloned().unwrap_or_default();

    assert!(
        call_keys.iter().any(|row| {
            row.first().is_some_and(|value| match value {
                RuntimeValue::String(handle) => handle.starts_with("call:src/lib.rs:"),
                _ => false,
            })
        }),
        "expected at least one call handle from supported call extraction; observed={call_keys:?}"
    );
    assert!(
        impl_keys.iter().any(|row| {
            row.first().is_some_and(|value| match value {
                RuntimeValue::String(handle) => handle.starts_with("impl:src/lib.rs:"),
                _ => false,
            })
        }),
        "expected at least one impl handle from supported impl extraction; observed={impl_keys:?}"
    );
}

#[test]
#[ignore = "manual repo-scale timing probe"]
fn workspace_service_repo_defs_probe_smoke() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");
    let manifest = repo_root.join("Cargo.toml");
    let mut service =
        WorkspaceService::from_manifest_path(manifest.as_path()).expect("service from repo manifest");
    let planned = plan_query(
        r#"
.decl def(D: Def) extern.
.func def_name(D: Def, Name: string) extern.
.decl hit() output.
hit() :- def(D), def_name(D, "load_and_plan").
"#,
    );
    let result = service.run_planned(&planned).expect("run query");
    assert_eq!(result.status, raql_engine::EvalStatus::Ok);
    let second = service.run_planned(&planned).expect("run warm query");
    assert_eq!(second.status, raql_engine::EvalStatus::Ok);
}

#[test]
#[ignore = "manual repo-scale exact-name timing probe"]
fn workspace_service_repo_def_name_exact_probe_smoke() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");
    let manifest = repo_root.join("Cargo.toml");
    let mut service =
        WorkspaceService::from_manifest_path(manifest.as_path()).expect("service from repo manifest");
    let planned = plan_query(
        r#"
.func def_name(D: Def, Name: string) extern.
.decl hit() output.
hit() :- def_name(_, "load_and_plan").
"#,
    );
    let result = service.run_planned(&planned).expect("run cold exact-name query");
    assert_eq!(result.status, raql_engine::EvalStatus::Ok);
    let second = service.run_planned(&planned).expect("run warm exact-name query");
    assert_eq!(second.status, raql_engine::EvalStatus::Ok);
}

#[test]
#[ignore = "repo-scale timing probe"]
fn workspace_service_repo_callers_probe_smoke() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");
    let manifest = repo_root.join("Cargo.toml");
    let mut service =
        WorkspaceService::from_manifest_path(manifest.as_path()).expect("service from repo manifest");
    let planned = plan_query(
        r#"
.type DefKind = {
  FN, METHOD,
  STRUCT, ENUM, UNION, TRAIT,
  MOD, IMPL, TYPE_ALIAS,
  CONST, STATIC,
  FIELD, VARIANT,
  ASSOC_TYPE, ASSOC_CONST,
  MACRO, OTHER
}.
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl def(D: Def) extern.
.mode def(-Def).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.mode call_edge(+Def, -Def, -Span, -DispatchKind).
.mode call_edge(-Def, +Def, -Span, -DispatchKind).
.mode call_edge(-Def, -Def, -Span, -DispatchKind).
.func def_path(D: Def, Path: string) extern.
.mode def_path(+Def, -string).
.decl is_fn(D: Def).
is_fn(D) :- def(D), def_kind(D, DefKind::FN).
.func def_kind(D: Def, Kind: DefKind) extern.
.mode def_kind(+Def, -DefKind).
.decl caller(Callee: Def, Caller: Def, Site: Span, Dispatch: DispatchKind).
.mode caller(+Def, -Def, -Span, -DispatchKind).
caller(Callee, Caller, Site, Dispatch) :- call_edge(Caller, Callee, Site, Dispatch).

.decl target(T: Def).
target(T) :-
  def_name(T, "load_and_plan"),
  is_fn(T).

.decl caller_stats(Caller: Def, Sites: int, Direct: int, ThroughTrait: int, Dyn: int).
caller_stats(Caller, Sites, Direct, ThroughTrait, Dyn) :-
  target(Target),
  caller(Target, Caller, _, _),
  Sites = count(S : caller(Target, Caller, S, _)),
  Direct = count(S : caller(Target, Caller, S, DispatchKind::DIRECT)),
  ThroughTrait = count(S : caller(Target, Caller, S, DispatchKind::THROUGH_TRAIT)),
  Dyn = count(S : caller(Target, Caller, S, DispatchKind::DYN)).

.decl caller_report(CallerPath: string, Sites: int, Direct: int, ThroughTrait: int, Dyn: int) output.
caller_report(CallerPath, Sites, Direct, ThroughTrait, Dyn) :-
  caller_stats(Caller, Sites, Direct, ThroughTrait, Dyn),
  def_path(Caller, CallerPath).
"#,
    );
    let result = service.run_planned(&planned).expect("run cold repo caller query");
    assert_eq!(
        result.status,
        raql_engine::EvalStatus::Ok,
        "cold notes={:?} relations={:?}",
        result.notes,
        result.relations.keys().collect::<Vec<_>>()
    );
    let second = service.run_planned(&planned).expect("run warm repo caller query");
    assert_eq!(
        second.status,
        raql_engine::EvalStatus::Ok,
        "warm notes={:?} relations={:?}",
        second.notes,
        second.relations.keys().collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "repo-scale timing probe"]
fn workspace_service_repo_callers_probe_after_prewarm_smoke() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");
    let manifest = repo_root.join("Cargo.toml");
    let mut service =
        WorkspaceService::from_manifest_path(manifest.as_path()).expect("service from repo manifest");
    service.prewarm_semantics().expect("prewarm semantics");
    let planned = plan_query(
        r#"
.type DefKind = {
  FN, METHOD,
  STRUCT, ENUM, UNION, TRAIT,
  MOD, IMPL, TYPE_ALIAS,
  CONST, STATIC,
  FIELD, VARIANT,
  ASSOC_TYPE, ASSOC_CONST,
  MACRO, OTHER
}.
.type DispatchKind = { DIRECT, THROUGH_TRAIT, DYN, CLOSURE, FN_POINTER }.
.decl def(D: Def) extern.
.mode def(-Def).
.func def_name(D: Def, Name: string) extern.
.mode def_name(+Def, -string).
.decl call_edge(Caller: Def, Callee: Def, Site: Span, Dispatch: DispatchKind) extern.
.mode call_edge(+Def, -Def, -Span, -DispatchKind).
.mode call_edge(-Def, +Def, -Span, -DispatchKind).
.mode call_edge(-Def, -Def, -Span, -DispatchKind).
.func def_path(D: Def, Path: string) extern.
.mode def_path(+Def, -string).
.decl is_fn(D: Def).
is_fn(D) :- def(D), def_kind(D, DefKind::FN).
.func def_kind(D: Def, Kind: DefKind) extern.
.mode def_kind(+Def, -DefKind).
.decl caller(Callee: Def, Caller: Def, Site: Span, Dispatch: DispatchKind).
.mode caller(+Def, -Def, -Span, -DispatchKind).
caller(Callee, Caller, Site, Dispatch) :- call_edge(Caller, Callee, Site, Dispatch).

.decl target(T: Def).
target(T) :-
  def_name(T, "load_and_plan"),
  is_fn(T).

.decl caller_stats(Caller: Def, Sites: int, Direct: int, ThroughTrait: int, Dyn: int).
caller_stats(Caller, Sites, Direct, ThroughTrait, Dyn) :-
  target(Target),
  caller(Target, Caller, _, _),
  Sites = count(S : caller(Target, Caller, S, _)),
  Direct = count(S : caller(Target, Caller, S, DispatchKind::DIRECT)),
  ThroughTrait = count(S : caller(Target, Caller, S, DispatchKind::THROUGH_TRAIT)),
  Dyn = count(S : caller(Target, Caller, S, DispatchKind::DYN)).

.decl caller_report(CallerPath: string, Sites: int, Direct: int, ThroughTrait: int, Dyn: int) output.
caller_report(CallerPath, Sites, Direct, ThroughTrait, Dyn) :-
  caller_stats(Caller, Sites, Direct, ThroughTrait, Dyn),
  def_path(Caller, CallerPath).
"#,
    );
    let result = service.run_planned(&planned).expect("run prewarmed caller query");
    assert_eq!(
        result.status,
        raql_engine::EvalStatus::Ok,
        "prewarmed notes={:?} relations={:?}",
        result.notes,
        result.relations.keys().collect::<Vec<_>>()
    );
}
