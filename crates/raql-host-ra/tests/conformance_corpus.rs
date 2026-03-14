use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

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
