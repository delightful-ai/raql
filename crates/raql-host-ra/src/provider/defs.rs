use hir::{Adt, ModuleDef};
use syntax::Edition;

use crate::{DefKind, SpanId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RaEntity {
    ModuleDef(ModuleDef),
}

impl RaEntity {
    pub(crate) fn as_function(&self) -> Option<hir::Function> {
        match self {
            Self::ModuleDef(ModuleDef::Function(function)) => Some(*function),
            _ => None,
        }
    }

    pub(crate) fn canonical_path(&self, db: &ide::RootDatabase) -> Option<String> {
        match self {
            Self::ModuleDef(ModuleDef::Function(function)) => {
                Some(canonical_function_path(db, *function))
            }
            Self::ModuleDef(def) => def.canonical_path(db, Edition::CURRENT),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RaSpan {
    pub(crate) file_id: span::EditionedFileId,
    pub(crate) range: syntax::TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LookupDefRecord {
    pub(crate) name: Box<str>,
    pub(crate) kind: DefKind,
    pub(crate) span: SpanId,
    pub(crate) path: Option<Box<str>>,
    pub(crate) entity: Option<RaEntity>,
    pub(crate) ra_span: Option<RaSpan>,
}

pub(crate) fn canonical_function_path(db: &ide::RootDatabase, function: hir::Function) -> String {
    let name = function.name(db).display(db, Edition::CURRENT).to_string();
    let raw_path = ModuleDef::Function(function)
        .canonical_path(db, Edition::CURRENT)
        .unwrap_or_else(|| name.clone());
    function
        .module(db)
        .krate(db)
        .display_name(db)
        .map(|crate_name| crate_name.to_string())
        .filter(|crate_name| {
            raw_path != *crate_name && !raw_path.starts_with(format!("{crate_name}::").as_str())
        })
        .map(|crate_name| format!("{crate_name}::{raw_path}"))
        .unwrap_or(raw_path)
}

pub(crate) fn module_def_kind(def: ModuleDef) -> Option<DefKind> {
    Some(match def {
        ModuleDef::Module(_) => DefKind::Mod,
        ModuleDef::Function(_) => DefKind::Fn,
        ModuleDef::Adt(Adt::Struct(_)) => DefKind::Struct,
        ModuleDef::Adt(Adt::Enum(_)) => DefKind::Enum,
        ModuleDef::Adt(Adt::Union(_)) => DefKind::Union,
        ModuleDef::Variant(_) => DefKind::Variant,
        ModuleDef::Const(_) => DefKind::Const,
        ModuleDef::Static(_) => DefKind::Static,
        ModuleDef::Trait(_) => DefKind::Trait,
        ModuleDef::TypeAlias(_) => DefKind::TypeAlias,
        ModuleDef::Macro(_) => DefKind::Macro,
        ModuleDef::BuiltinType(_) => return None,
    })
}
