use raql_host::{CapabilityId, CapabilitySet};

const DAY_ONE_SUPPORTED_CAPABILITIES: &[&str] = &[
    "def",
    "def_name",
    "def_kind",
    "def_span",
    "def_path",
    "call_edge",
    "method_of",
    "field",
    "variant",
    "method",
    "trait_method",
    "implements",
    "from_impl",
    "fn_error_type",
    "fn_return_type",
    "ty_app",
    "ty_arg",
    "ty_ref",
    "ty_ptr",
    "ty_tuple",
    "ty_slice",
    "ty_param",
    "ty_prim",
    "ty_unknown",
    "typeref_id",
    "dispatch_str",
    "call_id",
    "impl_id",
    // TODO(ra-native-audit): `search` stays disabled until rebuilt from RA-native symbol/query
    // search rather than the current legacy name/path token index.
    // TODO(ra-native-audit): `ref_id`, `compares`, and `writes` stay disabled until rebuilt
    // from RA-native reference truth rather than the current approximate event summary.
    // TODO(ra-native-audit): `constructs`, `propagates`, `converts`, and `handles` stay
    // disabled until rebuilt from RA-native error semantics rather than the current
    // Result/From-shaped approximation.
    "node_at",
    "node_kind",
    "node_span",
    "node_parent",
    "enclosing_control",
    "node_id",
    "span_allowed",
    "is_public",
    "in_test",
    "handle",
    "span_key",
];

pub fn day_one_supported_capabilities() -> CapabilitySet {
    DAY_ONE_SUPPORTED_CAPABILITIES
        .iter()
        .copied()
        .map(CapabilityId::from)
        .collect()
}

pub fn supports_day_one_capability(predicate: &str) -> bool {
    DAY_ONE_SUPPORTED_CAPABILITIES.contains(&predicate)
}
