use std::collections::BTreeMap;

use raql_host::{
    ExternLookupHostValue, ExternLookupHostValueKind, ExternLookupRequest, ExternLookupValue,
    SpanKey,
};

use crate::SpanId;

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
