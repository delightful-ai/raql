use std::collections::BTreeMap;
use std::path::Path;

use base_db::SourceDatabase;
use camino::Utf8PathBuf;
use raql_host::SpanCoord;
use raql_host::{
    ExternLookupHostValue, ExternLookupHostValueKind, ExternLookupRequest, ExternLookupValue,
    SpanKey,
};
use span::EditionedFileId;
use syntax::{AstNode, SyntaxNode, SyntaxNodePtr, TextRange, TextSize};
use vfs::{AbsPathBuf, VfsPath};

use crate::provider::core_index::CoreLookupIndex;
use crate::provider::defs::{LocalFile, lookup_span_key_from_text};
use crate::{DeterministicRaHost, NodeId, NodeKind, SpanId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LookupNodeRecord {
    pub(crate) kind: NodeKind,
    pub(crate) span: SpanId,
    pub(crate) parent: Option<NodeId>,
}
pub(crate) fn extract_syntax_nodes(
    db: &ide::RootDatabase,
    files: &BTreeMap<vfs::FileId, LocalFile>,
    host: &mut DeterministicRaHost,
) {
    let sema = hir::Semantics::new(db);
    let files = files.clone();
    for (file_id, local) in files {
        let editioned_file = base_db::EditionedFileId::current_edition(db, file_id);
        let root = sema.parse(editioned_file).syntax().clone();
        record_syntax_node(
            host,
            &root,
            editioned_file.span_file_id(db),
            &local,
            None,
        );
    }
}

pub(crate) fn lookup_span_allowed_rows(
    request: &ExternLookupRequest,
    core_index: Option<&CoreLookupIndex>,
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
    if lookup_spans.contains_key(&span) || core_index.and_then(|index| index.span_key(span)).is_some() {
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
    core_index: Option<&CoreLookupIndex>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
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
    let key = if let Some(key) = lookup_spans.get(&span) {
        key.clone()
    } else if let Some(key) = core_index.and_then(|index| index.span_key(span)).cloned() {
        lookup_spans.entry(span).or_insert_with(|| key.clone());
        key
    } else {
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

pub(crate) fn lookup_node_at_rows(
    request: &ExternLookupRequest,
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    core_index: Option<&CoreLookupIndex>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    lookup_nodes: &mut BTreeMap<NodeId, LookupNodeRecord>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut span = None::<SpanId>;
    let mut node_filter = None::<Option<NodeId>>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Span =>
            {
                span = Some(SpanId::new(host.stable_id()));
            }
            (1, ExternLookupValue::None) => node_filter = Some(None),
            (1, ExternLookupValue::Some(inner)) => match inner.as_ref() {
                ExternLookupValue::Host(host) if host.kind() == ExternLookupHostValueKind::Node => {
                    node_filter = Some(Some(NodeId::new(host.stable_id())));
                }
                _ => return Vec::new(),
            },
            _ => return Vec::new(),
        }
    }
    let Some(span) = span else {
        return Vec::new();
    };
    let node = ensure_lookup_node_for_span(
        db,
        vfs,
        workspace_root,
        core_index,
        lookup_spans,
        lookup_nodes,
        span,
    );
    if node_filter.is_some_and(|expected| expected != node) {
        return Vec::new();
    }
    vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Span,
            span.stable_id(),
        )),
        option_node_lookup_value(node),
    ]]
}

pub(crate) fn lookup_node_kind_rows(
    request: &ExternLookupRequest,
    lookup_nodes: &BTreeMap<NodeId, LookupNodeRecord>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut node = None::<NodeId>;
    let mut kind_filter = None::<NodeKind>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Node =>
            {
                node = Some(NodeId::new(host.stable_id()));
            }
            (1, ExternLookupValue::Enum { name, variant }) if name.as_ref() == "NodeKind" => {
                kind_filter = node_kind_from_variant(variant.as_ref());
            }
            _ => return Vec::new(),
        }
    }
    let Some(node) = node else {
        return Vec::new();
    };
    let Some(record) = lookup_nodes.get(&node) else {
        return Vec::new();
    };
    let public_kind = public_node_kind(record.kind);
    if kind_filter.is_some_and(|expected| expected != public_kind) {
        return Vec::new();
    }
    vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Node,
            node.stable_id(),
        )),
        node_kind_lookup_value(public_kind),
    ]]
}

pub(crate) fn lookup_node_span_rows(
    request: &ExternLookupRequest,
    lookup_nodes: &BTreeMap<NodeId, LookupNodeRecord>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut node = None::<NodeId>;
    let mut span_filter = None::<SpanId>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Node =>
            {
                node = Some(NodeId::new(host.stable_id()));
            }
            (1, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Span =>
            {
                span_filter = Some(SpanId::new(host.stable_id()));
            }
            _ => return Vec::new(),
        }
    }
    let Some(node) = node else {
        return Vec::new();
    };
    let Some(record) = lookup_nodes.get(&node) else {
        return Vec::new();
    };
    if span_filter.is_some_and(|expected| expected != record.span) {
        return Vec::new();
    }
    vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Node,
            node.stable_id(),
        )),
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Span,
            record.span.stable_id(),
        )),
    ]]
}

pub(crate) fn lookup_node_parent_rows(
    request: &ExternLookupRequest,
    lookup_nodes: &BTreeMap<NodeId, LookupNodeRecord>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut node = None::<NodeId>;
    let mut parent_filter = None::<Option<NodeId>>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Node =>
            {
                node = Some(NodeId::new(host.stable_id()));
            }
            (1, ExternLookupValue::None) => parent_filter = Some(None),
            (1, ExternLookupValue::Some(inner)) => match inner.as_ref() {
                ExternLookupValue::Host(host) if host.kind() == ExternLookupHostValueKind::Node => {
                    parent_filter = Some(Some(NodeId::new(host.stable_id())));
                }
                _ => return Vec::new(),
            },
            _ => return Vec::new(),
        }
    }
    let Some(node) = node else {
        return Vec::new();
    };
    let Some(record) = lookup_nodes.get(&node) else {
        return Vec::new();
    };
    if parent_filter.is_some_and(|expected| expected != record.parent) {
        return Vec::new();
    }
    vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Node,
            node.stable_id(),
        )),
        option_node_lookup_value(record.parent),
    ]]
}

pub(crate) fn lookup_node_id_rows(
    request: &ExternLookupRequest,
    lookup_nodes: &BTreeMap<NodeId, LookupNodeRecord>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut node = None::<NodeId>;
    let mut handle_filter = None::<&str>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Node =>
            {
                node = Some(NodeId::new(host.stable_id()));
            }
            (1, ExternLookupValue::String(handle)) => handle_filter = Some(handle.as_ref()),
            _ => return Vec::new(),
        }
    }
    let Some(node) = node else {
        return Vec::new();
    };
    if !lookup_nodes.contains_key(&node) {
        return Vec::new();
    }
    let handle = format!("node_id:{:#018x}", node.stable_id().as_u64());
    if handle_filter.is_some_and(|expected| expected != handle) {
        return Vec::new();
    }
    vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Node,
            node.stable_id(),
        )),
        ExternLookupValue::String(handle.into_boxed_str()),
    ]]
}

pub(crate) fn lookup_enclosing_control_rows(
    request: &ExternLookupRequest,
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    core_index: Option<&CoreLookupIndex>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    lookup_nodes: &mut BTreeMap<NodeId, LookupNodeRecord>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut span = None::<SpanId>;
    let mut kind_filter = None::<NodeKind>;
    let mut control_span_filter = None::<SpanId>;
    let mut dist_filter = None::<i64>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Span =>
            {
                span = Some(SpanId::new(host.stable_id()));
            }
            (1, ExternLookupValue::Enum { name, variant }) if name.as_ref() == "NodeKind" => {
                kind_filter = node_kind_from_variant(variant.as_ref());
            }
            (2, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Span =>
            {
                control_span_filter = Some(SpanId::new(host.stable_id()));
            }
            (3, ExternLookupValue::Int(v)) => dist_filter = Some(*v),
            _ => return Vec::new(),
        }
    }
    let Some(span) = span else {
        return Vec::new();
    };
    let Some(mut current) = ensure_lookup_node_for_span(
        db,
        vfs,
        workspace_root,
        core_index,
        lookup_spans,
        lookup_nodes,
        span,
    ) else {
        return Vec::new();
    };
    let mut distance = 0_i64;
    loop {
        let Some(record) = lookup_nodes.get(&current) else {
            return Vec::new();
        };
        if is_control_kind(record.kind) {
            if kind_filter.is_some_and(|expected| expected != record.kind)
                || control_span_filter.is_some_and(|expected| expected != record.span)
                || dist_filter.is_some_and(|expected| expected != distance)
            {
                return Vec::new();
            }
            return vec![vec![
                ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Span,
                    span.stable_id(),
                )),
                node_kind_lookup_value(record.kind),
                ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Span,
                    record.span.stable_id(),
                )),
                ExternLookupValue::Int(distance),
            ]];
        }
        let Some(parent) = record.parent else {
            return Vec::new();
        };
        current = parent;
        distance += 1;
    }
}

fn ensure_lookup_node_for_span(
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    core_index: Option<&CoreLookupIndex>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    lookup_nodes: &mut BTreeMap<NodeId, LookupNodeRecord>,
    span: SpanId,
) -> Option<NodeId> {
    let query_key = lookup_spans
        .get(&span)
        .cloned()
        .or_else(|| core_index.and_then(|index| index.span_key(span)).cloned())?;
    lookup_spans.entry(span).or_insert_with(|| query_key.clone());
    let (editioned_file, source_text, query_range) =
        lookup_file_and_range(db, vfs, workspace_root, &query_key)?;
    let sema = hir::Semantics::new(db);
    let root = sema.parse(editioned_file).syntax().clone();
    let mut temp_host = DeterministicRaHost::new();
    let mut candidates = Vec::new();
    collect_containing_nodes(
        &root,
        &query_range,
        query_key.rel_path(),
        source_text.as_str(),
        editioned_file.span_file_id(db),
        &mut temp_host,
        &mut candidates,
    );
    candidates.sort_by(|a, b| {
        span_specificity(&a.1)
            .cmp(&span_specificity(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let (node, _, _) = candidates.into_iter().next()?;
    cache_lookup_node_lineage(
        node,
        query_key.rel_path(),
        source_text.as_str(),
        editioned_file.span_file_id(db),
        lookup_spans,
        lookup_nodes,
    )
}

fn lookup_file_and_range(
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    key: &SpanKey,
) -> Option<(base_db::EditionedFileId, String, TextRange)> {
    let abs_path = workspace_root.join(key.rel_path());
    let utf8 = Utf8PathBuf::from_path_buf(abs_path).ok()?;
    let vfs_path = VfsPath::from(AbsPathBuf::assert(utf8));
    let (file_id, excluded) = vfs.file_id(&vfs_path)?;
    if matches!(excluded, vfs::FileExcluded::Yes) {
        return None;
    }
    let text = db.file_text(file_id).text(db).to_string();
    let start = offset_for_coord(text.as_str(), key.start())?;
    let end = offset_for_coord(text.as_str(), key.end())?;
    let range = TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32));
    Some((
        base_db::EditionedFileId::current_edition(db, file_id),
        text,
        range,
    ))
}

fn offset_for_coord(source_text: &str, coord: SpanCoord) -> Option<usize> {
    let mut line = 0_u32;
    let mut column = 0_u32;
    for (offset, ch) in source_text.char_indices() {
        if line == coord.line() && column == coord.column() {
            return Some(offset);
        }
        if ch == '\n' {
            line = line.saturating_add(1);
            column = 0;
        } else {
            column = column.saturating_add(1);
        }
    }
    (line == coord.line() && column == coord.column()).then_some(source_text.len())
}

fn collect_containing_nodes(
    node: &SyntaxNode,
    query_range: &TextRange,
    rel_path: &str,
    source_text: &str,
    file_id: EditionedFileId,
    id_host: &mut DeterministicRaHost,
    out: &mut Vec<(SyntaxNode, SpanKey, String)>,
) {
    let range = node.text_range();
    if range.start() > query_range.start() || range.end() < query_range.end() {
        return;
    }
    let Some(span_key) = lookup_span_key_from_text(rel_path, source_text, range) else {
        return;
    };
    let node_id = id_host.intern_node_from_syntax_ptr(SyntaxNodePtr::new(node));
    let stable = id_host.node_id(node_id).as_str().to_string();
    out.push((node.clone(), span_key, stable));
    for child in node.children() {
        collect_containing_nodes(
            &child,
            query_range,
            rel_path,
            source_text,
            file_id,
            id_host,
            out,
        );
    }
}

fn cache_lookup_node_lineage(
    node: SyntaxNode,
    rel_path: &str,
    source_text: &str,
    file_id: EditionedFileId,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    lookup_nodes: &mut BTreeMap<NodeId, LookupNodeRecord>,
) -> Option<NodeId> {
    let mut id_host = DeterministicRaHost::new();
    let lineage = node.ancestors().collect::<Vec<_>>();
    let mut parent = None::<NodeId>;
    let mut current = None::<NodeId>;
    for syntax in lineage.into_iter().rev() {
        let range = syntax.text_range();
        let span = id_host
            .intern_span_from_text(file_id, rel_path.to_string(), source_text, range)
            .ok()?;
        let span_key = lookup_span_key_from_text(rel_path, source_text, range)?;
        lookup_spans.entry(span).or_insert(span_key);
        let node_id = id_host.intern_node_from_syntax_ptr(SyntaxNodePtr::new(&syntax));
        lookup_nodes.insert(
            node_id,
            LookupNodeRecord {
                kind: syntax_node_kind(&syntax),
                span,
                parent,
            },
        );
        parent = Some(node_id);
        current = Some(node_id);
    }
    current
}

fn node_kind_lookup_value(kind: NodeKind) -> ExternLookupValue {
    let variant = match public_node_kind(kind) {
        NodeKind::If => "IF",
        NodeKind::Match => "MATCH",
        NodeKind::While => "WHILE",
        NodeKind::For => "FOR",
        NodeKind::Loop => "LOOP",
        NodeKind::Block => "BLOCK",
        NodeKind::Try => "TRY",
        NodeKind::Arm => "ARM",
        NodeKind::Other => "OTHER",
        NodeKind::Expr | NodeKind::Item => unreachable!("public_node_kind collapses syntax-only variants"),
    };
    ExternLookupValue::Enum {
        name: "NodeKind".into(),
        variant: variant.into(),
    }
}

fn public_node_kind(kind: NodeKind) -> NodeKind {
    match kind {
        NodeKind::Expr | NodeKind::Item => NodeKind::Other,
        other => other,
    }
}

fn node_kind_from_variant(variant: &str) -> Option<NodeKind> {
    match variant {
        "IF" => Some(NodeKind::If),
        "MATCH" => Some(NodeKind::Match),
        "WHILE" => Some(NodeKind::While),
        "FOR" => Some(NodeKind::For),
        "LOOP" => Some(NodeKind::Loop),
        "BLOCK" => Some(NodeKind::Block),
        "TRY" => Some(NodeKind::Try),
        "ARM" => Some(NodeKind::Arm),
        "OTHER" => Some(NodeKind::Other),
        "EXPR" => Some(NodeKind::Expr),
        "ITEM" => Some(NodeKind::Item),
        _ => None,
    }
}

fn option_node_lookup_value(node: Option<NodeId>) -> ExternLookupValue {
    match node {
        Some(node) => ExternLookupValue::Some(Box::new(ExternLookupValue::Host(
            ExternLookupHostValue::new(ExternLookupHostValueKind::Node, node.stable_id()),
        ))),
        None => ExternLookupValue::None,
    }
}

fn is_control_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::If | NodeKind::Match | NodeKind::While | NodeKind::For | NodeKind::Loop
    )
}

fn span_specificity(span: &SpanKey) -> (u32, u32, u32, u32) {
    let start = span.start();
    let end = span.end();
    let line_width = end.line().saturating_sub(start.line());
    let col_width = if line_width == 0 {
        end.column().saturating_sub(start.column())
    } else {
        u32::MAX
    };
    (line_width, col_width, start.line(), start.column())
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
