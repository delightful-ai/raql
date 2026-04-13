use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use hir::{AssocItem, ModuleDef};
use raql_host::{
    ExternLookupHostValue, ExternLookupHostValueKind, ExternLookupRequest, ExternLookupValue,
};

use crate::provider::core_index::CoreLookupIndex;
use crate::provider::defs::{LookupDefRecord, ensure_lookup_source_module_def};
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

    let mut id_host = DeterministicRaHost::new();
    let mut rows = BTreeSet::<Vec<ExternLookupValue>>::new();
    hir::attach_db(db, || {
        for krate in hir::Crate::all(db)
            .into_iter()
            .filter(|krate| krate.origin(db).is_local())
        {
            collect_exact_name_rows_in_module(
                krate.root_module(db),
                requested_name,
                db,
                vfs,
                workspace_root,
                lookup_defs,
                lookup_spans,
                &mut id_host,
                &mut rows,
                def_filter,
            );
        }
    });
    Ok(rows.into_iter().collect())
}

fn collect_exact_name_rows_in_module(
    module: hir::Module,
    requested_name: &str,
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    rows: &mut BTreeSet<Vec<ExternLookupValue>>,
    def_filter: Option<DefId>,
) {
    for def in module.declarations(db) {
        push_exact_name_row(
            def,
            requested_name,
            db,
            vfs,
            workspace_root,
            lookup_defs,
            lookup_spans,
            id_host,
            rows,
            def_filter,
        );
        match def {
            ModuleDef::Module(child) => collect_exact_name_rows_in_module(
                child,
                requested_name,
                db,
                vfs,
                workspace_root,
                lookup_defs,
                lookup_spans,
                id_host,
                rows,
                def_filter,
            ),
            ModuleDef::Trait(trait_def) => {
                for assoc in trait_def.items(db) {
                    if let AssocItem::Function(function) = assoc {
                        push_exact_name_row(
                            ModuleDef::Function(function),
                            requested_name,
                            db,
                            vfs,
                            workspace_root,
                            lookup_defs,
                            lookup_spans,
                            id_host,
                            rows,
                            def_filter,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    for impl_def in module.impl_defs(db) {
        for assoc in impl_def.items(db) {
            if let AssocItem::Function(function) = assoc {
                push_exact_name_row(
                    ModuleDef::Function(function),
                    requested_name,
                    db,
                    vfs,
                    workspace_root,
                    lookup_defs,
                    lookup_spans,
                    id_host,
                    rows,
                    def_filter,
                );
            }
        }
    }
}

fn push_exact_name_row(
    def: ModuleDef,
    requested_name: &str,
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    rows: &mut BTreeSet<Vec<ExternLookupValue>>,
    def_filter: Option<DefId>,
) {
    if def
        .name(db)
        .is_none_or(|name| name.display(db, syntax::Edition::CURRENT).to_string() != requested_name)
    {
        return;
    }
    let Some(def_id) = ensure_lookup_source_module_def(
        db,
        vfs,
        workspace_root,
        lookup_defs,
        lookup_spans,
        id_host,
        def,
    ) else {
        return;
    };
    if def_filter.is_some_and(|expected| expected != def_id) {
        return;
    }
    rows.insert(vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Def,
            def_id.stable_id(),
        )),
        ExternLookupValue::String(requested_name.to_string().into_boxed_str()),
    ]);
}
