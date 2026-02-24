use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;

use base_db::{EditionedFileId, SourceDatabase};
use hir::{
    Adt, AssocItem, CallableKind, Crate, Function, HasCrate, HasSource, HasVisibility, Impl, Module,
    ModuleDef, Trait, Type,
};
use ide::{Analysis, AnalysisHost, CallHierarchyConfig, FilePosition, RootDatabase, Semantics};
use ide_db::MiniCore;
use rustc_hash::FxHasher;
use span::TextRange;
use syntax::ast;
use syntax::ast::HasName;
use syntax::{AstNode, Edition, SourceFile, SyntaxNode};
use vfs::FileId;

use crate::{
    DefId, DefKind, DeterministicRaHost, DispatchKind, GenericArg, Mutability, NodeKind,
    RaHostInitError, TypeRefId, TypeShape, WorldStamp,
};

#[derive(Debug, Clone)]
struct LocalFile {
    rel_path: String,
    text: String,
    editioned_file_id: EditionedFileId,
}

#[derive(Debug, Clone)]
struct SourceLoc {
    file_id: EditionedFileId,
    rel_path: String,
    range: TextRange,
}

pub(crate) fn build_host_snapshot(
    db: &RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
) -> Result<DeterministicRaHost, RaHostInitError> {
    let sema = Semantics::new(db);
    let analysis = AnalysisHost::with_database(db.clone()).analysis();
    let files = collect_local_files(db, vfs, workspace_root, &sema)?;
    let semantic_identity = compute_semantic_identity(db, workspace_root);

    let mut host = DeterministicRaHost::new();
    host.set_world_stamp(compute_world_stamp(
        workspace_root,
        &files,
        semantic_identity.as_str(),
    ));

    let mut builder = SnapshotBuilder {
        db,
        sema,
        analysis,
        files,
        host,
        def_path_by_id: BTreeMap::new(),
        def_ranges_by_file: BTreeMap::new(),
        function_by_def: BTreeMap::new(),
        local_adts: HashSet::new(),
        local_traits: HashSet::new(),
        processed_impls: HashSet::new(),
        typeref_by_key: BTreeMap::new(),
    };

    run_snapshot_extraction(|| {
        hir_ty::attach_db(db, || {
            builder.extract_defs_and_types();
            builder.extract_calls_and_nodes();
        });
    })?;

    Ok(builder.host)
}

fn run_snapshot_extraction<F>(f: F) -> Result<(), RaHostInitError>
where
    F: FnOnce(),
{
    let extraction = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    if let Err(panic_payload) = extraction {
        if panic_payload.is::<base_db::salsa::Cancelled>() {
            return Err(RaHostInitError::SemanticBuild {
                details: "workspace semantic snapshot build was cancelled".to_string(),
            });
        }
        std::panic::resume_unwind(panic_payload);
    }
    Ok(())
}

#[cfg(test)]
mod strict_mode_tests {
    use super::run_snapshot_extraction;
    use crate::RaHostInitError;
    use base_db::salsa;
    use salsa::Database;
    use std::sync::mpsc;
    use std::time::Duration;

    #[salsa::db]
    #[derive(Clone)]
    struct CancellationProbeDb {
        storage: salsa::Storage<Self>,
    }

    impl Default for CancellationProbeDb {
        fn default() -> Self {
            Self {
                storage: salsa::Storage::default(),
            }
        }
    }

    #[salsa::db]
    impl salsa::Database for CancellationProbeDb {}

    fn real_cancelled_payload() -> salsa::Cancelled {
        let mut writer = CancellationProbeDb::default();
        let reader = writer.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();

        let worker = std::thread::spawn(move || {
            ready_tx
                .send(())
                .expect("worker should signal readiness for cancellation probe");
            loop {
                let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    reader.unwind_if_revision_cancelled();
                }));
                match unwind {
                    Ok(()) => std::thread::yield_now(),
                    Err(payload) => match payload.downcast::<salsa::Cancelled>() {
                        Ok(cancelled) => {
                            cancelled_tx
                                .send(*cancelled)
                                .expect("worker should forward cancellation payload");
                            break;
                        }
                        Err(payload) => std::panic::resume_unwind(payload),
                    },
                }
            }
        });

        ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker thread must start before triggering cancellation");
        writer.trigger_cancellation();
        let cancelled = cancelled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker thread should observe cancellation payload");
        worker
            .join()
            .expect("worker thread should terminate cleanly after cancellation");
        cancelled
    }

    #[test]
    fn snapshot_extraction_maps_salsa_cancelled_to_semantic_build_error() {
        let cancelled = real_cancelled_payload();
        let err = run_snapshot_extraction(|| {
            std::panic::panic_any(cancelled);
        })
        .expect_err("cancelled panic should map to semantic build error");
        assert!(matches!(err, RaHostInitError::SemanticBuild { .. }));
        assert!(err.to_string().contains("cancelled"));
    }

    #[test]
    #[should_panic(expected = "strict-mode-non-cancelled-panic")]
    fn snapshot_extraction_rethrows_non_cancelled_panics() {
        let _ = run_snapshot_extraction(|| {
            panic!("strict-mode-non-cancelled-panic");
        });
    }

    #[test]
    fn snapshot_extraction_returns_ok_without_panics() {
        assert!(run_snapshot_extraction(|| {}).is_ok());
    }
}

struct SnapshotBuilder<'db> {
    db: &'db RootDatabase,
    sema: Semantics<'db, RootDatabase>,
    analysis: Analysis,
    files: BTreeMap<FileId, LocalFile>,
    host: DeterministicRaHost,
    def_path_by_id: BTreeMap<DefId, String>,
    def_ranges_by_file: BTreeMap<FileId, Vec<(TextRange, DefId)>>,
    function_by_def: BTreeMap<DefId, Function>,
    local_adts: HashSet<Adt>,
    local_traits: HashSet<Trait>,
    processed_impls: HashSet<Impl>,
    typeref_by_key: BTreeMap<String, TypeRefId>,
}

impl<'db> SnapshotBuilder<'db> {
    fn extract_defs_and_types(&mut self) {
        for krate in Crate::all(self.db) {
            if !self.files.contains_key(&krate.root_file(self.db)) {
                continue;
            }
            self.visit_module(krate.root_module(self.db), false);
        }
        self.extract_impls();
    }

    fn visit_module(&mut self, module: Module, inherited_test: bool) {
        let module_test = inherited_test || self.module_is_test(module);

        let _ = self.register_module_def(ModuleDef::Module(module), module_test);

        for def in module.declarations(self.db) {
            let Some(def_id) = self.register_module_def(def, module_test) else {
                continue;
            };

            match def {
                ModuleDef::Module(child) => self.visit_module(child, module_test),
                ModuleDef::Function(function) => self.process_function(function, def_id),
                ModuleDef::Adt(adt) => self.process_adt(adt, def_id, module_test),
                ModuleDef::Trait(trait_def) => {
                    for assoc in trait_def.items(self.db) {
                        match assoc {
                            AssocItem::Function(function) => {
                                let Some(method_def) = self.register_module_def(
                                    ModuleDef::Function(function),
                                    module_test,
                                ) else {
                                    continue;
                                };
                                self.process_function(function, method_def);
                                self.host.insert_trait_method(def_id, method_def);
                            }
                            AssocItem::Const(const_) => {
                                let _ =
                                    self.register_module_def(ModuleDef::Const(const_), module_test);
                            }
                            AssocItem::TypeAlias(alias) => {
                                let _ = self
                                    .register_module_def(ModuleDef::TypeAlias(alias), module_test);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

    }

    fn process_function(&mut self, function: Function, function_def: DefId) {
        let ret_ref = self.intern_type_ref(function.ret_type(self.db), Some(function_def));
        self.host.set_fn_return_type(function_def, Some(ret_ref));
        self.host.set_fn_error_type(function_def, None);
        if let Some(error_def) = self.function_error_def_for_function(function) {
            self.host.set_fn_error_type(function_def, Some(error_def));
        }
    }

    fn process_adt(&mut self, adt: Adt, owner_def: DefId, in_test: bool) {
        self.local_adts.insert(adt);
        let owner_path = self
            .def_path_by_id
            .get(&owner_def)
            .cloned()
            .unwrap_or_else(|| "crate::<adt>".to_string());

        match adt {
            Adt::Struct(strukt) => {
                for field in strukt.fields(self.db) {
                    let field_name = field
                        .name(self.db)
                        .display(self.db, Edition::CURRENT)
                        .to_string();
                    let Some(field_loc) = self.source_loc_for_field(field) else {
                        continue;
                    };
                    let Some(field_span) = self.intern_span(&field_loc) else {
                        continue;
                    };

                    let field_path = format!("{owner_path}::{field_name}");
                    let field_def = self.host.intern_def_from_token(
                        format!(
                            "field:{}:{}:{}..{}",
                            field_path,
                            field_loc.rel_path,
                            u32::from(field_loc.range.start()),
                            u32::from(field_loc.range.end())
                        )
                        .as_str(),
                    );
                    self.host.insert_def(
                        field_def,
                        field_name.as_str(),
                        DefKind::Field,
                        field_span,
                        field_path.as_str(),
                    );
                    self.def_path_by_id.insert(field_def, field_path.clone());

                    let field_ty =
                        self.intern_type_ref(field.ty(self.db).to_type(self.db), Some(owner_def));
                    self.host
                        .insert_field(owner_def, field_name.as_str(), field_ty);

                    let root = field.krate(self.db).root_module(self.db);
                    self.host
                        .mark_public(field_def, field.is_visible_from(self.db, root));
                    self.host.mark_in_test(field_def, in_test);
                }
            }
            Adt::Union(union) => {
                for field in union.fields(self.db) {
                    let field_name = field
                        .name(self.db)
                        .display(self.db, Edition::CURRENT)
                        .to_string();
                    let Some(field_loc) = self.source_loc_for_field(field) else {
                        continue;
                    };
                    let Some(field_span) = self.intern_span(&field_loc) else {
                        continue;
                    };

                    let field_path = format!("{owner_path}::{field_name}");
                    let field_def = self.host.intern_def_from_token(
                        format!(
                            "field:{}:{}:{}..{}",
                            field_path,
                            field_loc.rel_path,
                            u32::from(field_loc.range.start()),
                            u32::from(field_loc.range.end())
                        )
                        .as_str(),
                    );
                    self.host.insert_def(
                        field_def,
                        field_name.as_str(),
                        DefKind::Field,
                        field_span,
                        field_path.as_str(),
                    );
                    self.def_path_by_id.insert(field_def, field_path.clone());

                    let field_ty =
                        self.intern_type_ref(field.ty(self.db).to_type(self.db), Some(owner_def));
                    self.host
                        .insert_field(owner_def, field_name.as_str(), field_ty);

                    let root = field.krate(self.db).root_module(self.db);
                    self.host
                        .mark_public(field_def, field.is_visible_from(self.db, root));
                    self.host.mark_in_test(field_def, in_test);
                }
            }
            Adt::Enum(enum_) => {
                for variant in enum_.variants(self.db) {
                    let Some(variant_def) =
                        self.register_module_def(ModuleDef::Variant(variant), in_test)
                    else {
                        continue;
                    };
                    let variant_name = variant
                        .name(self.db)
                        .display(self.db, Edition::CURRENT)
                        .to_string();
                    self.host
                        .insert_variant(owner_def, variant_name.as_str(), variant_def);

                    for field in variant.fields(self.db) {
                        let field_name = field
                            .name(self.db)
                            .display(self.db, Edition::CURRENT)
                            .to_string();
                        let field_ty =
                            self.intern_type_ref(field.ty(self.db).to_type(self.db), Some(variant_def));
                        self.host
                            .insert_field(variant_def, field_name.as_str(), field_ty);
                    }
                }
            }
        }
    }

    fn extract_impls(&mut self) {
        let mut impls = Vec::new();
        for adt in self.local_adts.iter().copied() {
            let ty = match adt {
                Adt::Struct(strukt) => strukt.ty(self.db),
                Adt::Union(union) => union.ty(self.db),
                Adt::Enum(enum_) => enum_.ty(self.db),
            };
            for impl_def in Impl::all_for_type(self.db, ty) {
                if self.source_loc_for_impl(impl_def).is_some() {
                    impls.push(impl_def);
                }
            }
        }
        for trait_ in self.local_traits.iter().copied() {
            for impl_def in Impl::all_for_trait(self.db, trait_) {
                if self.source_loc_for_impl(impl_def).is_some() {
                    impls.push(impl_def);
                }
            }
        }

        for impl_def in impls {
            if !self.processed_impls.insert(impl_def) {
                continue;
            }
            let in_test = self.module_is_test(impl_def.module(self.db));
            self.process_impl(impl_def, in_test);
        }
    }

    fn process_impl(&mut self, impl_def: Impl, in_test: bool) {
        let Some(impl_loc) = self.source_loc_for_impl(impl_def) else {
            return;
        };
        let Some(impl_span) = self.intern_span(&impl_loc) else {
            return;
        };

        let impl_path = format!(
            "crate::impl@{}..{}",
            u32::from(impl_loc.range.start()),
            u32::from(impl_loc.range.end())
        );
        let impl_record = self.host.intern_def_from_token(
            format!(
                "impl:{}:{}:{}..{}",
                impl_path,
                impl_loc.rel_path,
                u32::from(impl_loc.range.start()),
                u32::from(impl_loc.range.end())
            )
            .as_str(),
        );
        self.host.insert_def(
            impl_record,
            "impl",
            DefKind::Impl,
            impl_span,
            impl_path.as_str(),
        );
        self.def_path_by_id.insert(impl_record, impl_path);
        self.host.mark_in_test(impl_record, in_test);

        let self_ty_def = Some(self.def_from_type_head(impl_def.self_ty(self.db), Some(impl_record)));

        if let (Some(self_ty_def), Some(trait_def_hir)) = (self_ty_def, impl_def.trait_(self.db))
        {
            if let Some(trait_def) =
                self.register_module_def(ModuleDef::Trait(trait_def_hir), in_test)
            {
                let from_src = if trait_def_hir
                    .name(self.db)
                    .display(self.db, Edition::CURRENT)
                    .to_string()
                    == "From"
                {
                    impl_def
                        .trait_ref(self.db)
                        .and_then(|trait_ref| trait_ref.get_type_argument(1))
                        .map(|arg| arg.to_type(self.db))
                        .map(|src_ty| self.def_from_type_head(src_ty, Some(impl_record)))
                } else {
                    None
                };
                self.host
                    .insert_implements(self_ty_def, trait_def, impl_record);
                if let Some(src_def) = from_src {
                    self.host
                        .insert_from_impl(src_def, self_ty_def, impl_record);
                }
            }
        }

        for assoc in impl_def.items(self.db) {
            match assoc {
                AssocItem::Function(function) => {
                    let Some(method_def) =
                        self.register_module_def(ModuleDef::Function(function), in_test)
                    else {
                        continue;
                    };
                    self.process_function(function, method_def);
                    if let Some(self_ty_def) = self_ty_def {
                        self.host.insert_method(self_ty_def, method_def);
                    }
                }
                AssocItem::Const(const_) => {
                    let _ = self.register_module_def(ModuleDef::Const(const_), in_test);
                }
                AssocItem::TypeAlias(alias) => {
                    let _ = self.register_module_def(ModuleDef::TypeAlias(alias), in_test);
                }
            }
        }
    }

    fn register_module_def(&mut self, def: ModuleDef, in_test: bool) -> Option<DefId> {
        let kind = def_kind(def)?;
        let loc = self.source_loc_for_module_def(def)?;
        let span = self.intern_span(&loc)?;
        let name = def
            .name(self.db)?
            .display(self.db, Edition::CURRENT)
            .to_string();
        let path = def
            .canonical_path(self.db, Edition::CURRENT)
            .map(normalize_canonical_path)
            .unwrap_or_else(|| format!("crate::{name}"));

        let token = format!(
            "def:{kind:?}:{path}:{}:{}..{}",
            loc.rel_path,
            u32::from(loc.range.start()),
            u32::from(loc.range.end())
        );
        let def_id = self.host.intern_def_from_token(token.as_str());
        self.host
            .insert_def(def_id, name.as_str(), kind, span, path.as_str());

        self.def_path_by_id.insert(def_id, path.clone());
        self.def_ranges_by_file
            .entry(loc.file_id.file_id(self.db))
            .or_default()
            .push((loc.range, def_id));
        if let ModuleDef::Function(function) = def {
            self.function_by_def.insert(def_id, function);
        }
        if let ModuleDef::Trait(trait_) = def {
            self.local_traits.insert(trait_);
        }

        let is_public = match def {
            ModuleDef::Module(module) => module.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Function(function) => {
                function.visibility(self.db) == hir::Visibility::Public
            }
            ModuleDef::Adt(adt) => adt.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Variant(variant) => variant.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Const(const_) => const_.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Static(static_) => static_.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Trait(trait_) => trait_.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::TypeAlias(alias) => alias.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::Macro(mac) => mac.visibility(self.db) == hir::Visibility::Public,
            ModuleDef::BuiltinType(_) => true,
        };
        self.host.mark_public(def_id, is_public);

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

        Some(def_id)
    }

    fn extract_calls_and_nodes(&mut self) {
        let files = self
            .files
            .iter()
            .map(|(id, file)| (*id, file.editioned_file_id))
            .collect::<Vec<_>>();

        for (file_id, editioned) in files {
            let source = self.sema.parse(editioned);
            self.extract_nodes_for_file(file_id, &source);
            self.extract_calls_from_semantic_ast(file_id, &source);
            self.extract_error_flow_for_file(file_id, &source);
        }
        self.extract_calls_via_call_hierarchy();
    }

    fn extract_nodes_for_file(&mut self, file_id: FileId, source: &SourceFile) {
        let Some(local) = self.files.get(&file_id).cloned() else {
            return;
        };

        let mut ptr_to_id: HashMap<syntax::SyntaxNodePtr, crate::NodeId> = HashMap::new();

        for syntax in source.syntax().descendants() {
            let Some(kind) = syntax_node_kind(&syntax) else {
                continue;
            };

            let ptr = syntax::SyntaxNodePtr::new(&syntax);
            let node = self.host.intern_node_from_syntax_ptr(ptr.clone());
            let parent = syntax.ancestors().skip(1).find_map(|ancestor| {
                let key = syntax::SyntaxNodePtr::new(&ancestor);
                ptr_to_id.get(&key).copied()
            });

            let Ok(span) = self.host.intern_span_from_text(
                local.editioned_file_id.editioned_file_id(self.db),
                local.rel_path.as_str(),
                local.text.as_str(),
                syntax.text_range(),
            ) else {
                continue;
            };

            self.host.insert_node(node, kind, span, parent);
            ptr_to_id.insert(ptr, node);
        }
    }

    fn extract_calls_via_call_hierarchy(&mut self) {
        let config = CallHierarchyConfig {
            exclude_tests: false,
            minicore: MiniCore::default(),
        };
        let functions = self
            .function_by_def
            .iter()
            .map(|(def, function)| (*def, *function))
            .collect::<Vec<_>>();

        for (caller_def, function) in functions {
            let Some(source) = function.source(self.db) else {
                continue;
            };
            let Some(name) = source.value.name() else {
                continue;
            };
            let file_id = source.file_id.original_file(self.db).file_id(self.db);
            if !self.files.contains_key(&file_id) {
                continue;
            }
            let position = FilePosition {
                file_id,
                offset: name.syntax().text_range().start(),
            };
            let Ok(Some(outgoing)) = self.analysis.outgoing_calls(&config, position) else {
                continue;
            };

            for call in outgoing {
                let target_range = call.target.focus_or_full_range();
                let Some(callee) =
                    self.resolve_def_from_semantic_range(call.target.file_id, target_range)
                else {
                    continue;
                };
                for site_range in call.ranges {
                    let Some(local) = self.files.get(&site_range.file_id).cloned() else {
                        continue;
                    };
                    let Some(site) = self.span_for_range(&local, site_range.range) else {
                        continue;
                    };
                    let dispatch = self.dispatch_kind_for_call_site(site_range.file_id, site_range.range);
                    self.host.insert_call_edge(caller_def, callee, site, dispatch);
                }
            }
        }
    }

    fn extract_calls_from_semantic_ast(&mut self, file_id: FileId, source: &SourceFile) {
        let Some(local) = self.files.get(&file_id).cloned() else {
            return;
        };

        for call in source.syntax().descendants().filter_map(ast::CallExpr::cast) {
            let Some(caller_fn) = enclosing_fn(call.syntax()) else {
                continue;
            };
            let Some(caller_hir) = self.sema.to_fn_def(&caller_fn) else {
                continue;
            };
            let Some(caller_def) = self.register_module_def(ModuleDef::Function(caller_hir), false) else {
                continue;
            };

            let Some(site) = self.span_for_range(&local, call.syntax().text_range()) else {
                continue;
            };

            let (callee, dispatch) = if let Some(expr) = call.expr() {
                if let Some(callable) = self.sema.resolve_expr_as_callable(&expr) {
                    self.resolve_semantic_callable_target(callable.kind(), call.syntax().text_range())
                } else if let ast::Expr::PathExpr(path_expr) = &expr {
                    path_expr
                        .path()
                        .and_then(|path| {
                            self.resolve_call_target_from_semantic_path(
                                path,
                                call.syntax().text_range(),
                            )
                        })
                        .unwrap_or_else(|| {
                        (
                            self.synthetic_call_def("callable_dyn", call.syntax().text_range()),
                            DispatchKind::Dyn,
                        )
                    })
                } else {
                    match expr {
                        ast::Expr::ClosureExpr(_) => (
                            self.synthetic_call_def("callable_closure", call.syntax().text_range()),
                            DispatchKind::Closure,
                        ),
                        _ => (
                            self.synthetic_call_def("callable_dyn", call.syntax().text_range()),
                            DispatchKind::Dyn,
                        ),
                    }
                }
            } else {
                (
                    self.synthetic_call_def("callable_dyn", call.syntax().text_range()),
                    DispatchKind::Dyn,
                )
            };

            self.host.insert_call_edge(caller_def, callee, site, dispatch);
        }

        for method_call in source.syntax().descendants().filter_map(ast::MethodCallExpr::cast) {
            let Some(caller_fn) = enclosing_fn(method_call.syntax()) else {
                continue;
            };
            let Some(caller_hir) = self.sema.to_fn_def(&caller_fn) else {
                continue;
            };
            let Some(caller_def) = self.register_module_def(ModuleDef::Function(caller_hir), false) else {
                continue;
            };

            let Some(site) = self.span_for_range(&local, method_call.syntax().text_range()) else {
                continue;
            };

            let (callee, dispatch) = if let Some(resolved) = self.sema.resolve_method_call(&method_call) {
                let callee = self
                    .register_module_def(ModuleDef::Function(resolved), false)
                    .unwrap_or_else(|| {
                        self.synthetic_call_def("method_semantic", method_call.syntax().text_range())
                    });
                let dispatch = self
                    .sema
                    .resolve_method_call_as_callable(&method_call)
                    .map(|callable| Self::dispatch_from_callable_kind(callable.kind()))
                    .unwrap_or(DispatchKind::Direct);
                (callee, dispatch)
            } else {
                (
                    self.synthetic_call_def("method_dyn", method_call.syntax().text_range()),
                    DispatchKind::Dyn,
                )
            };

            self.host.insert_call_edge(caller_def, callee, site, dispatch);
        }
    }

    fn resolve_call_target_from_semantic_path(
        &mut self,
        path: ast::Path,
        site: TextRange,
    ) -> Option<(DefId, DispatchKind)> {
        let per_ns = self.sema.resolve_path_per_ns(&path)?;
        let resolution = per_ns.value_ns.or(per_ns.type_ns)?;
        match resolution {
            hir::PathResolution::Def(ModuleDef::Function(function)) => Some((
                self.register_module_def(ModuleDef::Function(function), false)
                    .unwrap_or_else(|| self.synthetic_call_def("path_function", site)),
                DispatchKind::Direct,
            )),
            hir::PathResolution::Def(ModuleDef::Variant(variant)) => Some((
                self.register_module_def(ModuleDef::Variant(variant), false)
                    .unwrap_or_else(|| self.synthetic_call_def("path_variant", site)),
                DispatchKind::Direct,
            )),
            hir::PathResolution::Def(ModuleDef::Adt(adt)) => Some((
                self.register_module_def(ModuleDef::Adt(adt), false)
                    .unwrap_or_else(|| self.synthetic_call_def("path_adt", site)),
                DispatchKind::Direct,
            )),
            _ => None,
        }
    }

    fn resolve_semantic_callable_target(
        &mut self,
        kind: CallableKind<'db>,
        site: TextRange,
    ) -> (DefId, DispatchKind) {
        match kind {
            CallableKind::Function(function) => (
                self.register_module_def(ModuleDef::Function(function), false)
                    .unwrap_or_else(|| self.synthetic_call_def("callable_function", site)),
                DispatchKind::Direct,
            ),
            CallableKind::TupleStruct(strukt) => (
                self.register_module_def(ModuleDef::Adt(Adt::Struct(strukt)), false)
                    .unwrap_or_else(|| self.synthetic_call_def("callable_tuple_struct", site)),
                DispatchKind::Direct,
            ),
            CallableKind::TupleEnumVariant(variant) => (
                self.register_module_def(ModuleDef::Variant(variant), false)
                    .unwrap_or_else(|| self.synthetic_call_def("callable_tuple_variant", site)),
                DispatchKind::Direct,
            ),
            CallableKind::Closure(_) => (
                self.synthetic_call_def("callable_closure", site),
                DispatchKind::Closure,
            ),
            CallableKind::FnPtr => (
                self.synthetic_call_def("callable_fn_ptr", site),
                DispatchKind::FnPointer,
            ),
            CallableKind::FnImpl(_) => (
                self.synthetic_call_def("callable_fn_impl", site),
                DispatchKind::ThroughTrait,
            ),
        }
    }

    fn synthetic_call_def(&mut self, label: &str, range: TextRange) -> DefId {
        self.host.intern_def_from_token(
            format!(
                "{label}:{}..{}",
                u32::from(range.start()),
                u32::from(range.end())
            )
            .as_str(),
        )
    }

    fn resolve_def_from_semantic_range(&self, file_id: FileId, range: TextRange) -> Option<DefId> {
        let candidates = self.def_ranges_by_file.get(&file_id)?;
        candidates
            .iter()
            .filter(|(candidate_range, _)| {
                candidate_range.start() <= range.start() && range.end() <= candidate_range.end()
            })
            .min_by_key(|(candidate_range, _)| {
                u32::from(candidate_range.end()) - u32::from(candidate_range.start())
            })
            .map(|(_, def_id)| *def_id)
    }

    fn dispatch_kind_for_call_site(&self, file_id: FileId, range: TextRange) -> DispatchKind {
        let Some(local) = self.files.get(&file_id) else {
            return DispatchKind::Dyn;
        };
        let source = self.sema.parse(local.editioned_file_id);

        for method_call in source.syntax().descendants().filter_map(ast::MethodCallExpr::cast) {
            let Some(name_ref) = method_call.name_ref() else {
                continue;
            };
            if name_ref.syntax().text_range() != range {
                continue;
            }
            if let Some(callable) = self.sema.resolve_method_call_as_callable(&method_call) {
                return Self::dispatch_from_callable_kind(callable.kind());
            }
            return DispatchKind::Dyn;
        }

        for call_expr in source.syntax().descendants().filter_map(ast::CallExpr::cast) {
            let Some(expr) = call_expr.expr() else {
                continue;
            };
            if expr.syntax().text_range() != range {
                continue;
            }
            if let Some(callable) = self.sema.resolve_expr_as_callable(&expr) {
                return Self::dispatch_from_callable_kind(callable.kind());
            }
            return match expr {
                ast::Expr::ClosureExpr(_) => DispatchKind::Closure,
                _ => DispatchKind::Dyn,
            };
        }

        DispatchKind::Dyn
    }

    fn dispatch_from_callable_kind(kind: CallableKind<'db>) -> DispatchKind {
        match kind {
            CallableKind::FnImpl(_) => DispatchKind::ThroughTrait,
            CallableKind::Closure(_) => DispatchKind::Closure,
            CallableKind::FnPtr => DispatchKind::FnPointer,
            CallableKind::Function(_)
            | CallableKind::TupleStruct(_)
            | CallableKind::TupleEnumVariant(_) => DispatchKind::Direct,
        }
    }

    fn extract_error_flow_for_file(&mut self, file_id: FileId, source: &SourceFile) {
        let Some(local) = self.files.get(&file_id).cloned() else {
            return;
        };

        for call in source
            .syntax()
            .descendants()
            .filter_map(ast::CallExpr::cast)
        {
            let Some((_, caller_def, Some(function_error))) =
                self.function_context_for_node(call.syntax())
            else {
                continue;
            };
            let Some(expr) = call.expr() else {
                continue;
            };
            let Some(callable) = self.sema.resolve_expr_as_callable(&expr) else {
                continue;
            };
            let CallableKind::TupleEnumVariant(variant) = callable.kind() else {
                continue;
            };
            let error_enum = self.register_error_enum_def(variant.parent_enum(self.db));
            if error_enum != function_error {
                continue;
            }
            let Some(site) = self.span_for_range(&local, call.syntax().text_range()) else {
                continue;
            };
            let variant_name = variant
                .name(self.db)
                .display(self.db, Edition::CURRENT)
                .to_string();
            self.host
                .insert_construct(function_error, variant_name.as_str(), site, caller_def);
        }

        for record in source
            .syntax()
            .descendants()
            .filter_map(ast::RecordExpr::cast)
        {
            let record_range = record.syntax().text_range();
            let Some((_, caller_def, Some(function_error))) =
                self.function_context_for_node(record.syntax())
            else {
                continue;
            };
            let Some(hir::VariantDef::Variant(variant)) = self.sema.resolve_variant(record) else {
                continue;
            };
            let error_enum = self.register_error_enum_def(variant.parent_enum(self.db));
            if error_enum != function_error {
                continue;
            }
            let Some(site) = self.span_for_range(&local, record_range) else {
                continue;
            };
            let variant_name = variant
                .name(self.db)
                .display(self.db, Edition::CURRENT)
                .to_string();
            self.host
                .insert_construct(function_error, variant_name.as_str(), site, caller_def);
        }

        for path_expr in source
            .syntax()
            .descendants()
            .filter_map(ast::PathExpr::cast)
        {
            let Some((_, caller_def, Some(function_error))) =
                self.function_context_for_node(path_expr.syntax())
            else {
                continue;
            };
            let Some(path) = path_expr.path() else {
                continue;
            };
            let Some(hir::PathResolution::Def(ModuleDef::Variant(variant))) =
                self.sema.resolve_path(&path)
            else {
                continue;
            };
            if !matches!(variant.kind(self.db), hir::StructKind::Unit) {
                continue;
            }
            let error_enum = self.register_error_enum_def(variant.parent_enum(self.db));
            if error_enum != function_error {
                continue;
            }
            let Some(site) = self.span_for_range(&local, path_expr.syntax().text_range()) else {
                continue;
            };
            let variant_name = variant
                .name(self.db)
                .display(self.db, Edition::CURRENT)
                .to_string();
            self.host
                .insert_construct(function_error, variant_name.as_str(), site, caller_def);
        }

        for try_expr in source.syntax().descendants().filter_map(ast::TryExpr::cast) {
            let Some(question_mark) = try_expr.question_mark_token() else {
                continue;
            };
            let Some((caller_hir, caller_def, Some(function_error))) =
                self.function_context_for_node(try_expr.syntax())
            else {
                continue;
            };
            let Some(site) = self.span_for_range(&local, question_mark.text_range()) else {
                continue;
            };
            self.host.insert_propagate(function_error, site, caller_def);

            let Some(operand) = try_expr.expr() else {
                continue;
            };
            let Some(operand_type) = self.sema.type_of_expr(&operand).map(|info| info.adjusted())
            else {
                continue;
            };
            let Some(source_error) = self.best_effort_result_error_def(caller_hir, operand_type)
            else {
                continue;
            };
            if source_error != function_error {
                self.host
                    .insert_convert(source_error, function_error, site, caller_def);
            }
        }

        for arm in source
            .syntax()
            .descendants()
            .filter_map(ast::MatchArm::cast)
        {
            let Some((_, caller_def, Some(function_error))) =
                self.function_context_for_node(arm.syntax())
            else {
                continue;
            };
            let Some(pat) = arm.pat() else {
                continue;
            };

            for path in pat.syntax().descendants().filter_map(ast::Path::cast) {
                let Some(hir::PathResolution::Def(ModuleDef::Variant(variant))) =
                    self.sema.resolve_path(&path)
                else {
                    continue;
                };
                let error_enum = self.register_error_enum_def(variant.parent_enum(self.db));
                if error_enum != function_error {
                    continue;
                }
                let Some(site) = self.span_for_range(&local, arm.syntax().text_range()) else {
                    continue;
                };
                let variant_name = variant
                    .name(self.db)
                    .display(self.db, Edition::CURRENT)
                    .to_string();
                self.host.insert_handle_error(
                    function_error,
                    Some(variant_name.into()),
                    site,
                    caller_def,
                );
                break;
            }
        }

        for bin in source.syntax().descendants().filter_map(ast::BinExpr::cast) {
            let Some((caller_hir, caller_def, Some(function_error))) =
                self.function_context_for_node(bin.syntax())
            else {
                continue;
            };
            let Some(op) = bin.op_kind() else {
                continue;
            };
            let Some(site) = self.span_for_range(&local, bin.syntax().text_range()) else {
                continue;
            };

            match op {
                ast::BinaryOp::CmpOp(cmp) => {
                    let lhs_matches = bin.lhs().is_some_and(|lhs| {
                        self.matches_function_error_type(&lhs, caller_hir, function_error)
                    });
                    let rhs_matches = bin.rhs().is_some_and(|rhs| {
                        self.matches_function_error_type(&rhs, caller_hir, function_error)
                    });
                    if lhs_matches || rhs_matches {
                        self.host
                            .insert_compare(function_error, site, cmp.to_string(), caller_def);
                    }
                }
                ast::BinaryOp::Assignment { .. } => {
                    let Some(lhs) = bin.lhs() else {
                        continue;
                    };
                    if self.matches_function_error_type(&lhs, caller_hir, function_error) {
                        self.host.insert_write(function_error, site, caller_def);
                    }
                }
                _ => {}
            }
        }
    }

    fn function_context_for_node(
        &mut self,
        node: &SyntaxNode,
    ) -> Option<(Function, DefId, Option<DefId>)> {
        let caller_fn = enclosing_fn(node)?;
        let caller_hir = self.sema.to_fn_def(&caller_fn)?;
        let caller_def = self.register_module_def(ModuleDef::Function(caller_hir), false)?;
        let function_error = self.function_error_def_for_function(caller_hir);
        Some((caller_hir, caller_def, function_error))
    }

    fn span_for_range(&mut self, local: &LocalFile, range: TextRange) -> Option<crate::SpanId> {
        self.host
            .intern_span_from_text(
                local.editioned_file_id.editioned_file_id(self.db),
                local.rel_path.as_str(),
                local.text.as_str(),
                range,
            )
            .ok()
    }

    fn register_error_enum_def(&mut self, enum_def: hir::Enum) -> DefId {
        self.register_module_def(ModuleDef::Adt(Adt::Enum(enum_def)), false)
            .unwrap_or_else(|| {
                let module_def = ModuleDef::Adt(Adt::Enum(enum_def));
                let path = module_def
                    .canonical_path(self.db, Edition::CURRENT)
                    .map(normalize_canonical_path)
                    .unwrap_or_else(|| {
                        format!(
                            "crate::{}",
                            enum_def.name(self.db).display(self.db, Edition::CURRENT)
                        )
                    });
                self.host
                    .intern_def_from_token(format!("error_enum:{path}").as_str())
            })
    }

    fn function_error_def_for_function(&mut self, function: Function) -> Option<DefId> {
        self.best_effort_result_error_def(function, function.ret_type(self.db))
            .or_else(|| {
                function
                    .async_ret_type(self.db)
                    .and_then(|ret| self.best_effort_result_error_def(function, ret))
            })
    }

    fn result_error_type_argument(&self, ty: Type<'db>) -> Option<Type<'db>> {
        let stripped = ty.strip_references();
        let adt = stripped.as_adt()?;
        if !self.is_result_head(adt) {
            return None;
        }
        stripped.type_arguments().nth(1)
    }

    fn is_result_head(&self, adt: Adt) -> bool {
        let Adt::Enum(enum_def) = adt else {
            return false;
        };
        let lang_items = hir_def::lang_item::lang_items(self.db, enum_def.krate(self.db).base());
        let Some(result_err_variant) = lang_items.ResultErr else {
            return false;
        };
        let result_enum = hir::Variant::from(result_err_variant).parent_enum(self.db);
        enum_def == result_enum
    }

    fn synthetic_type_head_from_module_def(
        &mut self,
        def: ModuleDef,
        owner: Option<DefId>,
        label: &str,
    ) -> DefId {
        let owner_key = owner
            .map(|d| format!("{:#018x}", d.stable_id().as_u64()))
            .unwrap_or_else(|| "none".to_string());
        let path = def
            .canonical_path(self.db, Edition::CURRENT)
            .or_else(|| {
                def.name(self.db)
                    .map(|name| format!("crate::{}", name.display(self.db, Edition::CURRENT)))
            })
            .unwrap_or_else(|| "unknown".to_string());
        self.host
            .intern_def_from_token(format!("{label}:{path}:{owner_key}").as_str())
    }

    fn matches_function_error_type(
        &mut self,
        expr: &ast::Expr,
        function: Function,
        function_error: DefId,
    ) -> bool {
        let Some(type_info) = self.sema.type_of_expr(expr) else {
            return false;
        };
        if self.matches_error_type(type_info.original, function, function_error) {
            return true;
        }
        type_info
            .adjusted
            .is_some_and(|adjusted| self.matches_error_type(adjusted, function, function_error))
    }

    fn matches_error_type(
        &mut self,
        ty: Type<'db>,
        function: Function,
        function_error: DefId,
    ) -> bool {
        if let Some(err) = self.best_effort_result_error_def(function, ty.clone()) {
            return err == function_error;
        }

        if let Some((inner, _)) = ty.as_reference() {
            return self.matches_error_type(inner, function, function_error);
        }
        if ty.is_raw_ptr()
            && let Some(inner) = ty.remove_raw_ptr()
        {
            return self.matches_error_type(inner, function, function_error);
        }

        self.def_from_type_head(ty, None) == function_error
    }

    fn source_loc_for_module_def(&self, def: ModuleDef) -> Option<SourceLoc> {
        match def {
            ModuleDef::Module(module) => {
                let range = module
                    .declaration_source_range(self.db)
                    .unwrap_or_else(|| module.definition_source_range(self.db));
                self.source_loc_for_hir_file(range.file_id, range.value)
            }
            ModuleDef::Function(function) => {
                let source = function.source(self.db)?;
                self.source_loc_for_hir_file(source.file_id, source.value.syntax().text_range())
            }
            ModuleDef::Adt(adt) => {
                let source = adt.source(self.db)?;
                self.source_loc_for_hir_file(source.file_id, source.value.syntax().text_range())
            }
            ModuleDef::Variant(variant) => {
                let source = variant.source(self.db)?;
                self.source_loc_for_hir_file(source.file_id, source.value.syntax().text_range())
            }
            ModuleDef::Const(const_) => {
                let source = const_.source(self.db)?;
                self.source_loc_for_hir_file(source.file_id, source.value.syntax().text_range())
            }
            ModuleDef::Static(static_) => {
                let source = static_.source(self.db)?;
                self.source_loc_for_hir_file(source.file_id, source.value.syntax().text_range())
            }
            ModuleDef::Trait(trait_) => {
                let source = trait_.source(self.db)?;
                self.source_loc_for_hir_file(source.file_id, source.value.syntax().text_range())
            }
            ModuleDef::TypeAlias(alias) => {
                let source = alias.source(self.db)?;
                self.source_loc_for_hir_file(source.file_id, source.value.syntax().text_range())
            }
            ModuleDef::Macro(mac) => {
                let source = mac.source(self.db)?;
                self.source_loc_for_hir_file(source.file_id, source.value.syntax().text_range())
            }
            ModuleDef::BuiltinType(_) => None,
        }
    }

    fn source_loc_for_field(&self, field: hir::Field) -> Option<SourceLoc> {
        let source = field.source(self.db)?;
        self.source_loc_for_hir_file(source.file_id, source.value.syntax().text_range())
    }

    fn source_loc_for_impl(&self, impl_def: Impl) -> Option<SourceLoc> {
        let source = impl_def.source_with_range(self.db)?;
        self.source_loc_for_hir_file(source.file_id, source.value.0)
    }

    fn source_loc_for_hir_file(
        &self,
        file_id: hir_expand::HirFileId,
        range: TextRange,
    ) -> Option<SourceLoc> {
        let editioned = file_id.original_file(self.db);
        let real = editioned.file_id(self.db);
        let local = self.files.get(&real)?;
        Some(SourceLoc {
            file_id: editioned,
            rel_path: local.rel_path.clone(),
            range,
        })
    }

    fn intern_span(&mut self, loc: &SourceLoc) -> Option<crate::SpanId> {
        let file = self.files.get(&loc.file_id.file_id(self.db))?;
        self.host
            .intern_span_from_text(
                loc.file_id.editioned_file_id(self.db),
                file.rel_path.as_str(),
                file.text.as_str(),
                loc.range,
            )
            .ok()
    }

    fn module_is_test(&self, module: Module) -> bool {
        module.path_to_root(self.db).into_iter().any(|m| {
            m.name(self.db)
                .is_some_and(|name| name.display(self.db, Edition::CURRENT).to_string() == "tests")
        })
    }

    fn intern_type_ref(&mut self, ty: Type<'db>, owner: Option<DefId>) -> TypeRefId {
        let owner_key = owner
            .map(|def| format!("{:#018x}", def.stable_id().as_u64()))
            .unwrap_or_else(|| "none".to_string());
        let key = format!("{}:{}", owner_key, self.type_fingerprint(ty.clone(), owner));
        if let Some(existing) = self.typeref_by_key.get(&key) {
            return *existing;
        }

        let type_ref = self.host.intern_typeref_from_token(&key);
        let shape = self.type_shape_from_ty(ty, owner);
        self.host.insert_type(type_ref, shape);
        self.typeref_by_key.insert(key, type_ref);
        type_ref
    }

    fn type_shape_from_ty(&mut self, ty: Type<'db>, owner: Option<DefId>) -> TypeShape {
        if ty.is_unknown() {
            return TypeShape::Unknown;
        }

        if let Some((inner, mutability)) = ty.as_reference() {
            return TypeShape::Ref {
                mutability: map_mutability(mutability),
                inner: self.intern_type_ref(inner, owner),
            };
        }

        if ty.is_raw_ptr()
            && let Some(inner) = ty.remove_raw_ptr()
        {
            return TypeShape::Ptr {
                mutability: Mutability::Shared,
                inner: self.intern_type_ref(inner, owner),
            };
        }

        if ty.is_tuple() {
            return TypeShape::Tuple(
                ty.tuple_fields(self.db)
                    .into_iter()
                    .map(|item| self.intern_type_ref(item, owner))
                    .collect(),
            );
        }

        if let Some(inner) = ty.as_slice() {
            return TypeShape::Slice(self.intern_type_ref(inner, owner));
        }

        if let Some(param) = ty.as_type_param(self.db) {
            let def = self.host.intern_def_from_token(
                format!(
                    "type_param:{}:{}",
                    param.name(self.db).display(self.db, Edition::CURRENT),
                    owner
                        .map(|d| format!("{:#018x}", d.stable_id().as_u64()))
                        .unwrap_or_else(|| "none".to_string())
                )
                .as_str(),
            );
            return TypeShape::Param(def);
        }

        if let Some(head_adt) = ty.as_adt() {
            let def = self
                .register_module_def(ModuleDef::Adt(head_adt), false)
                .unwrap_or_else(|| {
                    self.synthetic_type_head_from_module_def(
                        ModuleDef::Adt(head_adt),
                        owner,
                        "type_head:adt",
                    )
                });
            return TypeShape::App {
                head: def,
                args: ty
                    .type_arguments()
                    .into_iter()
                    .map(|arg| GenericArg::Type(self.intern_type_ref(arg, owner)))
                    .collect(),
            };
        }

        if let Some(head_trait) = ty.as_dyn_trait() {
            let def = self
                .register_module_def(ModuleDef::Trait(head_trait), false)
                .unwrap_or_else(|| {
                    self.synthetic_type_head_from_module_def(
                        ModuleDef::Trait(head_trait),
                        owner,
                        "type_head:dyn_trait",
                    )
                });
            return TypeShape::App {
                head: def,
                args: ty
                    .type_arguments()
                    .into_iter()
                    .map(|arg| GenericArg::Type(self.intern_type_ref(arg, owner)))
                    .collect(),
            };
        }

        if let Some(builtin) = ty.as_builtin() {
            return TypeShape::Prim(builtin.name().display_no_db(Edition::CURRENT).to_string());
        }

        TypeShape::Unknown
    }

    fn best_effort_result_error_def(&mut self, _function: Function, ret_ty: Type<'db>) -> Option<DefId> {
        if let Some(error_ty) = self.result_error_type_argument(ret_ty.clone()) {
            return Some(self.def_from_type_head(error_ty, None));
        }
        None
    }

    fn def_from_type_head(&mut self, ty: Type<'db>, owner: Option<DefId>) -> DefId {
        if let Some(adt) = ty.as_adt() {
            if let Some(def) = self.register_module_def(ModuleDef::Adt(adt), false) {
                return def;
            }
            return self.synthetic_type_head_from_module_def(
                ModuleDef::Adt(adt),
                owner,
                "type_head:adt",
            );
        }

        if let Some(trait_) = ty.as_dyn_trait() {
            if let Some(def) = self.register_module_def(ModuleDef::Trait(trait_), false) {
                return def;
            }
            return self.synthetic_type_head_from_module_def(
                ModuleDef::Trait(trait_),
                owner,
                "type_head:dyn_trait",
            );
        }

        if let Some(param) = ty.as_type_param(self.db) {
            return self.host.intern_def_from_token(
                format!(
                    "type_param:{}:{}",
                    param.name(self.db).display(self.db, Edition::CURRENT),
                    owner
                        .map(|d| format!("{:#018x}", d.stable_id().as_u64()))
                        .unwrap_or_else(|| "none".to_string())
                )
                .as_str(),
            );
        }

        self.host.intern_def_from_token(
            format!(
                "type_head:opaque:{}",
                owner
                    .map(|d| format!("{:#018x}", d.stable_id().as_u64()))
                    .unwrap_or_else(|| "none".to_string())
            )
            .as_str(),
        )
    }

    fn type_fingerprint(&mut self, ty: Type<'db>, owner: Option<DefId>) -> String {
        if ty.is_unknown() {
            return "unknown".to_string();
        }

        if let Some((inner, mutability)) = ty.as_reference() {
            return format!(
                "ref:{:?}:{}",
                map_mutability(mutability),
                self.type_fingerprint(inner, owner)
            );
        }

        if ty.is_raw_ptr()
            && let Some(inner) = ty.remove_raw_ptr()
        {
            return format!("ptr:{}", self.type_fingerprint(inner, owner));
        }

        if ty.is_tuple() {
            let items = ty
                .tuple_fields(self.db)
                .into_iter()
                .map(|item| self.type_fingerprint(item, owner))
                .collect::<Vec<_>>();
            return format!("tuple:{}", encode_fingerprint_parts(items));
        }

        if let Some(inner) = ty.as_slice() {
            return format!("slice:{}", self.type_fingerprint(inner, owner));
        }

        if let Some(param) = ty.as_type_param(self.db) {
            return format!(
                "param:{}",
                param.name(self.db).display(self.db, Edition::CURRENT)
            );
        }

        if let Some(head_adt) = ty.as_adt() {
            let path = self
                .register_module_def(ModuleDef::Adt(head_adt), false)
                .and_then(|def| self.def_path_by_id.get(&def).cloned())
                .unwrap_or_else(|| {
                    format!(
                        "crate::{}",
                        head_adt.name(self.db).display(self.db, Edition::CURRENT)
                    )
                });
            let args = ty
                .type_arguments()
                .into_iter()
                .map(|arg| self.type_fingerprint(arg, owner))
                .collect::<Vec<_>>();
            return if args.is_empty() {
                format!("app:{path}")
            } else {
                format!("app:{path}<{}>", encode_fingerprint_parts(args))
            };
        }

        if let Some(head_trait) = ty.as_dyn_trait() {
            let path = self
                .register_module_def(ModuleDef::Trait(head_trait), false)
                .and_then(|def| self.def_path_by_id.get(&def).cloned())
                .unwrap_or_else(|| {
                    format!(
                        "crate::{}",
                        head_trait.name(self.db).display(self.db, Edition::CURRENT)
                    )
                });
            let args = ty
                .type_arguments()
                .into_iter()
                .map(|arg| self.type_fingerprint(arg, owner))
                .collect::<Vec<_>>();
            return if args.is_empty() {
                format!("dyn:{path}")
            } else {
                format!("dyn:{path}<{}>", encode_fingerprint_parts(args))
            };
        }

        if let Some(builtin) = ty.as_builtin() {
            return format!("prim:{}", builtin.name().display_no_db(Edition::CURRENT));
        }

        format!(
            "opaque:{}",
            owner
                .map(|d| format!("{:#018x}", d.stable_id().as_u64()))
                .unwrap_or_else(|| "none".to_string())
        )
    }
}

fn compute_world_stamp(
    workspace_root: &Path,
    files: &BTreeMap<FileId, LocalFile>,
    semantic_identity: &str,
) -> WorldStamp {
    let mut pairs = files
        .values()
        .map(|file| (file.rel_path.clone(), file.text.as_bytes().to_vec()))
        .collect::<Vec<_>>();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = FxHasher::default();
    for (path, text) in pairs {
        path.hash(&mut hasher);
        0xff_u8.hash(&mut hasher);
        text.hash(&mut hasher);
        0xfe_u8.hash(&mut hasher);
    }
    semantic_identity.hash(&mut hasher);
    0xfd_u8.hash(&mut hasher);

    WorldStamp::new(format!(
        "ra-workspace:{}:{:016x}",
        workspace_root.to_string_lossy(),
        hasher.finish()
    ))
}

fn compute_semantic_identity(db: &RootDatabase, workspace_root: &Path) -> String {
    let mut crate_signatures = Crate::all(db)
        .into_iter()
        .map(|krate| {
            let display_name = krate
                .display_name(db)
                .map(|name| name.to_string())
                .unwrap_or_else(|| "<anon>".to_string());
            let version = krate.version(db).unwrap_or_default();
            let edition = format!("{:?}", krate.edition(db));
            let cfg = format!("{:?}", krate.cfg(db));
            let mut deps = krate
                .dependencies(db)
                .into_iter()
                .map(|dep| {
                    let dep_name = dep
                        .krate
                        .display_name(db)
                        .map(|name| name.to_string())
                        .unwrap_or_else(|| "<anon>".to_string());
                    format!("{:?}->{dep_name}", dep.name)
                })
                .collect::<Vec<_>>();
            deps.sort();
            format!(
                "crate:{display_name}:{version}:{edition}:{cfg}:deps=[{}]",
                deps.join(",")
            )
        })
        .collect::<Vec<_>>();
    crate_signatures.sort();

    let mut env_axes = [
        "RUSTUP_TOOLCHAIN",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_CFG_TARGET_ARCH",
        "CARGO_CFG_TARGET_OS",
        "CARGO_CFG_TARGET_ENV",
    ]
    .into_iter()
    .map(|name| (name, std::env::var(name).unwrap_or_default()))
    .collect::<Vec<_>>();
    env_axes.sort_by(|a, b| a.0.cmp(b.0));

    let toolchain_facts = [
        command_output(workspace_root, "rustc", ["-vV"]),
        command_output(workspace_root, "cargo", ["-V"]),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    let config_files = collect_workspace_config_file_hashes(workspace_root);

    format!(
        "semantic:crates=[{}];env=[{}];tools=[{}];configs=[{}]",
        crate_signatures.join("|"),
        env_axes
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("|"),
        toolchain_facts.join("|"),
        config_files.join("|"),
    )
}

fn command_output<const N: usize>(
    workspace_root: &Path,
    bin: &str,
    args: [&str; N],
) -> Option<String> {
    let output = Command::new(bin)
        .current_dir(workspace_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    Some(format!("{bin}:{}", stdout.trim()))
}

fn collect_workspace_config_file_hashes(workspace_root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_workspace_config_files_recursive(workspace_root, workspace_root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
        .into_iter()
        .map(|(rel_path, bytes)| {
            let mut hasher = FxHasher::default();
            bytes.hash(&mut hasher);
            format!("{rel_path}:{:016x}", hasher.finish())
        })
        .collect()
}

fn collect_workspace_config_files_recursive(
    workspace_root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths = entries.filter_map(Result::ok).collect::<Vec<_>>();
    paths.sort_by_key(|entry| entry.path());

    for entry in paths {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            let dir_name = entry.file_name();
            let dir_name = dir_name.to_string_lossy();
            if dir_name == "target" || dir_name == ".git" || dir_name == ".jj" {
                continue;
            }
            collect_workspace_config_files_recursive(workspace_root, path.as_path(), out);
            continue;
        }

        if !file_type.is_file() {
            continue;
        }
        let Some(file_name) = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            continue;
        };
        let tracked = matches!(
            file_name.as_str(),
            "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml" | "config.toml"
        ) || path.ends_with(".cargo/config.toml");
        if !tracked {
            continue;
        }

        let Ok(mut bytes) = std::fs::read(path.as_path()) else {
            continue;
        };
        if file_name == "Cargo.lock"
            && let Ok(text) = String::from_utf8(bytes.clone())
        {
            bytes = normalize_cargo_lock_sources(text.as_str(), workspace_root).into_bytes();
        }
        let rel_path = path
            .strip_prefix(workspace_root)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        out.push((rel_path, bytes));
    }
}

fn normalize_cargo_lock_sources(lock_text: &str, workspace_root: &Path) -> String {
    lock_text
        .lines()
        .map(|line| {
            let prefix = "source = \"path+file://";
            let Some(start) = line.find(prefix) else {
                return line.to_string();
            };
            let path_start = start + prefix.len();
            let Some(path_end_rel) = line[path_start..].find('"') else {
                return line.to_string();
            };
            let path_end = path_start + path_end_rel;
            let raw_path = &line[path_start..path_end];
            let normalized = normalize_lock_source_path(raw_path, workspace_root);
            format!("{}{}{}", &line[..path_start], normalized, &line[path_end..])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_lock_source_path(raw_path: &str, workspace_root: &Path) -> String {
    let candidate = Path::new(raw_path);
    if let Ok(rel) = candidate.strip_prefix(workspace_root) {
        let rel = rel.to_string_lossy().replace('\\', "/");
        return format!("<workspace>/{rel}");
    }
    "<external-path>".to_string()
}

fn collect_local_files(
    db: &RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    sema: &Semantics<'_, RootDatabase>,
) -> Result<BTreeMap<FileId, LocalFile>, RaHostInitError> {
    #[derive(Debug)]
    struct FileCandidate {
        file_id: FileId,
        source_root_id: base_db::SourceRootId,
        is_library: bool,
        abs_path: String,
    }

    let selected_source_roots = selected_source_root_ids(db, vfs, workspace_root);
    let mut candidates = Vec::new();
    for (file_id, path) in vfs.iter() {
        let source_root_id = db.file_source_root(file_id).source_root_id(db);
        if !selected_source_roots.contains(&source_root_id) {
            continue;
        }
        let source_root = db.source_root(source_root_id).source_root(db);
        let Some(abs_path) = path.as_path() else {
            continue;
        };
        candidates.push(FileCandidate {
            file_id,
            source_root_id,
            is_library: source_root.is_library,
            abs_path: abs_path.as_str().to_string(),
        });
    }
    candidates.sort_by(|a, b| {
        a.abs_path
            .cmp(&b.abs_path)
            .then_with(|| a.is_library.cmp(&b.is_library))
            .then_with(|| a.source_root_id.0.cmp(&b.source_root_id.0))
    });

    let mut files = BTreeMap::new();
    for candidate in candidates {
        if candidate.is_library {
            continue;
        }

        let abs_path = Path::new(candidate.abs_path.as_str());

        let rel_path = normalize_snapshot_path(workspace_root, abs_path, candidate.is_library);
        let text = db.file_text(candidate.file_id).text(db).to_string();
        let editioned = sema.attach_first_edition(candidate.file_id);

        files.insert(
            candidate.file_id,
            LocalFile {
                rel_path,
                text,
                editioned_file_id: editioned,
            },
        );
    }

    if files.is_empty() {
        return Err(RaHostInitError::SemanticBuild {
            details: "workspace snapshot has no source files".to_string(),
        });
    }

    Ok(files)
}

fn selected_source_root_ids(
    db: &RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
) -> HashSet<base_db::SourceRootId> {
    let mut selected_crates = HashSet::new();
    let mut queue = VecDeque::new();

    for krate in Crate::all(db) {
        let root_path = vfs.file_path(krate.root_file(db));
        let in_workspace = root_path
            .as_path()
            .is_some_and(|path| Path::new(path.as_str()).starts_with(workspace_root));
        if krate.origin(db).is_local() || in_workspace {
            if selected_crates.insert(krate) {
                queue.push_back(krate);
            }
        }
    }

    while let Some(krate) = queue.pop_front() {
        for dep in krate.dependencies(db) {
            if dep.krate.origin(db).is_lang() {
                continue;
            }
            if selected_crates.insert(dep.krate) {
                queue.push_back(dep.krate);
            }
        }
    }

    if selected_crates.is_empty() {
        selected_crates.extend(Crate::all(db).into_iter().filter(|krate| !krate.origin(db).is_lang()));
    }

    selected_crates
        .into_iter()
        .map(|krate| db.file_source_root(krate.root_file(db)).source_root_id(db))
        .collect()
}

fn normalize_rel_path(workspace_root: &Path, file_path: &Path) -> String {
    let rel = file_path.strip_prefix(workspace_root).unwrap_or(file_path);
    let path = rel.to_string_lossy().replace('\\', "/");
    if path.is_empty() {
        ".".to_string()
    } else {
        path
    }
}

fn normalize_snapshot_path(workspace_root: &Path, file_path: &Path, is_library: bool) -> String {
    if !is_library {
        return normalize_rel_path(workspace_root, file_path);
    }

    if let Ok(rel) = file_path.strip_prefix(workspace_root) {
        return format!("workspace-dep/{}", rel.to_string_lossy().replace('\\', "/"));
    }

    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        let cargo_home = Path::new(cargo_home.as_os_str());
        if let Ok(rel) = file_path.strip_prefix(cargo_home) {
            return format!("<cargo-home>/{}", rel.to_string_lossy().replace('\\', "/"));
        }
    }
    if let Some(rustup_home) = std::env::var_os("RUSTUP_HOME") {
        let rustup_home = Path::new(rustup_home.as_os_str());
        if let Ok(rel) = file_path.strip_prefix(rustup_home) {
            return format!("<rustup-home>/{}", rel.to_string_lossy().replace('\\', "/"));
        }
    }

    let normalized = file_path.to_string_lossy().replace('\\', "/");
    if let Some((_, tail)) = normalized.split_once("/registry/src/") {
        return format!("<cargo-registry>/{tail}");
    }
    if let Some((_, tail)) = normalized.split_once("/git/checkouts/") {
        return format!("<cargo-git>/{tail}");
    }

    format!(
        "<external>/{}/{}",
        short_path_hash(file_path),
        file_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    )
}

fn short_path_hash(path: &Path) -> String {
    let mut hasher = FxHasher::default();
    path.to_string_lossy().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn normalize_canonical_path(path: String) -> String {
    if path.starts_with("crate::") {
        return path;
    }
    if let Some((_, tail)) = path.split_once("::") {
        format!("crate::{tail}")
    } else {
        format!("crate::{path}")
    }
}

fn encode_fingerprint_parts(parts: Vec<String>) -> String {
    parts
        .into_iter()
        .map(|part| format!("{}#{}", part.len(), part))
        .collect::<Vec<_>>()
        .join(";")
}

fn map_mutability(mutability: hir::Mutability) -> Mutability {
    match mutability {
        hir::Mutability::Shared => Mutability::Shared,
        hir::Mutability::Mut => Mutability::Mut,
    }
}

fn def_kind(def: ModuleDef) -> Option<DefKind> {
    let kind = match def {
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
    };
    Some(kind)
}

fn syntax_node_kind(node: &SyntaxNode) -> Option<NodeKind> {
    if ast::IfExpr::cast(node.clone()).is_some() {
        return Some(NodeKind::If);
    }
    if ast::MatchExpr::cast(node.clone()).is_some() {
        return Some(NodeKind::Match);
    }
    if ast::WhileExpr::cast(node.clone()).is_some() {
        return Some(NodeKind::While);
    }
    if ast::ForExpr::cast(node.clone()).is_some() {
        return Some(NodeKind::For);
    }
    if ast::LoopExpr::cast(node.clone()).is_some() {
        return Some(NodeKind::Loop);
    }
    if ast::BlockExpr::cast(node.clone()).is_some() {
        return Some(NodeKind::Block);
    }
    if ast::TryExpr::cast(node.clone()).is_some() {
        return Some(NodeKind::Try);
    }
    if ast::MatchArm::cast(node.clone()).is_some() {
        return Some(NodeKind::Arm);
    }
    None
}

fn enclosing_fn(node: &SyntaxNode) -> Option<ast::Fn> {
    node.ancestors().find_map(ast::Fn::cast)
}
