use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use hir::{HasSource, ModuleDef};
use ide_db::search::ReferenceCategory;
use raql_host::{ExternLookupHostValue, ExternLookupHostValueKind, ExternLookupValue, SpanKey};
use syntax::{ast, AstNode};

use crate::provider::defs::{
    LocalFile, LookupDefRecord, ensure_lookup_function_def, ensure_lookup_synthetic_callable_def,
    lookup_local_file, lookup_span_key_from_text,
};
use crate::{DefId, DeterministicRaHost, SpanId};

pub(crate) struct CallLookupFilters {
    pub(crate) peer_filter: Option<DefId>,
    pub(crate) site_filter: Option<SpanId>,
    pub(crate) dispatch_filter: Option<crate::DispatchKind>,
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
