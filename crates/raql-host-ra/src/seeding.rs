use super::*;
use std::fs;
use std::path::Path;
use syntax::ast::{self, HasAttrs, HasGenericArgs, HasModuleItem, HasName, HasVisibility};
use syntax::{AstNode, Edition, SourceFile, SyntaxNode};
use vfs::FileId;

#[derive(Debug, Default)]
struct SeedContext {
    callables_by_name: BTreeMap<String, BTreeSet<DefId>>,
    callables_by_scope: BTreeMap<String, BTreeMap<String, BTreeSet<DefId>>>,
    callables_by_path: BTreeMap<String, DefId>,
    methods_by_name: BTreeMap<String, BTreeSet<DefId>>,
    methods_by_scope: BTreeMap<String, BTreeMap<String, BTreeSet<DefId>>>,
    fn_bodies: Vec<FnBodySeed>,
    next_impl_index: usize,
}

#[derive(Debug, Clone)]
struct FnBodySeed {
    caller: DefId,
    caller_scope: String,
    body: SyntaxNode,
    rel_path: String,
    source: String,
    file_id: EditionedFileId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ModuleScope {
    segments: Vec<String>,
}

impl ModuleScope {
    fn crate_root() -> Self {
        Self {
            segments: vec!["crate".to_string()],
        }
    }

    fn from_qualified_path(path: &str) -> Self {
        let scope = path.rsplit_once("::").map(|(scope, _)| scope).unwrap_or("");
        if scope.is_empty() {
            Self {
                segments: Vec::new(),
            }
        } else {
            Self {
                segments: scope
                    .split("::")
                    .map(|segment| segment.to_string())
                    .collect(),
            }
        }
    }

    fn extend(&self, segment: &str) -> Self {
        let mut segments = self.segments.clone();
        segments.push(segment.to_string());
        Self { segments }
    }

    fn qualified_name(&self, member: &str) -> String {
        if self.segments.is_empty() {
            member.to_string()
        } else {
            format!("{}::{member}", self.segments.join("::"))
        }
    }

    fn key(&self) -> String {
        self.segments.join("::")
    }
}

impl SeedContext {
    fn register_callable(&mut self, scope: &str, path: &str, name: &str, def: DefId) {
        let scope_key = scope.to_string();
        let name_key = name.to_string();
        self.callables_by_name
            .entry(name_key.clone())
            .or_default()
            .insert(def);
        self.callables_by_scope
            .entry(scope_key)
            .or_default()
            .entry(name_key)
            .or_default()
            .insert(def);
        self.callables_by_path.insert(path.to_string(), def);
    }

    fn register_method(&mut self, scope: &str, name: &str, def: DefId) {
        let scope_key = scope.to_string();
        let name_key = name.to_string();
        self.methods_by_name
            .entry(name_key.clone())
            .or_default()
            .insert(def);
        self.methods_by_scope
            .entry(scope_key)
            .or_default()
            .entry(name_key)
            .or_default()
            .insert(def);
    }

    fn resolve_path(&self, path: &str) -> Option<DefId> {
        self.callables_by_path.get(path).copied()
    }

    fn resolve_callable(&self, scope: &str, name: &str) -> Option<DefId> {
        let mut current = Some(scope);
        while let Some(scope_name) = current {
            if let Some(candidates) = self
                .callables_by_scope
                .get(scope_name)
                .and_then(|entries| entries.get(name))
                .and_then(|defs| defs.iter().next().copied())
            {
                return Some(candidates);
            }
            current = scope_name.rsplit_once("::").map(|(parent, _)| parent);
        }
        self.callables_by_name
            .get(name)
            .and_then(|defs| defs.iter().next().copied())
    }

    fn resolve_unambiguous_method(&self, scope: &str, name: &str) -> Option<DefId> {
        let mut candidates = BTreeSet::new();
        let mut current = Some(scope);
        while let Some(scope_name) = current {
            if let Some(defs) = self
                .methods_by_scope
                .get(scope_name)
                .and_then(|entries| entries.get(name))
            {
                candidates.extend(defs.iter().copied());
            }
            current = scope_name.rsplit_once("::").map(|(parent, _)| parent);
        }
        if candidates.is_empty()
            && let Some(defs) = self.methods_by_name.get(name)
        {
            candidates.extend(defs.iter().copied());
        }
        if candidates.len() == 1 {
            candidates.into_iter().next()
        } else {
            None
        }
    }

    fn next_impl_index(&mut self) -> usize {
        self.next_impl_index += 1;
        self.next_impl_index
    }
}

pub(super) fn seed_host_from_source(
    host: &mut DeterministicRaHost,
    rel_path: &str,
    source: &str,
    file_id: EditionedFileId,
) {
    let mut context = SeedContext::default();
    let root_scope = ModuleScope::crate_root();
    seed_host_from_source_with_context(host, &mut context, rel_path, source, file_id, &root_scope);
    seed_call_edges(host, &context);
}

fn seed_host_from_source_with_context(
    host: &mut DeterministicRaHost,
    context: &mut SeedContext,
    rel_path: &str,
    source: &str,
    file_id: EditionedFileId,
    root_scope: &ModuleScope,
) {
    let parse = SourceFile::parse(source, Edition::CURRENT);
    let file = parse.tree();
    for item in file.items() {
        seed_item(
            host, context, rel_path, source, file_id, root_scope, false, item,
        );
    }
}

pub(super) fn seed_host_from_source_tree(
    host: &mut DeterministicRaHost,
    root_file: &Path,
    root_source: &str,
) {
    let mut context = SeedContext::default();
    for (path, source) in collect_rust_sources(root_file, root_source) {
        let scope = module_scope_for_file(root_file, path.as_str());
        seed_host_from_source_with_context(
            host,
            &mut context,
            path.as_str(),
            source.as_str(),
            seeded_file_id(path.as_str()),
            &scope,
        );
    }
    seed_call_edges(host, &context);
}

fn module_scope_for_file(root_file: &Path, file_path: &str) -> ModuleScope {
    let mut scope = ModuleScope::crate_root();
    let root_path = canonical_utf8ish(root_file);
    if file_path == root_path {
        return scope;
    }

    let Some(root_dir) = root_file.parent() else {
        return scope;
    };

    let path = Path::new(file_path);
    let relative = path
        .strip_prefix(root_dir)
        .or_else(|_| {
            let canonical_root_dir = canonical_utf8ish(root_dir);
            path.strip_prefix(Path::new(canonical_root_dir.as_str()))
        })
        .ok();
    let Some(relative) = relative else {
        return scope;
    };

    let mut parts = relative
        .iter()
        .map(|part| part.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let Some(file_name) = parts.pop() else {
        return scope;
    };

    let file_segment = if file_name != "mod.rs" && file_name != "lib.rs" && file_name != "main.rs" {
        Path::new(file_name.as_str())
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(|stem| stem.to_string())
    } else {
        None
    };

    for segment in parts
        .into_iter()
        .filter(|segment| !segment.is_empty() && segment != ".")
    {
        scope = scope.extend(segment.as_str());
    }
    if let Some(segment) = file_segment {
        scope = scope.extend(segment.as_str());
    }
    scope
}

fn collect_rust_sources(root_file: &Path, root_source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let root_path = canonical_utf8ish(root_file);
    let mut seen = BTreeSet::new();
    seen.insert(root_path.clone());
    out.push((root_path, root_source.to_string()));

    let Some(root_dir) = root_file.parent() else {
        return out;
    };

    let mut stack = vec![root_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let path_text = canonical_utf8ish(path.as_path());
            if !seen.insert(path_text.clone()) {
                continue;
            }
            let Ok(text) = fs::read_to_string(path.as_path()) else {
                continue;
            };
            out.push((path_text, text));
        }
    }

    out
}

fn seeded_file_id(path: &str) -> EditionedFileId {
    let raw = (deterministic_stable_id("seed_file", path).as_u64() as u32) & 0x007f_ffff;
    EditionedFileId::current_edition(FileId::from_raw(raw))
}

fn seed_item(
    host: &mut DeterministicRaHost,
    context: &mut SeedContext,
    rel_path: &str,
    source: &str,
    file_id: EditionedFileId,
    scope: &ModuleScope,
    in_test_scope: bool,
    item: ast::Item,
) {
    match item {
        ast::Item::Fn(function) => {
            seed_function_item(
                host,
                context,
                rel_path,
                source,
                file_id,
                scope,
                in_test_scope,
                function,
                DefKind::Fn,
                None,
                MethodContainer::None,
            );
        }
        ast::Item::Struct(struct_item) => {
            let Some(name) = ast_name(&struct_item) else {
                return;
            };
            let path = scope.qualified_name(name.as_str());
            let span = intern_ast_span(host, file_id, rel_path, source, struct_item.syntax());
            let def = host.intern_def_from_token(
                format!(
                    "def:struct:{path}@{}..{}",
                    u32::from(struct_item.syntax().text_range().start()),
                    u32::from(struct_item.syntax().text_range().end())
                )
                .as_str(),
            );
            register_item_def(
                host,
                def,
                name.as_str(),
                DefKind::Struct,
                span,
                path.as_str(),
                ast_is_public(&struct_item),
                in_test_scope || ast_mentions_test(&struct_item),
            );
            if let Some(field_list) = struct_item.field_list() {
                seed_field_list(host, rel_path, source, file_id, def, field_list);
            }
        }
        ast::Item::Enum(enum_item) => {
            let Some(name) = ast_name(&enum_item) else {
                return;
            };
            let path = scope.qualified_name(name.as_str());
            let span = intern_ast_span(host, file_id, rel_path, source, enum_item.syntax());
            let def = host.intern_def_from_token(
                format!(
                    "def:enum:{path}@{}..{}",
                    u32::from(enum_item.syntax().text_range().start()),
                    u32::from(enum_item.syntax().text_range().end())
                )
                .as_str(),
            );
            register_item_def(
                host,
                def,
                name.as_str(),
                DefKind::Enum,
                span,
                path.as_str(),
                ast_is_public(&enum_item),
                in_test_scope || ast_mentions_test(&enum_item),
            );
            if let Some(variants) = enum_item.variant_list() {
                for variant in variants.variants() {
                    let Some(variant_name) = ast_name(&variant) else {
                        continue;
                    };
                    let variant_path = format!("{path}::{}", variant_name);
                    let variant_span =
                        intern_ast_span(host, file_id, rel_path, source, variant.syntax());
                    let variant_def = host.intern_def_from_token(
                        format!(
                            "def:variant:{variant_path}@{}..{}",
                            u32::from(variant.syntax().text_range().start()),
                            u32::from(variant.syntax().text_range().end())
                        )
                        .as_str(),
                    );
                    host.insert_def(
                        variant_def,
                        variant_name.as_str(),
                        DefKind::Variant,
                        variant_span,
                        variant_path.as_str(),
                    );
                    host.mark_public(variant_def, ast_is_public(&variant));
                    host.mark_in_test(variant_def, in_test_scope || ast_mentions_test(&variant));
                    host.insert_variant(def, variant_name.as_str(), variant_def);
                }
            }
        }
        ast::Item::Trait(trait_item) => {
            let Some(name) = ast_name(&trait_item) else {
                return;
            };
            let path = scope.qualified_name(name.as_str());
            let span = intern_ast_span(host, file_id, rel_path, source, trait_item.syntax());
            let def = host.intern_def_from_token(
                format!(
                    "def:trait:{path}@{}..{}",
                    u32::from(trait_item.syntax().text_range().start()),
                    u32::from(trait_item.syntax().text_range().end())
                )
                .as_str(),
            );
            register_item_def(
                host,
                def,
                name.as_str(),
                DefKind::Trait,
                span,
                path.as_str(),
                ast_is_public(&trait_item),
                in_test_scope || ast_mentions_test(&trait_item),
            );
            if let Some(assoc_items) = trait_item.assoc_item_list() {
                for assoc_item in assoc_items.assoc_items() {
                    if let ast::AssocItem::Fn(function) = assoc_item {
                        seed_function_item(
                            host,
                            context,
                            rel_path,
                            source,
                            file_id,
                            scope,
                            in_test_scope || ast_mentions_test(&trait_item),
                            function,
                            DefKind::Method,
                            Some(def),
                            MethodContainer::Trait,
                        );
                    }
                }
            }
        }
        ast::Item::Impl(impl_item) => {
            seed_impl_item(
                host,
                context,
                rel_path,
                source,
                file_id,
                scope,
                in_test_scope,
                impl_item,
            );
        }
        ast::Item::Module(module) => {
            let Some(name) = ast_name(&module) else {
                return;
            };
            let module_path = scope.qualified_name(name.as_str());
            let span = intern_ast_span(host, file_id, rel_path, source, module.syntax());
            let module_def = host.intern_def_from_token(
                format!(
                    "def:mod:{module_path}@{}..{}",
                    u32::from(module.syntax().text_range().start()),
                    u32::from(module.syntax().text_range().end())
                )
                .as_str(),
            );
            let module_is_test =
                in_test_scope || ast_mentions_test(&module) || name.as_str() == "tests";
            register_item_def(
                host,
                module_def,
                name.as_str(),
                DefKind::Mod,
                span,
                module_path.as_str(),
                ast_is_public(&module),
                module_is_test,
            );
            if let Some(items) = module.item_list() {
                let child_scope = scope.extend(name.as_str());
                for child in items.items() {
                    seed_item(
                        host,
                        context,
                        rel_path,
                        source,
                        file_id,
                        &child_scope,
                        module_is_test,
                        child,
                    );
                }
            }
        }
        ast::Item::Union(union_item) => {
            let Some(name) = ast_name(&union_item) else {
                return;
            };
            let path = scope.qualified_name(name.as_str());
            let span = intern_ast_span(host, file_id, rel_path, source, union_item.syntax());
            let def = host.intern_def_from_token(
                format!(
                    "def:union:{path}@{}..{}",
                    u32::from(union_item.syntax().text_range().start()),
                    u32::from(union_item.syntax().text_range().end())
                )
                .as_str(),
            );
            register_item_def(
                host,
                def,
                name.as_str(),
                DefKind::Union,
                span,
                path.as_str(),
                ast_is_public(&union_item),
                in_test_scope || ast_mentions_test(&union_item),
            );
        }
        ast::Item::TypeAlias(alias_item) => {
            let Some(name) = ast_name(&alias_item) else {
                return;
            };
            let path = scope.qualified_name(name.as_str());
            let span = intern_ast_span(host, file_id, rel_path, source, alias_item.syntax());
            let def = host.intern_def_from_token(
                format!(
                    "def:type_alias:{path}@{}..{}",
                    u32::from(alias_item.syntax().text_range().start()),
                    u32::from(alias_item.syntax().text_range().end())
                )
                .as_str(),
            );
            register_item_def(
                host,
                def,
                name.as_str(),
                DefKind::TypeAlias,
                span,
                path.as_str(),
                ast_is_public(&alias_item),
                in_test_scope || ast_mentions_test(&alias_item),
            );
            if let Some(ty) = alias_item.ty() {
                let _ = seed_type_ref(host, rel_path, source, file_id, ty);
            }
        }
        ast::Item::Const(const_item) => {
            let Some(name) = ast_name(&const_item) else {
                return;
            };
            let path = scope.qualified_name(name.as_str());
            let span = intern_ast_span(host, file_id, rel_path, source, const_item.syntax());
            let def = host.intern_def_from_token(
                format!(
                    "def:const:{path}@{}..{}",
                    u32::from(const_item.syntax().text_range().start()),
                    u32::from(const_item.syntax().text_range().end())
                )
                .as_str(),
            );
            register_item_def(
                host,
                def,
                name.as_str(),
                DefKind::Const,
                span,
                path.as_str(),
                ast_is_public(&const_item),
                in_test_scope || ast_mentions_test(&const_item),
            );
            if let Some(ty) = const_item.ty() {
                let _ = seed_type_ref(host, rel_path, source, file_id, ty);
            }
        }
        ast::Item::Static(static_item) => {
            let Some(name) = ast_name(&static_item) else {
                return;
            };
            let path = scope.qualified_name(name.as_str());
            let span = intern_ast_span(host, file_id, rel_path, source, static_item.syntax());
            let def = host.intern_def_from_token(
                format!(
                    "def:static:{path}@{}..{}",
                    u32::from(static_item.syntax().text_range().start()),
                    u32::from(static_item.syntax().text_range().end())
                )
                .as_str(),
            );
            register_item_def(
                host,
                def,
                name.as_str(),
                DefKind::Static,
                span,
                path.as_str(),
                ast_is_public(&static_item),
                in_test_scope || ast_mentions_test(&static_item),
            );
            if let Some(ty) = static_item.ty() {
                let _ = seed_type_ref(host, rel_path, source, file_id, ty);
            }
        }
        ast::Item::MacroCall(macro_call) => {
            let span = intern_ast_span(host, file_id, rel_path, source, macro_call.syntax());
            let macro_path = scope.qualified_name("macro_call");
            let def = host.intern_def_from_token(
                format!(
                    "def:macro:{}@{}..{}",
                    macro_path,
                    u32::from(macro_call.syntax().text_range().start()),
                    u32::from(macro_call.syntax().text_range().end())
                )
                .as_str(),
            );
            host.insert_def(def, "macro_call", DefKind::Macro, span, macro_path);
            host.mark_in_test(def, in_test_scope || ast_mentions_test(&macro_call));
        }
        _ => {}
    }
}

fn register_item_def(
    host: &mut DeterministicRaHost,
    def: DefId,
    name: &str,
    kind: DefKind,
    span: SpanId,
    path: &str,
    is_public: bool,
    in_test_scope: bool,
) {
    host.insert_def(def, name, kind, span, path);
    host.mark_public(def, is_public);
    host.mark_in_test(def, in_test_scope);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MethodContainer {
    None,
    Impl,
    Trait,
}

fn seed_function_item(
    host: &mut DeterministicRaHost,
    context: &mut SeedContext,
    rel_path: &str,
    source: &str,
    file_id: EditionedFileId,
    scope: &ModuleScope,
    in_test_scope: bool,
    function: ast::Fn,
    kind: DefKind,
    owner: Option<DefId>,
    container: MethodContainer,
) -> DefId {
    let name = ast_name(&function).unwrap_or_else(|| "anonymous".to_string());
    let path = if kind == DefKind::Method {
        if let Some(owner) = owner {
            format!("{}::{}", host.def_path_for(owner), name)
        } else {
            scope.qualified_name(name.as_str())
        }
    } else {
        scope.qualified_name(name.as_str())
    };
    let span = intern_ast_span(host, file_id, rel_path, source, function.syntax());
    let def = host.intern_def_from_token(
        format!(
            "def:{}:{path}@{}..{}",
            if kind == DefKind::Method {
                "method"
            } else {
                "fn"
            },
            u32::from(function.syntax().text_range().start()),
            u32::from(function.syntax().text_range().end())
        )
        .as_str(),
    );
    host.insert_def(def, name.as_str(), kind, span, path.as_str());
    host.mark_public(def, ast_is_public(&function));
    host.mark_in_test(def, in_test_scope || ast_mentions_test(&function));
    if let Some(owner) = owner {
        match container {
            MethodContainer::Impl => host.insert_method(owner, def),
            MethodContainer::Trait => host.insert_trait_method(owner, def),
            MethodContainer::None => {}
        }
    }

    let return_type = function
        .ret_type()
        .and_then(|ret| ret.ty())
        .map(|ty| seed_type_ref(host, rel_path, source, file_id, ty))
        .unwrap_or_else(|| {
            let token = format!(
                "type:unit-return:{}:{}..{}",
                path,
                u32::from(function.syntax().text_range().start()),
                u32::from(function.syntax().text_range().end())
            );
            let tr = host.intern_typeref_from_token(&token);
            host.insert_type(tr, TypeShape::Tuple(Vec::new()));
            tr
        });
    host.set_fn_return_type(def, Some(return_type));
    if let Some(err_def) = result_error_head_for_return(host, return_type) {
        host.set_fn_error_type(def, Some(err_def));
    } else {
        host.set_fn_error_type(def, None);
    }

    let callable_scope = ModuleScope::from_qualified_path(path.as_str()).key();
    let lexical_scope = scope.key();
    context.register_callable(callable_scope.as_str(), path.as_str(), name.as_str(), def);
    if kind == DefKind::Method {
        context.register_method(callable_scope.as_str(), name.as_str(), def);
        if lexical_scope != callable_scope {
            context.register_method(lexical_scope.as_str(), name.as_str(), def);
        }
    }
    if let Some(body) = function.body() {
        context.fn_bodies.push(FnBodySeed {
            caller: def,
            caller_scope: callable_scope,
            body: body.syntax().clone(),
            rel_path: rel_path.to_string(),
            source: source.to_string(),
            file_id,
        });
    }
    def
}

fn seed_impl_item(
    host: &mut DeterministicRaHost,
    context: &mut SeedContext,
    rel_path: &str,
    source: &str,
    file_id: EditionedFileId,
    scope: &ModuleScope,
    in_test_scope: bool,
    impl_item: ast::Impl,
) {
    let impl_index = context.next_impl_index();
    let impl_path = scope.qualified_name(format!("impl#{impl_index}").as_str());
    let impl_span = intern_ast_span(host, file_id, rel_path, source, impl_item.syntax());
    let impl_def = host.intern_def_from_token(
        format!(
            "def:impl:{impl_path}@{}..{}",
            u32::from(impl_item.syntax().text_range().start()),
            u32::from(impl_item.syntax().text_range().end())
        )
        .as_str(),
    );
    host.insert_def(
        impl_def,
        format!("impl#{impl_index}"),
        DefKind::Impl,
        impl_span,
        impl_path,
    );
    host.mark_in_test(impl_def, in_test_scope || ast_mentions_test(&impl_item));

    let owner_def = impl_item
        .self_ty()
        .map(|ty| seed_type_ref(host, rel_path, source, file_id, ty))
        .and_then(|tr| type_head_def_for(host, tr));
    let trait_def = impl_item
        .trait_()
        .map(|ty| seed_type_ref(host, rel_path, source, file_id, ty))
        .and_then(|tr| type_head_def_for(host, tr));

    if let (Some(owner), Some(tr)) = (owner_def, trait_def) {
        host.insert_implements(owner, tr, impl_def);
    }

    if let (Some(owner), Some(trait_ty)) = (owner_def, impl_item.trait_()) {
        if let ast::Type::PathType(path_ty) = trait_ty
            && let Some((trait_path, args)) = path_with_type_args(path_ty)
        {
            if trait_path.ends_with("From")
                && let Some(src_ty) = args.first()
            {
                let src_ref = seed_type_ref(host, rel_path, source, file_id, src_ty.clone());
                if let Some(src_def) = type_head_def_for(host, src_ref) {
                    host.insert_from_impl(src_def, owner, impl_def);
                }
            }
        }
    }

    if let Some(assoc_items) = impl_item.assoc_item_list() {
        for assoc_item in assoc_items.assoc_items() {
            if let ast::AssocItem::Fn(function) = assoc_item {
                seed_function_item(
                    host,
                    context,
                    rel_path,
                    source,
                    file_id,
                    scope,
                    in_test_scope || ast_mentions_test(&impl_item),
                    function,
                    DefKind::Method,
                    owner_def,
                    MethodContainer::Impl,
                );
            }
        }
    }
}

fn seed_field_list(
    host: &mut DeterministicRaHost,
    rel_path: &str,
    source: &str,
    file_id: EditionedFileId,
    owner: DefId,
    field_list: ast::FieldList,
) {
    match field_list {
        ast::FieldList::RecordFieldList(fields) => {
            for field in fields.fields() {
                let name = ast_name(&field).unwrap_or_else(|| "_".to_string());
                let ty = field
                    .ty()
                    .map(|ty| seed_type_ref(host, rel_path, source, file_id, ty))
                    .unwrap_or_else(|| {
                        let token = format!(
                            "type:field:{}:{}..{}",
                            name,
                            u32::from(field.syntax().text_range().start()),
                            u32::from(field.syntax().text_range().end())
                        );
                        let tr = host.intern_typeref_from_token(&token);
                        host.insert_type(tr, TypeShape::Unknown);
                        tr
                    });
                host.insert_field(owner, name.as_str(), ty);
            }
        }
        ast::FieldList::TupleFieldList(fields) => {
            for (index, field) in fields.fields().enumerate() {
                let name = format!("#{index}");
                let ty = field
                    .ty()
                    .map(|ty| seed_type_ref(host, rel_path, source, file_id, ty))
                    .unwrap_or_else(|| {
                        let token = format!(
                            "type:tuple-field:{}:{}..{}",
                            index,
                            u32::from(field.syntax().text_range().start()),
                            u32::from(field.syntax().text_range().end())
                        );
                        let tr = host.intern_typeref_from_token(&token);
                        host.insert_type(tr, TypeShape::Unknown);
                        tr
                    });
                host.insert_field(owner, name.as_str(), ty);
            }
        }
    }
}

fn seed_call_edges(host: &mut DeterministicRaHost, context: &SeedContext) {
    for body_seed in &context.fn_bodies {
        for call in body_seed.body.descendants().filter_map(ast::CallExpr::cast) {
            let callee = call_expr_callee_path(&call)
                .and_then(|path| {
                    context.resolve_path(path.as_str()).or_else(|| {
                        path.rsplit_once("::").and_then(|(_, name)| {
                            context.resolve_callable(body_seed.caller_scope.as_str(), name)
                        })
                    })
                })
                .or_else(|| {
                    call_expr_callee_name(&call).and_then(|name| {
                        context.resolve_callable(body_seed.caller_scope.as_str(), name.as_str())
                    })
                });
            let Some(callee) = callee else {
                continue;
            };
            let site = intern_ast_span(
                host,
                body_seed.file_id,
                body_seed.rel_path.as_str(),
                body_seed.source.as_str(),
                &call.syntax().clone(),
            );
            host.insert_call_edge(body_seed.caller, callee, site, DispatchKind::Direct);
        }
        for call in body_seed
            .body
            .descendants()
            .filter_map(ast::MethodCallExpr::cast)
        {
            let Some(name) = call.name_ref().map(ast_name_ref) else {
                continue;
            };
            let Some(callee) =
                context.resolve_unambiguous_method(body_seed.caller_scope.as_str(), name.as_str())
            else {
                continue;
            };
            let dispatch = match host
                .method_owner_for(callee)
                .map(|owner| host.def_kind_for(owner))
            {
                Some(DefKind::Trait) => DispatchKind::ThroughTrait,
                _ => DispatchKind::Direct,
            };
            let site = intern_ast_span(
                host,
                body_seed.file_id,
                body_seed.rel_path.as_str(),
                body_seed.source.as_str(),
                &call.syntax().clone(),
            );
            host.insert_call_edge(body_seed.caller, callee, site, dispatch);
        }
    }
}

fn seed_type_ref(
    host: &mut DeterministicRaHost,
    rel_path: &str,
    source: &str,
    file_id: EditionedFileId,
    ty: ast::Type,
) -> TypeRefId {
    let token = format!(
        "type:{rel_path}:{}..{}",
        u32::from(ty.syntax().text_range().start()),
        u32::from(ty.syntax().text_range().end())
    );
    let tr = host.intern_typeref_from_token(&token);
    match ty {
        ast::Type::PathType(path_ty) => {
            if let Some((path, args)) = path_with_type_args(path_ty) {
                if is_primitive_name(path.as_str()) {
                    host.insert_type(tr, TypeShape::Prim(path));
                } else if is_type_param_name(path.as_str()) {
                    let param = host.intern_def_from_token(format!("type-param:{path}").as_str());
                    if !host.def_records.contains_key(&param) {
                        host.insert_def(
                            param,
                            path.as_str(),
                            DefKind::AssocType,
                            fallback_def_span(param),
                            format!("type_param::{path}"),
                        );
                    }
                    host.insert_type(tr, TypeShape::Param(param));
                } else {
                    let head_def = host.intern_def_from_token(format!("type-head:{path}").as_str());
                    if !host.def_records.contains_key(&head_def) {
                        host.insert_def(
                            head_def,
                            path.rsplit("::").next().unwrap_or(path.as_str()),
                            DefKind::Other,
                            fallback_def_span(head_def),
                            path.as_str(),
                        );
                    }
                    let mut type_args = Vec::new();
                    for arg in args {
                        let arg_ref = seed_type_ref(host, rel_path, source, file_id, arg);
                        type_args.push(GenericArg::Type(arg_ref));
                    }
                    host.insert_type(
                        tr,
                        TypeShape::App {
                            head: head_def,
                            args: type_args,
                        },
                    );
                }
            } else {
                host.insert_type(tr, TypeShape::Unknown);
            }
        }
        ast::Type::RefType(ref_type) => {
            if let Some(inner_ty) = ref_type.ty() {
                let inner = seed_type_ref(host, rel_path, source, file_id, inner_ty);
                host.insert_type(
                    tr,
                    TypeShape::Ref {
                        mutability: if ref_type.mut_token().is_some() {
                            Mutability::Mut
                        } else {
                            Mutability::Shared
                        },
                        inner,
                    },
                );
            } else {
                host.insert_type(tr, TypeShape::Unknown);
            }
        }
        ast::Type::PtrType(ptr_type) => {
            if let Some(inner_ty) = ptr_type.ty() {
                let inner = seed_type_ref(host, rel_path, source, file_id, inner_ty);
                host.insert_type(
                    tr,
                    TypeShape::Ptr {
                        mutability: if ptr_type.mut_token().is_some() {
                            Mutability::Mut
                        } else {
                            Mutability::Shared
                        },
                        inner,
                    },
                );
            } else {
                host.insert_type(tr, TypeShape::Unknown);
            }
        }
        ast::Type::TupleType(tuple_type) => {
            let elems = tuple_type
                .fields()
                .map(|field| seed_type_ref(host, rel_path, source, file_id, field))
                .collect::<Vec<_>>();
            host.insert_type(tr, TypeShape::Tuple(elems));
        }
        ast::Type::SliceType(slice_type) => {
            if let Some(inner_ty) = slice_type.ty() {
                let inner = seed_type_ref(host, rel_path, source, file_id, inner_ty);
                host.insert_type(tr, TypeShape::Slice(inner));
            } else {
                host.insert_type(tr, TypeShape::Unknown);
            }
        }
        _ => {
            host.insert_type(tr, TypeShape::Unknown);
        }
    }
    tr
}

fn result_error_head_for_return(
    host: &DeterministicRaHost,
    return_type: TypeRefId,
) -> Option<DefId> {
    let TypeShape::App { head, args } = host.type_shapes.get(&return_type)? else {
        return None;
    };
    let head_path = host.def_path_for(*head);
    if head_path != "std::result::Result"
        && head_path != "core::result::Result"
        && head_path != "Result"
    {
        return None;
    }
    let err_ty = args.get(1)?;
    let GenericArg::Type(err_ty) = err_ty else {
        return None;
    };
    type_head_def_for(host, *err_ty)
}

fn type_head_def_for(host: &DeterministicRaHost, type_ref: TypeRefId) -> Option<DefId> {
    match host.type_shapes.get(&type_ref) {
        Some(TypeShape::App { head, .. }) => Some(*head),
        Some(TypeShape::Param(param)) => Some(*param),
        _ => None,
    }
}

fn path_with_type_args(path_ty: ast::PathType) -> Option<(String, Vec<ast::Type>)> {
    let path = path_ty.path()?;
    let normalized = normalize_path(path.clone());
    let last_segment = path.segment()?;
    let args = last_segment
        .generic_arg_list()
        .into_iter()
        .flat_map(|list| list.generic_args())
        .filter_map(|arg| match arg {
            ast::GenericArg::TypeArg(type_arg) => type_arg.ty(),
            _ => None,
        })
        .collect::<Vec<_>>();
    Some((normalized, args))
}

fn normalize_path(path: ast::Path) -> String {
    let mut names = Vec::new();
    collect_path_segments(path, &mut names);
    if names.is_empty() {
        "unknown".to_string()
    } else {
        names.join("::")
    }
}

fn collect_path_segments(path: ast::Path, out: &mut Vec<String>) {
    if let Some(qualifier) = path.qualifier() {
        collect_path_segments(qualifier, out);
    }
    if let Some(segment) = path.segment() {
        if let Some(name_ref) = segment.name_ref() {
            out.push(ast_name_ref(name_ref));
        }
    }
}

fn call_expr_callee_name(call: &ast::CallExpr) -> Option<String> {
    let expr = call.expr()?;
    let ast::Expr::PathExpr(path_expr) = expr else {
        return None;
    };
    let path = path_expr.path()?;
    let segment = path.segment()?;
    segment.name_ref().map(ast_name_ref)
}

fn call_expr_callee_path(call: &ast::CallExpr) -> Option<String> {
    let expr = call.expr()?;
    let ast::Expr::PathExpr(path_expr) = expr else {
        return None;
    };
    Some(normalize_path(path_expr.path()?))
}

fn ast_name<T: HasName>(node: &T) -> Option<String> {
    Some(node.name()?.syntax().text().to_string())
}

fn ast_name_ref(name: ast::NameRef) -> String {
    name.syntax().text().to_string()
}

fn ast_is_public<T: HasVisibility>(node: &T) -> bool {
    node.visibility().is_some()
}

fn ast_mentions_test<T: HasAttrs>(node: &T) -> bool {
    node.attrs()
        .any(|attr| attr.syntax().text().to_string().contains("test"))
}

fn intern_ast_span(
    host: &mut DeterministicRaHost,
    file_id: EditionedFileId,
    rel_path: &str,
    source: &str,
    node: &SyntaxNode,
) -> SpanId {
    host.intern_span_from_text(file_id, rel_path.to_string(), source, node.text_range())
        .unwrap_or_else(|_| {
            SpanId::new(deterministic_stable_id(
                "seed_span",
                format!(
                    "{rel_path}:{}..{}",
                    u32::from(node.text_range().start()),
                    u32::from(node.text_range().end())
                )
                .as_str(),
            ))
        })
}

fn is_primitive_name(path: &str) -> bool {
    matches!(
        path,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
            | "str"
            | "String"
    )
}

fn is_type_param_name(path: &str) -> bool {
    !path.contains("::")
        && path
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch == '_' || ch.is_ascii_digit())
}
