//! Predicate declarations and their completeness contract (SPEC §4.3, §8.1).

use crate::mode::{AccessKind, ModeDef, Pattern};
use crate::schema::ArgDef;

/// Completeness class of an extern predicate (SPEC §4.3). There is no fourth
/// class: "approximately complete" is `Disabled` until it is honest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completeness {
    /// Complete with respect to RA's semantic model for the declared scope.
    RaExact,
    /// As complete as RA's name/type resolution; unresolvable sites are
    /// absent, and the named caveats document exactly what is absent.
    RaResolved { caveats: &'static [&'static str] },
    /// The honest implementation doesn't exist yet; querying it is a
    /// compile-time capability error naming the predicate.
    Disabled,
}

/// One extern predicate (SPEC §8.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PredicateDef {
    pub name: &'static str,
    pub args: &'static [ArgDef],
    pub doc: &'static str,
    pub completeness: Completeness,
    pub modes: &'static [ModeDef],
}

impl PredicateDef {
    pub fn arity(&self) -> usize {
        self.args.len()
    }

    pub fn is_disabled(&self) -> bool {
        matches!(self.completeness, Completeness::Disabled)
    }

    /// The declared modes whose `+` set is a subset of `bound` (SPEC §8.2:
    /// a goal is plannable if the bound set is a superset of some declared
    /// mode's `+` set), cheapest first; scans always sort last (SPEC §10.2).
    pub fn satisfiable_modes(&self, bound: &Pattern) -> Vec<&'static ModeDef> {
        assert_eq!(bound.arity(), self.arity(), "binding pattern arity mismatch");
        let mut modes: Vec<&'static ModeDef> = self
            .modes
            .iter()
            .filter(|mode| mode.satisfied_by(bound))
            .collect();
        modes.sort_by_key(|mode| (mode.access == AccessKind::Scan, mode.cost));
        modes
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::v0_catalog;
    use crate::mode::{Binding, Pattern};
    use crate::operator::OperatorId;

    #[test]
    fn def_name_mode_selection_is_binding_aware() {
        let def_name = v0_catalog().predicate("def_name").expect("def_name in catalog");

        // Def bound: the C0 projection wins.
        let modes = def_name.satisfiable_modes(&Pattern::from(vec![true, false]));
        assert_eq!(modes[0].operator, OperatorId::NameOfDef);

        // Name bound: the C2 seed wins; the scan is satisfiable but always
        // sorts last (SPEC §10.2).
        let modes = def_name.satisfiable_modes(&Pattern::from(vec![false, true]));
        assert_eq!(
            modes.iter().map(|m| m.operator).collect::<Vec<_>>(),
            vec![OperatorId::DefsByExactName, OperatorId::DefNamesScan],
        );

        // Both bound: every mode satisfiable, cheapest first.
        let modes = def_name.satisfiable_modes(&Pattern::from(vec![true, true]));
        assert_eq!(modes[0].operator, OperatorId::NameOfDef);

        // Nothing bound: only the declared scan applies — a scan is never
        // inferred, but a declared one is a legal (visible) access path.
        let modes = def_name.satisfiable_modes(&Pattern::from(vec![false, false]));
        assert_eq!(
            modes.iter().map(|m| m.operator).collect::<Vec<_>>(),
            vec![OperatorId::DefNamesScan],
        );
    }

    #[test]
    fn unknown_predicates_do_not_exist() {
        assert!(v0_catalog().predicate("search").is_none());
        assert!(v0_catalog().predicate("world_stamp").is_none());
    }

    #[test]
    fn mode_patterns_never_use_bound_wildcards() {
        // Every declared pattern spells out each position (SPEC §8.2). A
        // pure filter (every mode all-bound: `is_public(+)`) is legitimate;
        // an all-bound mode next to a freer mode is a redundant membership
        // test, which the freer mode already answers.
        for predicate in v0_catalog().entries() {
            let pure_filter = predicate
                .modes
                .iter()
                .all(|mode| mode.pattern.iter().all(|b| *b == Binding::Bound));
            for mode in predicate.modes {
                assert_eq!(mode.pattern.len(), predicate.arity());
                assert!(
                    pure_filter || mode.pattern.contains(&Binding::Free),
                    "`{}` declares an all-bound mode alongside freer modes — \
                     a redundant membership test",
                    predicate.name,
                );
            }
        }
    }

    #[test]
    fn filters_and_scans_are_declared() {
        let catalog = v0_catalog();
        // Scans exist only where SPEC §8.6 declares them.
        let scan_predicates: Vec<&str> = catalog
            .entries()
            .iter()
            .filter(|p| p.modes.iter().any(|m| m.access == crate::mode::AccessKind::Scan))
            .map(|p| p.name)
            .collect();
        assert_eq!(scan_predicates, vec!["def_name", "def", "fn_def", "call_edge"]);

        // The roadmap families are present but disabled: no modes.
        for name in ["constructs", "propagates", "converts", "handles", "compares", "writes"] {
            let predicate = catalog.predicate(name).expect("roadmap entry exists");
            assert!(predicate.is_disabled());
            assert!(predicate.modes.is_empty());
        }
    }
}
