use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ide_db::symbol_index::{Query, world_symbols};
use raql_host::{
    ExternLookupHostValue, ExternLookupHostValueKind, ExternLookupRequest, ExternLookupValue,
};

use crate::provider::core_index::CoreLookupIndex;
use crate::provider::defs::{LookupDefRecord, ensure_lookup_symbol_module_def};
use crate::{DefId, DeterministicRaHost, RaHostInitError, SpanId, SpanKey};

pub(crate) fn lookup_def_name_rows(
    request: &ExternLookupRequest,
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    core_index: Option<&CoreLookupIndex>,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
) -> Result<Vec<Vec<ExternLookupValue>>, RaHostInitError> {
    let mut requested_name = None::<&str>;
    let mut def_filter = None::<DefId>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                def_filter = Some(DefId::new(host.stable_id()));
            }
            (1, ExternLookupValue::String(name)) => requested_name = Some(name.as_ref()),
            _ => return Ok(Vec::new()),
        }
    }
    if let Some(def) = def_filter {
        if let Some(record) = lookup_defs.get(&def) {
            if requested_name.is_some_and(|expected| expected != record.name.as_ref()) {
                return Ok(Vec::new());
            }
            return Ok(vec![vec![
                ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Def,
                    def.stable_id(),
                )),
                ExternLookupValue::String(record.name.clone()),
            ]]);
        }
        if let Some(name) = core_index.and_then(|index| index.def_name(def)) {
            if requested_name.is_some_and(|expected| expected != name) {
                return Ok(Vec::new());
            }
            return Ok(vec![vec![
                ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Def,
                    def.stable_id(),
                )),
                ExternLookupValue::String(name.to_owned().into_boxed_str()),
            ]]);
        }
    }
    let Some(requested_name) = requested_name else {
        return Ok(Vec::new());
    };

    let mut symbol_id_host = DeterministicRaHost::new();
    let mut symbol_rows = BTreeSet::<Vec<ExternLookupValue>>::new();
    let mut collect_symbol_rows = |mut query: Query, include_functions: bool| {
        query.exact();
        query.exclude_imports();
        for symbol in world_symbols(db, query) {
            if symbol.is_alias || symbol.is_import {
                continue;
            }
            let def = symbol.def;
            if matches!(def, hir::ModuleDef::Function(_)) != include_functions {
                continue;
            }
            let Some(module) = def.module(db) else {
                continue;
            };
            if !module.krate(db).origin(db).is_local() {
                continue;
            }
            let original = symbol.loc.hir_file_id.original_file_respecting_includes(db);
            let Some(def_id) = ensure_lookup_symbol_module_def(
                db,
                vfs,
                workspace_root,
                lookup_defs,
                lookup_spans,
                &mut symbol_id_host,
                def,
                original.editioned_file_id(db),
                symbol.loc.ptr.text_range(),
            ) else {
                continue;
            };
            if def_filter.is_some_and(|expected| expected != def_id) {
                continue;
            }
            symbol_rows.insert(vec![
                ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Def,
                    def_id.stable_id(),
                )),
                ExternLookupValue::String(requested_name.to_string().into_boxed_str()),
            ]);
        }
    };
    collect_symbol_rows(Query::new(requested_name.to_string()), false);
    collect_symbol_rows(Query::new(requested_name.to_string()), true);
    Ok(symbol_rows.into_iter().collect())
}
