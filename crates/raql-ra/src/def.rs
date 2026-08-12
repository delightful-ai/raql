//! The `Def` union of RA definition handles and its RA-native accessors
//! (SPEC §7: "the `Def` language type is the union of the `hir::*`
//! definition variants; `def_kind` is a projection of the variant").

use hir::db::HirDatabase;
use hir::{AsAssocItem, HasSource, ModuleDef};
use syntax::{AstNode, Edition};

/// An RA-owned definition identity, live within one snapshot (SPEC §4.1).
///
/// Upstream note: `hir::TraitAlias` no longer exists on the pinned RA rev
/// (trait aliases were folded away), so the SPEC §7 sketch's `TraitAlias`
/// variant has no successor here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Def {
    Function(hir::Function),
    Adt(hir::Adt),
    Trait(hir::Trait),
    Module(hir::Module),
    Const(hir::Const),
    Static(hir::Static),
    TypeAlias(hir::TypeAlias),
    Macro(hir::Macro),
    Impl(hir::Impl),
    Field(hir::Field),
    Variant(hir::EnumVariant),
}

/// Language-level definition kind (`DefKind` in `std.raql`). Variant names
/// are the catalog's enum tags; keep them in sync with the stdlib.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DefKind {
    Fn,
    Method,
    Struct,
    Enum,
    Union,
    Trait,
    Mod,
    Impl,
    TypeAlias,
    AssocType,
    Const,
    AssocConst,
    Static,
    Field,
    Variant,
    Macro,
}

impl DefKind {
    /// The catalog enum-tag spelling (matches `std.raql`'s `DefKind`).
    pub fn tag(self) -> &'static str {
        match self {
            DefKind::Fn => "FN",
            DefKind::Method => "METHOD",
            DefKind::Struct => "STRUCT",
            DefKind::Enum => "ENUM",
            DefKind::Union => "UNION",
            DefKind::Trait => "TRAIT",
            DefKind::Mod => "MOD",
            DefKind::Impl => "IMPL",
            DefKind::TypeAlias => "TYPE_ALIAS",
            DefKind::AssocType => "ASSOC_TYPE",
            DefKind::Const => "CONST",
            DefKind::AssocConst => "ASSOC_CONST",
            DefKind::Static => "STATIC",
            DefKind::Field => "FIELD",
            DefKind::Variant => "VARIANT",
            DefKind::Macro => "MACRO",
        }
    }

    /// The SPEC §13.2 handle-grammar kind token.
    pub(crate) fn handle_kind(self) -> &'static str {
        match self {
            DefKind::Fn | DefKind::Method => "fn",
            DefKind::Struct => "struct",
            DefKind::Enum => "enum",
            DefKind::Union => "union",
            DefKind::Trait => "trait",
            DefKind::Mod => "mod",
            DefKind::Impl => "impl",
            DefKind::TypeAlias | DefKind::AssocType => "type",
            DefKind::Const | DefKind::AssocConst => "const",
            DefKind::Static => "static",
            DefKind::Field => "field",
            DefKind::Variant => "variant",
            DefKind::Macro => "macro",
        }
    }
}

impl Def {
    /// Lower an RA `ModuleDef`. Builtin types have no definition identity.
    pub fn from_module_def(def: ModuleDef) -> Option<Def> {
        Some(match def {
            ModuleDef::Module(it) => Def::Module(it),
            ModuleDef::Function(it) => Def::Function(it),
            ModuleDef::Adt(it) => Def::Adt(it),
            ModuleDef::EnumVariant(it) => Def::Variant(it),
            ModuleDef::Const(it) => Def::Const(it),
            ModuleDef::Static(it) => Def::Static(it),
            ModuleDef::Trait(it) => Def::Trait(it),
            ModuleDef::TypeAlias(it) => Def::TypeAlias(it),
            ModuleDef::Macro(it) => Def::Macro(it),
            ModuleDef::BuiltinType(_) => return None,
        })
    }

    /// Lower an ide-db `Definition` (the classify-at-position result shape).
    /// Non-definition identities (locals, generic params, labels, builtin
    /// attrs, ...) have no `Def`; they are honestly absent (SPEC §4.3).
    pub fn from_ide_definition(db: &dyn HirDatabase, def: ide_db::defs::Definition<'_>) -> Option<Def> {
        use ide_db::defs::Definition;
        Some(match def {
            Definition::Macro(it) => Def::Macro(it),
            Definition::Field(it) => Def::Field(it),
            Definition::Module(it) => Def::Module(it),
            Definition::Crate(it) => Def::Module(it.root_module(db)),
            Definition::Function(it) => Def::Function(it),
            Definition::Adt(it) => Def::Adt(it),
            Definition::EnumVariant(it) => Def::Variant(it),
            Definition::Const(it) => Def::Const(it),
            Definition::Static(it) => Def::Static(it),
            Definition::Trait(it) => Def::Trait(it),
            Definition::TypeAlias(it) => Def::TypeAlias(it),
            Definition::SelfType(it) => Def::Impl(it),
            _ => return None,
        })
    }

    /// The definition's own name. Impls (and other unnamed defs) have none.
    pub fn name(self, db: &dyn HirDatabase) -> Option<String> {
        let name = match self {
            Def::Function(it) => it.name(db),
            Def::Adt(it) => it.name(db),
            Def::Trait(it) => it.name(db),
            Def::Module(it) => it.name(db)?,
            Def::Const(it) => it.name(db)?,
            Def::Static(it) => it.name(db),
            Def::TypeAlias(it) => it.name(db),
            Def::Macro(it) => it.name(db),
            Def::Impl(_) => return None,
            Def::Field(it) => it.name(db),
            Def::Variant(it) => it.name(db),
        };
        Some(name.display(db, Edition::CURRENT).to_string())
    }

    /// `def_kind` projection of the variant (SPEC §7). Distinguishes
    /// associated items (`METHOD`, `ASSOC_TYPE`, `ASSOC_CONST`) by container.
    pub fn kind(self, db: &dyn HirDatabase) -> DefKind {
        match self {
            Def::Function(it) => {
                if it.as_assoc_item(db).is_some() {
                    DefKind::Method
                } else {
                    DefKind::Fn
                }
            }
            Def::Adt(hir::Adt::Struct(_)) => DefKind::Struct,
            Def::Adt(hir::Adt::Enum(_)) => DefKind::Enum,
            Def::Adt(hir::Adt::Union(_)) => DefKind::Union,
            Def::Trait(_) => DefKind::Trait,
            Def::Module(_) => DefKind::Mod,
            Def::Const(it) => {
                if it.as_assoc_item(db).is_some() {
                    DefKind::AssocConst
                } else {
                    DefKind::Const
                }
            }
            Def::Static(_) => DefKind::Static,
            Def::TypeAlias(it) => {
                if it.as_assoc_item(db).is_some() {
                    DefKind::AssocType
                } else {
                    DefKind::TypeAlias
                }
            }
            Def::Macro(_) => DefKind::Macro,
            Def::Impl(_) => DefKind::Impl,
            Def::Field(_) => DefKind::Field,
            Def::Variant(_) => DefKind::Variant,
        }
    }

    /// The module owning this definition.
    pub fn module(self, db: &dyn HirDatabase) -> hir::Module {
        match self {
            Def::Function(it) => it.module(db),
            Def::Adt(it) => it.module(db),
            Def::Trait(it) => it.module(db),
            Def::Module(it) => it,
            Def::Const(it) => it.module(db),
            Def::Static(it) => it.module(db),
            Def::TypeAlias(it) => it.module(db),
            Def::Macro(it) => it.module(db),
            Def::Impl(it) => it.module(db),
            Def::Field(it) => it.parent_def(db).module(db),
            Def::Variant(it) => it.module(db),
        }
    }

    /// Canonical module path (def-map path, not re-export path), prefixed
    /// with the crate display name per the SPEC §13.2 handle grammar.
    /// Associated items are qualified by their container (trait, or the impl
    /// self-type ADT). `None` when the def has no canonical path (impls;
    /// impls on non-ADT self types; defs whose name is missing) — the caller
    /// renders that as unprojectable, never invents.
    pub fn canonical_path(self, db: &dyn HirDatabase) -> Option<String> {
        // Owner-qualified defs first: fields, enum variants, and associated
        // items (RA's `canonical_path` qualifies by module only; selectors
        // must be constructible as `crate::module::Owner::member`).
        if let Def::Field(it) = self {
            let owner = match it.parent_def(db) {
                hir::Variant::Struct(s) => Def::Adt(hir::Adt::Struct(s)),
                hir::Variant::Union(u) => Def::Adt(hir::Adt::Union(u)),
                hir::Variant::EnumVariant(v) => Def::Variant(v),
            };
            return Some(format!("{}::{}", owner.canonical_path(db)?, self.name(db)?));
        }
        if let Def::Variant(it) = self {
            let owner = Def::Adt(hir::Adt::Enum(it.parent_enum(db)));
            return Some(format!("{}::{}", owner.canonical_path(db)?, self.name(db)?));
        }
        let assoc = match self {
            Def::Function(it) => it.as_assoc_item(db),
            Def::Const(it) => it.as_assoc_item(db),
            Def::TypeAlias(it) => it.as_assoc_item(db),
            _ => None,
        };
        if let Some(assoc) = assoc {
            let container = match assoc.container(db) {
                hir::AssocItemContainer::Trait(trait_) => Def::Trait(trait_),
                hir::AssocItemContainer::Impl(impl_) => {
                    Def::Adt(impl_.self_ty(db).as_adt()?)
                }
            };
            return Some(format!("{}::{}", container.canonical_path(db)?, self.name(db)?));
        }

        let raw = match self {
            Def::Impl(_) => return None,
            Def::Field(_) => unreachable!("handled above"),
            Def::Function(it) => ModuleDef::Function(it).canonical_path(db, Edition::CURRENT),
            Def::Adt(it) => ModuleDef::Adt(it).canonical_path(db, Edition::CURRENT),
            Def::Trait(it) => ModuleDef::Trait(it).canonical_path(db, Edition::CURRENT),
            Def::Module(it) => ModuleDef::Module(it).canonical_path(db, Edition::CURRENT),
            Def::Const(it) => ModuleDef::Const(it).canonical_path(db, Edition::CURRENT),
            Def::Static(it) => ModuleDef::Static(it).canonical_path(db, Edition::CURRENT),
            Def::TypeAlias(it) => ModuleDef::TypeAlias(it).canonical_path(db, Edition::CURRENT),
            Def::Macro(it) => ModuleDef::Macro(it).canonical_path(db, Edition::CURRENT),
            Def::Variant(_) => unreachable!("handled above"),
        }?;
        let krate = self.module(db).krate(db);
        let crate_name = krate.display_name(db)?.to_string();
        if raw == crate_name || raw.starts_with(&format!("{crate_name}::")) {
            Some(raw)
        } else {
            Some(format!("{crate_name}::{raw}"))
        }
    }

    /// Primary-source range of the definition's declaration, macro-aware via
    /// RA's `original_file_range_rooted` (SPEC §6.3): a def produced by a
    /// macro reports its invocation-site location (the same fallback RA's
    /// own navigation targets use). Defs without a resolvable primary
    /// location are absent, never approximated.
    pub fn original_span(self, db: &dyn HirDatabase) -> Option<ide_db::FileRange> {
        fn item_range<D>(db: &dyn HirDatabase, def: D) -> Option<ide_db::FileRange>
        where
            D: HasSource,
            D::Ast: AstNode,
        {
            let source = def.source(db)?;
            let range = source
                .as_ref()
                .map(|ast| ast.syntax())
                .original_file_range_rooted(db);
            Some(range.into_file_id(db))
        }
        match self {
            Def::Function(it) => item_range(db, it),
            Def::Adt(it) => item_range(db, it),
            Def::Trait(it) => item_range(db, it),
            Def::Module(it) => {
                // Modules have declaration (`mod m;` / `mod m {..}`) and
                // definition (file) sources; prefer the declaration.
                let range = it
                    .declaration_source_range(db)
                    .unwrap_or_else(|| it.definition_source_range(db));
                // A module declared inside a macro expansion has no honest
                // primary location here; absent per SPEC §4.3.
                let file_id = range.file_id.file_id()?;
                Some(ide_db::FileRange { file_id: file_id.file_id(db), range: range.value })
            }
            Def::Const(it) => item_range(db, it),
            Def::Static(it) => item_range(db, it),
            Def::TypeAlias(it) => item_range(db, it),
            Def::Macro(it) => item_range(db, it),
            Def::Impl(it) => item_range(db, it),
            Def::Field(it) => item_range(db, it),
            Def::Variant(it) => item_range(db, it),
        }
    }
}
