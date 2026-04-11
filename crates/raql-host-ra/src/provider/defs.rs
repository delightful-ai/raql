use std::collections::BTreeMap;
use std::path::Path;

use base_db::SourceDatabase;
use hir::{Adt, ModuleDef};
use ide::LineIndex;
use syntax::Edition;
use vfs::{AbsPathBuf, VfsPath};

use crate::{DefId, DefKind, DeterministicRaHost, SpanCoord, SpanId, SpanKey};

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

#[derive(Debug, Clone)]
pub(crate) struct LocalFile {
    pub(crate) rel_path: String,
    pub(crate) text: String,
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

pub(crate) fn lookup_span_key_from_text(
    rel_path: &str,
    source_text: &str,
    range: syntax::TextRange,
) -> Option<SpanKey> {
    let line_index = LineIndex::new(source_text);
    let start = line_index.try_line_col(range.start())?;
    let end = line_index.try_line_col(range.end())?;
    Some(SpanKey::new(
        rel_path.to_string(),
        SpanCoord::new(start.line, start.col),
        SpanCoord::new(end.line, end.col),
    ))
}

pub(crate) fn lookup_local_file(
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    db: &ide::RootDatabase,
    file_id: vfs::FileId,
) -> Option<LocalFile> {
    let abs_path = vfs.file_path(file_id).as_path()?;
    let path: &Path = abs_path.as_ref();
    let rel_path = if path.starts_with(workspace_root) {
        path.strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    } else {
        path.to_string_lossy().replace('\\', "/")
    };
    Some(LocalFile {
        rel_path,
        text: db.file_text(file_id).text(db).to_string(),
    })
}

pub(crate) fn ensure_lookup_function_def(
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    function: hir::Function,
) -> Option<DefId> {
    ensure_lookup_source_module_def(
        db,
        vfs,
        workspace_root,
        lookup_defs,
        lookup_spans,
        id_host,
        ModuleDef::Function(function),
    )
}

pub(crate) fn ensure_lookup_source_module_def(
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    def: ModuleDef,
) -> Option<DefId> {
    let (file_id, range) = match def {
        ModuleDef::Module(module) => {
            let range = module
                .declaration_source_range(db)
                .unwrap_or_else(|| module.definition_source_range(db));
            (range.file_id.original_file(db).editioned_file_id(db), range.value)
        }
        ModuleDef::Function(function) => {
            let source = function.source(db)?;
            (
                source.file_id.original_file(db).editioned_file_id(db),
                source.value.syntax().text_range(),
            )
        }
        ModuleDef::Adt(adt) => {
            let source = adt.source(db)?;
            (
                source.file_id.original_file(db).editioned_file_id(db),
                source.value.syntax().text_range(),
            )
        }
        ModuleDef::Variant(variant) => {
            let source = variant.source(db)?;
            (
                source.file_id.original_file(db).editioned_file_id(db),
                source.value.syntax().text_range(),
            )
        }
        ModuleDef::Const(const_) => {
            let source = const_.source(db)?;
            (
                source.file_id.original_file(db).editioned_file_id(db),
                source.value.syntax().text_range(),
            )
        }
        ModuleDef::Static(static_) => {
            let source = static_.source(db)?;
            (
                source.file_id.original_file(db).editioned_file_id(db),
                source.value.syntax().text_range(),
            )
        }
        ModuleDef::Trait(trait_) => {
            let source = trait_.source(db)?;
            (
                source.file_id.original_file(db).editioned_file_id(db),
                source.value.syntax().text_range(),
            )
        }
        ModuleDef::TypeAlias(alias) => {
            let source = alias.source(db)?;
            (
                source.file_id.original_file(db).editioned_file_id(db),
                source.value.syntax().text_range(),
            )
        }
        ModuleDef::Macro(mac) => {
            let source = mac.source(db)?;
            (
                source.file_id.original_file(db).editioned_file_id(db),
                source.value.syntax().text_range(),
            )
        }
        ModuleDef::BuiltinType(_) => return None,
    };
    ensure_lookup_symbol_module_def(
        db,
        vfs,
        workspace_root,
        lookup_defs,
        lookup_spans,
        id_host,
        def,
        file_id,
        range,
    )
}

pub(crate) fn ensure_lookup_symbol_module_def(
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    def: ModuleDef,
    file_id: span::EditionedFileId,
    range: syntax::TextRange,
) -> Option<DefId> {
    let kind = module_def_kind(def)?;
    let local = lookup_local_file(vfs, workspace_root, db, file_id.file_id())?;
    let span_key = lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range)?;
    let span = id_host
        .intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range)
        .ok()?;
    let entity = RaEntity::ModuleDef(def);
    let path = entity.canonical_path(db);
    let token = path
        .as_ref()
        .map(|path| match kind {
            DefKind::Fn | DefKind::Method => format!("def:function:{path}"),
            _ => format!("def:{kind:?}:{path}"),
        })
        .unwrap_or_else(|| {
            format!(
                "lookup_def:{kind:?}:{}:{}..{}",
                local.rel_path,
                u32::from(range.start()),
                u32::from(range.end())
            )
        });
    let def_id = id_host.intern_def_from_token(token.as_str());
    let name = def.name(db)?.display(db, Edition::CURRENT).to_string();
    lookup_defs.insert(
        def_id,
        LookupDefRecord {
            name: name.into_boxed_str(),
            kind,
            span,
            path: path.map(Into::into),
            entity: Some(entity),
            ra_span: Some(RaSpan { file_id, range }),
        },
    );
    lookup_spans.insert(span, span_key);
    Some(def_id)
}

pub(crate) fn ensure_lookup_synthetic_callable_def(
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
    id_host: &mut DeterministicRaHost,
    prefix: &str,
    syntax: &syntax::SyntaxNode,
    file_id: span::EditionedFileId,
    local: &LocalFile,
) -> Option<DefId> {
    let range = syntax.text_range();
    let span_key = lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range)?;
    let span = id_host
        .intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range)
        .ok()?;
    let label = syntax.text().to_string();
    let path = format!(
        "{prefix}::{}:{}..{}:{}",
        local.rel_path,
        u32::from(range.start()),
        u32::from(range.end()),
        label
    );
    let def_id = id_host.intern_def_from_token(format!("def:{prefix}:{path}").as_str());
    lookup_defs.insert(
        def_id,
        LookupDefRecord {
            name: label.into_boxed_str(),
            kind: DefKind::Other,
            span,
            path: Some(path.into_boxed_str()),
            entity: None,
            ra_span: Some(RaSpan { file_id, range }),
        },
    );
    lookup_spans.insert(span, span_key);
    Some(def_id)
}
