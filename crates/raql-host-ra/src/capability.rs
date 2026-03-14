use raql_host::{CapabilityId, CapabilitySet};

const DAY_ONE_SUPPORTED_CAPABILITIES: &[&str] = &[
    "def",
    "search",
    "def_name",
    "def_kind",
    "def_span",
    "def_path",
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
