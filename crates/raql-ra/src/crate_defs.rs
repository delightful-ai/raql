//! Crate-level definition enumeration (SPEC §8.6 `def` / `fn_def` scans).
//!
//! [`raql_crate_defs`] is a Salsa-tracked query per SPEC §6.2's middle
//! band: one crate's def-map traversal is meaningful reusable derived work
//! with moderate dependency width (the crate's def maps and item lists —
//! not bodies, so body edits do not invalidate it).
//!
//! The enumeration domain is the crate's module tree: every module, its
//! declarations, ADT fields and enum variants, impls, and trait/impl
//! associated items. Items declared inside function bodies live in block
//! def maps outside the module tree and are absent — the catalog documents
//! this as the declared scope (SPEC §4.3 `ra_exact` is scope-relative).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hir::db::HirDatabase;
use hir::ModuleDef;

use crate::def::Def;
use crate::snapshot::with_attached_dyn;

static RAQL_CRATE_DEFS_EXECUTIONS: AtomicU64 = AtomicU64::new(0);

/// Number of times the `raql_crate_defs` query body actually executed
/// (observability for the SPEC §16.3 incrementality tests; not semantic
/// state).
pub fn raql_crate_defs_execution_count() -> u64 {
    RAQL_CRATE_DEFS_EXECUTIONS.load(Ordering::Relaxed)
}

/// Every definition in one crate's module tree, in deterministic def-map
/// traversal order.
pub fn raql_crate_defs(db: &dyn HirDatabase, krate: hir::Crate) -> Arc<[Def]> {
    #[salsa::interned]
    struct InternedCrate {
        #[returns(copy)]
        krate: hir::Crate,
    }

    #[salsa::tracked(returns(ref))]
    fn raql_crate_defs<'db>(db: &'db dyn HirDatabase, krate: InternedCrate<'db>) -> Arc<[Def]> {
        RAQL_CRATE_DEFS_EXECUTIONS.fetch_add(1, Ordering::Relaxed);
        // Salsa may (re-)execute this body from any caller's verification
        // stack, where the TLS slot may be empty (SPEC §6.1).
        with_attached_dyn(db, || compute_crate_defs(db, krate.krate(db)))
    }

    raql_crate_defs(db, InternedCrate::new(db, krate)).clone()
}

/// The workspace-local crates, the enumeration domain of workspace scans
/// (SPEC §8.6: "workspace = union over `hir::Crate::all` in-workspace").
/// "Local" is RA's own notion (`CrateOrigin::is_local`): workspace members
/// *and* path dependencies — editable code — but never registry/git
/// dependencies or the sysroot. The `def_name(-,+)` symbol-index seed
/// scopes itself the same way; the seed and the scans must agree on the
/// domain.
pub(crate) fn workspace_local_crates(db: &dyn HirDatabase) -> Vec<hir::Crate> {
    hir::Crate::all(db)
        .into_iter()
        .filter(|krate| krate.origin(db).is_local())
        .collect()
}

fn compute_crate_defs(db: &dyn HirDatabase, krate: hir::Crate) -> Arc<[Def]> {
    let mut defs = Vec::new();
    for module in krate.modules(db) {
        defs.push(Def::Module(module));
        for decl in module.declarations(db) {
            match decl {
                // Modules are enumerated from `krate.modules` (above) and
                // enum variants from their parent enum (below); listing
                // them here too would double-count.
                ModuleDef::Module(_) | ModuleDef::EnumVariant(_) => {}
                ModuleDef::Adt(adt) => {
                    defs.push(Def::Adt(adt));
                    push_adt_children(db, adt, &mut defs);
                }
                ModuleDef::Trait(trait_) => {
                    defs.push(Def::Trait(trait_));
                    push_assoc_items(trait_.items(db), &mut defs);
                }
                other => defs.extend(Def::from_module_def(other)),
            }
        }
        // `declarations` covers the types and values scopes only;
        // `macro_rules!` definitions live in the legacy-macro scope (the
        // same split RA's own symbol collector bridges). That scope also
        // holds macros textually inherited from earlier modules, so keep
        // only the ones this module declares.
        defs.extend(
            module
                .legacy_macros(db)
                .into_iter()
                .filter(|makro| makro.module(db) == module)
                .map(Def::Macro),
        );
        for impl_ in module.impl_defs(db) {
            defs.push(Def::Impl(impl_));
            push_assoc_items(impl_.items(db), &mut defs);
        }
    }
    defs.into()
}

fn push_adt_children(db: &dyn HirDatabase, adt: hir::Adt, defs: &mut Vec<Def>) {
    match adt {
        hir::Adt::Struct(strukt) => {
            defs.extend(strukt.fields(db).into_iter().map(Def::Field));
        }
        hir::Adt::Union(union_) => {
            defs.extend(union_.fields(db).into_iter().map(Def::Field));
        }
        hir::Adt::Enum(enum_) => {
            for variant in enum_.variants(db) {
                defs.push(Def::Variant(variant));
                defs.extend(variant.fields(db).into_iter().map(Def::Field));
            }
        }
    }
}

fn push_assoc_items(items: Vec<hir::AssocItem>, defs: &mut Vec<Def>) {
    for item in items {
        defs.push(match item {
            hir::AssocItem::Function(it) => Def::Function(it),
            hir::AssocItem::Const(it) => Def::Const(it),
            hir::AssocItem::TypeAlias(it) => Def::TypeAlias(it),
        });
    }
}
