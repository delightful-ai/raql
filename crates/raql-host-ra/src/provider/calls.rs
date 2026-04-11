use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use hir::{HasSource, ModuleDef};
use ide_db::search::ReferenceCategory;
use raql_host::{
    ExternLookupHostValue, ExternLookupHostValueKind, ExternLookupRequest, ExternLookupValue,
    SpanKey,
};
use syntax::{ast, AstNode};

use crate::provider::defs::{
    LocalFile, LookupDefRecord, RaEntity, ensure_lookup_function_def,
    ensure_lookup_synthetic_callable_def, lookup_local_file, lookup_span_key_from_text,
};
use crate::{DefId, DeterministicRaHost, RaHostInitError, SpanId};

pub(crate) struct CallLookupFilters {
    pub(crate) peer_filter: Option<DefId>,
    pub(crate) site_filter: Option<SpanId>,
    pub(crate) dispatch_filter: Option<crate::DispatchKind>,
}

pub(crate) fn lookup_call_edge_rows(
    request: &ExternLookupRequest,
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
) -> Result<Option<Vec<Vec<ExternLookupValue>>>, RaHostInitError> {
    let mut caller_filter = None::<DefId>;
    let mut callee_filter = None::<DefId>;
    let mut site_filter = None::<SpanId>;
    let mut dispatch_filter = None::<crate::DispatchKind>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                caller_filter = Some(DefId::new(host.stable_id()));
            }
            (1, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                callee_filter = Some(DefId::new(host.stable_id()));
            }
            (2, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Span =>
            {
                site_filter = Some(SpanId::new(host.stable_id()));
            }
            (3, ExternLookupValue::Enum { name, variant }) if name.as_ref() == "DispatchKind" => {
                let Some(dispatch) = dispatch_kind_from_variant(variant.as_ref()) else {
                    return Ok(Some(Vec::new()));
                };
                dispatch_filter = Some(dispatch);
            }
            _ => return Ok(None),
        }
    }
    if caller_filter.is_none() && callee_filter.is_none() {
        return Ok(None);
    }

    let mut id_host = DeterministicRaHost::new();
    let mut rows = BTreeSet::<Vec<ExternLookupValue>>::new();
    let mut unsupported = false;
    let mut next_lookup_defs = lookup_defs.clone();
    let mut next_lookup_spans = lookup_spans.clone();
    let mut lookup_error = None::<Option<Vec<Vec<ExternLookupValue>>>>;
    hir::attach_db(db, || {
        let sema = hir::Semantics::new(db);
        if let Some(caller) = caller_filter {
            let Some(record) = next_lookup_defs.get(&caller).cloned() else {
                lookup_error = Some(None);
                return;
            };
            let Some(function) = record.entity.as_ref().and_then(RaEntity::as_function) else {
                lookup_error = Some(Some(Vec::new()));
                return;
            };
            if let Some(entry) = next_lookup_defs.get_mut(&caller) {
                entry.entity = Some(RaEntity::ModuleDef(ModuleDef::Function(function)));
            }
            if !collect_lookup_call_edges_for_function(
                db,
                vfs,
                workspace_root,
                &sema,
                &mut next_lookup_defs,
                &mut next_lookup_spans,
                &mut id_host,
                &mut rows,
                caller,
                function,
                &CallLookupFilters {
                    peer_filter: callee_filter,
                    site_filter,
                    dispatch_filter,
                },
            ) {
                unsupported = true;
            }
        } else if let Some(callee) = callee_filter {
            let Some(record) = next_lookup_defs.get(&callee).cloned() else {
                lookup_error = Some(None);
                return;
            };
            let collect_started = Instant::now();
            let resolve_function_started = Instant::now();
            let resolved = record.entity.as_ref().and_then(RaEntity::as_function);
            let resolve_function_elapsed = resolve_function_started.elapsed();
            if let Some(resolved) = resolved {
                if let Some(entry) = next_lookup_defs.get_mut(&callee) {
                    entry.entity = Some(RaEntity::ModuleDef(ModuleDef::Function(resolved)));
                }
                let collect_callers_started = Instant::now();
                if !collect_lookup_callers_for_function(
                    db,
                    vfs,
                    workspace_root,
                    &sema,
                    &mut next_lookup_defs,
                    &mut next_lookup_spans,
                    &mut id_host,
                    &mut rows,
                    resolved,
                    callee,
                    &CallLookupFilters {
                        peer_filter: caller_filter,
                        site_filter,
                        dispatch_filter,
                    },
                ) {
                    unsupported = true;
                }
                if std::env::var_os("RAQL_TRACE_TIMINGS").is_some() {
                    eprintln!(
                        "raql-timing workspace_service.lookup_call_edge_rows.callee_parts resolve_function_ms={} collect_callers_ms={}",
                        resolve_function_elapsed.as_millis(),
                        collect_callers_started.elapsed().as_millis(),
                    );
                }
            } else {
                lookup_error = Some(Some(Vec::new()));
                return;
            }
            if std::env::var_os("RAQL_TRACE_TIMINGS").is_some() {
                eprintln!(
                    "raql-timing workspace_service.lookup_call_edge_rows.callee_collect {}",
                    collect_started.elapsed().as_millis()
                );
            }
        }
    });

    if let Some(result) = lookup_error {
        return Ok(result);
    }
    if unsupported {
        return Ok(None);
    }
    *lookup_defs = next_lookup_defs;
    *lookup_spans = next_lookup_spans;
    Ok(Some(rows.into_iter().collect()))
}

pub(crate) fn collect_lookup_call_edges_for_function(
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    sema: &hir::Semantics<'_, ide::RootDatabase>,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    rows: &mut BTreeSet<Vec<ExternLookupValue>>,
    caller_def: DefId,
    function: hir::Function,
    filters: &CallLookupFilters,
) -> bool {
    let Some(source) = function.source(db) else {
        return true;
    };
    let editioned = source.file_id.original_file(db);
    let Some(local) = lookup_local_file(vfs, workspace_root, db, editioned.file_id(db)) else {
        return true;
    };
    let Some(body) = source.value.body() else {
        return true;
    };
    for callable in body.syntax().descendants().filter_map(ast::CallableExpr::cast) {
        if lookup_callable_owner_def(
            db,
            vfs,
            workspace_root,
            sema,
            lookup_defs,
            lookup_spans,
            id_host,
            callable.syntax(),
            editioned.editioned_file_id(db),
            &local,
        ) != Some(caller_def)
        {
            continue;
        }
        match callable {
            ast::CallableExpr::Call(call) => {
                let Some(callee_expr) = call.expr() else {
                    continue;
                };
                let Some(type_info) = sema.type_of_expr(&callee_expr) else {
                    continue;
                };
                let Some(callable) = type_info.original.as_callable(db) else {
                    continue;
                };
                let Some((callee_def, dispatch)) = lookup_call_target_from_callable(
                    db,
                    vfs,
                    workspace_root,
                    sema,
                    lookup_defs,
                    lookup_spans,
                    id_host,
                    &callable,
                    &callee_expr,
                    editioned.editioned_file_id(db),
                    &local,
                ) else {
                    return false;
                };
                push_lookup_call_edge_row(
                    lookup_spans,
                    id_host,
                    rows,
                    caller_def,
                    callee_def,
                    editioned.editioned_file_id(db),
                    &local,
                    call.syntax().text_range(),
                    dispatch,
                    filters,
                );
            }
            ast::CallableExpr::MethodCall(method_call) => {
                let Some(callee_function) = sema.resolve_method_call(&method_call) else {
                    continue;
                };
                let Some(callee_def) = ensure_lookup_function_def(
                    db,
                    vfs,
                    workspace_root,
                    lookup_defs,
                    lookup_spans,
                    id_host,
                    callee_function,
                ) else {
                    return false;
                };
                let dispatch = method_dispatch_kind(sema, &method_call, callee_function, db);
                push_lookup_call_edge_row(
                    lookup_spans,
                    id_host,
                    rows,
                    caller_def,
                    callee_def,
                    editioned.editioned_file_id(db),
                    &local,
                    method_call.syntax().text_range(),
                    dispatch,
                    filters,
                );
            }
        }
    }
    true
}

pub(crate) fn collect_lookup_callers_for_function(
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    sema: &hir::Semantics<'_, ide::RootDatabase>,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    rows: &mut BTreeSet<Vec<ExternLookupValue>>,
    function: hir::Function,
    callee_def: DefId,
    filters: &CallLookupFilters,
) -> bool {
    let function_krate = function.module(db).krate(db);
    let mut search_files = Vec::new();
    for rev_dep in function_krate.transitive_reverse_dependencies(db) {
        let root_file = rev_dep.root_file(db);
        let source_root_id = db.file_source_root(root_file).source_root_id(db);
        let source_root = db.source_root(source_root_id).source_root(db);
        if source_root.is_library {
            continue;
        }
        search_files.extend(
            source_root
                .iter()
                .map(|file_id| base_db::EditionedFileId::new(db, file_id, rev_dep.edition(db), rev_dep.into())),
        );
    }
    let scope = ide_db::search::SearchScope::files(&search_files);
    let references = ide_db::defs::Definition::Function(function)
        .usages(sema)
        .in_scope(&scope)
        .all();
    for (editioned, file_references) in references {
        let file_id = editioned.file_id(db);
        let Some(local) = lookup_local_file(vfs, workspace_root, db, file_id) else {
            continue;
        };
        for reference in file_references {
            if reference.category.contains(ReferenceCategory::IMPORT) {
                return false;
            }
            let Some(name_ref) = reference.name.as_name_ref().cloned() else {
                continue;
            };
            if let Some(method_call) = name_ref
                .syntax()
                .ancestors()
                .find_map(ast::MethodCallExpr::cast)
            {
                let Some(resolved) = sema.resolve_method_call(&method_call) else {
                    continue;
                };
                if resolved != function {
                    continue;
                }
                let Some(caller_def) = lookup_callable_owner_def(
                    db,
                    vfs,
                    workspace_root,
                    sema,
                    lookup_defs,
                    lookup_spans,
                    id_host,
                    method_call.syntax(),
                    editioned.editioned_file_id(db),
                    &local,
                ) else {
                    continue;
                };
                push_lookup_call_edge_row(
                    lookup_spans,
                    id_host,
                    rows,
                    caller_def,
                    callee_def,
                    editioned.editioned_file_id(db),
                    &local,
                    method_call.syntax().text_range(),
                    method_dispatch_kind(sema, &method_call, resolved, db),
                    filters,
                );
                continue;
            }
            let path_segment: Option<ast::PathSegment> = name_ref
                .syntax()
                .ancestors()
                .find_map(ast::PathSegment::cast);
            let Some(path) = path_segment.map(|segment| segment.parent_path()) else {
                continue;
            };
            let Some(path_parent) = path.syntax().parent() else {
                continue;
            };
            let Some(path_expr) = ast::PathExpr::cast(path_parent) else {
                continue;
            };
            let Some(call_parent) = path_expr.syntax().parent() else {
                continue;
            };
            let Some(call) = ast::CallExpr::cast(call_parent) else {
                continue;
            };
            let Some(resolved) = sema.resolve_path(&path).and_then(|resolved| match resolved {
                hir::PathResolution::Def(ModuleDef::Function(resolved)) => Some(resolved),
                _ => None,
            }) else {
                continue;
            };
            if resolved != function {
                continue;
            }
            let Some(caller_def) = lookup_callable_owner_def(
                db,
                vfs,
                workspace_root,
                sema,
                lookup_defs,
                lookup_spans,
                id_host,
                call.syntax(),
                editioned.editioned_file_id(db),
                &local,
            ) else {
                continue;
            };
            push_lookup_call_edge_row(
                lookup_spans,
                id_host,
                rows,
                caller_def,
                callee_def,
                editioned.editioned_file_id(db),
                &local,
                call.syntax().text_range(),
                crate::DispatchKind::Direct,
                filters,
            );
        }
    }
    true
}

pub(crate) fn lookup_callable_owner_def(
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    sema: &hir::Semantics<'_, ide::RootDatabase>,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    syntax: &syntax::SyntaxNode,
    file_id: span::EditionedFileId,
    local: &LocalFile,
) -> Option<DefId> {
    for ancestor in syntax.ancestors().skip(1) {
        if let Some(closure) = ast::ClosureExpr::cast(ancestor.clone()) {
            return ensure_lookup_synthetic_callable_def(
                lookup_defs,
                lookup_spans,
                id_host,
                "closure",
                closure.syntax(),
                file_id,
                local,
            );
        }
        if let Some(ast_fn) = ast::Fn::cast(ancestor) {
            let Some(function) = sema.to_def(&ast_fn) else {
                return None;
            };
            return ensure_lookup_function_def(
                db,
                vfs,
                workspace_root,
                lookup_defs,
                lookup_spans,
                id_host,
                function,
            );
        }
    }
    None
}

pub(crate) fn lookup_call_target_from_callable(
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    callable: &hir::Callable<'_>,
    callee_expr: &ast::Expr,
    file_id: span::EditionedFileId,
    local: &LocalFile,
) -> Option<(DefId, crate::DispatchKind)> {
    match callable.kind() {
        hir::CallableKind::Function(function) => ensure_lookup_function_def(
            db,
            vfs,
            workspace_root,
            lookup_defs,
            lookup_spans,
            id_host,
            function,
        )
        .map(|def| (def, crate::DispatchKind::Direct)),
        hir::CallableKind::Closure(_) => ensure_lookup_synthetic_callable_def(
            lookup_defs,
            lookup_spans,
            id_host,
            "closure",
            callee_expr.syntax(),
            file_id,
            local,
        )
        .map(|def| (def, crate::DispatchKind::Closure)),
        hir::CallableKind::FnPtr | hir::CallableKind::FnImpl(_) => ensure_lookup_synthetic_callable_def(
            lookup_defs,
            lookup_spans,
            id_host,
            "fn_pointer",
            callee_expr.syntax(),
            file_id,
            local,
        )
        .map(|def| (def, crate::DispatchKind::FnPointer)),
        hir::CallableKind::TupleStruct(_) | hir::CallableKind::TupleEnumVariant(_) => None,
    }
}

pub(crate) fn push_lookup_call_edge_row(
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    rows: &mut BTreeSet<Vec<ExternLookupValue>>,
    caller_def: DefId,
    callee_def: DefId,
    file_id: span::EditionedFileId,
    local: &LocalFile,
    range: syntax::TextRange,
    dispatch: crate::DispatchKind,
    filters: &CallLookupFilters,
) {
    if filters
        .peer_filter
        .is_some_and(|expected| expected != callee_def && expected != caller_def)
    {
        return;
    }
    if filters
        .dispatch_filter
        .is_some_and(|expected| expected != dispatch)
    {
        return;
    }
    let Some(span_key) = lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range) else {
        return;
    };
    let Ok(site) = id_host.intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range) else {
        return;
    };
    if filters.site_filter.is_some_and(|expected| expected != site) {
        return;
    }
    lookup_spans.insert(site, span_key);
    rows.insert(vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Def,
            caller_def.stable_id(),
        )),
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Def,
            callee_def.stable_id(),
        )),
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Span,
            site.stable_id(),
        )),
        dispatch_lookup_value(dispatch),
    ]);
}

pub(crate) fn dispatch_lookup_value(dispatch: crate::DispatchKind) -> ExternLookupValue {
    let variant = match dispatch {
        crate::DispatchKind::Direct => "DIRECT",
        crate::DispatchKind::ThroughTrait => "THROUGH_TRAIT",
        crate::DispatchKind::Dyn => "DYN",
        crate::DispatchKind::Closure => "CLOSURE",
        crate::DispatchKind::FnPointer => "FN_POINTER",
    };
    ExternLookupValue::Enum {
        name: "DispatchKind".into(),
        variant: variant.into(),
    }
}

pub(crate) fn method_dispatch_kind(
    sema: &hir::Semantics<'_, ide::RootDatabase>,
    method_call: &ast::MethodCallExpr,
    function: hir::Function,
    db: &dyn hir::db::HirDatabase,
) -> crate::DispatchKind {
    let receiver_is_dyn = method_call
        .receiver()
        .and_then(|receiver| sema.type_of_expr(&receiver))
        .map(|info| {
            let receiver_ty = info.original;
            receiver_ty.as_dyn_trait().is_some()
                || receiver_ty
                    .autoderef(db)
                    .any(|candidate| candidate.as_dyn_trait().is_some())
        })
        .unwrap_or(false);
    if receiver_is_dyn {
        return crate::DispatchKind::Dyn;
    }
    if hir::AssocItem::Function(function)
        .container_or_implemented_trait(db)
        .is_some()
    {
        return crate::DispatchKind::ThroughTrait;
    }
    crate::DispatchKind::Direct
}

fn dispatch_kind_from_variant(variant: &str) -> Option<crate::DispatchKind> {
    match variant {
        "DIRECT" => Some(crate::DispatchKind::Direct),
        "THROUGH_TRAIT" => Some(crate::DispatchKind::ThroughTrait),
        "DYN" => Some(crate::DispatchKind::Dyn),
        "CLOSURE" => Some(crate::DispatchKind::Closure),
        "FN_POINTER" => Some(crate::DispatchKind::FnPointer),
        _ => None,
    }
}
