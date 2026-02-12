use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::Path;

use base_db::{EditionedFileId, SourceDatabase};
use hir::{
    Adt, AssocItem, CallableKind, Crate, Function, HasCrate, HasSource, HasVisibility, Impl,
    Module, ModuleDef, Type,
};
use ide::{RootDatabase, Semantics};
use rustc_hash::FxHasher;
use span::TextRange;
use syntax::ast;
use syntax::{AstNode, Edition, SourceFile, SyntaxNode};
use vfs::FileId;

use crate::{
    DefId, DefKind, DeterministicRaHost, DispatchKind, Mutability, NodeKind, RaHostInitError,
    TypeRefId, TypeShape, WorldStamp,
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
    let files = collect_local_files(db, vfs, workspace_root, &sema)?;

    let mut host = DeterministicRaHost::new();
    host.set_world_stamp(compute_world_stamp(workspace_root, &files));

    let mut builder = SnapshotBuilder {
        db,
        sema,
        files,
        host,
        defs_by_path: BTreeMap::new(),
        defs_by_name: BTreeMap::new(),
        def_path_by_id: BTreeMap::new(),
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
    files: BTreeMap<FileId, LocalFile>,
    host: DeterministicRaHost,
    defs_by_path: BTreeMap<String, DefId>,
    defs_by_name: BTreeMap<String, BTreeSet<DefId>>,
    def_path_by_id: BTreeMap<DefId, String>,
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

        for impl_def in module.impl_defs(self.db) {
            self.process_impl(impl_def, module_test);
        }
    }

    fn process_function(&mut self, function: Function, function_def: DefId) {
        let ret = function.ret_type(self.db);
        let ret_ref = self.intern_type_ref(ret.clone(), Some(function_def));
        self.host.set_fn_return_type(function_def, Some(ret_ref));
        if let Some(error_def) = self.best_effort_result_error_def(function, ret) {
            self.host.set_fn_error_type(function_def, Some(error_def));
        }
    }

    fn process_adt(&mut self, adt: Adt, owner_def: DefId, in_test: bool) {
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
                    self.defs_by_name
                        .entry(field_name.clone())
                        .or_default()
                        .insert(field_def);

                    let field_ty = self.unknown_type_ref(
                        format!("field_ty:{}:{}", owner_path, field_name).as_str(),
                    );
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
                    self.defs_by_name
                        .entry(field_name.clone())
                        .or_default()
                        .insert(field_def);

                    let field_ty = self.unknown_type_ref(
                        format!("field_ty:{}:{}", owner_path, field_name).as_str(),
                    );
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
                        let field_ty = self.unknown_type_ref(
                            format!("variant_field_ty:{}:{}", variant_name, field_name).as_str(),
                        );
                        self.host
                            .insert_field(variant_def, field_name.as_str(), field_ty);
                    }
                }
            }
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

        let self_ty = impl_def.self_ty(self.db);
        let self_ty_def = self.def_from_type_head(self_ty, Some(impl_record));

        if let Some(trait_def_hir) = impl_def.trait_(self.db) {
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
                        .map(|src_ty| {
                            self.def_from_type_head(src_ty.to_type(self.db), Some(impl_record))
                        })
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
                    self.host.insert_method(self_ty_def, method_def);
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

        self.defs_by_path.insert(path.clone(), def_id);
        self.defs_by_name
            .entry(name.clone())
            .or_default()
            .insert(def_id);
        self.def_path_by_id.insert(def_id, path.clone());

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
            self.extract_calls_for_file(file_id, &source);
        }
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

    fn extract_calls_for_file(&mut self, file_id: FileId, source: &SourceFile) {
        let Some(local) = self.files.get(&file_id).cloned() else {
            return;
        };

        for call in source
            .syntax()
            .descendants()
            .filter_map(ast::CallExpr::cast)
        {
            let Some(caller_fn) = enclosing_fn(call.syntax()) else {
                continue;
            };
            let Some(caller_hir) = self.sema.to_fn_def(&caller_fn) else {
                continue;
            };
            let Some(caller_def) = self.register_module_def(ModuleDef::Function(caller_hir), false)
            else {
                continue;
            };

            let Ok(site) = self.host.intern_span_from_text(
                local.editioned_file_id.editioned_file_id(self.db),
                local.rel_path.as_str(),
                local.text.as_str(),
                call.syntax().text_range(),
            ) else {
                continue;
            };

            let (callee, dispatch) = if let Some(expr) = call.expr() {
                if let Some(callee) = self.resolve_call_expr_target(&expr) {
                    (callee, DispatchKind::Direct)
                } else if matches!(expr, ast::Expr::ClosureExpr(_)) {
                    (
                        self.synthetic_def("callable_closure", call.syntax().text_range()),
                        DispatchKind::Closure,
                    )
                } else {
                    (
                        self.synthetic_def("callable_dyn", call.syntax().text_range()),
                        DispatchKind::Dyn,
                    )
                }
            } else {
                (
                    self.synthetic_def("callable_dyn", call.syntax().text_range()),
                    DispatchKind::Dyn,
                )
            };

            self.host
                .insert_call_edge(caller_def, callee, site, dispatch);
        }

        for method_call in source
            .syntax()
            .descendants()
            .filter_map(ast::MethodCallExpr::cast)
        {
            let Some(caller_fn) = enclosing_fn(method_call.syntax()) else {
                continue;
            };
            let Some(caller_hir) = self.sema.to_fn_def(&caller_fn) else {
                continue;
            };
            let Some(caller_def) = self.register_module_def(ModuleDef::Function(caller_hir), false)
            else {
                continue;
            };

            let Ok(site) = self.host.intern_span_from_text(
                local.editioned_file_id.editioned_file_id(self.db),
                local.rel_path.as_str(),
                local.text.as_str(),
                method_call.syntax().text_range(),
            ) else {
                continue;
            };

            let (callee, dispatch) = self
                .resolve_method_call_target_semantic(&method_call)
                .or_else(|| {
                    self.resolve_method_call_target(&method_call)
                        .map(|callee| (callee, DispatchKind::Direct))
                })
                .unwrap_or_else(|| {
                    (
                        self.synthetic_def("method_dyn", method_call.syntax().text_range()),
                        DispatchKind::Dyn,
                    )
                });

            self.host
                .insert_call_edge(caller_def, callee, site, dispatch);
        }
    }

    fn resolve_call_expr_target(&self, expr: &ast::Expr) -> Option<DefId> {
        match expr {
            ast::Expr::PathExpr(path_expr) => {
                let path = path_expr.path()?;
                self.resolve_def_from_call_path(&path)
            }
            _ => None,
        }
    }

    fn resolve_method_call_target(&self, method_call: &ast::MethodCallExpr) -> Option<DefId> {
        let name = method_call.name_ref()?.syntax().text().to_string();
        let candidates = self.defs_by_name.get(name.as_str())?;
        if candidates.len() == 1 {
            candidates.iter().next().copied()
        } else {
            None
        }
    }

    fn resolve_method_call_target_semantic(
        &mut self,
        method_call: &ast::MethodCallExpr,
    ) -> Option<(DefId, DispatchKind)> {
        let resolved = self.sema.resolve_method_call(method_call)?;

        let callee = self
            .register_module_def(ModuleDef::Function(resolved), false)
            .unwrap_or_else(|| {
                self.synthetic_def("method_semantic", method_call.syntax().text_range())
            });
        let dispatch = self
            .sema
            .resolve_method_call_as_callable(method_call)
            .map(|callable| callable.kind())
            .map(|kind| match kind {
                CallableKind::FnImpl(_) => DispatchKind::ThroughTrait,
                CallableKind::Closure(_) => DispatchKind::Closure,
                CallableKind::Function(_)
                | CallableKind::TupleStruct(_)
                | CallableKind::TupleEnumVariant(_)
                | CallableKind::FnPtr => DispatchKind::Direct,
            })
            .unwrap_or(DispatchKind::Direct);

        Some((callee, dispatch))
    }

    fn resolve_def_from_call_path(&self, path: &ast::Path) -> Option<DefId> {
        let path_text = path_to_string(path)?;
        self.resolve_def_from_type_path(path_text.as_str())
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

        if let Some(head_adt) = ty.as_adt()
            && let Some(def) = self.register_module_def(ModuleDef::Adt(head_adt), false)
        {
            return TypeShape::App {
                head: def,
                args: Vec::new(),
            };
        }

        if let Some(head_trait) = ty.as_dyn_trait()
            && let Some(def) = self.register_module_def(ModuleDef::Trait(head_trait), false)
        {
            return TypeShape::App {
                head: def,
                args: Vec::new(),
            };
        }

        if let Some(builtin) = ty.as_builtin() {
            return TypeShape::Prim(builtin.name().display_no_db(Edition::CURRENT).to_string());
        }

        TypeShape::Unknown
    }

    fn best_effort_result_error_def(
        &mut self,
        function: Function,
        ret_ty: Type<'db>,
    ) -> Option<DefId> {
        let adt = ret_ty.as_adt()?;
        if adt
            .name(self.db)
            .display(self.db, Edition::CURRENT)
            .to_string()
            != "Result"
        {
            return None;
        }

        let args = ret_ty
            .type_and_const_arguments(self.db, function.krate(self.db).to_display_target(self.db))
            .collect::<Vec<_>>();
        let error_name = args.get(1)?.to_string();

        if let Some(def) = self.defs_by_path.get(error_name.as_str()) {
            return Some(*def);
        }

        if let Some(candidates) = self.defs_by_name.get(error_name.as_str())
            && candidates.len() == 1
        {
            return candidates.iter().next().copied();
        }

        Some(
            self.host
                .intern_def_from_token(format!("result_error:{error_name}").as_str()),
        )
    }

    fn def_from_type_head(&mut self, ty: Type<'db>, owner: Option<DefId>) -> DefId {
        if let Some(adt) = ty.as_adt()
            && let Some(def) = self.register_module_def(ModuleDef::Adt(adt), false)
        {
            return def;
        }

        if let Some(trait_) = ty.as_dyn_trait()
            && let Some(def) = self.register_module_def(ModuleDef::Trait(trait_), false)
        {
            return def;
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

    fn resolve_def_from_type_path(&self, path: &str) -> Option<DefId> {
        let mut candidates = Vec::new();
        let trimmed = path.trim().trim_start_matches("::");
        if trimmed.is_empty() {
            return None;
        }

        candidates.push(trimmed.to_string());
        if let Some(stripped) = trimmed.strip_prefix("crate::") {
            candidates.push(stripped.to_string());
        } else {
            candidates.push(format!("crate::{trimmed}"));
        }
        if let Some(stripped) = trimmed.strip_prefix("self::") {
            candidates.push(format!("crate::{stripped}"));
        }
        if let Some(stripped) = trimmed.strip_prefix("super::") {
            candidates.push(format!("crate::{stripped}"));
        }

        for candidate in candidates {
            if let Some(def) = self.defs_by_path.get(candidate.as_str()) {
                return Some(*def);
            }
        }

        let tail = trimmed.rsplit("::").next().unwrap_or(trimmed);
        if let Some(candidates) = self.defs_by_name.get(tail)
            && candidates.len() == 1
        {
            return candidates.iter().next().copied();
        }

        None
    }

    fn synthetic_def(&mut self, label: &str, range: TextRange) -> DefId {
        self.host.intern_def_from_token(
            format!(
                "{label}:{}..{}",
                u32::from(range.start()),
                u32::from(range.end())
            )
            .as_str(),
        )
    }

    fn unknown_type_ref(&mut self, token: &str) -> TypeRefId {
        if let Some(existing) = self.typeref_by_key.get(token) {
            return *existing;
        }
        let type_ref = self.host.intern_typeref_from_token(&token);
        self.host.insert_type(type_ref, TypeShape::Unknown);
        self.typeref_by_key.insert(token.to_string(), type_ref);
        type_ref
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
            return format!("tuple:{}", items.join(","));
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
            return format!("app:{path}");
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
            return format!("dyn:{path}");
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

fn compute_world_stamp(workspace_root: &Path, files: &BTreeMap<FileId, LocalFile>) -> WorldStamp {
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

    WorldStamp::new(format!(
        "ra-workspace:{}:{:016x}",
        workspace_root.to_string_lossy(),
        hasher.finish()
    ))
}

fn collect_local_files(
    db: &RootDatabase,
    vfs: &vfs::Vfs,
    workspace_root: &Path,
    sema: &Semantics<'_, RootDatabase>,
) -> Result<BTreeMap<FileId, LocalFile>, RaHostInitError> {
    let mut files = BTreeMap::new();

    for (file_id, path) in vfs.iter() {
        let source_root_id = db.file_source_root(file_id).source_root_id(db);
        let source_root = db.source_root(source_root_id).source_root(db);
        if source_root.is_library {
            continue;
        }

        let Some(abs_path) = path.as_path() else {
            continue;
        };

        let rel_path = normalize_rel_path(workspace_root, abs_path.as_ref());
        let text = db.file_text(file_id).text(db).to_string();
        let editioned = sema.attach_first_edition(file_id);

        files.insert(
            file_id,
            LocalFile {
                rel_path,
                text,
                editioned_file_id: editioned,
            },
        );
    }

    if files.is_empty() {
        return Err(RaHostInitError::SemanticBuild {
            details: "workspace snapshot has no local source files".to_string(),
        });
    }

    Ok(files)
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

fn path_to_string(path: &ast::Path) -> Option<String> {
    let segments = path
        .segments()
        .filter_map(|segment| {
            segment
                .name_ref()
                .map(|name| name.syntax().text().to_string())
        })
        .collect::<Vec<_>>();
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("::"))
    }
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
