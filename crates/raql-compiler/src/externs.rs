//! Catalog-fed extern signatures and the engine-builtin table (SPEC §8.1).
//!
//! Extern predicate declarations are injected from `raql_plan::v0_catalog`
//! during resolution — the `.decl … extern` / `.mode` blocks are gone from
//! `std.raql`, and a user program cannot redeclare a catalog name. The
//! engine-managed builtins (SPEC §8.6: engine-implemented, no semantic
//! operator) are declared here too: the monomorphic ones are injected like
//! catalog externs; `fmt`/`coalesce` stay user-declared reserved names
//! because their schemas are use-site-specific, and the reserved-shape
//! validation in `typecheck` keeps them honest.

use raql_plan::{ArgType, Binding, v0_catalog};
use raql_syntax::{DeclAttr, DeclarationKind, RaqlFileId, SrcSpan};

use crate::program::{CompilerType, PredicateDecl};

/// Sentinel file id for declarations that have no source text: catalog
/// externs and injected engine builtins.
pub(crate) const CATALOG_FILE: RaqlFileId = RaqlFileId(u32::MAX);

pub(crate) fn catalog_span() -> SrcSpan {
    SrcSpan::new(CATALOG_FILE, 0, 0)
}

pub(crate) fn compiler_type_of(ty: ArgType) -> CompilerType {
    match ty {
        ArgType::Def => CompilerType::Named("Def".to_string()),
        ArgType::Span => CompilerType::Named("Span".to_string()),
        ArgType::File => CompilerType::Named("File".to_string()),
        ArgType::Position => CompilerType::Named("Position".to_string()),
        ArgType::String => CompilerType::String,
        ArgType::Enum(name) => CompilerType::Named(name.to_string()),
        ArgType::Int => CompilerType::Int,
    }
}

/// The declarations a program receives without writing them: every catalog
/// predicate (including `disabled` roadmap entries, so referencing one
/// typechecks and then fails honestly at plan time with RAQL0302), plus
/// the injected engine builtins.
pub(crate) fn injected_declarations() -> Vec<(String, PredicateDecl)> {
    let mut decls = Vec::new();
    for predicate in v0_catalog().entries() {
        decls.push((
            predicate.name.to_string(),
            PredicateDecl {
                kind: DeclarationKind::Relation,
                args: predicate.args.iter().map(|arg| compiler_type_of(arg.ty)).collect(),
                attrs: vec![DeclAttr::Extern],
                span: catalog_span(),
                inferred: false,
                inferred_from: None,
            },
        ));
    }
    for builtin in injected_engine_builtins() {
        decls.push((
            builtin.name.to_string(),
            PredicateDecl {
                kind: builtin.kind,
                args: builtin.args.to_vec(),
                attrs: vec![DeclAttr::Extern],
                span: catalog_span(),
                inferred: false,
                inferred_from: None,
            },
        ));
    }
    decls
}

pub(crate) struct EngineBuiltinDecl {
    pub name: &'static str,
    pub kind: DeclarationKind,
    pub args: Vec<CompilerType>,
}

fn named(name: &str) -> CompilerType {
    CompilerType::Named(name.to_string())
}

/// Monomorphic engine builtins injected as declarations (formerly declared
/// in `std.raql`).
fn injected_engine_builtins() -> Vec<EngineBuiltinDecl> {
    vec![
        EngineBuiltinDecl {
            name: "contains",
            kind: DeclarationKind::Relation,
            args: vec![CompilerType::String, CompilerType::String],
        },
        EngineBuiltinDecl {
            name: "starts_with",
            kind: DeclarationKind::Relation,
            args: vec![CompilerType::String, CompilerType::String],
        },
        EngineBuiltinDecl {
            name: "dispatch_str",
            kind: DeclarationKind::Function,
            args: vec![named("DispatchKind"), CompilerType::String],
        },
        EngineBuiltinDecl {
            name: "witness_path",
            kind: DeclarationKind::Relation,
            args: vec![CompilerType::String, named("Def"), named("Def"), named("Path")],
        },
        EngineBuiltinDecl {
            name: "path_hop",
            kind: DeclarationKind::Relation,
            args: vec![
                named("Path"),
                CompilerType::Int,
                named("Def"),
                named("Def"),
                CompilerType::String,
                named("Span"),
            ],
        },
    ]
}

const B: Binding = Binding::Bound;
const F: Binding = Binding::Free;

/// Binding patterns of every engine-managed builtin, keyed by name. A name
/// in this table is evaluated by the engine, never by a semantic operator;
/// the lowering turns its goals into planner builtins with these patterns.
pub(crate) fn engine_builtin_patterns(name: &str) -> Option<Vec<Vec<Binding>>> {
    match name {
        "contains" | "starts_with" => Some(vec![vec![B, B]]),
        "dispatch_str" => Some(vec![vec![B, F]]),
        "witness_path" => Some(vec![vec![B, B, B, F]]),
        "path_hop" => Some(vec![vec![B, F, F, F, F, F]]),
        "fmt" => Some(vec![vec![B, B, F]]),
        "coalesce" => Some(vec![vec![B, B, F]]),
        _ => None,
    }
}

/// Reserved engine scalar inputs (defaults injected by the engine; a
/// program may override with facts). Declared by programs, never injected.
pub(crate) fn is_scalar_input(name: &str) -> bool {
    matches!(name, "path_limit" | "path_max_depth" | "control_max_depth" | "opt_max_iters")
}
