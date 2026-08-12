//! Shared test support: fixture loading through `load_workspace_into_db`
//! (the same entry point the future `raql-server` uses) and position/lookup
//! helpers. Test-only; the library must never own workspace loading.

// Each integration-test binary compiles its own copy; not all use every helper.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use ide_db::RootDatabase;
use load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_into_db};
use project_model::{CargoConfig, ProjectManifest, ProjectWorkspace};
use raql_ra::Position;
use vfs::{AbsPathBuf, VfsPath};

pub struct Fixture {
    pub db: RootDatabase,
    pub vfs: vfs::Vfs,
    pub root: AbsPathBuf,
}

impl Fixture {
    /// Load `tests/fixtures/<name>` through the real cargo/RA pipeline.
    pub fn load(name: &str) -> Fixture {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let root = AbsPathBuf::assert_utf8(root.canonicalize().expect("fixture exists"));
        let manifest = ProjectManifest::discover_single(&root).expect("fixture manifest");
        let cargo_config = CargoConfig { sysroot: None, ..CargoConfig::default() };
        let workspace =
            ProjectWorkspace::load(manifest, &cargo_config, &|_| {}).expect("fixture loads");
        let load_config = LoadCargoConfig {
            load_out_dirs_from_check: false,
            with_proc_macro_server: ProcMacroServerChoice::None,
            prefill_caches: false,
            num_worker_threads: 1,
            proc_macro_processes: 1,
        };
        let mut db = RootDatabase::new(None);
        let (vfs, _proc_macro_server) =
            load_workspace_into_db(workspace, &cargo_config.extra_env, &load_config, &mut db)
                .expect("load_workspace_into_db succeeds");
        Fixture { db, vfs, root }
    }

    pub fn workspace_root(&self) -> PathBuf {
        PathBuf::from(self.root.to_string())
    }

    pub fn file_id(&self, rel_path: &str) -> ide_db::FileId {
        let path = self.root.join(rel_path);
        let (file_id, excluded) = self
            .vfs
            .file_id(&VfsPath::from(path))
            .unwrap_or_else(|| panic!("fixture file `{rel_path}` in vfs"));
        assert!(
            matches!(excluded, vfs::FileExcluded::No),
            "fixture file `{rel_path}` must not be excluded",
        );
        file_id
    }

    /// Overlay `new_text` for one fixture file and apply it as an RA change.
    /// Overlay-only: the on-disk fixture is never modified.
    pub fn apply_edit(&mut self, rel_path: &str, new_text: &str) {
        let path = self.root.join(rel_path);
        let changed = self
            .vfs
            .set_file_contents(VfsPath::from(path), Some(new_text.as_bytes().to_vec()));
        assert!(changed, "edit must change file contents");
        let mut change = base_db::FileChange::default();
        for (_, file) in self.vfs.take_changes() {
            if let vfs::Change::Create(contents, _) | vfs::Change::Modify(contents, _) =
                file.change
            {
                let text = String::from_utf8(contents).expect("fixture is utf-8");
                change.change_file(file.file_id, Some(text));
            }
        }
        change.apply(&mut self.db);
    }
}

/// Find a workspace function by name via hir crate walking.
pub fn find_fn(db: &RootDatabase, name: &str) -> hir::Function {
    hir::attach_db(db, || {
        hir::Crate::all(db)
            .into_iter()
            .flat_map(|krate| krate.modules(db))
            .flat_map(|module| module.declarations(db))
            .find_map(|decl| match decl {
                hir::ModuleDef::Function(f) if f.name(db).as_str() == name => Some(f),
                _ => None,
            })
            .unwrap_or_else(|| panic!("function `{name}` in fixture"))
    })
}

/// Find a workspace ADT by name via hir crate walking.
pub fn find_adt(db: &RootDatabase, name: &str) -> hir::Adt {
    hir::attach_db(db, || {
        hir::Crate::all(db)
            .into_iter()
            .flat_map(|krate| krate.modules(db))
            .flat_map(|module| module.declarations(db))
            .find_map(|decl| match decl {
                hir::ModuleDef::Adt(adt) if adt.name(db).as_str() == name => Some(adt),
                _ => None,
            })
            .unwrap_or_else(|| panic!("adt `{name}` in fixture"))
    })
}

/// The unique def with `name` whose projected canonical path is `path`,
/// found through the catalog seeding operator.
pub fn def_by_path(fixture: &Fixture, name: &str, path: &str) -> raql_ra::Def {
    use raql_plan::{OperatorId, OperatorSet};
    use raql_ra::{SnapshotOperators, Value, project_def};

    let mut ops = SnapshotOperators::new(&fixture.db);
    let rows = ops
        .invoke(OperatorId::DefsByExactName, &[Value::string(name)])
        .expect("seeding succeeds");
    let mut matches = rows.into_iter().filter_map(|row| match row.as_slice() {
        [Value::Def(def), _]
            if project_def(&fixture.db, &fixture.workspace_root(), *def)
                .path
                .to_string()
                == path =>
        {
            Some(*def)
        }
        _ => None,
    });
    let found = matches.next().unwrap_or_else(|| panic!("def `{path}` found by name `{name}`"));
    assert!(matches.next().is_none(), "`{path}` is unique");
    found
}

/// Zero-based position of the start of the `occurrence`-th (0-based) match
/// of `needle` in `text` (`LineCol` convention; ASCII fixtures only).
pub fn position_of(text: &str, needle: &str, occurrence: usize) -> Position {
    let mut from = 0;
    let mut found = None;
    for _ in 0..=occurrence {
        let at = text[from..].find(needle).expect("needle occurs in text") + from;
        found = Some(at);
        from = at + needle.len();
    }
    let offset = found.expect("at least one occurrence requested");
    let before = &text[..offset];
    let line = before.matches('\n').count() as u32;
    let col = (offset - before.rfind('\n').map_or(0, |nl| nl + 1)) as u32;
    Position { line, col }
}
