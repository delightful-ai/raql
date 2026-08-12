//! Keyed projections of one definition: name, kind, canonical path, span
//! (SPEC §8.6 `def_name(+,-)`, `def_kind`, `def_path`, `def_span`).
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
