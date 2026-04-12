use std::collections::BTreeMap;
use std::path::Path;

use base_db::SourceDatabase;
use hir::{Adt, HasSource, HasVisibility, Module, ModuleDef};
use ide::LineIndex;
use ide_db::symbol_index::{Query, world_symbols};
use raql_host::{
    ExternLookupHostValue, ExternLookupHostValueKind, ExternLookupRequest, ExternLookupValue,
};
use syntax::{AstNode, Edition};

use crate::provider::core_index::CoreLookupIndex;
use crate::RaHostInitError;
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

pub(crate) fn def_kind_lookup_value(kind: DefKind) -> ExternLookupValue {
    let variant = match kind {
        DefKind::Fn => "FN",
        DefKind::Method => "METHOD",
        DefKind::Struct => "STRUCT",
        DefKind::Enum => "ENUM",
        DefKind::Union => "UNION",
        DefKind::Trait => "TRAIT",
        DefKind::Mod => "MOD",
        DefKind::Impl => "IMPL",
        DefKind::TypeAlias => "TYPE_ALIAS",
        DefKind::Const => "CONST",
        DefKind::Static => "STATIC",
        DefKind::Field => "FIELD",
        DefKind::Variant => "VARIANT",
        DefKind::AssocType => "ASSOC_TYPE",
        DefKind::AssocConst => "ASSOC_CONST",
        DefKind::Macro => "MACRO",
        DefKind::Other => "OTHER",
    };
    ExternLookupValue::Enum {
        name: "DefKind".into(),
        variant: variant.into(),
    }
}

pub(crate) fn def_kind_from_variant(variant: &str) -> Option<DefKind> {
    match variant {
        "FN" => Some(DefKind::Fn),
        "METHOD" => Some(DefKind::Method),
        "STRUCT" => Some(DefKind::Struct),
        "ENUM" => Some(DefKind::Enum),
        "UNION" => Some(DefKind::Union),
        "TRAIT" => Some(DefKind::Trait),
        "MOD" => Some(DefKind::Mod),
        "IMPL" => Some(DefKind::Impl),
        "TYPE_ALIAS" => Some(DefKind::TypeAlias),
        "CONST" => Some(DefKind::Const),
        "STATIC" => Some(DefKind::Static),
        "FIELD" => Some(DefKind::Field),
        "VARIANT" => Some(DefKind::Variant),
        "ASSOC_TYPE" => Some(DefKind::AssocType),
        "ASSOC_CONST" => Some(DefKind::AssocConst),
        "MACRO" => Some(DefKind::Macro),
        "OTHER" => Some(DefKind::Other),
        _ => None,
    }
}

pub(crate) fn module_def_is_public(def: ModuleDef, db: &dyn hir::db::HirDatabase) -> bool {
    match def {
        ModuleDef::Module(module) => module.visibility(db) == hir::Visibility::Public,
        ModuleDef::Function(function) => function.visibility(db) == hir::Visibility::Public,
        ModuleDef::Adt(adt) => adt.visibility(db) == hir::Visibility::Public,
        ModuleDef::Variant(variant) => variant.visibility(db) == hir::Visibility::Public,
        ModuleDef::Const(const_) => const_.visibility(db) == hir::Visibility::Public,
        ModuleDef::Static(static_) => static_.visibility(db) == hir::Visibility::Public,
        ModuleDef::Trait(trait_) => trait_.visibility(db) == hir::Visibility::Public,
        ModuleDef::TypeAlias(alias) => alias.visibility(db) == hir::Visibility::Public,
        ModuleDef::Macro(mac) => mac.visibility(db) == hir::Visibility::Public,
        ModuleDef::BuiltinType(_) => false,
    }
}

pub(crate) fn module_def_in_test(def: ModuleDef, db: &dyn hir::db::HirDatabase) -> bool {
    let module = match def {
        ModuleDef::Module(module) => module,
        ModuleDef::Function(function) => {
            return function.is_test(db) || module_is_test_scope(function.module(db), db);
        }
        ModuleDef::Adt(adt) => adt.module(db),
        ModuleDef::Variant(variant) => variant.module(db),
        ModuleDef::Const(const_) => const_.module(db),
        ModuleDef::Static(static_) => static_.module(db),
        ModuleDef::Trait(trait_) => trait_.module(db),
        ModuleDef::TypeAlias(alias) => alias.module(db),
        ModuleDef::Macro(mac) => mac.module(db),
        ModuleDef::BuiltinType(_) => return false,
    };
    module_is_test_scope(module, db)
}

pub(crate) fn module_is_test_scope(module: Module, db: &dyn hir::db::HirDatabase) -> bool {
    module.path_to_root(db).into_iter().any(|m| {
        m.name(db)
            .is_some_and(|name| name.display(db, Edition::CURRENT).to_string() == "tests")
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

fn lookup_bound_def_name_from_core_index(
    core_index: Option<&CoreLookupIndex>,
    def: DefId,
) -> Option<Box<str>> {
    core_index?
        .def_name(def)
        .map(|name| name.to_owned().into_boxed_str())
}

fn lookup_bound_def_path_from_core_index(
    core_index: Option<&CoreLookupIndex>,
    def: DefId,
) -> Option<Box<str>> {
    core_index?
        .def_path(def)
        .map(|path| path.to_owned().into_boxed_str())
}

fn lookup_bound_def_flag_from_core_index(
    core_index: Option<&CoreLookupIndex>,
    def: DefId,
    predicate: &str,
) -> Option<bool> {
    match predicate {
        "is_public" => core_index?.is_public(def),
        "in_test" => core_index?.in_test(def),
        _ => None,
    }
}

pub(crate) fn lookup_def_rows(
    request: &ExternLookupRequest,
    core_index: Option<&CoreLookupIndex>,
    lookup_defs: &BTreeMap<DefId, LookupDefRecord>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut def = None::<DefId>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                def = Some(DefId::new(host.stable_id()));
            }
            _ => return Vec::new(),
        }
    }
    if let Some(def) = def {
        if lookup_defs.contains_key(&def) || core_index.is_some_and(|index| index.contains_def(def)) {
            return vec![vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                def.stable_id(),
            ))]];
        }
        return Vec::new();
    }
    let mut rows = Vec::new();
    if let Some(core_index) = core_index {
        rows.extend(core_index.def_ids().map(|def| {
            vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                ExternLookupHostValueKind::Def,
                def.stable_id(),
            ))]
        }));
    }
    rows.extend(
        lookup_defs
            .keys()
            .copied()
            .filter(|def_id| !core_index.is_some_and(|index| index.contains_def(*def_id)))
            .map(|def| {
                vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Def,
                    def.stable_id(),
                ))]
            }),
    );
    rows
}

pub(crate) fn lookup_def_kind_rows(
    request: &ExternLookupRequest,
    core_index: Option<&CoreLookupIndex>,
    lookup_defs: &BTreeMap<DefId, LookupDefRecord>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut def = None::<DefId>;
    let mut kind_filter = None::<DefKind>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                def = Some(DefId::new(host.stable_id()));
            }
            (1, ExternLookupValue::Enum { name, variant }) if name.as_ref() == "DefKind" => {
                kind_filter = def_kind_from_variant(variant.as_ref());
            }
            _ => return Vec::new(),
        }
    }
    let Some(def) = def else {
        return Vec::new();
    };
    let Some(kind) = lookup_defs
        .get(&def)
        .map(|record| record.kind)
        .or_else(|| core_index.and_then(|index| index.def_kind(def)))
    else {
        return Vec::new();
    };
    if kind_filter.is_some_and(|expected| expected != kind) {
        return Vec::new();
    }
    vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Def,
            def.stable_id(),
        )),
        def_kind_lookup_value(kind),
    ]]
}

pub(crate) fn lookup_def_span_rows(
    request: &ExternLookupRequest,
    core_index: Option<&CoreLookupIndex>,
    lookup_defs: &BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut def = None::<DefId>;
    let mut span_filter = None::<SpanId>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                def = Some(DefId::new(host.stable_id()));
            }
            (1, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Span =>
            {
                span_filter = Some(SpanId::new(host.stable_id()));
            }
            _ => return Vec::new(),
        }
    }
    let Some(def) = def else {
        return Vec::new();
    };
    let span = if let Some(record) = lookup_defs.get(&def) {
        record.span
    } else if let Some(index) = core_index {
        let Some(span) = index.def_span(def) else {
            return Vec::new();
        };
        if let Some(key) = index.span_key(span) {
            lookup_spans.entry(span).or_insert_with(|| key.clone());
        }
        span
    } else {
        return Vec::new();
    };
    if span_filter.is_some_and(|expected| expected != span) {
        return Vec::new();
    }
    vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Def,
            def.stable_id(),
        )),
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Span,
            span.stable_id(),
        )),
    ]]
}

pub(crate) fn lookup_def_handle_rows(
    request: &ExternLookupRequest,
    core_index: Option<&CoreLookupIndex>,
    lookup_defs: &BTreeMap<DefId, LookupDefRecord>,
) -> Vec<Vec<ExternLookupValue>> {
    let mut def = None::<DefId>;
    let mut handle_filter = None::<&str>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                def = Some(DefId::new(host.stable_id()));
            }
            (1, ExternLookupValue::String(handle)) => handle_filter = Some(handle.as_ref()),
            _ => return Vec::new(),
        }
    }
    let Some(def) = def else {
        return Vec::new();
    };
    let handle = if let Some(record) = lookup_defs.get(&def) {
        record
            .path
            .as_deref()
            .map(|path| format!("def://{path}"))
            .map(|handle| ExternLookupValue::String(handle.into_boxed_str()))
    } else {
        core_index
            .and_then(|index| index.def_handle(def))
            .map(|handle| ExternLookupValue::String(handle.as_str().to_string().into_boxed_str()))
    };
    let Some(ExternLookupValue::String(handle)) = handle else {
        return Vec::new();
    };
    if handle_filter.is_some_and(|expected| expected != handle.as_ref()) {
        return Vec::new();
    }
    vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Def,
            def.stable_id(),
        )),
        ExternLookupValue::String(handle),
    ]]
}

pub(crate) fn lookup_def_name_rows(
    request: &ExternLookupRequest,
    db: &ide::RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    core_index: Option<&CoreLookupIndex>,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
    lookup_spans: &mut BTreeMap<SpanId, SpanKey>,
) -> Result<Vec<Vec<ExternLookupValue>>, RaHostInitError> {
    let mut requested_name = None::<&str>;
    let mut def_filter = None::<DefId>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                def_filter = Some(DefId::new(host.stable_id()));
            }
            (1, ExternLookupValue::String(name)) => requested_name = Some(name.as_ref()),
            _ => return Ok(Vec::new()),
        }
    }
    if let Some(def) = def_filter {
        if let Some(record) = lookup_defs.get(&def) {
            if requested_name.is_some_and(|expected| expected != record.name.as_ref()) {
                return Ok(Vec::new());
            }
            return Ok(vec![vec![
                ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Def,
                    def.stable_id(),
                )),
                ExternLookupValue::String(record.name.clone()),
            ]]);
        }
        if let Some(name) = lookup_bound_def_name_from_core_index(core_index, def) {
            if requested_name.is_some_and(|expected| expected != name.as_ref()) {
                return Ok(Vec::new());
            }
            return Ok(vec![vec![
                ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Def,
                    def.stable_id(),
                )),
                ExternLookupValue::String(name),
            ]]);
        }
    }
    let Some(requested_name) = requested_name else {
        return Ok(Vec::new());
    };

    let mut symbol_id_host = DeterministicRaHost::new();
    let mut symbol_rows = std::collections::BTreeSet::<Vec<ExternLookupValue>>::new();
    let mut collect_symbol_rows = |mut query: Query, include_functions: bool| {
        query.exact();
        query.exclude_imports();
        for symbol in world_symbols(db, query) {
            if symbol.is_alias || symbol.is_import {
                continue;
            }
            let def = symbol.def;
            if matches!(def, ModuleDef::Function(_)) != include_functions {
                continue;
            }
            let Some(module) = def.module(db) else {
                continue;
            };
            if !module.krate(db).origin(db).is_local() {
                continue;
            }
            let original = symbol.loc.hir_file_id.original_file_respecting_includes(db);
            let Some(def_id) = ensure_lookup_symbol_module_def(
                db,
                vfs,
                workspace_root,
                lookup_defs,
                lookup_spans,
                &mut symbol_id_host,
                def,
                original.editioned_file_id(db),
                symbol.loc.ptr.text_range(),
            ) else {
                continue;
            };
            if def_filter.is_some_and(|expected| expected != def_id) {
                continue;
            }
            symbol_rows.insert(vec![
                ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Def,
                    def_id.stable_id(),
                )),
                ExternLookupValue::String(requested_name.to_string().into_boxed_str()),
            ]);
        }
    };
    collect_symbol_rows(Query::new(requested_name.to_string()), false);
    collect_symbol_rows(Query::new(format!("{requested_name}#")), true);
    if !symbol_rows.is_empty() {
        return Ok(symbol_rows.into_iter().collect());
    }

    let mut id_host = DeterministicRaHost::new();
    let mut rows = std::collections::BTreeSet::<Vec<ExternLookupValue>>::new();
    for krate in hir::Crate::all(db)
        .into_iter()
        .filter(|krate| krate.origin(db).is_local())
    {
        let mut modules = vec![krate.root_module(db)];
        while let Some(module) = modules.pop() {
            let query_name =
                ide_db::imports::import_assets::NameToImport::Exact(requested_name.to_owned(), true);
            let _ = ide_db::items_locator::items_with_name_in_module(
                db,
                module,
                query_name,
                ide_db::items_locator::AssocSearchMode::Include,
                |item| {
                    let def = item.into_module_def();
                    let Some(def_id) = ensure_lookup_source_module_def(
                        db,
                        vfs,
                        workspace_root,
                        lookup_defs,
                        lookup_spans,
                        &mut id_host,
                        def,
                    ) else {
                        return std::ops::ControlFlow::<()>::Continue(());
                    };
                    if def_filter.is_some_and(|expected| expected != def_id) {
                        return std::ops::ControlFlow::<()>::Continue(());
                    }
                    rows.insert(vec![
                        ExternLookupValue::Host(ExternLookupHostValue::new(
                            ExternLookupHostValueKind::Def,
                            def_id.stable_id(),
                        )),
                        ExternLookupValue::String(requested_name.to_owned().into_boxed_str()),
                    ]);
                    std::ops::ControlFlow::<()>::Continue(())
                },
            );
            modules.extend(module.children(db));
        }
    }
    Ok(rows.into_iter().collect())
}

pub(crate) fn lookup_def_path_rows(
    request: &ExternLookupRequest,
    db: &ide::RootDatabase,
    core_index: Option<&CoreLookupIndex>,
    lookup_defs: &mut BTreeMap<DefId, LookupDefRecord>,
) -> Result<Option<Vec<Vec<ExternLookupValue>>>, RaHostInitError> {
    let mut def = None::<DefId>;
    let mut path_filter = None::<&str>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                def = Some(DefId::new(host.stable_id()));
            }
            (1, ExternLookupValue::String(path)) => path_filter = Some(path.as_ref()),
            _ => return Ok(None),
        }
    }
    let Some(def) = def else {
        return Ok(None);
    };
    let mut path = if let Some(record) = lookup_defs.get(&def).cloned() {
        let mut record = record;
        if record.path.is_none() {
            hir::attach_db(db, || {
                if let Some(function) = record.entity.as_ref().and_then(RaEntity::as_function) {
                    record.path = Some(canonical_function_path(db, function).into_boxed_str());
                } else if let Some(path) =
                    record.entity.as_ref().and_then(|entity| entity.canonical_path(db))
                {
                    record.path = Some(path.into_boxed_str());
                }
            });
            if let Some(entry) = lookup_defs.get_mut(&def) {
                if entry.path.is_none() {
                    entry.path = record.path.clone();
                }
                if entry.entity.is_none() {
                    entry.entity = record.entity.clone();
                }
            }
        }
        record
            .path
            .or_else(|| lookup_bound_def_path_from_core_index(core_index, def))
    } else {
        lookup_bound_def_path_from_core_index(core_index, def)
    };
    let Some(path) = path.take() else {
        return Ok(None);
    };
    if path_filter.is_some_and(|expected| expected != path.as_ref()) {
        return Ok(Some(Vec::new()));
    }
    Ok(Some(vec![vec![
        ExternLookupValue::Host(ExternLookupHostValue::new(
            ExternLookupHostValueKind::Def,
            def.stable_id(),
        )),
        ExternLookupValue::String(path),
    ]]))
}

pub(crate) fn lookup_def_flag_rows(
    request: &ExternLookupRequest,
    core_index: Option<&CoreLookupIndex>,
    predicate: &str,
) -> Option<Vec<Vec<ExternLookupValue>>> {
    let mut def = None::<DefId>;
    for (idx, value) in request.bound_positions().iter().zip(request.bound_values()) {
        match (*idx, value) {
            (0, ExternLookupValue::Host(host))
                if host.kind() == ExternLookupHostValueKind::Def =>
            {
                def = Some(DefId::new(host.stable_id()));
            }
            _ => return None,
        }
    }
    if let Some(def) = def {
        let flag = lookup_bound_def_flag_from_core_index(core_index, def, predicate)?;
        if !flag {
            return Some(Vec::new());
        }
        return Some(vec![vec![ExternLookupValue::Host(
            ExternLookupHostValue::new(ExternLookupHostValueKind::Def, def.stable_id()),
        )]]);
    }
    let core_index = core_index?;
    Some(
        core_index
            .def_ids()
            .filter(|def| {
                lookup_bound_def_flag_from_core_index(Some(core_index), *def, predicate)
                    == Some(true)
            })
            .map(|def| {
                vec![ExternLookupValue::Host(ExternLookupHostValue::new(
                    ExternLookupHostValueKind::Def,
                    def.stable_id(),
                ))]
            })
            .collect(),
    )
}
