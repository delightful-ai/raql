//! Keyed projections and filters of one definition: name, kind, canonical
//! path, span, visibility, test scope, handle (SPEC §8.6 `def_name(+,-)`,
//! `def_kind`, `def_path`, `def_span`, `is_public`, `in_test`, `handle`).
//!
//! All of these are cheap and already Salsa-backed inside RA, so they are
//! plain functions, not tracked queries (SPEC §6.2). They run in the
//! attached context ([`Attached`]) because name display, assoc-item
//! containers, and span upmapping reach the hir layer.

use crate::def::Def;
use crate::operators::OperatorError;
use crate::snapshot::Attached;
use crate::value::{EnumTag, Value};

pub(super) fn name_of_def(
    att: &Attached<'_>,
    def: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let Some(name) = def.name(att.db()) else {
        // Unnamed defs (impls) simply have no `def_name` row.
        return Ok(Vec::new());
    };
    Ok(vec![vec![Value::Def(def), Value::string(name)]])
}

pub(super) fn kind_of_def(
    att: &Attached<'_>,
    def: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let kind = def.kind(att.db());
    Ok(vec![vec![
        Value::Def(def),
        Value::Enum(EnumTag { ty: "DefKind", variant: kind.tag() }),
    ]])
}

pub(super) fn canonical_path_of_def(
    att: &Attached<'_>,
    def: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let Some(path) = def.canonical_path(att.db()) else {
        // No canonical path (impls, unnamed defs): no row, no invention.
        return Ok(Vec::new());
    };
    Ok(vec![vec![Value::Def(def), Value::string(path)]])
}

pub(super) fn span_of_def(
    att: &Attached<'_>,
    def: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let Some(range) = def.original_span(att.db()) else {
        return Ok(Vec::new());
    };
    Ok(vec![vec![Value::Def(def), Value::FileRange(range)]])
}

/// `is_public(+D)`: a row iff the def's declared visibility is `pub`.
pub(super) fn is_public_filter(
    att: &Attached<'_>,
    def: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    if def.is_public(att.db()) {
        Ok(vec![vec![Value::Def(def)]])
    } else {
        Ok(Vec::new())
    }
}

/// `in_test(+D)`: a row iff the def is test code.
pub(super) fn in_test_filter(
    att: &Attached<'_>,
    def: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    if def.in_test(att.db()) {
        Ok(vec![vec![Value::Def(def)]])
    } else {
        Ok(Vec::new())
    }
}

/// `handle(+D, -H)`: the §13.2 handle as a predicate, sharing the output
/// boundary's projection. Defs with no honest handle have no row (never
/// invented).
pub(super) fn handle_of_def(
    att: &Attached<'_>,
    workspace_root: &std::path::Path,
    def: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    match crate::projection::handle_text(att.db(), workspace_root, def) {
        Ok(text) => Ok(vec![vec![Value::Def(def), Value::string(text)]]),
        Err(_) => Ok(Vec::new()),
    }
}
