use std::collections::BTreeMap;

use raql_host::{
    ExternLookupHostValue, ExternLookupHostValueKind, ExternLookupRequest, ExternLookupValue,
    SpanKey,
};
use syntax::AstNode;

use crate::provider::defs::LocalFile;
use crate::{DeterministicRaHost, NodeId, NodeKind, SpanId};
pub(crate) fn extract_syntax_nodes(
    db: &ide::RootDatabase,
    files: &BTreeMap<vfs::FileId, LocalFile>,
    host: &mut DeterministicRaHost,
) {
    let sema = hir::Semantics::new(db);
    let files = files.clone();
    for (file_id, local) in files {
        let editioned_file = base_db::EditionedFileId::current_edition_guess_origin(db, file_id);
        let root = sema.parse(editioned_file).syntax().clone();
        record_syntax_node(
            host,
            &root,
            editioned_file.editioned_file_id(db),
            &local,
            None,
        );
    }
}

pub(crate) fn lookup_span_allowed_rows(
    request: &ExternLookupRequest,
    lookup_spans: &BTreeMap<SpanId, SpanKey>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut span = None::<SpanId>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Span =>
            {
                span = Some(SpanId::new(host.stable_id()));
            }
            _ => return Vec::new(),
        }
    }
    let Some(span) = span else {
        return Vec::new();
    };
    if lookup_spans.contains_key(&span) {
        vec![vec![ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Span,
            span.stable_id(),
        ))]]
    } else {
        Vec::new()
    }
}

pub(crate) fn lookup_span_key_rows(
    request: &ExternLookupRequest,
    lookup_spans: &BTreeMap<SpanId, SpanKey>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut span = None::<SpanId>;
    let mut rel_path_filter = None::<&str>;
    let mut l0_filter = None::<i64>;
    let mut c0_filter = None::<i64>;
    let mut l1_filter = None::<i64>;
    let mut c1_filter = None::<i64>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Span =>
            {
                span = Some(SpanId::new(host.stable_id()));
            }
            (1, ExternLookupValue::String(path)) => rel_path_filter = Some(path.as_ref()),
            (2, ExternLookupValue::Int(v)) => l0_filter = Some(*v),
            (3, ExternLookupValue::Int(v)) => c0_filter = Some(*v),
            (4, ExternLookupValue::Int(v)) => l1_filter = Some(*v),
            (5, ExternLookupValue::Int(v)) => c1_filter = Some(*v),
            _ => return Vec::new(),
        }
    }
    let Some(span) = span else {
        return Vec::new();
    };
    let Some(key) = lookup_spans.get(&span) else {
        return Vec::new();
    };
    if rel_path_filter.is_some_and(|expected| expected != key.rel_path())
        || l0_filter.is_some_and(|expected| expected != key.start().line() as i64)
        || c0_filter.is_some_and(|expected| expected != key.start().column() as i64)
        || l1_filter.is_some_and(|expected| expected != key.end().line() as i64)
        || c1_filter.is_some_and(|expected| expected != key.end().column() as i64)
    {
        return Vec::new();
    }
    vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Span,
            span.stable_id(),
        )),
        ExternLookupValue::String(key.rel_path().to_string().into_boxed_str()),
        ExternLookupValue::Int(key.start().line() as i64),
        ExternLookupValue::Int(key.start().column() as i64),
        ExternLookupValue::Int(key.end().line() as i64),
        ExternLookupValue::Int(key.end().column() as i64),
    ]]
}

fn record_syntax_node(
    host: &mut DeterministicRaHost,
    node: &syntax::SyntaxNode,
    file_id: span::EditionedFileId,
    local: &LocalFile,
    parent: Option<NodeId>,
) {
    let range = node.text_range();
    let Ok(span) =
        host.intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range)
    else {
        for child in node.children() {
            record_syntax_node(host, &child, file_id, local, parent);
        }
        return;
    };
    let token = format!(
        "{}:{}..{}:{:?}",
        local.rel_path,
        u32::from(range.start()),
        u32::from(range.end()),
        node.kind()
    );
    let node_id = NodeId::new(crate::deterministic_stable_id("node", token.as_str()));
    host.insert_node_id(node_id, format!("node:{token}"));
    host.insert_node(node_id, syntax_node_kind(node), span, parent);
    for child in node.children() {
        record_syntax_node(host, &child, file_id, local, Some(node_id));
    }
}

fn syntax_node_kind(node: &syntax::SyntaxNode) -> NodeKind {
    if syntax::ast::IfExpr::cast(node.clone()).is_some() {
        NodeKind::If
    } else if syntax::ast::MatchExpr::cast(node.clone()).is_some() {
        NodeKind::Match
    } else if syntax::ast::WhileExpr::cast(node.clone()).is_some() {
        NodeKind::While
    } else if syntax::ast::ForExpr::cast(node.clone()).is_some() {
        NodeKind::For
    } else if syntax::ast::LoopExpr::cast(node.clone()).is_some() {
        NodeKind::Loop
    } else if syntax::ast::BlockExpr::cast(node.clone()).is_some() {
        NodeKind::Block
    } else if syntax::ast::TryExpr::cast(node.clone()).is_some() {
        NodeKind::Try
    } else if syntax::ast::MatchArm::cast(node.clone()).is_some() {
        NodeKind::Arm
    } else if syntax::ast::Item::cast(node.clone()).is_some() {
        NodeKind::Item
    } else if syntax::ast::Expr::cast(node.clone()).is_some() {
        NodeKind::Expr
    } else {
        NodeKind::Other
    }
}
