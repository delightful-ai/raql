//! The catalog extern predicates this host can answer.
//!
//! A capability is a catalog name (SPEC §8.1), and the set below is
//! exactly the set `raql_ra::SnapshotOperators` implements operators for.
//! Catalog entries whose completeness is `Disabled` are absent on purpose:
//! demanding one fails at plan time (RAQL0302), and this gate is the
//! daemon's backstop for anything that reaches execution anyway.
//!
//! When `raql-ra` grows an operator family, add its catalog name here in
//! the same change — a supported set wider than the operator dispatch is a
//! lie the daemon would report to clients.

use raql_host::{CapabilityId, CapabilitySet};

const SUPPORTED_CATALOG_PREDICATES: &[&str] = &[
    // Definition identity + projections
    "def",
    "def_at",
    "def_kind",
    "def_name",
    "def_path",
    "def_span",
    "fn_def",
    // Call family
    "call_edge",
    "callee",
    "caller",
    // Filters
    "in_test",
    "is_public",
    "span_allowed",
    // Output-boundary predicates
    "handle",
    "span_key",
];

pub fn supported_capabilities() -> CapabilitySet {
    SUPPORTED_CATALOG_PREDICATES
        .iter()
        .copied()
        .map(CapabilityId::from)
        .collect()
}
