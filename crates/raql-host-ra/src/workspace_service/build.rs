use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use base_db::SourceDatabase;
use hir::{Adt, AssocItem, HasSource, HasVisibility, Impl, Module, ModuleDef};
use ide::AnalysisHost;
use ide_db::symbol_index::{Query, world_symbols};
use syntax::ast::{HasGenericArgs, HasName};
use syntax::{ast, AstNode, Edition};

use super::{CoreFactsBuilder, CoreHostBuildSpec, RaHostInitError, trace_timing};
use crate::provider::core_index::CoreLookupIndex;
use crate::provider::defs::{
    LocalFile, canonical_function_path, lookup_span_key_from_text, module_def_in_test,
    module_def_is_public, module_def_kind,
};
use crate::{DefId, DefKind, GenericArg, Mutability, SpanId, TypeShape, deterministic_stable_id};
use hir::import_map::AssocSearchMode;

pub(super) fn build_core_index_only(
    analysis_host: &AnalysisHost,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    tracked_files: &std::collections::BTreeSet<PathBuf>,
    build_spec: &CoreHostBuildSpec,
) -> Result<CoreLookupIndex, RaHostInitError> {
    let build_started = std::time::Instant::now();
    let db = analysis_host.raw_database();
    let mut files = BTreeMap::new();
    for (file_id, path) in vfs.iter() {
        let Some(abs) = path.as_path() else {
            continue;
        };
        let abs_path: &Path = abs.as_ref();
        let owned_path = abs_path.to_path_buf();
        if !tracked_files.contains(&owned_path) {
            continue;
        }
        let rel_path = if abs_path.starts_with(workspace_root) {
            abs_path
                .strip_prefix(workspace_root)
                .unwrap_or(abs_path)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            abs_path.to_string_lossy().replace('\\', "/")
        };
        let text = db.file_text(file_id).text(db).to_string();
        files.insert(file_id, LocalFile { rel_path, text });
    }

    let query_started = std::time::Instant::now();
    let mut query = Query::new(String::new());
    query.exclude_imports();
    query.assoc_search_mode(AssocSearchMode::Exclude);
    let symbols = world_symbols(db, query);
    trace_timing("workspace_service.build_core_index.world_symbols", query_started.elapsed());

    let lower_started = std::time::Instant::now();
    let mut core_index = CoreLookupIndex::default();
    hir::attach_db(db, || {
        let sema = hir::Semantics::new(db);
        let mut parsed_by_file = HashMap::new();
        for symbol in symbols {
            if symbol.is_import || symbol.is_alias {
                continue;
            }
            let Some(kind) = module_def_kind(symbol.def) else {
                continue;
            };
            let editioned = symbol.loc.hir_file_id.original_file(db);
            let Some(local) = files.get(&editioned.file_id(db)) else {
                continue;
            };
            let name = symbol.name.as_str();
            let path = match symbol.def {
                ModuleDef::Function(function) => canonical_function_path(db, function),
                def => def
                    .canonical_path(db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}")),
            };
            let token = match symbol.def {
                ModuleDef::Function(_) => format!("def:function:{path}"),
                _ => format!("def:{kind:?}:{path}"),
            };
            let def_id = DefId::new(deterministic_stable_id("def", token.as_str()));
            if core_index.contains_def(def_id) {
                continue;
            }
            core_index.record_def(
                def_id,
                name,
                kind,
                path.as_str(),
                Some(local.rel_path.as_str()),
            );
            if let ModuleDef::Function(function) = symbol.def {
                core_index.record_function(def_id, function);
            }
            if build_spec.def_spans {
                let root = parsed_by_file
                    .entry(symbol.loc.hir_file_id)
                    .or_insert_with(|| sema.parse_or_expand(symbol.loc.hir_file_id));
                let syntax = symbol.loc.ptr.to_node(root);
                let range = syntax.text_range();
                if let Some(span_key) =
                    lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range)
                {
                    let file_id = editioned.editioned_file_id(db);
                    let token = format!(
                        "{}:{}..{}",
                        file_id.as_u32(),
                        u32::from(range.start()),
                        u32::from(range.end())
                    );
                    let span = SpanId::new(deterministic_stable_id("span", token.as_str()));
                    core_index.record_span(def_id, span, span_key);
                }
            }
            if build_spec.def_publicity {
                core_index.mark_public(def_id, module_def_is_public(symbol.def, db));
            }
            if build_spec.def_test_flags {
                core_index.mark_in_test(def_id, module_def_in_test(symbol.def, db));
            }
        }
    });
    trace_timing("workspace_service.build_core_index.lower", lower_started.elapsed());
    trace_timing("workspace_service.build_core_index.total", build_started.elapsed());
    Ok(core_index)
}

impl<'db> CoreFactsBuilder<'db> {
    pub(super) fn process_adt(&mut self, adt: Adt, owner_def: DefId) {
        self.local_adts.push(adt);
        if !self.build_spec.adt_structure {
            return;
        }
        match adt {
            Adt::Struct(strukt) => self.insert_struct_fields(owner_def, strukt),
            Adt::Union(union) => self.insert_union_fields(owner_def, union),
            Adt::Enum(enum_) => {
                for variant in enum_.variants(self.db) {
                    let Some(variant_def) = self.register_module_def(ModuleDef::Variant(variant), false) else {
                        continue;
                    };
                    let variant_name = variant.name(self.db).display(self.db, Edition::CURRENT).to_string();
                    self.host.insert_variant(owner_def, variant_name, variant_def);
                }
            }
        }
    }

    fn insert_struct_fields(&mut self, owner_def: DefId, strukt: hir::Struct) {
        let Some(source) = strukt.source(self.db) else {
            return;
        };
        match source.value.field_list() {
            Some(ast::FieldList::RecordFieldList(fields)) => {
                for (source_field, hir_field) in fields.fields().zip(strukt.fields(self.db)) {
                    let Some(name) = source_field.name().map(|name| name.text().to_string()) else {
                        continue;
                    };
                    self.insert_field(
                        owner_def,
                        name.as_str(),
                        hir_field.ty(self.db).to_type(self.db),
                        source_field.ty(),
                    );
                }
            }
            Some(ast::FieldList::TupleFieldList(fields)) => {
                for ((index, source_field), hir_field) in
                    fields.fields().enumerate().zip(strukt.fields(self.db))
                {
                    let name = index.to_string();
                    self.insert_field(
                        owner_def,
                        name.as_str(),
                        hir_field.ty(self.db).to_type(self.db),
                        source_field.ty(),
                    );
                }
            }
            None => {}
        }
    }

    fn insert_union_fields(&mut self, owner_def: DefId, union: hir::Union) {
        let Some(source) = union.source(self.db) else {
            return;
        };
        let Some(fields) = source.value.record_field_list() else {
            return;
        };
        for (source_field, hir_field) in fields.fields().zip(union.fields(self.db)) {
            let Some(name) = source_field.name().map(|name| name.text().to_string()) else {
                continue;
            };
            self.insert_field(
                owner_def,
                name.as_str(),
                hir_field.ty(self.db).to_type(self.db),
                source_field.ty(),
            );
        }
    }

    fn insert_field(
        &mut self,
        owner_def: DefId,
        name: &str,
        ty: hir::Type,
        source_ty: Option<ast::Type>,
    ) {
        let owner_path = self
            .def_path_by_id
            .get(&owner_def)
            .map(String::as_str)
            .unwrap_or("unknown_owner");
        let type_ref =
            self.lower_type(format!("field:{owner_path}:{name}").as_str(), ty, source_ty.as_ref());
        self.host.insert_field(owner_def, name.to_string(), type_ref);
    }

    pub(super) fn extract_impls(&mut self) {
        let mut impls = Vec::new();
        for adt in self.local_adts.iter().copied() {
            let ty = match adt {
                Adt::Struct(strukt) => strukt.ty(self.db),
                Adt::Union(union) => union.ty(self.db),
                Adt::Enum(enum_) => enum_.ty(self.db),
            };
            impls.extend(Impl::all_for_type(self.db, ty));
        }
        for trait_ in self.local_traits.iter().copied() {
            impls.extend(Impl::all_for_trait(self.db, trait_));
        }
        for impl_def in impls {
            let impl_record_def = self.register_impl_def(impl_def);
            let self_ty_def = impl_def
                .self_ty(self.db)
                .as_adt()
                .and_then(|adt| self.register_module_def(ModuleDef::Adt(adt), false));
            if let (Some(owner), Some(trait_def), Some(impl_record)) = (
                self_ty_def,
                impl_def
                    .trait_(self.db)
                    .and_then(|trait_| self.register_module_def(ModuleDef::Trait(trait_), false)),
                impl_record_def,
            ) {
                self.host.insert_implements(owner, trait_def, impl_record);
            }
            let is_from_impl = impl_def
                .trait_(self.db)
                .is_some_and(|trait_| {
                    ModuleDef::Trait(trait_)
                        .canonical_path(self.db, Edition::CURRENT)
                        .is_some_and(|path| path.ends_with("::From"))
                        || trait_.name(self.db).display(self.db, Edition::CURRENT).to_string() == "From"
                });
            if is_from_impl
                && let (Some(dst), Some(impl_record), Some(src)) = (
                    self_ty_def,
                    impl_record_def,
                    impl_def
                        .trait_ref(self.db)
                        .and_then(|trait_ref| trait_ref.get_type_argument(1))
                        .and_then(|src_ty| src_ty.to_type(self.db).as_adt())
                        .and_then(|adt| self.register_module_def(ModuleDef::Adt(adt), false)),
                )
            {
                self.host.insert_from_impl(src, dst, impl_record);
            }
            for assoc in impl_def.items(self.db) {
                if let AssocItem::Function(function) = assoc {
                    let Some(method_def) = self.register_module_def(ModuleDef::Function(function), false) else {
                        continue;
                    };
                    self.local_functions.push(function);
                    self.register_function_types(method_def, function);
                    if let Some(owner) = self_ty_def {
                        self.host.insert_method(owner, method_def);
                    }
                }
            }
        }
    }

    pub(super) fn register_function_types(&mut self, function_def: DefId, function: hir::Function) {
        let source_ty = function
            .source(self.db)
            .and_then(|source| source.value.ret_type())
            .and_then(|ret| ret.ty());
        let function_path = self
            .def_path_by_id
            .get(&function_def)
            .cloned()
            .unwrap_or_else(|| format!("fn:{:#018x}", function_def.stable_id().as_u64()));
        let return_ty = function
            .async_ret_type(self.db)
            .unwrap_or_else(|| function.ret_type(self.db));
        let return_ref = self.lower_type(
            format!("fn_return:{function_path}").as_str(),
            return_ty.clone(),
            source_ty.as_ref(),
        );
        let error_def = self.result_error_def(return_ty);
        self.host.set_fn_return_type(function_def, Some(return_ref));
        self.host.set_fn_error_type(function_def, error_def);
    }

    fn result_error_def(&mut self, ty: hir::Type) -> Option<DefId> {
        let head = ty.as_adt()?;
        let path = ModuleDef::Adt(head).canonical_path(self.db, Edition::CURRENT)?;
        if !(matches!(
            path.as_str(),
            "std::result::Result" | "core::result::Result" | "result::Result"
        ) || path.ends_with("::Result"))
        {
            return None;
        }
        let err_ty = ty.type_arguments().nth(1)?;
        if let Some(adt) = err_ty.as_adt() {
            return Some(self.register_adt_def(adt));
        }
        err_ty
            .as_type_param(self.db)
            .map(|param| self.register_type_param_def(param, "fn_error"))
    }

    fn lower_type(
        &mut self,
        key: &str,
        ty: hir::Type,
        source_ty: Option<&ast::Type>,
    ) -> crate::TypeRefId {
        let type_ref = self.host.intern_typeref_from_token(&format!("ty:{key}"));
        let normalized_source = source_ty.cloned().map(normalize_type_ast);
        let shape = if ty.is_unknown() {
            TypeShape::Unknown
        } else if let Some((inner, mutability)) = ty.as_reference() {
            let inner_ast = normalized_source.as_ref().and_then(ref_inner_type);
            TypeShape::Ref {
                mutability: map_mutability(mutability),
                inner: self.lower_type(format!("{key}/ref").as_str(), inner, inner_ast.as_ref()),
            }
        } else if ty.is_raw_ptr() {
            let inner = ty
                .remove_raw_ptr()
                .expect("raw pointer types should expose inner type");
            let (ptr_mutability, inner_ast) = normalized_source
                .as_ref()
                .map(ptr_parts)
                .unwrap_or((Mutability::Shared, None));
            TypeShape::Ptr {
                mutability: ptr_mutability,
                inner: self.lower_type(format!("{key}/ptr").as_str(), inner, inner_ast.as_ref()),
            }
        } else if ty.is_tuple() {
            let item_asts = normalized_source.as_ref().map(tuple_item_asts).unwrap_or_default();
            TypeShape::Tuple(
                ty.tuple_fields(self.db)
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let child_ast = item_asts.get(index);
                        self.lower_type(format!("{key}/tuple/{index}").as_str(), item, child_ast)
                    })
                    .collect(),
            )
        } else if let Some(inner) = ty.as_slice() {
            let inner_ast = normalized_source.as_ref().and_then(slice_inner_type);
            TypeShape::Slice(self.lower_type(
                format!("{key}/slice").as_str(),
                inner,
                inner_ast.as_ref(),
            ))
        } else if let Some(param) = ty.as_type_param(self.db) {
            TypeShape::Param(self.register_type_param_def(param, key))
        } else if ty.is_never() {
            TypeShape::Prim("!".to_string())
        } else if let Some(builtin) = ty.as_builtin() {
            TypeShape::Prim(builtin.name().as_str().to_string())
        } else if let Some(adt) = ty.as_adt() {
            let arg_asts = normalized_source
                .as_ref()
                .map(path_type_arg_asts)
                .unwrap_or_default();
            TypeShape::App {
                head: self.register_adt_def(adt),
                args: ty
                    .type_arguments()
                    .enumerate()
                    .map(|(index, arg)| {
                        GenericArg::Type(self.lower_type(
                            format!("{key}/arg/{index}").as_str(),
                            arg,
                            arg_asts.get(index),
                        ))
                    })
                    .collect(),
            }
        } else {
            TypeShape::Unknown
        };
        self.host.insert_type(type_ref, shape);
        type_ref
    }

    pub(super) fn register_adt_def(&mut self, adt: Adt) -> DefId {
        if let Some(def_id) = self.register_module_def(ModuleDef::Adt(adt), false) {
            return def_id;
        }
        let name = ModuleDef::Adt(adt)
            .name(self.db)
            .map(|name| name.display(self.db, Edition::CURRENT).to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let raw_path = ModuleDef::Adt(adt)
            .canonical_path(self.db, Edition::CURRENT)
            .unwrap_or_else(|| format!("external::{name}"));
        let path = adt
            .module(self.db)
            .krate(self.db)
            .display_name(self.db)
            .map(|crate_name| crate_name.to_string())
            .filter(|crate_name| {
                raw_path != *crate_name && !raw_path.starts_with(format!("{crate_name}::").as_str())
            })
            .map(|crate_name| format!("{crate_name}::{raw_path}"))
            .unwrap_or(raw_path);
        let def_id = self
            .host
            .intern_def_from_token(format!("def:adt:{path}").as_str());
        if !self.def_path_by_id.contains_key(&def_id) {
            let kind = match adt {
                Adt::Struct(_) => DefKind::Struct,
                Adt::Enum(_) => DefKind::Enum,
                Adt::Union(_) => DefKind::Union,
            };
            self.host
                .insert_synthetic_def(def_id, name.as_str(), kind, path.as_str());
            self.host.mark_public(def_id, true);
            self.host.mark_in_test(def_id, false);
            self.record_core_def(def_id, name.as_str(), kind, path.as_str(), None);
            self.core_index.mark_public(def_id, true);
            self.core_index.mark_in_test(def_id, false);
        }
        def_id
    }

    pub(super) fn register_function_def(&mut self, function: hir::Function) -> Option<DefId> {
        self.register_module_def(ModuleDef::Function(function), false)
    }

    pub(super) fn register_synthetic_callable_def(
        &mut self,
        prefix: &str,
        syntax: &syntax::SyntaxNode,
        file_id: span::EditionedFileId,
        local: &LocalFile,
    ) -> DefId {
        let range = syntax.text_range();
        let label = syntax.text().to_string();
        let path = format!(
            "{prefix}::{}:{}..{}:{}",
            local.rel_path,
            u32::from(range.start()),
            u32::from(range.end()),
            label
        );
        let def_id = self
            .host
            .intern_def_from_token(format!("def:{prefix}:{path}").as_str());
        if !self.def_path_by_id.contains_key(&def_id) {
            let span = self
                .host
                .intern_span_from_text(file_id, local.rel_path.clone(), local.text.as_str(), range)
                .ok();
            if let Some(span) = span {
                self.host
                    .insert_def(def_id, label.as_str(), DefKind::Other, span, path.as_str());
                self.host.insert_handle(def_id, format!("def://{path}"));
                self.host.mark_public(def_id, false);
                self.host.mark_in_test(def_id, false);
            } else {
                self.host
                    .insert_synthetic_def(def_id, label.as_str(), DefKind::Other, path.as_str());
                self.host.mark_public(def_id, false);
                self.host.mark_in_test(def_id, false);
            }
            self.record_core_def(
                def_id,
                label.as_str(),
                DefKind::Other,
                path.as_str(),
                Some(local.rel_path.as_str()),
            );
            self.core_index.mark_public(def_id, false);
            self.core_index.mark_in_test(def_id, false);
        }
        def_id
    }

    fn register_type_param_def(&mut self, param: hir::TypeParam, key: &str) -> DefId {
        let name = param.name(self.db).display(self.db, Edition::CURRENT).to_string();
        let path = format!("type_param::{key}::{name}");
        let def_id = self
            .host
            .intern_def_from_token(format!("def:type_param:{path}").as_str());
        if !self.def_path_by_id.contains_key(&def_id) {
            self.host
                .insert_synthetic_def(def_id, name.as_str(), DefKind::Other, path.as_str());
            self.host.mark_public(def_id, false);
            self.host.mark_in_test(def_id, false);
            self.record_core_def(def_id, name.as_str(), DefKind::Other, path.as_str(), None);
            self.core_index.mark_public(def_id, false);
            self.core_index.mark_in_test(def_id, false);
        }
        def_id
    }

    fn register_impl_def(&mut self, impl_def: Impl) -> Option<DefId> {
        let source = impl_def.source(self.db)?;
        let editioned = source.file_id.original_file(self.db);
        let local = self.files.get(&editioned.file_id(self.db))?;
        let rel_path = local.rel_path.clone();
        let range = source.value.syntax().text_range();
        let span = self
            .host
            .intern_span_from_text(
                editioned.editioned_file_id(self.db),
                rel_path.clone(),
                local.text.as_str(),
                range,
            )
            .ok()?;
        let path = format!(
            "impl::{rel_path}:{}..{}",
            u32::from(range.start()),
            u32::from(range.end())
        );
        let def_id = self.host.intern_def_from_token(
            format!(
                "def:Impl:{rel_path}:{}..{}",
                u32::from(range.start()),
                u32::from(range.end())
            )
            .as_str(),
        );
        self.host.insert_def(def_id, "impl", DefKind::Impl, span, path.as_str());
        self.host.insert_handle(def_id, format!("def://{path}"));
        let impl_token = format!(
            "{rel_path}:{}..{}",
            u32::from(range.start()),
            u32::from(range.end())
        );
        let impl_id = crate::ImplId::new(crate::deterministic_stable_id(
            "impl",
            impl_token.as_str(),
        ));
        self.host
            .insert_impl_id(impl_id, format!("impl:{impl_token}"));
        self.host.mark_public(def_id, false);
        self.host
            .mark_in_test(def_id, rel_path.starts_with("tests/") || rel_path.contains("/tests/"));
        self.record_core_def(
            def_id,
            "impl",
            DefKind::Impl,
            path.as_str(),
            Some(rel_path.as_str()),
        );
        self.core_index.mark_public(def_id, false);
        self.core_index.mark_in_test(
            def_id,
            rel_path.starts_with("tests/") || rel_path.contains("/tests/"),
        );
        Some(def_id)
    }

    pub(super) fn register_module_def(&mut self, def: ModuleDef, in_test: bool) -> Option<DefId> {
        let name = def.name(self.db)?.display(self.db, Edition::CURRENT).to_string();
        let (kind, path, def_id) = match def {
            ModuleDef::Function(function) => {
                let kind = if function.has_self_param(self.db) {
                    DefKind::Method
                } else {
                    DefKind::Fn
                };
                let path = canonical_function_path(self.db, function);
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:function:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Module(_) => {
                let kind = DefKind::Mod;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Adt(Adt::Struct(_)) => {
                let kind = DefKind::Struct;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Adt(Adt::Enum(_)) => {
                let kind = DefKind::Enum;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Adt(Adt::Union(_)) => {
                let kind = DefKind::Union;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Variant(_) => {
                let kind = DefKind::Variant;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Const(_) => {
                let kind = DefKind::Const;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Static(_) => {
                let kind = DefKind::Static;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Trait(_) => {
                let kind = DefKind::Trait;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::TypeAlias(_) => {
                let kind = DefKind::TypeAlias;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::Macro(_) => {
                let kind = DefKind::Macro;
                let path = def
                    .canonical_path(self.db, Edition::CURRENT)
                    .unwrap_or_else(|| format!("crate::{name}"));
                let def_id = self
                    .host
                    .intern_def_from_token(format!("def:{kind:?}:{path}").as_str());
                (kind, path, def_id)
            }
            ModuleDef::BuiltinType(_) => return None,
        };
        if self.build_spec.def_spans {
            let (editioned_file, rel_path, range) = match def {
                ModuleDef::Module(module) => {
                    let range = module
                        .declaration_source_range(self.db)
                        .unwrap_or_else(|| module.definition_source_range(self.db));
                    let editioned = range.file_id.original_file(self.db);
                    let local = self.files.get(&editioned.file_id(self.db))?;
                    (editioned, local.rel_path.clone(), range.value)
                }
                ModuleDef::Function(function) => {
                    let source = function.source(self.db)?;
                    let editioned = source.file_id.original_file(self.db);
                    let local = self.files.get(&editioned.file_id(self.db))?;
                    (editioned, local.rel_path.clone(), source.value.syntax().text_range())
                }
                ModuleDef::Adt(adt) => {
                    let source = adt.source(self.db)?;
                    let editioned = source.file_id.original_file(self.db);
                    let local = self.files.get(&editioned.file_id(self.db))?;
                    (editioned, local.rel_path.clone(), source.value.syntax().text_range())
                }
                ModuleDef::Variant(variant) => {
                    let source = variant.source(self.db)?;
                    let editioned = source.file_id.original_file(self.db);
                    let local = self.files.get(&editioned.file_id(self.db))?;
                    (editioned, local.rel_path.clone(), source.value.syntax().text_range())
                }
                ModuleDef::Const(const_) => {
                    let source = const_.source(self.db)?;
                    let editioned = source.file_id.original_file(self.db);
                    let local = self.files.get(&editioned.file_id(self.db))?;
                    (editioned, local.rel_path.clone(), source.value.syntax().text_range())
                }
                ModuleDef::Static(static_) => {
                    let source = static_.source(self.db)?;
                    let editioned = source.file_id.original_file(self.db);
                    let local = self.files.get(&editioned.file_id(self.db))?;
                    (editioned, local.rel_path.clone(), source.value.syntax().text_range())
                }
                ModuleDef::Trait(trait_) => {
                    let source = trait_.source(self.db)?;
                    let editioned = source.file_id.original_file(self.db);
                    let local = self.files.get(&editioned.file_id(self.db))?;
                    (editioned, local.rel_path.clone(), source.value.syntax().text_range())
                }
                ModuleDef::TypeAlias(alias) => {
                    let source = alias.source(self.db)?;
                    let editioned = source.file_id.original_file(self.db);
                    let local = self.files.get(&editioned.file_id(self.db))?;
                    (editioned, local.rel_path.clone(), source.value.syntax().text_range())
                }
                ModuleDef::Macro(mac) => {
                    let source = mac.source(self.db)?;
                    let editioned = source.file_id.original_file(self.db);
                    let local = self.files.get(&editioned.file_id(self.db))?;
                    (editioned, local.rel_path.clone(), source.value.syntax().text_range())
                }
                ModuleDef::BuiltinType(_) => return None,
            };
            let local = self.files.get(&editioned_file.file_id(self.db))?;
            let span = self
                .host
                .intern_span_from_text(
                    editioned_file.editioned_file_id(self.db),
                    rel_path,
                    local.text.as_str(),
                    range,
                )
                .ok()?;
            if let Some(span_key) =
                lookup_span_key_from_text(local.rel_path.as_str(), local.text.as_str(), range)
            {
                self.core_index.record_span(def_id, span, span_key);
            }
            self.host.insert_def(def_id, name.as_str(), kind, span, path.as_str());
        } else {
            self.host
                .insert_synthetic_def(def_id, name.as_str(), kind, path.as_str());
        }
        if self.build_spec.def_handles {
            self.host.insert_handle(def_id, format!("def://{path}"));
        }
        let source_rel_path = match def {
            ModuleDef::Module(module) => {
                let range = module
                    .declaration_source_range(self.db)
                    .unwrap_or_else(|| module.definition_source_range(self.db));
                let editioned = range.file_id.original_file(self.db);
                self.files
                    .get(&editioned.file_id(self.db))
                    .map(|local| local.rel_path.clone())
            }
            ModuleDef::Function(function) => function.source(self.db).and_then(|source| {
                let editioned = source.file_id.original_file(self.db);
                self.files
                    .get(&editioned.file_id(self.db))
                    .map(|local| local.rel_path.clone())
            }),
            ModuleDef::Adt(adt) => adt.source(self.db).and_then(|source| {
                let editioned = source.file_id.original_file(self.db);
                self.files
                    .get(&editioned.file_id(self.db))
                    .map(|local| local.rel_path.clone())
            }),
            ModuleDef::Variant(variant) => variant.source(self.db).and_then(|source| {
                let editioned = source.file_id.original_file(self.db);
                self.files
                    .get(&editioned.file_id(self.db))
                    .map(|local| local.rel_path.clone())
            }),
            ModuleDef::Const(const_) => const_.source(self.db).and_then(|source| {
                let editioned = source.file_id.original_file(self.db);
                self.files
                    .get(&editioned.file_id(self.db))
                    .map(|local| local.rel_path.clone())
            }),
            ModuleDef::Static(static_) => static_.source(self.db).and_then(|source| {
                let editioned = source.file_id.original_file(self.db);
                self.files
                    .get(&editioned.file_id(self.db))
                    .map(|local| local.rel_path.clone())
            }),
            ModuleDef::Trait(trait_) => trait_.source(self.db).and_then(|source| {
                let editioned = source.file_id.original_file(self.db);
                self.files
                    .get(&editioned.file_id(self.db))
                    .map(|local| local.rel_path.clone())
            }),
            ModuleDef::TypeAlias(alias) => alias.source(self.db).and_then(|source| {
                let editioned = source.file_id.original_file(self.db);
                self.files
                    .get(&editioned.file_id(self.db))
                    .map(|local| local.rel_path.clone())
            }),
            ModuleDef::Macro(mac) => mac.source(self.db).and_then(|source| {
                let editioned = source.file_id.original_file(self.db);
                self.files
                    .get(&editioned.file_id(self.db))
                    .map(|local| local.rel_path.clone())
            }),
            ModuleDef::BuiltinType(_) => None,
        };
        self.record_core_def(
            def_id,
            name.as_str(),
            kind,
            path.as_str(),
            source_rel_path.as_deref(),
        );
        if let ModuleDef::Function(function) = def {
            self.core_index.record_function(def_id, function);
        }
        if self.build_spec.def_publicity {
            let is_public = match def {
                ModuleDef::Module(module) => module.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Function(function) => function.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Adt(adt) => adt.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Variant(variant) => variant.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Const(const_) => const_.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Static(static_) => static_.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Trait(trait_) => trait_.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::TypeAlias(alias) => alias.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::Macro(mac) => mac.visibility(self.db) == hir::Visibility::Public,
                ModuleDef::BuiltinType(_) => false,
            };
            self.host.mark_public(def_id, is_public);
            self.core_index.mark_public(def_id, is_public);
        }
        if self.build_spec.def_test_flags {
            let in_test_scope = match def {
                ModuleDef::Function(function) => {
                    function.is_test(self.db) || self.module_is_test(function.module(self.db))
                }
                ModuleDef::Module(module) => self.module_is_test(module),
                ModuleDef::Adt(adt) => self.module_is_test(adt.module(self.db)),
                ModuleDef::Variant(variant) => self.module_is_test(variant.module(self.db)),
                ModuleDef::Const(const_) => self.module_is_test(const_.module(self.db)),
                ModuleDef::Static(static_) => self.module_is_test(static_.module(self.db)),
                ModuleDef::Trait(trait_) => self.module_is_test(trait_.module(self.db)),
                ModuleDef::TypeAlias(alias) => self.module_is_test(alias.module(self.db)),
                ModuleDef::Macro(mac) => self.module_is_test(mac.module(self.db)),
                ModuleDef::BuiltinType(_) => false,
            };
            self.host.mark_in_test(def_id, in_test || in_test_scope);
            self.core_index.mark_in_test(def_id, in_test || in_test_scope);
        }
        Some(def_id)
    }

    pub(super) fn record_core_def(
        &mut self,
        def_id: DefId,
        name: &str,
        kind: DefKind,
        path: &str,
        source_rel_path: Option<&str>,
    ) {
        self.def_path_by_id.insert(def_id, path.to_owned());
        self.core_index
            .record_def(def_id, name, kind, path, source_rel_path);
    }

    pub(super) fn module_is_test(&self, module: Module) -> bool {
        module.path_to_root(self.db).into_iter().any(|m| {
            m.name(self.db)
                .is_some_and(|name| name.display(self.db, Edition::CURRENT).to_string() == "tests")
        })
    }
}

fn normalize_type_ast(ty: ast::Type) -> ast::Type {
    match ty {
        ast::Type::ParenType(paren) => paren.ty().map(normalize_type_ast).unwrap_or(ast::Type::ParenType(paren)),
        other => other,
    }
}

fn map_mutability(mutability: hir::Mutability) -> Mutability {
    match mutability {
        hir::Mutability::Mut => Mutability::Mut,
        hir::Mutability::Shared => Mutability::Shared,
    }
}

fn ref_inner_type(ty: &ast::Type) -> Option<ast::Type> {
    match ty {
        ast::Type::RefType(inner) => inner.ty().map(normalize_type_ast),
        _ => None,
    }
}

fn ptr_parts(ty: &ast::Type) -> (Mutability, Option<ast::Type>) {
    match ty {
        ast::Type::PtrType(ptr) => (
            if ptr.mut_token().is_some() {
                Mutability::Mut
            } else {
                Mutability::Shared
            },
            ptr.ty().map(normalize_type_ast),
        ),
        _ => (Mutability::Shared, None),
    }
}

fn tuple_item_asts(ty: &ast::Type) -> Vec<ast::Type> {
    match ty {
        ast::Type::TupleType(tuple) => tuple.fields().map(normalize_type_ast).collect(),
        _ => Vec::new(),
    }
}

fn slice_inner_type(ty: &ast::Type) -> Option<ast::Type> {
    match ty {
        ast::Type::SliceType(slice) => slice.ty().map(normalize_type_ast),
        _ => None,
    }
}

fn path_type_arg_asts(ty: &ast::Type) -> Vec<ast::Type> {
    match ty {
        ast::Type::PathType(path_ty) => path_ty
            .path()
            .and_then(|path| path.segment())
            .and_then(|segment| segment.generic_arg_list())
            .map(|args| {
                args.generic_args()
                    .filter_map(|arg| match arg {
                        ast::GenericArg::TypeArg(ty_arg) => ty_arg.ty().map(normalize_type_ast),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
