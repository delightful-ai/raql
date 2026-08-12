//! Binding modes and access paths (SPEC §8.2, §8.3, §8.5).

use crate::operator::OperatorId;

/// Declared asymptotic cost band of an access path (SPEC §8.3). Honest bands
/// for planning and explain output, not measurements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CostClass {
    /// O(1) projection of an already-interned value.
    C0,
    /// Definition-local work: one body/item traversal, one def-map path walk.
    C1,
    /// Name-bounded search: symbol index or text-prefiltered reference search.
    C2,
    /// Crate-wide enumeration.
    C3,
    /// Workspace-wide enumeration or scan composition.
    C4,
}

impl CostClass {
    pub fn name(self) -> &'static str {
        match self {
            CostClass::C0 => "C0",
            CostClass::C1 => "C1",
            CostClass::C2 => "C2",
            CostClass::C3 => "C3",
            CostClass::C4 => "C4",
        }
    }
}

/// Binding state of one argument position within a mode (SPEC §8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// `+` — must be bound when the goal runs.
    Bound,
    /// `-` — the operator binds it.
    Free,
}

/// Whether an access path is a keyed lookup or an explicit enumeration
/// (SPEC §8.5: scans are declared modes, never inferred, never a fallback).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    Keyed,
    Scan,
}

/// One supported binding mode of a predicate (SPEC §8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeDef {
    /// One entry per predicate argument, in declaration order.
    pub pattern: &'static [Binding],
    pub cost: CostClass,
    pub access: AccessKind,
    pub operator: OperatorId,
    /// RA primitives this access path is built on (capabilities output).
    pub ra_primitives: &'static [&'static str],
}

impl ModeDef {
    /// Whether this mode's `+` set is a subset of `bound` (SPEC §8.2).
    pub(crate) fn satisfied_by(&self, bound: &[bool]) -> bool {
        self.pattern
            .iter()
            .zip(bound)
            .all(|(binding, is_bound)| *binding == Binding::Free || *is_bound)
    }
}
