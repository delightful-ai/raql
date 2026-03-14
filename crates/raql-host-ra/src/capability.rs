use raql_host::{CapabilityId, CapabilitySet};

const DAY_ONE_SUPPORTED_CAPABILITIES: &[&str] = &[
    "def",
    "def_name",
    "def_kind",
    "def_span",
    "def_path",
    "method_of",
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
