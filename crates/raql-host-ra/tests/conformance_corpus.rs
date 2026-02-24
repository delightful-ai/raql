use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use raql_compiler::{plan, resolve, typecheck};
use raql_engine::{EngineHostView, EvalStatus, execute};
use raql_host::HostRuntime;
use raql_host_ra::RaHostRuntime;
use raql_syntax::parse_program;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CorpusManifest {
    version: u32,
    case: Vec<CorpusCase>,
}

#[derive(Debug, Deserialize, Clone)]
struct CorpusCase {
    name: String,
    kind: CorpusKind,
    repo: String,
    rev: String,
    workspace: String,
    local_path: Option<String>,
    expected_symbol: String,
    expected_path: String,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum CorpusKind {
    NoStd,
    ProcMacroHeavy,
    HugeMonorepo,
    FeatureMatrix,
}

#[test]
fn conformance_manifest_has_required_dimensions() {
    let manifest = load_manifest();
    assert_eq!(manifest.version, 1);
    assert!(!manifest.case.is_empty());

    let mut names = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for case in &manifest.case {
        assert!(names.insert(case.name.clone()), "duplicate case `{}`", case.name);
        assert!(
            is_hex_commit(case.rev.as_str()),
            "case `{}` must pin a full 40-char commit hash",
            case.name
        );
        assert!(
            !case.repo.trim().is_empty(),
            "case `{}` must include a repository URL",
            case.name
        );
        assert!(
            !case.workspace.trim().is_empty(),
            "case `{}` must include a workspace path",
            case.name
        );
        assert!(
            !case.expected_symbol.trim().is_empty(),
            "case `{}` must include an expected symbol probe",
            case.name
        );
        assert!(
            !case.expected_path.trim().is_empty(),
            "case `{}` must include an expected path probe",
            case.name
        );
        kinds.insert(case.kind);
    }

    let expected = BTreeSet::from([
        CorpusKind::NoStd,
        CorpusKind::ProcMacroHeavy,
        CorpusKind::HugeMonorepo,
        CorpusKind::FeatureMatrix,
    ]);
    assert_eq!(kinds, expected);
}

#[test]
#[ignore = "release gate: enable with RAQL_CONFORMANCE=1"]
fn conformance_corpus_runtime_gate() {
    if std::env::var("RAQL_CONFORMANCE").ok().as_deref() != Some("1") {
        return;
    }

    let manifest = load_manifest();
    let repo_root = repo_root();
    let cache_root = std::env::var_os("RAQL_CONFORMANCE_CACHE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join("raql-conformance")
                .join(format!("pid-{}", std::process::id()))
        });
    fs::create_dir_all(&cache_root).expect("create conformance cache root");

    for case in &manifest.case {
        let workspace = resolve_case_workspace(case, &repo_root, &cache_root);
        let mut runtime = RaHostRuntime::from_workspace_root(workspace.as_path()).unwrap_or_else(|err| {
            panic!(
                "failed to initialize conformance case `{}` at `{}`: {err}",
                case.name,
                workspace.display()
            )
        });

        assert!(
            runtime.analysis_status_ok(),
            "analysis status failed for case `{}`",
            case.name
        );

        let parsed = parse_program(
            r#"
.decl has_def().
.decl def(D: Def) extern.

has_def() :- def(D).
"#,
        )
        .expect("parse conformance query");
        let resolved = resolve(parsed).expect("resolve conformance query");
        let typed = typecheck(resolved).expect("typecheck conformance query");
        let planned = plan(typed).expect("plan conformance query");
        let result = execute(&planned, &mut runtime);
        assert_eq!(
            result.status,
            EvalStatus::Ok,
            "query status must be ok for case `{}`",
            case.name
        );
        assert!(
            result
                .relations
                .get("has_def")
                .is_some_and(|rows| !rows.is_empty()),
            "def relation should be populated for case `{}`",
            case.name
        );

        let symbol_query = format!(
            r#"
.decl symbol_path(Path: string).
.decl def(D: Def) extern.
.decl def_name(D: Def, Name: string) extern.
.decl def_path(D: Def, Path: string) extern.

symbol_path(Path) :- def(D), def_name(D, "{symbol}"), def_path(D, Path).
"#,
            symbol = case.expected_symbol
        );
        let parsed = parse_program(symbol_query.as_str()).expect("parse symbol probe");
        let resolved = resolve(parsed).expect("resolve symbol probe");
        let typed = typecheck(resolved).expect("typecheck symbol probe");
        let planned = plan(typed).expect("plan symbol probe");
        let result = execute(&planned, &mut runtime);
        assert_eq!(
            result.status,
            EvalStatus::Ok,
            "symbol probe status must be ok for case `{}`",
            case.name
        );
        assert!(
            result
                .relations
                .get("symbol_path")
                .is_some_and(|rows| !rows.is_empty()),
            "expected symbol `{}` not found in case `{}`",
            case.expected_symbol,
            case.name
        );
        let observed_paths = result
            .relations
            .get("symbol_path")
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        row.first().and_then(|value| match value {
                            raql_engine::RuntimeValue::String(text) => Some(text.clone()),
                            _ => None,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let has_expected_path = observed_paths
            .iter()
            .any(|path| path == case.expected_path.as_str());
        assert!(
            has_expected_path,
            "expected symbol `{}` exists but no path matched `{}` in case `{}`; observed paths: {:?}",
            case.expected_symbol,
            case.expected_path,
            case.name,
            observed_paths
        );

        let call_edge_rows = EngineHostView::extern_relation_rows(&mut runtime, "call_edge")
            .unwrap_or_else(|err| panic!("call_edge extern failed for `{}`: {err}", case.name));
        assert!(
            call_edge_rows.is_some(),
            "call_edge relation must be available for case `{}`",
            case.name
        );

        let stamp = HostRuntime::world_stamp(&runtime)
            .expect("world stamp")
            .as_str()
            .to_string();
        assert!(
            stamp.starts_with("ra-workspace:"),
            "unexpected world stamp format for case `{}`: {stamp}",
            case.name
        );
    }
}

fn load_manifest() -> CorpusManifest {
    let manifest_path = repo_root().join("conformance/corpus.toml");
    let text = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|err| panic!("read `{}`: {err}", manifest_path.display()));
    toml::from_str::<CorpusManifest>(&text)
        .unwrap_or_else(|err| panic!("parse `{}`: {err}", manifest_path.display()))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn resolve_case_workspace(case: &CorpusCase, repo_root: &Path, cache_root: &Path) -> PathBuf {
    if let Some(local_path) = case.local_path.as_ref() {
        let repo_path = repo_root.join(local_path);
        assert!(
            repo_path.exists(),
            "local_path for case `{}` not found: {}",
            case.name,
            repo_path.display()
        );
        assert_git_revision(repo_path.as_path(), case.rev.as_str(), case.name.as_str());
        return repo_path.join(case.workspace.as_str());
    }

    let repo_path = cache_root.join(case.name.as_str());
    if !repo_path.exists() {
        run_git(
            repo_root,
            [
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                case.repo.as_str(),
                repo_path.to_string_lossy().as_ref(),
            ],
        );
    }
    run_git(
        repo_path.as_path(),
        ["fetch", "--depth", "1", "origin", case.rev.as_str()],
    );
    run_git(repo_path.as_path(), ["checkout", "--detach", case.rev.as_str()]);
    repo_path.join(case.workspace.as_str())
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    if output.status.success() {
        return;
    }
    panic!(
        "git {:?} failed in `{}`: {}",
        args,
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_git_revision(repo_path: &Path, expected_rev: &str, case_name: &str) {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .expect("git rev-parse");
    if !output.status.success() {
        panic!(
            "failed to read local revision for case `{}` at `{}`: {}",
            case_name,
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(
        head,
        expected_rev,
        "local checkout for case `{}` must be pinned to {}",
        case_name,
        expected_rev
    );
}

fn is_hex_commit(rev: &str) -> bool {
    rev.len() == 40 && rev.chars().all(|ch| ch.is_ascii_hexdigit())
}

#[test]
fn conformance_manifest_case_lookup_is_stable() {
    let manifest = load_manifest();
    let cases = manifest
        .case
        .iter()
        .map(|case| (case.kind, case.name.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        cases.get(&CorpusKind::HugeMonorepo).map(String::as_str),
        Some("rust-analyzer-monorepo")
    );
}
