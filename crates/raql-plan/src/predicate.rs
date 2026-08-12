//! Predicate declarations and their completeness contract (SPEC §4.3, §8.1).

use crate::mode::{AccessKind, ModeDef};
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
    pub fn satisfiable_modes(&self, bound: &[bool]) -> Vec<&'static ModeDef> {
        assert_eq!(bound.len(), self.arity(), "binding vector arity mismatch");
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
    use crate::mode::Binding;
    use crate::operator::OperatorId;

    #[test]
    fn def_name_mode_selection_is_binding_aware() {
        let def_name = v0_catalog().predicate("def_name").expect("def_name in catalog");

        // Def bound: the C0 projection wins.
        let modes = def_name.satisfiable_modes(&[true, false]);
        assert_eq!(modes[0].operator, OperatorId::NameOfDef);

        // Name bound: the C2 seed is the only satisfiable mode.
        let modes = def_name.satisfiable_modes(&[false, true]);
        assert_eq!(
            modes.iter().map(|m| m.operator).collect::<Vec<_>>(),
            vec![OperatorId::DefsByExactName],
        );

        // Both bound: every mode satisfiable, cheapest first.
        let modes = def_name.satisfiable_modes(&[true, true]);
        assert_eq!(modes[0].operator, OperatorId::NameOfDef);

        // Nothing bound: no keyed mode applies and no scan is declared yet,
        // so the goal is unplannable (RAQL0301 territory, SPEC §10.3).
        assert!(def_name.satisfiable_modes(&[false, false]).is_empty());
    }

    #[test]
    fn unknown_predicates_do_not_exist() {
        assert!(v0_catalog().predicate("search").is_none());
        assert!(v0_catalog().predicate("world_stamp").is_none());
    }

    #[test]
    fn mode_patterns_never_use_bound_wildcards() {
        // Every declared pattern spells out each position (SPEC §8.2).
        for predicate in v0_catalog().entries() {
            for mode in predicate.modes {
                assert_eq!(mode.pattern.len(), predicate.arity());
                assert!(
                    mode.pattern.iter().any(|b| *b == Binding::Free) || predicate.arity() == 0,
                    "`{}` declares a mode binding every argument and freeing none — \
                     that is a membership test, which every keyed mode already answers",
                    predicate.name,
                );
            }
        }
    }
}
