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

/// Which arguments of a goal are bound at some point (SPEC §8.2): the
/// observed binding state the planner keys on — demand-specialization
/// keys, derived support sets, satisfiability checks. Distinct from
/// [`ModeDef::pattern`], which is a *declared requirement* over
/// [`Binding`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pattern(Vec<bool>);

impl Pattern {
    pub fn new(bound: Vec<bool>) -> Pattern {
        Pattern(bound)
    }

    /// Lower a declared `Binding` sequence: `Bound` positions are bound.
    pub fn from_bindings(bindings: &[Binding]) -> Pattern {
        Pattern(bindings.iter().map(|b| *b == Binding::Bound).collect())
    }

    /// Every pattern of the given arity, in deterministic order.
    pub fn all(arity: usize) -> Vec<Pattern> {
        assert!(arity <= 16, "predicate arity out of range for pattern enumeration");
        (0..(1usize << arity))
            .map(|mask| Pattern((0..arity).map(|i| mask & (1 << i) != 0).collect()))
            .collect()
    }

    pub fn arity(&self) -> usize {
        self.0.len()
    }

    pub fn is_bound(&self, index: usize) -> bool {
        self.0[index]
    }

    pub fn iter(&self) -> impl Iterator<Item = bool> + '_ {
        self.0.iter().copied()
    }

    /// Nothing bound (and there is something to bind): calling a derived
    /// predicate under this pattern is a scan of it (SPEC §9.2).
    pub fn is_unseeded(&self) -> bool {
        !self.0.is_empty() && self.0.iter().all(|bound| !*bound)
    }

    /// `∀i: self[i] → other[i]` — every argument this pattern binds is
    /// bound in `other` (SPEC §8.2 satisfiability direction).
    pub fn subset_of(&self, other: &Pattern) -> bool {
        self.0.iter().zip(&other.0).all(|(s, o)| !*s || *o)
    }

    /// `(+,-)` rendering, as used in explain output and plan errors.
    pub fn render(&self) -> String {
        let inner = self
            .0
            .iter()
            .map(|bound| if *bound { "+" } else { "-" })
            .collect::<Vec<_>>()
            .join(",");
        format!("({inner})")
    }
}

impl From<Vec<bool>> for Pattern {
    fn from(bound: Vec<bool>) -> Pattern {
        Pattern(bound)
    }
}

impl FromIterator<bool> for Pattern {
    fn from_iter<I: IntoIterator<Item = bool>>(iter: I) -> Pattern {
        Pattern(iter.into_iter().collect())
    }
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
    /// Completeness caveats of *this access path* beyond the predicate's
    /// own (SPEC §8.4) — e.g. a seed index that cannot surface some of the
    /// predicate's domain. Empty for most modes.
    pub caveats: &'static [&'static str],
}

impl ModeDef {
    /// Whether this mode's `+` set is a subset of `bound` (SPEC §8.2).
    pub(crate) fn satisfied_by(&self, bound: &Pattern) -> bool {
        self.pattern
            .iter()
            .zip(bound.iter())
            .all(|(binding, is_bound)| *binding == Binding::Free || is_bound)
    }
}
