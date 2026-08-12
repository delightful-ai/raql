//! Call-family operators (SPEC §8.6 `callee`, `caller`, `call_edge`).
//!
//! Thin row-shaping over [`crate::calls`]. Row orders follow the catalog:
//! `callee(F, Callee, Site, Disp)` and `call_edge(Caller, Callee, Site,
//! Disp)` coincide for the outgoing direction, so `CallEdgesByCaller`
//! shares the `callee` body; the incoming composition reorders `caller`
//! rows into `call_edge` column order.
//!
//! Only `Def::Function` inputs produce rows: other defs have no call edges
//! in v0 (const/static initializer bodies are future work, not
//! approximated).

use crate::calls::{CallSite, callers_of, raql_callees};
use crate::def::Def;
use crate::operators::OperatorError;
use crate::snapshot::Attached;
use crate::value::{EnumTag, Value};

pub(super) fn dispatch_value(site: &CallSite) -> Value {
    Value::Enum(EnumTag { ty: "DispatchKind", variant: site.dispatch.tag() })
}

/// `callee(+F, -Callee, -Site, -Disp)` rows; also `call_edge(+Caller, ..)`.
pub(super) fn callees_of_fn(
    att: &Attached<'_>,
    def: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let Def::Function(function) = def else {
        return Ok(Vec::new());
    };
    Ok(raql_callees(att.db(), function)
        .iter()
        .map(|site| {
            vec![
                Value::Def(def),
                Value::Def(Def::Function(site.peer)),
                Value::FileRange(site.range),
                dispatch_value(site),
            ]
        })
        .collect())
}

/// `caller(+F, -CallerFn, -Site, -Disp)` rows.
pub(super) fn callers_of_fn(
    att: &Attached<'_>,
    def: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let Def::Function(function) = def else {
        return Ok(Vec::new());
    };
    Ok(callers_of(att.db(), function)
        .iter()
        .map(|site| {
            vec![
                Value::Def(def),
                Value::Def(Def::Function(site.peer)),
                Value::FileRange(site.range),
                dispatch_value(site),
            ]
        })
        .collect())
}

/// `call_edge(-Caller, +Callee, -Site, -Disp)`: `caller` rows reordered
/// into `(Caller, Callee, Site, Disp)` column order.
pub(super) fn call_edges_by_callee(
    att: &Attached<'_>,
    callee: Def,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let Def::Function(function) = callee else {
        return Ok(Vec::new());
    };
    Ok(callers_of(att.db(), function)
        .iter()
        .map(|site| {
            vec![
                Value::Def(Def::Function(site.peer)),
                Value::Def(callee),
                Value::FileRange(site.range),
                dispatch_value(site),
            ]
        })
        .collect())
}
