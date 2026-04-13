use std::collections::HashMap;

use hir::{Adt, HasSource};
use syntax::ast::HasName;
use syntax::{ast, AstNode};

use crate::provider::defs::LocalFile;
use crate::{DefId, SpanId};

pub(crate) trait CallGraphProvider {
    fn local_file(&self, file_id: vfs::FileId) -> Option<LocalFile>;
    fn register_function_def_for_call_graph(&mut self, function: hir::Function) -> Option<DefId>;
    fn register_adt_def_for_call_graph(&mut self, adt: Adt) -> DefId;
    fn register_variant_def_for_call_graph(&mut self, variant: hir::Variant) -> Option<DefId>;
    fn register_synthetic_callable_for_call_graph(
        &mut self,
        prefix: &str,
        syntax: &syntax::SyntaxNode,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) -> DefId;
    fn intern_call_site_for_call_graph(
        &mut self,
        file_id: span::EditionedFileId,
        rel_path: String,
        source_text: &str,
        range: syntax::TextRange,
    ) -> Option<SpanId>;
    fn insert_call_id_for_call_graph(&mut self, call_id: crate::CallId, value: String);
    fn insert_call_edge_for_call_graph(
        &mut self,
        caller: DefId,
        callee: DefId,
        site: SpanId,
        dispatch: crate::DispatchKind,
    );
}

pub(crate) fn extract_call_edges<P: CallGraphProvider>(
    provider: &mut P,
    db: &ide::RootDatabase,
    local_functions: &[hir::Function],
) {
    let sema = hir::Semantics::new(db);
    let local_functions = local_functions.to_vec();
    let mut parsed_by_file = HashMap::new();
    for function in local_functions {
        let Some(source) = function.source(db) else {
            continue;
        };
        let editioned = source.file_id.original_file(db);
        let Some(local) = provider.local_file(editioned.file_id(db)) else {
            continue;
        };
        let parsed = parsed_by_file
            .entry(source.file_id)
            .or_insert_with(|| sema.parse_or_expand(source.file_id));
        let lookup_offset = source
            .value
            .name()
            .map(|name| name.syntax().text_range().start())
            .unwrap_or_else(|| source.value.syntax().text_range().start());
        let Some(ast_fn) = sema.find_node_at_offset_with_descend::<ast::Fn>(parsed, lookup_offset)
        else {
            continue;
        };
        let Some(caller_def) = provider.register_function_def_for_call_graph(function) else {
            continue;
        };
        let Some(body) = ast_fn.body() else {
            continue;
        };
        let owner_item = ast::Item::Fn(ast_fn.clone());
        for callable_expr in body.syntax().descendants().filter_map(ast::CallableExpr::cast) {
            if !belongs_to_item(callable_expr.syntax(), owner_item.syntax()) {
                continue;
            }
            match callable_expr {
                ast::CallableExpr::Call(call) => {
                    record_call_expr(
                        provider,
                        db,
                        &sema,
                        caller_def,
                        &call,
                        editioned.editioned_file_id(db),
                        &local,
                    );
                }
                ast::CallableExpr::MethodCall(method_call) => {
                    record_method_call(
                        provider,
                        db,
                        &sema,
                        caller_def,
                        &method_call,
                        editioned.editioned_file_id(db),
                        &local,
                    );
                }
            }
        }
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

pub(crate) fn dispatch_kind_from_variant(variant: &str) -> Option<crate::DispatchKind> {
    match variant {
        "DIRECT" => Some(crate::DispatchKind::Direct),
        "THROUGH_TRAIT" => Some(crate::DispatchKind::ThroughTrait),
        "DYN" => Some(crate::DispatchKind::Dyn),
        "CLOSURE" => Some(crate::DispatchKind::Closure),
        "FN_POINTER" => Some(crate::DispatchKind::FnPointer),
        _ => None,
    }
}

fn record_call_expr<P: CallGraphProvider>(
    provider: &mut P,
    db: &ide::RootDatabase,
    sema: &hir::Semantics<'_, ide::RootDatabase>,
    caller_def: DefId,
    call: &ast::CallExpr,
    file_id: span::EditionedFileId,
    local: &LocalFile,
) {
    let Some(callee_expr) = call.expr() else {
        return;
    };
    let Some(type_info) = sema.type_of_expr(&callee_expr) else {
        return;
    };
    let Some(callable) = type_info.original.as_callable(db) else {
        return;
    };
    let Some((callee_def, dispatch)) =
        call_target_from_callable(provider, &callable, &callee_expr, file_id, local)
    else {
        return;
    };
    let Some(site) = provider.intern_call_site_for_call_graph(
        file_id,
        local.rel_path.clone(),
        local.text.as_str(),
        call.syntax().text_range(),
    ) else {
        return;
    };
    let range = call.syntax().text_range();
    let token = format!(
        "{}:{}..{}",
        local.rel_path,
        u32::from(range.start()),
        u32::from(range.end())
    );
    let call_id = crate::CallId::new(crate::deterministic_stable_id("call", token.as_str()));
    provider.insert_call_id_for_call_graph(call_id, format!("call:{token}"));
    provider.insert_call_edge_for_call_graph(caller_def, callee_def, site, dispatch);
}

fn record_method_call<P: CallGraphProvider>(
    provider: &mut P,
    db: &ide::RootDatabase,
    sema: &hir::Semantics<'_, ide::RootDatabase>,
    caller_def: DefId,
    method_call: &ast::MethodCallExpr,
    file_id: span::EditionedFileId,
    local: &LocalFile,
) {
    let Some(function) = sema.resolve_method_call(method_call) else {
        return;
    };
    let Some(callee_def) = provider.register_function_def_for_call_graph(function) else {
        return;
    };
    let dispatch = method_dispatch_kind(sema, method_call, function, db);
    let Some(site) = provider.intern_call_site_for_call_graph(
        file_id,
        local.rel_path.clone(),
        local.text.as_str(),
        method_call.syntax().text_range(),
    ) else {
        return;
    };
    let range = method_call.syntax().text_range();
    let token = format!(
        "{}:{}..{}",
        local.rel_path,
        u32::from(range.start()),
        u32::from(range.end())
    );
    let call_id = crate::CallId::new(crate::deterministic_stable_id("call", token.as_str()));
    provider.insert_call_id_for_call_graph(call_id, format!("call:{token}"));
    provider.insert_call_edge_for_call_graph(caller_def, callee_def, site, dispatch);
}

fn call_target_from_callable<P: CallGraphProvider>(
    provider: &mut P,
    callable: &hir::Callable<'_>,
    callee_expr: &ast::Expr,
    file_id: span::EditionedFileId,
    local: &LocalFile,
) -> Option<(DefId, crate::DispatchKind)> {
    match callable.kind() {
        hir::CallableKind::Function(function) => provider
            .register_function_def_for_call_graph(function)
            .map(|def| (def, crate::DispatchKind::Direct)),
        hir::CallableKind::TupleStruct(strukt) => Some((
            provider.register_adt_def_for_call_graph(Adt::Struct(strukt)),
            crate::DispatchKind::Direct,
        )),
        hir::CallableKind::TupleEnumVariant(variant) => provider
            .register_variant_def_for_call_graph(variant)
            .map(|def| (def, crate::DispatchKind::Direct)),
        hir::CallableKind::Closure(_) => Some((
            provider.register_synthetic_callable_for_call_graph(
                "closure",
                callee_expr.syntax(),
                file_id,
                local,
            ),
            crate::DispatchKind::Closure,
        )),
        hir::CallableKind::FnPtr | hir::CallableKind::FnImpl(_) => Some((
            provider.register_synthetic_callable_for_call_graph(
                "fn_pointer",
                callee_expr.syntax(),
                file_id,
                local,
            ),
            crate::DispatchKind::FnPointer,
        )),
    }
}

fn belongs_to_item(node: &syntax::SyntaxNode, owner_item: &syntax::SyntaxNode) -> bool {
    node.ancestors()
        .find_map(ast::Item::cast)
        .is_some_and(|item| item.syntax() == owner_item)
}
