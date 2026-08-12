//! `raql capabilities` output, mechanically derived from the catalog
//! (SPEC §8.1: no handwritten capability list exists anywhere).

use std::fmt::Write;

use crate::catalog::Catalog;
use crate::mode::{AccessKind, Binding};
use crate::predicate::Completeness;

impl Catalog {
    /// Per-predicate status, modes, costs, and RA primitives.
    pub fn capabilities_text(&self) -> String {
        let mut out = String::new();
        for predicate in self.entries() {
            let status = match predicate.completeness {
                Completeness::RaExact => "ra_exact".to_owned(),
                Completeness::RaResolved { caveats } => {
                    if caveats.is_empty() {
                        "ra_resolved".to_owned()
                    } else {
                        format!("ra_resolved ({})", caveats.join(", "))
                    }
                }
                Completeness::Disabled => "disabled".to_owned(),
            };
            let args = predicate
                .args
                .iter()
                .map(|arg| format!("{}: {}", arg.name, arg.ty.name()))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(out, "{}({args}) — {status}", predicate.name).unwrap();
            for mode in predicate.modes {
                let pattern = mode
                    .pattern
                    .iter()
                    .map(|b| match b {
                        Binding::Bound => "+",
                        Binding::Free => "-",
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let scan = match mode.access {
                    AccessKind::Scan => " scan",
                    AccessKind::Keyed => "",
                };
                writeln!(
                    out,
                    "  ({pattern}) {}{scan} via {} [{}]",
                    mode.cost.name(),
                    mode.operator.name(),
                    mode.ra_primitives.join(", "),
                )
                .unwrap();
            }
        }
        out
    }
}
