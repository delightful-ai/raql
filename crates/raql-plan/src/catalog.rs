//! The catalog registry and its v0 contents (SPEC §8.6, normative).
//!
//! Entries appear here only together with their operator implementation and
//! proof matrix (SPEC §16), with one exception: the `disabled` roadmap
//! families (error flow, reference events) have entries with no modes so
//! the capability listing shows the roadmap and the planner can name them
//! in capability errors (SPEC §4.3).
//!
//! Scan cost bands: scans are declared at their workspace-wide band (C4).
//! When request scoping lands (SPEC §14 `options.scope`), a scan narrowed
//! to one crate is C3; the planner will refine the band then. The v1
//! structure/trait/type families enter with the semantic-expansion step.

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

/// The v0 catalog (SPEC §8.6).
pub fn v0_catalog() -> Catalog {
    Catalog::new(V0_ENTRIES)
}

const DEF: ArgDef = ArgDef { name: "D", ty: ArgType::Def };
const SPAN: ArgDef = ArgDef { name: "S", ty: ArgType::Span };

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
                caveats: &[],
            },
            ModeDef {
                pattern: &[Binding::Free, Binding::Bound],
                cost: CostClass::C2,
                access: AccessKind::Keyed,
                operator: OperatorId::DefsByExactName,
                ra_primitives: &["ide_db::symbol_index::world_symbols"],
                // The symbol index never surfaces fields; seed a field's
                // owner and project instead.
                caveats: &["fields_not_in_symbol_index"],
            },
            ModeDef {
                pattern: &[Binding::Free, Binding::Free],
                cost: CostClass::C4,
                access: AccessKind::Scan,
                operator: OperatorId::DefNamesScan,
                ra_primitives: &["raql_crate_defs (tracked)", "hir name projection"],
                caveats: &[],
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
            caveats: &[],
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
            caveats: &[],
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
            caveats: &[],
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
            caveats: &[],
        }],
    },
    PredicateDef {
        name: "def",
        args: &[DEF],
        doc: "Every definition in the enumeration scope: crate def-map \
              traversal (modules, their declarations, fields, variants, \
              impls, and associated items). Items declared inside function \
              bodies are not part of the module tree and are absent.",
        completeness: Completeness::RaExact,
        modes: &[ModeDef {
            pattern: &[Binding::Free],
            cost: CostClass::C4,
            access: AccessKind::Scan,
            operator: OperatorId::DefsScan,
            ra_primitives: &["raql_crate_defs (tracked)", "hir::Crate::all"],
            caveats: &[],
        }],
    },
    PredicateDef {
        name: "fn_def",
        args: &[DEF],
        doc: "`def(D)` filtered to functions (free fns and methods) during \
              enumeration. The explicit way to say \"enumerate functions\" \
              (SPEC §9.3).",
        completeness: Completeness::RaExact,
        modes: &[ModeDef {
            pattern: &[Binding::Free],
            cost: CostClass::C4,
            access: AccessKind::Scan,
            operator: OperatorId::FnDefsScan,
            ra_primitives: &["raql_crate_defs (tracked)", "hir::Crate::all"],
            caveats: &[],
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
            caveats: &[],
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
            caveats: &[],
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
              (callee bound). The unbound scan is the SPEC §8.5 rewrite \
              `fn_def(C), callee(C, K, S, D)`: enumeration expanded through \
              the outgoing operator, never through reference search.",
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
                caveats: &[],
            },
            ModeDef {
                pattern: &[Binding::Free, Binding::Bound, Binding::Free, Binding::Free],
                cost: CostClass::C2,
                access: AccessKind::Keyed,
                operator: OperatorId::CallEdgesByCallee,
                ra_primitives: &["Definition::usages"],
                caveats: &[],
            },
            ModeDef {
                pattern: &[Binding::Free, Binding::Free, Binding::Free, Binding::Free],
                cost: CostClass::C4,
                access: AccessKind::Scan,
                operator: OperatorId::CallEdgesScan,
                ra_primitives: &["raql_crate_defs (tracked)", "raql_callees (tracked)"],
                caveats: &[],
            },
        ],
    },
    PredicateDef {
        name: "is_public",
        args: &[DEF],
        doc: "The definition's declared visibility is exactly `pub` \
              (`pub(crate)`/`pub(super)`/private are not public). Impls \
              have no visibility: no row.",
        completeness: Completeness::RaExact,
        modes: &[ModeDef {
            pattern: &[Binding::Bound],
            cost: CostClass::C0,
            access: AccessKind::Keyed,
            operator: OperatorId::IsPublicFilter,
            ra_primitives: &["hir::HasVisibility"],
            caveats: &[],
        }],
    },
    PredicateDef {
        name: "in_test",
        args: &[DEF],
        doc: "The definition is test code: a `#[test]` function, or \
              declared under a module named `tests` anywhere up its module \
              path.",
        completeness: Completeness::RaResolved {
            caveats: &["test_modules_detected_by_name_only"],
        },
        modes: &[ModeDef {
            pattern: &[Binding::Bound],
            cost: CostClass::C1,
            access: AccessKind::Keyed,
            operator: OperatorId::InTestFilter,
            ra_primitives: &["hir::Function::is_test", "hir::Module::path_to_root"],
            caveats: &[],
        }],
    },
    PredicateDef {
        name: "span_allowed",
        args: &[SPAN],
        doc: "Request-scope filter, not a semantic fact: the span's file \
              lies in the request's allowed scope (v0: workspace-local \
              source roots; scope options compile onto this filter).",
        completeness: Completeness::RaExact,
        modes: &[ModeDef {
            pattern: &[Binding::Bound],
            cost: CostClass::C0,
            access: AccessKind::Keyed,
            operator: OperatorId::SpanAllowedFilter,
            ra_primitives: &["source-root partition"],
            caveats: &[],
        }],
    },
    PredicateDef {
        name: "handle",
        args: &[DEF, ArgDef { name: "H", ty: ArgType::String }],
        doc: "The SPEC §13.2 handle of the definition — a semantic \
              selector, never identity. Impl handles are the self-type's \
              canonical path, ordinal-qualified in stable source order when \
              several impls share it. Defs with no canonical path have no \
              handle row.",
        completeness: Completeness::RaExact,
        modes: &[ModeDef {
            pattern: &[Binding::Bound, Binding::Free],
            cost: CostClass::C1,
            access: AccessKind::Keyed,
            operator: OperatorId::HandleOfDef,
            ra_primitives: &["canonical path via def maps", "hir::Impl::all_for_type"],
            caveats: &[],
        }],
    },
    PredicateDef {
        name: "span_key",
        args: &[
            SPAN,
            ArgDef { name: "Path", ty: ArgType::String },
            ArgDef { name: "L0", ty: ArgType::Int },
            ArgDef { name: "C0", ty: ArgType::Int },
            ArgDef { name: "L1", ty: ArgType::Int },
            ArgDef { name: "C1", ty: ArgType::Int },
        ],
        doc: "Workspace-relative location projection of a span: path plus \
              zero-based start/end line and column (`LineIndex` \
              convention; the output boundary renders 1-based).",
        completeness: Completeness::RaExact,
        modes: &[ModeDef {
            pattern: &[
                Binding::Bound,
                Binding::Free,
                Binding::Free,
                Binding::Free,
                Binding::Free,
                Binding::Free,
            ],
            cost: CostClass::C0,
            access: AccessKind::Keyed,
            operator: OperatorId::SpanKeyOfSpan,
            ra_primitives: &["ide_db::line_index", "source-root path"],
            caveats: &[],
        }],
    },
    // ── Roadmap families (SPEC §8.6): entries exist so the capability
    // listing shows them and the planner can name them in capability
    // errors; querying them is a compile-time error until each has an
    // honest RA-native operator. ─────────────────────────────────────────
    PredicateDef {
        name: "constructs",
        args: &[
            ArgDef { name: "ErrType", ty: ArgType::Def },
            ArgDef { name: "Variant", ty: ArgType::String },
            ArgDef { name: "Site", ty: ArgType::Span },
            ArgDef { name: "Fn", ty: ArgType::Def },
        ],
        doc: "Error-flow family: construction sites of an error type. \
              Disabled until it has an honest RA-native operator.",
        completeness: Completeness::Disabled,
        modes: &[],
    },
    PredicateDef {
        name: "propagates",
        args: &[
            ArgDef { name: "ErrType", ty: ArgType::Def },
            ArgDef { name: "Site", ty: ArgType::Span },
            ArgDef { name: "Fn", ty: ArgType::Def },
        ],
        doc: "Error-flow family: `?`-propagation sites of an error type. \
              Disabled until it has an honest RA-native operator.",
        completeness: Completeness::Disabled,
        modes: &[],
    },
    PredicateDef {
        name: "converts",
        args: &[
            ArgDef { name: "SrcErr", ty: ArgType::Def },
            ArgDef { name: "DstErr", ty: ArgType::Def },
            ArgDef { name: "Site", ty: ArgType::Span },
            ArgDef { name: "Fn", ty: ArgType::Def },
        ],
        doc: "Error-flow family: error-type conversion sites. Disabled \
              until it has an honest RA-native operator.",
        completeness: Completeness::Disabled,
        modes: &[],
    },
    PredicateDef {
        name: "handles",
        args: &[
            ArgDef { name: "ErrType", ty: ArgType::Def },
            ArgDef { name: "Variant", ty: ArgType::String },
            ArgDef { name: "Site", ty: ArgType::Span },
            ArgDef { name: "Fn", ty: ArgType::Def },
        ],
        doc: "Error-flow family: handling sites (match/if-let) of an error \
              type. Disabled until it has an honest RA-native operator.",
        completeness: Completeness::Disabled,
        modes: &[],
    },
    PredicateDef {
        name: "compares",
        args: &[
            ArgDef { name: "Type", ty: ArgType::Def },
            ArgDef { name: "Site", ty: ArgType::Span },
            ArgDef { name: "Op", ty: ArgType::String },
            ArgDef { name: "Fn", ty: ArgType::Def },
        ],
        doc: "Reference events: comparison sites of a type. Disabled until \
              it has an honest RA-native operator.",
        completeness: Completeness::Disabled,
        modes: &[],
    },
    PredicateDef {
        name: "writes",
        args: &[
            ArgDef { name: "Subject", ty: ArgType::Def },
            ArgDef { name: "Site", ty: ArgType::Span },
            ArgDef { name: "Fn", ty: ArgType::Def },
        ],
        doc: "Reference events: write sites of a place. Disabled until it \
              has an honest RA-native operator.",
        completeness: Completeness::Disabled,
        modes: &[],
    },
];
