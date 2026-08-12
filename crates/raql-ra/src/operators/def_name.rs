//! Exact-name definition seeding via RA's symbol index
//! (SPEC §8.6 `def_name(-,+)`, cost C2).
//!
//! Ported from the previous host's `world_symbols` seeding (raql-host-ra
//! `provider/def_name.rs`), minus its stable-id bookkeeping and fallback
//! paths: rows come straight from the symbol index, and defs the index does
//! not surface are absent.

use std::collections::HashSet;

use hir::import_map::AssocSearchMode;
use ide_db::symbol_index::{Query, world_symbols};
use syntax::Edition;

use crate::def::Def;
use crate::operators::OperatorError;
use crate::snapshot::Snapshot;
use crate::value::Value;

/// Takes `&mut Snapshot` — the unattached context — because
/// `world_symbols` parallelizes over database clones on the calling thread,
/// which panics if a different database instance is TLS-attached (RA
/// documents this in `Analysis::symbol_search`). The borrow checker keeps
/// this call out of any attach scope. The per-symbol filtering below is
/// def-map-level work that needs no attachment.
pub(super) fn defs_by_exact_name(
    snapshot: &mut Snapshot<'_>,
    name: &str,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let db = snapshot.unattached_db();
    let mut query = Query::new(name.to_owned());
    query.exact();
    query.case_sensitive();
    query.exclude_imports();
    query.assoc_search_mode(AssocSearchMode::Include);

    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for symbol in world_symbols(db, query) {
        // Workspace-local defs only: `def_name(-,+)` is a seed for
        // workspace queries, not a dependency search.
        if symbol
            .def
            .module(db)
            .is_none_or(|module| !module.krate(db).origin(db).is_local())
        {
            continue;
        }
        // The symbol index matches by symbol text; re-check against the
        // definition's own name to keep the contract honest.
        if symbol
            .def
            .name(db)
            .is_none_or(|n| n.display(db, Edition::CURRENT).to_string() != name)
        {
            continue;
        }
        let Some(def) = Def::from_module_def(symbol.def) else {
            continue;
        };
        if seen.insert(def) {
            rows.push(vec![Value::Def(def), Value::string(name)]);
        }
    }
    Ok(rows)
}
