//! The catalog registry and its v0 contents (SPEC §8.6, normative).
//!
//! Entries appear here only together with their operator implementation and
//! proof matrix (SPEC §16). The remaining v0 predicates (`def`/`fn_def`
//! scans, the `call_edge` scan mode, `is_public`, `in_test`,
//! `span_allowed`, `handle`, `span_key`) land with later steps; the
//! `disabled` roadmap families enter with the semantic-expansion step.

use crate::mode::{AccessKind, Binding, CostClass, ModeDef};
use crate::operator::OperatorId;
use crate::predicate::{Completeness, PredicateDef};
use crate::schema::{ArgDef, ArgType};

/// A catalog: the set of extern predicates. The v0 instance below is the
/// single source of truth; tests and generators walk it.
#[derive(Clone, Copy, Debug)]
pub struct Catalog {
    entries: &'static [PredicateDef],
}

impl Catalog {
    pub const fn new(entries: &'static [PredicateDef]) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &'static [PredicateDef] {
        self.entries
    }

    pub fn predicate(&self, name: &str) -> Option<&'static PredicateDef> {
        self.entries.iter().find(|p| p.name == name)
    }
}

/// The v0 catalog (SPEC §8.6), currently the slice-A definition family.
pub fn v0_catalog() -> Catalog {
    Catalog::new(V0_ENTRIES)
}

const DEF: ArgDef = ArgDef { name: "D", ty: ArgType::Def };

static V0_ENTRIES: &[PredicateDef] = &[
    PredicateDef {
        name: "def_name",
        args: &[DEF, ArgDef { name: "Name", ty: ArgType::String }],
        doc: "The definition's own name (not its path).",
        completeness: Completeness::RaExact,
        modes: &[
            ModeDef {
                pattern: &[Binding::Bound, Binding::Free],
                cost: CostClass::C0,
                access: AccessKind::Keyed,
                operator: OperatorId::NameOfDef,
                ra_primitives: &["hir name projection"],
            },
            ModeDef {
                pattern: &[Binding::Free, Binding::Bound],
                cost: CostClass::C2,
                access: AccessKind::Keyed,
                operator: OperatorId::DefsByExactName,
                ra_primitives: &["ide_db::symbol_index::world_symbols"],
            },
        ],
    },
    PredicateDef {
        name: "def_at",
        args: &[
            ArgDef { name: "File", ty: ArgType::File },
            ArgDef { name: "Pos", ty: ArgType::Position },
            DEF,
        ],
        doc: "The definition referenced or declared at a file position \
              (goto-definition shape).",
        completeness: Completeness::RaResolved { caveats: &["unresolved_positions_absent"] },
        modes: &[ModeDef {
            pattern: &[Binding::Bound, Binding::Bound, Binding::Free],
            cost: CostClass::C1,
            access: AccessKind::Keyed,
            operator: OperatorId::DefAtPosition,
            ra_primitives: &["hir::Semantics", "IdentClass::classify_node"],
        }],
    },
    PredicateDef {
        name: "def_kind",
        args: &[DEF, ArgDef { name: "K", ty: ArgType::Enum("DefKind") }],
        doc: "The definition's kind (fn, struct, enum, ...).",
        completeness: Completeness::RaExact,
        modes: &[ModeDef {
            pattern: &[Binding::Bound, Binding::Free],
            cost: CostClass::C0,
            access: AccessKind::Keyed,
            operator: OperatorId::KindOfDef,
            ra_primitives: &["value variant projection"],
        }],
    },
    PredicateDef {
        name: "def_path",
        args: &[DEF, ArgDef { name: "P", ty: ArgType::String }],
        doc: "Canonical module path of the definition (def-map path, not \
              re-export path).",
        completeness: Completeness::RaExact,
        modes: &[ModeDef {
            pattern: &[Binding::Bound, Binding::Free],
            cost: CostClass::C1,
            access: AccessKind::Keyed,
            operator: OperatorId::CanonicalPathOfDef,
            ra_primitives: &["hir::Module::path_to_root"],
        }],
    },
    PredicateDef {
        name: "def_span",
        args: &[DEF, ArgDef { name: "S", ty: ArgType::Span }],
        doc: "Primary-source span of the definition's name (macro-aware).",
        completeness: Completeness::RaExact,
        modes: &[ModeDef {
            pattern: &[Binding::Bound, Binding::Free],
            cost: CostClass::C1,
            access: AccessKind::Keyed,
            operator: OperatorId::SpanOfDef,
            ra_primitives: &["hir::HasSource", "original_file_range_rooted"],
        }],
    },
    PredicateDef {
        name: "callee",
        args: &[
            ArgDef { name: "F", ty: ArgType::Def },
            ArgDef { name: "Callee", ty: ArgType::Def },
            ArgDef { name: "Site", ty: ArgType::Span },
            ArgDef { name: "Disp", ty: ArgType::Enum("DispatchKind") },
        ],
        doc: "Resolved outgoing call edges of one function body, dispatch \
              classified at the callsite. Unresolvable callees (closures, \
              fn pointers, constructors) and macro-generated callsites are \
              absent.",
        completeness: Completeness::RaResolved {
            caveats: &["unresolved_callsites_absent", "macro_generated_callsites_absent"],
        },
        modes: &[ModeDef {
            pattern: &[Binding::Bound, Binding::Free, Binding::Free, Binding::Free],
            cost: CostClass::C1,
            access: AccessKind::Keyed,
            operator: OperatorId::CalleesOfFn,
            ra_primitives: &["raql_callees (tracked)", "Semantics", "Type::as_callable"],
        }],
    },
    PredicateDef {
        name: "caller",
        args: &[
            ArgDef { name: "F", ty: ArgType::Def },
            ArgDef { name: "CallerFn", ty: ArgType::Def },
            ArgDef { name: "Site", ty: ArgType::Span },
            ArgDef { name: "Disp", ty: ArgType::Enum("DispatchKind") },
        ],
        doc: "Resolved incoming call edges (reference-search backed). \
              Non-call references are absent; callsites inside closures are \
              attributed to the enclosing named function.",
        completeness: Completeness::RaResolved {
            caveats: &[
                "unresolved_references_absent",
                "closure_callsites_attributed_to_enclosing_fn",
                "macro_definition_body_callsites_absent",
            ],
        },
        modes: &[ModeDef {
            pattern: &[Binding::Bound, Binding::Free, Binding::Free, Binding::Free],
            cost: CostClass::C2,
            access: AccessKind::Keyed,
            operator: OperatorId::CallersOfFn,
            ra_primitives: &["Definition::usages", "ancestors_with_macros"],
        }],
    },
    PredicateDef {
        name: "call_edge",
        args: &[
            ArgDef { name: "Caller", ty: ArgType::Def },
            ArgDef { name: "Callee", ty: ArgType::Def },
            ArgDef { name: "Site", ty: ArgType::Span },
            ArgDef { name: "Disp", ty: ArgType::Enum("DispatchKind") },
        ],
        doc: "Call edges, composed from `callee` (caller bound) or `caller` \
              (callee bound). The unbound scan mode lands with the \
              enumeration slice (SPEC §8.5).",
        completeness: Completeness::RaResolved {
            caveats: &["unresolved_callsites_absent", "macro_expansion_spans"],
        },
        modes: &[
            ModeDef {
                pattern: &[Binding::Bound, Binding::Free, Binding::Free, Binding::Free],
                cost: CostClass::C1,
                access: AccessKind::Keyed,
                operator: OperatorId::CallEdgesByCaller,
                ra_primitives: &["raql_callees (tracked)"],
            },
            ModeDef {
                pattern: &[Binding::Free, Binding::Bound, Binding::Free, Binding::Free],
                cost: CostClass::C2,
                access: AccessKind::Keyed,
                operator: OperatorId::CallEdgesByCallee,
                ra_primitives: &["Definition::usages"],
            },
        ],
    },
];
