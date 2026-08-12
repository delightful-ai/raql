//! Enumeration scans (SPEC §8.5, §8.6): `def(-)`, `fn_def(-)`,
//! `def_name(-,-)`, `call_edge(-,-,-,-)`.
//!
//! All of these enumerate the workspace-local crates through the tracked
//! `raql_crate_defs` query. The `call_edge` scan is the SPEC §8.5 rewrite
//! `fn_def(C), callee(C, K, S, D)`: enumeration expanded through the
//! *outgoing* operator — the wide direction (reference search) is never
//! the enumerator.

use crate::calls::raql_callees;
use crate::crate_defs::{raql_crate_defs, workspace_local_crates};
use crate::def::Def;
use crate::operators::OperatorError;
use crate::snapshot::Attached;
use crate::value::Value;

fn for_each_workspace_def(att: &Attached<'_>, mut push: impl FnMut(Def)) {
    let db = att.db();
    for krate in workspace_local_crates(db) {
        for def in raql_crate_defs(db, krate).iter() {
            push(*def);
        }
    }
}

/// `def(-D)` rows.
pub(super) fn defs_scan(att: &Attached<'_>) -> Result<Vec<Vec<Value>>, OperatorError> {
    let mut rows = Vec::new();
    for_each_workspace_def(att, |def| rows.push(vec![Value::Def(def)]));
    Ok(rows)
}

/// `fn_def(-D)` rows: `def` filtered to functions during enumeration.
pub(super) fn fn_defs_scan(att: &Attached<'_>) -> Result<Vec<Vec<Value>>, OperatorError> {
    let mut rows = Vec::new();
    for_each_workspace_def(att, |def| {
        if matches!(def, Def::Function(_)) {
            rows.push(vec![Value::Def(def)]);
        }
    });
    Ok(rows)
}

/// `def_name(-D, -Name)` rows: enumeration × name projection. Unnamed defs
/// (impls, the crate root module) have no name and no row.
pub(super) fn def_names_scan(att: &Attached<'_>) -> Result<Vec<Vec<Value>>, OperatorError> {
    let db = att.db();
    let mut rows = Vec::new();
    for_each_workspace_def(att, |def| {
        if let Some(name) = def.name(db) {
            rows.push(vec![Value::Def(def), Value::string(name)]);
        }
    });
    Ok(rows)
}

/// `call_edge(-,-,-,-)` rows: the §8.5 composite scan over the outgoing
/// direction. Inherits `callee`'s caveats (macro-generated callsites and
/// unresolvable callees are absent).
pub(super) fn call_edges_scan(att: &Attached<'_>) -> Result<Vec<Vec<Value>>, OperatorError> {
    let db = att.db();
    let mut rows = Vec::new();
    for_each_workspace_def(att, |def| {
        let Def::Function(function) = def else {
            return;
        };
        for site in raql_callees(db, function).iter() {
            rows.push(vec![
                Value::Def(def),
                Value::Def(Def::Function(site.peer)),
                Value::FileRange(site.range),
                super::calls::dispatch_value(site),
            ]);
        }
    });
    Ok(rows)
}
