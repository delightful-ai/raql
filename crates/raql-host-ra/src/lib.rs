//! RAQL rust-analyzer host adapter and deterministic host model for required
//! section 16 predicates.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use hir_def::{DefWithBodyId, ImplId as HirImplId, ModuleDefId};
use hir_expand::MacroCallId;
use ide::{Analysis, AnalysisHost};
use line_index::LineIndex;
use raql_engine::{EngineHostError, EngineHostView, HostValueKind, RuntimeValue};
use raql_host::HostRuntime;
use rustc_hash::FxHasher;
use span::{EditionedFileId, TextRange};
use syntax::{AstNode, Edition, SourceFile, SyntaxNodePtr};
use thiserror::Error;

mod workspace_loader;
mod workspace_snapshot;

pub use raql_host::{
    CallId, DefId, ImplId, NodeId, RefId, RuntimeScalarOptions, ScalarInputKey, SpanCoord, SpanId,
    SpanKey, StableHandle, TypeRefId, WorldStamp,
};
pub use raql_ir::{ScalarValue, StableId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mutability {
    Shared,
    Mut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    Const,
    Static,
    Field,
    Variant,
    AssocType,
    AssocConst,
    Macro,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DispatchKind {
    Direct,
    ThroughTrait,
    Dyn,
    Closure,
    FnPointer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericArg {
    Type(TypeRefId),
    Lifetime,
    Const,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeShape {
    App {
        head: DefId,
        args: Vec<GenericArg>,
    },
    Ref {
        mutability: Mutability,
        inner: TypeRefId,
    },
    Ptr {
        mutability: Mutability,
        inner: TypeRefId,
    },
    Tuple(Vec<TypeRefId>),
    Slice(TypeRefId),
    Param(DefId),
    Prim(String),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKind {
    If,
    Match,
    While,
    For,
    Loop,
    Block,
    Try,
    Arm,
    Other,
    Expr,
    Item,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnclosingControl {
    pub kind: NodeKind,
    pub span: SpanId,
    pub distance: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RaHostError {
    #[error(
        "span range {start}..{end} is invalid for source text of {len} bytes (source: {rel_path})"
    )]
    InvalidSpanRange {
        rel_path: Box<str>,
        start: u32,
        end: u32,
        len: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RaHostInitError {
    #[error("workspace manifest not found for `{input_path}`")]
    WorkspaceNotFound { input_path: String },
    #[error("failed to load workspace `{manifest}`: {details}")]
    WorkspaceLoad { manifest: String, details: String },
    #[error("failed to build semantic workspace snapshot: {details}")]
    SemanticBuild { details: String },
    #[error("proc-macro server unavailable during workspace load")]
    ProcMacroUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkspaceInitMode {
    Strict,
    #[default]
    Resilient,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NodeRecord {
    kind: NodeKind,
    span: SpanId,
    parent: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefRecord {
    name: Box<str>,
    kind: DefKind,
    span: SpanId,
    path: Box<str>,
    method_owner: Option<DefId>,
    return_type: Option<TypeRefId>,
    error_type: Option<DefId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct CallEdgeRecord {
    caller: DefId,
    callee: DefId,
    site: SpanId,
    dispatch: DispatchKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct FieldRecord {
    owner: DefId,
    name: StableHandle,
    ty: TypeRefId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct VariantRecord {
    owner: DefId,
    name: StableHandle,
    variant_def: DefId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ImplRecord {
    ty: DefId,
    tr: DefId,
    impl_def: DefId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ConstructRecord {
    err_type: DefId,
    variant: StableHandle,
    site: SpanId,
    function: DefId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct PropagateRecord {
    err_type: DefId,
    site: SpanId,
    function: DefId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ConvertRecord {
    src: DefId,
    dst: DefId,
    site: SpanId,
    function: DefId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct HandleRecord {
    err_type: DefId,
    variant: Option<StableHandle>,
    site: SpanId,
    function: DefId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct CompareRecord {
    subject: DefId,
    site: SpanId,
    op: StableHandle,
    function: DefId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct WriteRecord {
    subject: DefId,
    site: SpanId,
    function: DefId,
}

/// Deterministic host model used for section-16 compliance tests and as a
/// reusable substrate for rust-analyzer-backed runtime wiring.
#[derive(Debug, Clone)]
pub struct DeterministicRaHost {
    world_stamp: WorldStamp,
    runtime_scalar_options: RuntimeScalarOptions,
    scalar_inputs: BTreeMap<ScalarInputKey, ScalarValue>,
    stable_overrides: BTreeMap<(String, String), StableId>,
    control_max_depth: u32,
    runtime_notes: BTreeSet<String>,
    extern_relation_errors: BTreeMap<String, RaHostError>,

    type_shapes: BTreeMap<TypeRefId, TypeShape>,
    node_records: BTreeMap<NodeId, NodeRecord>,
    span_keys: BTreeMap<SpanId, SpanKey>,
    def_records: BTreeMap<DefId, DefRecord>,
    call_edges: BTreeSet<CallEdgeRecord>,
    field_records: BTreeSet<FieldRecord>,
    variant_records: BTreeSet<VariantRecord>,
    method_records: BTreeSet<(DefId, DefId)>,
    trait_method_records: BTreeSet<(DefId, DefId)>,
    impl_records: BTreeSet<ImplRecord>,
    from_impl_records: BTreeSet<(DefId, DefId, DefId)>,
    construct_records: BTreeSet<ConstructRecord>,
    propagate_records: BTreeSet<PropagateRecord>,
    convert_records: BTreeSet<ConvertRecord>,
    handle_records: BTreeSet<HandleRecord>,
    compare_records: BTreeSet<CompareRecord>,
    write_records: BTreeSet<WriteRecord>,
    public_defs: BTreeSet<DefId>,
    test_defs: BTreeSet<DefId>,
    allowed_spans: Option<BTreeSet<SpanId>>,

    handle_keys: BTreeMap<DefId, StableHandle>,
    typeref_keys: BTreeMap<TypeRefId, StableHandle>,
    node_keys: BTreeMap<NodeId, StableHandle>,
    call_keys: BTreeMap<CallId, StableHandle>,
    ref_keys: BTreeMap<RefId, StableHandle>,
    impl_keys: BTreeMap<ImplId, StableHandle>,
}

impl Default for DeterministicRaHost {
    fn default() -> Self {
        Self::new()
    }
}

impl DeterministicRaHost {
    pub fn new() -> Self {
        Self {
            world_stamp: WorldStamp::new("ra-host:deterministic"),
            runtime_scalar_options: RuntimeScalarOptions::default(),
            scalar_inputs: BTreeMap::new(),
            stable_overrides: BTreeMap::new(),
            control_max_depth: 32,
            runtime_notes: BTreeSet::new(),
            extern_relation_errors: BTreeMap::new(),
            type_shapes: BTreeMap::new(),
            node_records: BTreeMap::new(),
            span_keys: BTreeMap::new(),
            def_records: BTreeMap::new(),
            call_edges: BTreeSet::new(),
            field_records: BTreeSet::new(),
            variant_records: BTreeSet::new(),
            method_records: BTreeSet::new(),
            trait_method_records: BTreeSet::new(),
            impl_records: BTreeSet::new(),
            from_impl_records: BTreeSet::new(),
            construct_records: BTreeSet::new(),
            propagate_records: BTreeSet::new(),
            convert_records: BTreeSet::new(),
            handle_records: BTreeSet::new(),
            compare_records: BTreeSet::new(),
            write_records: BTreeSet::new(),
            public_defs: BTreeSet::new(),
            test_defs: BTreeSet::new(),
            allowed_spans: None,
            handle_keys: BTreeMap::new(),
            typeref_keys: BTreeMap::new(),
            node_keys: BTreeMap::new(),
            call_keys: BTreeMap::new(),
            ref_keys: BTreeMap::new(),
            impl_keys: BTreeMap::new(),
        }
    }

    pub fn set_world_stamp(&mut self, stamp: impl Into<WorldStamp>) {
        self.world_stamp = stamp.into();
    }

    pub fn set_runtime_scalar_options(&mut self, options: RuntimeScalarOptions) {
        self.runtime_scalar_options = options;
    }

    pub fn insert_scalar_input(&mut self, key: ScalarInputKey, value: ScalarValue) {
        self.scalar_inputs.insert(key, value);
    }

    pub fn insert_stable_id_override(
        &mut self,
        namespace: impl Into<String>,
        logical_name: impl Into<String>,
        id: StableId,
    ) {
        self.stable_overrides
            .insert((namespace.into(), logical_name.into()), id);
    }

    pub fn insert_handle(&mut self, def: DefId, handle: impl Into<StableHandle>) {
        self.handle_keys.insert(def, handle.into());
    }

    pub fn insert_typeref_id(&mut self, type_ref: TypeRefId, handle: impl Into<StableHandle>) {
        self.typeref_keys.insert(type_ref, handle.into());
    }

    pub fn insert_node_id(&mut self, node: NodeId, handle: impl Into<StableHandle>) {
        self.node_keys.insert(node, handle.into());
    }

    pub fn insert_call_id(&mut self, call: CallId, handle: impl Into<StableHandle>) {
        self.call_keys.insert(call, handle.into());
    }

    pub fn insert_ref_id(&mut self, r#ref: RefId, handle: impl Into<StableHandle>) {
        self.ref_keys.insert(r#ref, handle.into());
    }

    pub fn insert_impl_id(&mut self, r#impl: ImplId, handle: impl Into<StableHandle>) {
        self.impl_keys.insert(r#impl, handle.into());
    }

    pub fn insert_def(
        &mut self,
        def: DefId,
        name: impl Into<Box<str>>,
        kind: DefKind,
        span: SpanId,
        path: impl Into<Box<str>>,
    ) {
        let (method_owner, return_type, error_type) = self
            .def_records
            .get(&def)
            .map(|existing| {
                (
                    existing.method_owner,
                    existing.return_type,
                    existing.error_type,
                )
            })
            .unwrap_or((None, None, None));
        self.def_records.insert(
            def,
            DefRecord {
                name: name.into(),
                kind,
                span,
                path: path.into(),
                method_owner,
                return_type,
                error_type,
            },
        );
    }

    pub fn set_method_owner(&mut self, method: DefId, owner: Option<DefId>) {
        self.ensure_def_record_mut(method).method_owner = owner;
    }

    pub fn set_fn_return_type(&mut self, function: DefId, return_type: Option<TypeRefId>) {
        self.ensure_def_record_mut(function).return_type = return_type;
    }

    pub fn set_fn_error_type(&mut self, function: DefId, error_type: Option<DefId>) {
        self.ensure_def_record_mut(function).error_type = error_type;
    }

    pub fn mark_public(&mut self, def: DefId, is_public: bool) {
        if is_public {
            self.public_defs.insert(def);
        } else {
            self.public_defs.remove(&def);
        }
    }

    pub fn mark_in_test(&mut self, def: DefId, in_test: bool) {
        if in_test {
            self.test_defs.insert(def);
        } else {
            self.test_defs.remove(&def);
        }
    }

    pub fn set_allowed_spans(&mut self, spans: Option<BTreeSet<SpanId>>) {
        self.allowed_spans = spans;
    }

    pub fn insert_call_edge(
        &mut self,
        caller: DefId,
        callee: DefId,
        site: SpanId,
        dispatch: DispatchKind,
    ) {
        self.call_edges.insert(CallEdgeRecord {
            caller,
            callee,
            site,
            dispatch,
        });
    }

    pub fn insert_field(&mut self, owner: DefId, name: impl Into<StableHandle>, ty: TypeRefId) {
        self.field_records.insert(FieldRecord {
            owner,
            name: name.into(),
            ty,
        });
    }

    pub fn insert_variant(
        &mut self,
        owner: DefId,
        name: impl Into<StableHandle>,
        variant_def: DefId,
    ) {
        self.variant_records.insert(VariantRecord {
            owner,
            name: name.into(),
            variant_def,
        });
    }

    pub fn insert_method(&mut self, owner: DefId, method: DefId) {
        self.method_records.insert((owner, method));
        let record = self.ensure_def_record_mut(method);
        record.kind = DefKind::Method;
        record.method_owner = Some(owner);
    }

    pub fn insert_trait_method(&mut self, owner: DefId, method: DefId) {
        self.trait_method_records.insert((owner, method));
        let record = self.ensure_def_record_mut(method);
        record.kind = DefKind::Method;
        record.method_owner = Some(owner);
    }

    pub fn insert_implements(&mut self, ty: DefId, tr: DefId, impl_def: DefId) {
        self.impl_records.insert(ImplRecord { ty, tr, impl_def });
    }

    pub fn insert_from_impl(&mut self, src: DefId, dst: DefId, impl_def: DefId) {
        self.from_impl_records.insert((src, dst, impl_def));
    }

    pub fn insert_construct(
        &mut self,
        err_type: DefId,
        variant: impl Into<StableHandle>,
        site: SpanId,
        function: DefId,
    ) {
        self.construct_records.insert(ConstructRecord {
            err_type,
            variant: variant.into(),
            site,
            function,
        });
    }

    pub fn insert_propagate(&mut self, err_type: DefId, site: SpanId, function: DefId) {
        self.propagate_records.insert(PropagateRecord {
            err_type,
            site,
            function,
        });
    }

    pub fn insert_convert(&mut self, src: DefId, dst: DefId, site: SpanId, function: DefId) {
        self.convert_records.insert(ConvertRecord {
            src,
            dst,
            site,
            function,
        });
    }

    pub fn insert_handle_error(
        &mut self,
        err_type: DefId,
        variant: Option<StableHandle>,
        site: SpanId,
        function: DefId,
    ) {
        self.handle_records.insert(HandleRecord {
            err_type,
            variant,
            site,
            function,
        });
    }

    pub fn insert_compare(
        &mut self,
        subject: DefId,
        site: SpanId,
        op: impl Into<StableHandle>,
        function: DefId,
    ) {
        self.compare_records.insert(CompareRecord {
            subject,
            site,
            op: op.into(),
            function,
        });
    }

    pub fn insert_write(&mut self, subject: DefId, site: SpanId, function: DefId) {
        self.write_records.insert(WriteRecord {
            subject,
            site,
            function,
        });
    }

    #[cfg(test)]
    pub(crate) fn insert_extern_relation_error(
        &mut self,
        predicate: impl Into<String>,
        error: RaHostError,
    ) {
        self.extern_relation_errors.insert(predicate.into(), error);
    }

    #[cfg(test)]
    pub(crate) fn clear_extern_relation_error(&mut self, predicate: &str) {
        self.extern_relation_errors.remove(predicate);
    }

    pub fn insert_type(&mut self, type_ref: TypeRefId, shape: TypeShape) {
        self.type_shapes.insert(type_ref, shape);
    }

    pub fn ty_app(&self, type_ref: TypeRefId) -> Option<DefId> {
        match self.type_shapes.get(&type_ref) {
            Some(TypeShape::App { head, .. }) => Some(*head),
            _ => None,
        }
    }

    pub fn ty_args(&self, type_ref: TypeRefId) -> Vec<(usize, TypeRefId)> {
        match self.type_shapes.get(&type_ref) {
            Some(TypeShape::App { args, .. }) => args
                .iter()
                .filter_map(|arg| match arg {
                    GenericArg::Type(type_ref) => Some(*type_ref),
                    GenericArg::Lifetime | GenericArg::Const => None,
                })
                .enumerate()
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn ty_arg(&self, type_ref: TypeRefId, index: usize) -> Option<TypeRefId> {
        self.ty_args(type_ref)
            .into_iter()
            .find_map(|(i, arg)| (i == index).then_some(arg))
    }

    pub fn ty_ref(&self, type_ref: TypeRefId) -> Option<(Mutability, TypeRefId)> {
        match self.type_shapes.get(&type_ref) {
            Some(TypeShape::Ref { mutability, inner }) => Some((*mutability, *inner)),
            _ => None,
        }
    }

    pub fn ty_ptr(&self, type_ref: TypeRefId) -> Option<(Mutability, TypeRefId)> {
        match self.type_shapes.get(&type_ref) {
            Some(TypeShape::Ptr { mutability, inner }) => Some((*mutability, *inner)),
            _ => None,
        }
    }

    pub fn ty_tuples(&self, type_ref: TypeRefId) -> Vec<(usize, TypeRefId)> {
        match self.type_shapes.get(&type_ref) {
            Some(TypeShape::Tuple(items)) => items.iter().copied().enumerate().collect(),
            _ => Vec::new(),
        }
    }

    pub fn ty_tuple(&self, type_ref: TypeRefId, index: usize) -> Option<TypeRefId> {
        self.ty_tuples(type_ref)
            .into_iter()
            .find_map(|(i, elem)| (i == index).then_some(elem))
    }

    pub fn ty_slice(&self, type_ref: TypeRefId) -> Option<TypeRefId> {
        match self.type_shapes.get(&type_ref) {
            Some(TypeShape::Slice(inner)) => Some(*inner),
            _ => None,
        }
    }

    pub fn ty_param(&self, type_ref: TypeRefId) -> Option<DefId> {
        match self.type_shapes.get(&type_ref) {
            Some(TypeShape::Param(param)) => Some(*param),
            _ => None,
        }
    }

    pub fn ty_prim(&self, type_ref: TypeRefId) -> Option<&str> {
        match self.type_shapes.get(&type_ref) {
            Some(TypeShape::Prim(name)) => Some(name.as_str()),
            _ => None,
        }
    }

    pub fn ty_unknown(&self, type_ref: TypeRefId) -> bool {
        matches!(self.type_shapes.get(&type_ref), Some(TypeShape::Unknown))
    }

    pub fn insert_span(&mut self, span: SpanId, key: SpanKey) {
        self.span_keys.insert(span, key);
    }

    pub fn insert_node(
        &mut self,
        node: NodeId,
        kind: NodeKind,
        span: SpanId,
        parent: Option<NodeId>,
    ) {
        self.node_records
            .insert(node, NodeRecord { kind, span, parent });
    }

    pub fn node_kind(&self, node: NodeId) -> Option<NodeKind> {
        self.node_records.get(&node).map(|record| record.kind)
    }

    pub fn node_span(&self, node: NodeId) -> Option<SpanId> {
        self.node_records.get(&node).map(|record| record.span)
    }

    pub fn node_parent(&self, node: NodeId) -> Option<Option<NodeId>> {
        self.node_records.get(&node).map(|record| record.parent)
    }

    fn known_defs(&self) -> BTreeSet<DefId> {
        let mut defs = BTreeSet::new();
        defs.extend(self.def_records.keys().copied());
        defs.extend(self.handle_keys.keys().copied());
        for shape in self.type_shapes.values() {
            match shape {
                TypeShape::App { head, .. } | TypeShape::Param(head) => {
                    defs.insert(*head);
                }
                _ => {}
            }
        }
        defs.extend(
            self.call_edges
                .iter()
                .flat_map(|edge| [edge.caller, edge.callee]),
        );
        defs.extend(self.field_records.iter().map(|record| record.owner));
        defs.extend(
            self.variant_records
                .iter()
                .flat_map(|record| [record.owner, record.variant_def]),
        );
        defs.extend(
            self.method_records
                .iter()
                .flat_map(|(owner, method)| [*owner, *method]),
        );
        defs.extend(
            self.trait_method_records
                .iter()
                .flat_map(|(owner, method)| [*owner, *method]),
        );
        defs.extend(
            self.impl_records
                .iter()
                .flat_map(|record| [record.ty, record.tr, record.impl_def]),
        );
        defs.extend(
            self.from_impl_records
                .iter()
                .flat_map(|(src, dst, impl_def)| [*src, *dst, *impl_def]),
        );
        defs.extend(self.construct_records.iter().map(|record| record.err_type));
        defs.extend(self.construct_records.iter().map(|record| record.function));
        defs.extend(self.propagate_records.iter().map(|record| record.err_type));
        defs.extend(self.propagate_records.iter().map(|record| record.function));
        defs.extend(
            self.convert_records
                .iter()
                .flat_map(|record| [record.src, record.dst, record.function]),
        );
        defs.extend(self.handle_records.iter().map(|record| record.err_type));
        defs.extend(self.handle_records.iter().map(|record| record.function));
        defs.extend(self.compare_records.iter().map(|record| record.subject));
        defs.extend(self.compare_records.iter().map(|record| record.function));
        defs.extend(self.write_records.iter().map(|record| record.subject));
        defs.extend(self.write_records.iter().map(|record| record.function));
        defs.extend(self.public_defs.iter().copied());
        defs.extend(self.test_defs.iter().copied());
        defs
    }

    fn known_functions(&self) -> BTreeSet<DefId> {
        self.known_defs()
            .into_iter()
            .filter(|def| is_function_kind(self.def_kind_for(*def)))
            .collect()
    }

    fn rows_for_known_defs<F>(&self, row_for_def: F) -> Vec<Vec<RuntimeValue>>
    where
        F: FnMut(DefId) -> Vec<RuntimeValue>,
    {
        self.known_defs().into_iter().map(row_for_def).collect()
    }

    fn rows_for_known_functions<F>(&self, row_for_function: F) -> Vec<Vec<RuntimeValue>>
    where
        F: FnMut(DefId) -> Vec<RuntimeValue>,
    {
        self.known_functions()
            .into_iter()
            .map(row_for_function)
            .collect()
    }

    fn known_typerefs(&self) -> BTreeSet<TypeRefId> {
        let mut refs = BTreeSet::new();
        refs.extend(self.typeref_keys.keys().copied());
        refs.extend(self.type_shapes.keys().copied());
        for shape in self.type_shapes.values() {
            match shape {
                TypeShape::App { args, .. } => {
                    refs.extend(args.iter().filter_map(|arg| match arg {
                        GenericArg::Type(type_ref) => Some(*type_ref),
                        GenericArg::Lifetime | GenericArg::Const => None,
                    }));
                }
                TypeShape::Ref { inner, .. } | TypeShape::Ptr { inner, .. } => {
                    refs.insert(*inner);
                }
                TypeShape::Tuple(items) => {
                    refs.extend(items.iter().copied());
                }
                TypeShape::Slice(inner) => {
                    refs.insert(*inner);
                }
                TypeShape::Param(_) | TypeShape::Prim(_) | TypeShape::Unknown => {}
            }
        }
        refs.extend(self.field_records.iter().map(|record| record.ty));
        refs.extend(
            self.def_records
                .values()
                .filter_map(|record| record.return_type),
        );
        refs
    }

    fn known_spans(&self) -> BTreeSet<SpanId> {
        let mut spans = BTreeSet::new();
        spans.extend(self.span_keys.keys().copied());
        spans.extend(self.node_records.values().map(|record| record.span));
        spans.extend(self.def_records.values().map(|record| record.span));
        spans.extend(self.known_defs().iter().map(|def| self.def_span_for(*def)));
        spans.extend(self.call_edges.iter().map(|edge| edge.site));
        spans.extend(self.construct_records.iter().map(|record| record.site));
        spans.extend(self.propagate_records.iter().map(|record| record.site));
        spans.extend(self.convert_records.iter().map(|record| record.site));
        spans.extend(self.handle_records.iter().map(|record| record.site));
        spans.extend(self.compare_records.iter().map(|record| record.site));
        spans.extend(self.write_records.iter().map(|record| record.site));
        if let Some(allowed) = &self.allowed_spans {
            spans.extend(allowed.iter().copied());
        }
        spans
    }

    pub fn node_at(&self, query: SpanId) -> Option<NodeId> {
        let query_key = self.span_key(query).ok()?;
        let mut candidates = Vec::new();
        for (node, record) in &self.node_records {
            let node_key = self
                .span_keys
                .get(&record.span)
                .cloned()
                .unwrap_or_else(|| deterministic_span_key(record.span));
            if span_contains(&node_key, &query_key) {
                candidates.push((*node, node_key));
            }
        }
        candidates.sort_by(|(node_a, span_a), (node_b, span_b)| {
            let specificity_a = span_specificity(span_a);
            let specificity_b = span_specificity(span_b);
            specificity_a
                .cmp(&specificity_b)
                .then_with(|| stable_node_key(self, *node_a).cmp(&stable_node_key(self, *node_b)))
        });
        candidates.first().map(|(node, _)| *node)
    }

    pub fn control_max_depth(&self) -> u32 {
        self.control_max_depth
    }

    pub fn set_control_max_depth(&mut self, depth: u32) {
        self.control_max_depth = depth;
    }

    pub fn enclosing_control(&self, span: SpanId) -> Option<EnclosingControl> {
        self.enclosing_control_with_depth(span, self.control_max_depth)
    }

    pub fn enclosing_control_with_depth(
        &self,
        span: SpanId,
        max_depth: u32,
    ) -> Option<EnclosingControl> {
        let mut current = self.node_at(span)?;
        let mut distance = 0_u32;

        loop {
            if distance > max_depth {
                return None;
            }

            let record = self.node_records.get(&current)?;
            if is_control_kind(record.kind) {
                return Some(EnclosingControl {
                    kind: record.kind,
                    span: record.span,
                    distance,
                });
            }

            let Some(parent) = record.parent else {
                return None;
            };
            current = parent;
            distance += 1;
        }
    }

    pub fn intern_def_from_token(&mut self, token: &str) -> DefId {
        let id = DefId::new(deterministic_stable_id("def", token));
        self.handle_keys
            .entry(id)
            .or_insert_with(|| StableHandle::new(format!("def:{token}")));
        id
    }

    pub fn intern_typeref_from_token<T: fmt::Debug>(&mut self, token: &T) -> TypeRefId {
        let token = format!("{token:?}");
        let id = TypeRefId::new(deterministic_stable_id("typeref", token.as_str()));
        self.typeref_keys
            .entry(id)
            .or_insert_with(|| StableHandle::new(format!("typeref:{token}")));
        id
    }

    pub fn intern_node_from_syntax_ptr(&mut self, node: SyntaxNodePtr) -> NodeId {
        let token = format!("{node:?}");
        let id = NodeId::new(deterministic_stable_id("node", token.as_str()));
        self.node_keys
            .entry(id)
            .or_insert_with(|| StableHandle::new(format!("node:{token}")));
        id
    }

    pub fn intern_call_from_macro_call(&mut self, call: MacroCallId) -> CallId {
        let token = format!("{call:?}");
        let id = CallId::new(deterministic_stable_id("call", token.as_str()));
        self.call_keys
            .entry(id)
            .or_insert_with(|| StableHandle::new(format!("call:{token}")));
        id
    }

    pub fn intern_ref_from_token<T: fmt::Debug>(&mut self, token: &T) -> RefId {
        let token = format!("{token:?}");
        let id = RefId::new(deterministic_stable_id("ref", token.as_str()));
        self.ref_keys
            .entry(id)
            .or_insert_with(|| StableHandle::new(format!("ref:{token}")));
        id
    }

    pub fn intern_impl_from_hir_impl(&mut self, impl_id: HirImplId) -> ImplId {
        let token = format!("{impl_id:?}");
        let id = ImplId::new(deterministic_stable_id("impl", token.as_str()));
        self.impl_keys
            .entry(id)
            .or_insert_with(|| StableHandle::new(format!("impl:{token}")));
        id
    }

    pub fn intern_span_from_text(
        &mut self,
        file_id: EditionedFileId,
        rel_path: impl Into<Box<str>>,
        source_text: &str,
        range: TextRange,
    ) -> Result<SpanId, RaHostError> {
        let rel_path = rel_path.into();
        let line_index = LineIndex::new(source_text);
        let start = line_index.try_line_col(range.start()).ok_or_else(|| {
            RaHostError::InvalidSpanRange {
                rel_path: rel_path.clone(),
                start: u32::from(range.start()),
                end: u32::from(range.end()),
                len: source_text.len(),
            }
        })?;
        let end =
            line_index
                .try_line_col(range.end())
                .ok_or_else(|| RaHostError::InvalidSpanRange {
                    rel_path: rel_path.clone(),
                    start: u32::from(range.start()),
                    end: u32::from(range.end()),
                    len: source_text.len(),
                })?;

        let token = format!(
            "{}:{}..{}",
            file_id.as_u32(),
            u32::from(range.start()),
            u32::from(range.end())
        );
        let id = SpanId::new(deterministic_stable_id("span", token.as_str()));
        self.insert_span(
            id,
            SpanKey::new(
                rel_path,
                SpanCoord::new(start.line, start.col),
                SpanCoord::new(end.line, end.col),
            ),
        );
        Ok(id)
    }

    pub fn parse_rust_and_intern_root_node(&mut self, text: &str, edition: Edition) -> NodeId {
        let parse = SourceFile::parse(text, edition);
        self.intern_node_from_syntax_ptr(SyntaxNodePtr::new(parse.tree().syntax()))
    }

    fn ensure_def_record_mut(&mut self, def: DefId) -> &mut DefRecord {
        self.def_records.entry(def).or_insert_with(|| DefRecord {
            name: fallback_def_name(def).into_boxed_str(),
            kind: DefKind::Other,
            span: fallback_def_span(def),
            path: fallback_def_path(def).into_boxed_str(),
            method_owner: None,
            return_type: None,
            error_type: None,
        })
    }

    fn def_name_for(&self, def: DefId) -> String {
        self.def_records
            .get(&def)
            .map(|record| record.name.to_string())
            .unwrap_or_else(|| fallback_def_name(def))
    }

    fn def_kind_for(&self, def: DefId) -> DefKind {
        self.def_records
            .get(&def)
            .map(|record| record.kind)
            .unwrap_or(DefKind::Other)
    }

    fn def_span_for(&self, def: DefId) -> SpanId {
        self.def_records
            .get(&def)
            .map(|record| record.span)
            .unwrap_or_else(|| fallback_def_span(def))
    }

    fn def_path_for(&self, def: DefId) -> String {
        self.def_records
            .get(&def)
            .map(|record| record.path.to_string())
            .unwrap_or_else(|| fallback_def_path(def))
    }

    fn method_owner_for(&self, method: DefId) -> Option<DefId> {
        self.def_records
            .get(&method)
            .and_then(|record| record.method_owner)
    }

    fn fn_error_for(&self, function: DefId) -> Option<DefId> {
        self.def_records
            .get(&function)
            .and_then(|record| record.error_type)
    }

    fn fn_return_for(&self, function: DefId) -> TypeRefId {
        self.def_records
            .get(&function)
            .and_then(|record| record.return_type)
            .unwrap_or_else(|| fallback_fn_return_typeref(function))
    }

    fn search_rows(&self) -> Vec<Vec<RuntimeValue>> {
        let mut rows = Vec::new();
        for def in self.known_defs() {
            let name = self.def_name_for(def);
            let path = self.def_path_for(def);
            let mut keys = BTreeSet::new();
            keys.insert(name.clone());
            keys.insert(name.to_lowercase());
            keys.insert(path.clone());
            keys.insert(path.to_lowercase());
            for segment in path.split("::").filter(|segment| !segment.is_empty()) {
                keys.insert(segment.to_string());
                keys.insert(segment.to_lowercase());
            }
            for key in keys {
                let score = if key == name {
                    100
                } else if key.eq_ignore_ascii_case(name.as_str()) {
                    95
                } else if path.contains(key.as_str()) {
                    85
                } else {
                    70
                };
                rows.push(vec![
                    RuntimeValue::String(key),
                    rv_def(def),
                    RuntimeValue::Int(score),
                ]);
            }
        }
        rows
    }
}

impl HostRuntime for DeterministicRaHost {
    type Error = RaHostError;

    fn stable_id(&self, namespace: &str, logical_name: &str) -> Result<StableId, Self::Error> {
        if let Some(id) = self
            .stable_overrides
            .get(&(namespace.to_owned(), logical_name.to_owned()))
        {
            return Ok(*id);
        }
        Ok(deterministic_stable_id(namespace, logical_name))
    }

    fn world_stamp(&self) -> Result<WorldStamp, Self::Error> {
        Ok(self.world_stamp.clone())
    }

    fn handle(&self, def: DefId) -> Result<StableHandle, Self::Error> {
        Ok(self
            .handle_keys
            .get(&def)
            .cloned()
            .unwrap_or_else(|| fallback_def_handle(def)))
    }

    fn span_key(&self, span: SpanId) -> Result<SpanKey, Self::Error> {
        Ok(self
            .span_keys
            .get(&span)
            .cloned()
            .unwrap_or_else(|| deterministic_span_key(span)))
    }

    fn typeref_id(&self, type_ref: TypeRefId) -> Result<StableHandle, Self::Error> {
        Ok(self.typeref_id(type_ref))
    }

    fn node_id(&self, node: NodeId) -> Result<StableHandle, Self::Error> {
        Ok(self.node_id(node))
    }

    fn call_id(&self, call: CallId) -> Result<StableHandle, Self::Error> {
        Ok(self.call_keys.get(&call).cloned().unwrap_or_else(|| {
            StableHandle::new(format!("call_id:{:#018x}", call.stable_id().as_u64()))
        }))
    }

    fn ref_id(&self, r#ref: RefId) -> Result<StableHandle, Self::Error> {
        Ok(self.ref_keys.get(&r#ref).cloned().unwrap_or_else(|| {
            StableHandle::new(format!("ref_id:{:#018x}", r#ref.stable_id().as_u64()))
        }))
    }

    fn impl_id(&self, r#impl: ImplId) -> Result<StableHandle, Self::Error> {
        Ok(self.impl_keys.get(&r#impl).cloned().unwrap_or_else(|| {
            StableHandle::new(format!("impl_id:{:#018x}", r#impl.stable_id().as_u64()))
        }))
    }

    fn runtime_scalar_options(&self) -> Result<RuntimeScalarOptions, Self::Error> {
        Ok(self.runtime_scalar_options)
    }

    fn scalar_input(&self, key: &ScalarInputKey) -> Result<Option<ScalarValue>, Self::Error> {
        Ok(self.scalar_inputs.get(key).cloned())
    }
}

impl DeterministicRaHost {
    pub fn typeref_id(&self, type_ref: TypeRefId) -> StableHandle {
        self.typeref_keys
            .get(&type_ref)
            .cloned()
            .unwrap_or_else(|| {
                StableHandle::new(format!(
                    "typeref_id:{:#018x}",
                    type_ref.stable_id().as_u64()
                ))
            })
    }

    pub fn node_id(&self, node: NodeId) -> StableHandle {
        self.node_keys.get(&node).cloned().unwrap_or_else(|| {
            StableHandle::new(format!("node_id:{:#018x}", node.stable_id().as_u64()))
        })
    }

    pub(crate) fn extern_relation_rows_for_predicate(
        &self,
        predicate: &str,
    ) -> Option<Vec<Vec<RuntimeValue>>> {
        match predicate {
            "world_stamp" => Some(vec![vec![RuntimeValue::String(
                self.world_stamp.as_str().to_string(),
            )]]),
            "def" => Some(self.rows_for_known_defs(|def| vec![rv_def(def)])),
            "search" => Some(self.search_rows()),
            "def_name" => Some(self.rows_for_known_defs(|def| {
                vec![rv_def(def), RuntimeValue::String(self.def_name_for(def))]
            })),
            "def_kind" => {
                Some(self.rows_for_known_defs(|def| {
                    vec![rv_def(def), rv_def_kind(self.def_kind_for(def))]
                }))
            }
            "def_span" => Some(
                self.rows_for_known_defs(|def| vec![rv_def(def), rv_span(self.def_span_for(def))]),
            ),
            "def_path" => Some(self.rows_for_known_defs(|def| {
                vec![rv_def(def), RuntimeValue::String(self.def_path_for(def))]
            })),
            "method_of" => Some(self.rows_for_known_defs(|method| {
                vec![rv_def(method), rv_option_def(self.method_owner_for(method))]
            })),
            "fn_error_type" => Some(self.rows_for_known_functions(|function| {
                vec![rv_def(function), rv_option_def(self.fn_error_for(function))]
            })),
            "fn_return_type" => Some(self.rows_for_known_functions(|function| {
                vec![rv_def(function), rv_typeref(self.fn_return_for(function))]
            })),
            "call_edge" => Some(
                self.call_edges
                    .iter()
                    .map(|edge| {
                        vec![
                            rv_def(edge.caller),
                            rv_def(edge.callee),
                            rv_span(edge.site),
                            rv_dispatch_kind(edge.dispatch),
                        ]
                    })
                    .collect(),
            ),
            "field" => Some(
                self.field_records
                    .iter()
                    .map(|record| {
                        vec![
                            rv_def(record.owner),
                            RuntimeValue::String(record.name.as_str().to_string()),
                            rv_typeref(record.ty),
                        ]
                    })
                    .collect(),
            ),
            "variant" => Some(
                self.variant_records
                    .iter()
                    .map(|record| {
                        vec![
                            rv_def(record.owner),
                            RuntimeValue::String(record.name.as_str().to_string()),
                            rv_def(record.variant_def),
                        ]
                    })
                    .collect(),
            ),
            "method" => Some(
                self.method_records
                    .iter()
                    .map(|(owner, method)| vec![rv_def(*owner), rv_def(*method)])
                    .collect(),
            ),
            "trait_method" => Some(
                self.trait_method_records
                    .iter()
                    .map(|(owner, method)| vec![rv_def(*owner), rv_def(*method)])
                    .collect(),
            ),
            "implements" => Some(
                self.impl_records
                    .iter()
                    .map(|record| {
                        vec![
                            rv_def(record.ty),
                            rv_def(record.tr),
                            rv_def(record.impl_def),
                        ]
                    })
                    .collect(),
            ),
            "from_impl" => Some(
                self.from_impl_records
                    .iter()
                    .map(|(src, dst, impl_def)| vec![rv_def(*src), rv_def(*dst), rv_def(*impl_def)])
                    .collect(),
            ),
            "constructs" => Some(
                self.construct_records
                    .iter()
                    .map(|record| {
                        vec![
                            rv_def(record.err_type),
                            RuntimeValue::String(record.variant.as_str().to_string()),
                            rv_span(record.site),
                            rv_def(record.function),
                        ]
                    })
                    .collect(),
            ),
            "propagates" => Some(
                self.propagate_records
                    .iter()
                    .map(|record| {
                        vec![
                            rv_def(record.err_type),
                            rv_span(record.site),
                            rv_def(record.function),
                        ]
                    })
                    .collect(),
            ),
            "converts" => Some(
                self.convert_records
                    .iter()
                    .map(|record| {
                        vec![
                            rv_def(record.src),
                            rv_def(record.dst),
                            rv_span(record.site),
                            rv_def(record.function),
                        ]
                    })
                    .collect(),
            ),
            "handles" => Some(
                self.handle_records
                    .iter()
                    .map(|record| {
                        vec![
                            rv_def(record.err_type),
                            rv_option_string(record.variant.as_ref().map(|value| value.as_str())),
                            rv_span(record.site),
                            rv_def(record.function),
                        ]
                    })
                    .collect(),
            ),
            "compares" => Some(
                self.compare_records
                    .iter()
                    .map(|record| {
                        vec![
                            rv_def(record.subject),
                            rv_span(record.site),
                            RuntimeValue::String(record.op.as_str().to_string()),
                            rv_def(record.function),
                        ]
                    })
                    .collect(),
            ),
            "writes" => Some(
                self.write_records
                    .iter()
                    .map(|record| {
                        vec![
                            rv_def(record.subject),
                            rv_span(record.site),
                            rv_def(record.function),
                        ]
                    })
                    .collect(),
            ),
            "span_allowed" => {
                let spans = self
                    .allowed_spans
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| self.known_spans());
                Some(spans.into_iter().map(|span| vec![rv_span(span)]).collect())
            }
            "is_public" => Some(
                self.public_defs
                    .iter()
                    .copied()
                    .map(|def| vec![rv_def(def)])
                    .collect(),
            ),
            "in_test" => Some(
                self.test_defs
                    .iter()
                    .copied()
                    .map(|def| vec![rv_def(def)])
                    .collect(),
            ),
            "dispatch_str" => Some(vec![
                vec![
                    rv_dispatch_kind(DispatchKind::Direct),
                    RuntimeValue::String("direct".to_string()),
                ],
                vec![
                    rv_dispatch_kind(DispatchKind::ThroughTrait),
                    RuntimeValue::String("through_trait".to_string()),
                ],
                vec![
                    rv_dispatch_kind(DispatchKind::Dyn),
                    RuntimeValue::String("dyn".to_string()),
                ],
                vec![
                    rv_dispatch_kind(DispatchKind::Closure),
                    RuntimeValue::String("closure".to_string()),
                ],
                vec![
                    rv_dispatch_kind(DispatchKind::FnPointer),
                    RuntimeValue::String("fn_pointer".to_string()),
                ],
            ]),
            "ty_app" => {
                let mut rows = Vec::new();
                for (type_ref, shape) in &self.type_shapes {
                    if let TypeShape::App { head, .. } = shape {
                        rows.push(vec![rv_typeref(*type_ref), rv_def(*head)]);
                    }
                }
                Some(rows)
            }
            "ty_arg" => {
                let mut rows = Vec::new();
                for type_ref in self.type_shapes.keys() {
                    for (index, arg) in self.ty_args(*type_ref) {
                        rows.push(vec![
                            rv_typeref(*type_ref),
                            RuntimeValue::Int(index as i64),
                            rv_typeref(arg),
                        ]);
                    }
                }
                Some(rows)
            }
            "ty_ref" => {
                let mut rows = Vec::new();
                for (type_ref, shape) in &self.type_shapes {
                    if let TypeShape::Ref { mutability, inner } = shape {
                        rows.push(vec![
                            rv_typeref(*type_ref),
                            rv_mutability(*mutability),
                            rv_typeref(*inner),
                        ]);
                    }
                }
                Some(rows)
            }
            "ty_ptr" => {
                let mut rows = Vec::new();
                for (type_ref, shape) in &self.type_shapes {
                    if let TypeShape::Ptr { mutability, inner } = shape {
                        rows.push(vec![
                            rv_typeref(*type_ref),
                            rv_mutability(*mutability),
                            rv_typeref(*inner),
                        ]);
                    }
                }
                Some(rows)
            }
            "ty_tuple" => {
                let mut rows = Vec::new();
                for (type_ref, shape) in &self.type_shapes {
                    if let TypeShape::Tuple(items) = shape {
                        for (index, elem) in items.iter().copied().enumerate() {
                            rows.push(vec![
                                rv_typeref(*type_ref),
                                RuntimeValue::Int(index as i64),
                                rv_typeref(elem),
                            ]);
                        }
                    }
                }
                Some(rows)
            }
            "ty_slice" => {
                let mut rows = Vec::new();
                for (type_ref, shape) in &self.type_shapes {
                    if let TypeShape::Slice(inner) = shape {
                        rows.push(vec![rv_typeref(*type_ref), rv_typeref(*inner)]);
                    }
                }
                Some(rows)
            }
            "ty_param" => {
                let mut rows = Vec::new();
                for (type_ref, shape) in &self.type_shapes {
                    if let TypeShape::Param(param) = shape {
                        rows.push(vec![rv_typeref(*type_ref), rv_def(*param)]);
                    }
                }
                Some(rows)
            }
            "ty_prim" => {
                let mut rows = Vec::new();
                for (type_ref, shape) in &self.type_shapes {
                    if let TypeShape::Prim(name) = shape {
                        rows.push(vec![
                            rv_typeref(*type_ref),
                            RuntimeValue::String(name.clone()),
                        ]);
                    }
                }
                Some(rows)
            }
            "ty_unknown" => {
                let mut rows = Vec::new();
                for (type_ref, shape) in &self.type_shapes {
                    if matches!(shape, TypeShape::Unknown) {
                        rows.push(vec![rv_typeref(*type_ref)]);
                    }
                }
                Some(rows)
            }
            "node_at" => {
                let mut rows = Vec::new();
                for span in self.known_spans() {
                    rows.push(vec![rv_span(span), rv_option_node(self.node_at(span))]);
                }
                Some(rows)
            }
            "node_kind" => {
                let mut rows = Vec::new();
                for (node, record) in &self.node_records {
                    rows.push(vec![rv_node(*node), rv_node_kind(record.kind)]);
                }
                Some(rows)
            }
            "node_span" => {
                let mut rows = Vec::new();
                for (node, record) in &self.node_records {
                    rows.push(vec![rv_node(*node), rv_span(record.span)]);
                }
                Some(rows)
            }
            "node_parent" => {
                let mut rows = Vec::new();
                for (node, record) in &self.node_records {
                    rows.push(vec![rv_node(*node), rv_option_node(record.parent)]);
                }
                Some(rows)
            }
            "enclosing_control" => {
                let mut rows = Vec::new();
                for span in self.known_spans() {
                    let Some(control) = self.enclosing_control(span) else {
                        continue;
                    };
                    rows.push(vec![
                        rv_span(span),
                        rv_node_kind(control.kind),
                        rv_span(control.span),
                        RuntimeValue::Int(control.distance as i64),
                    ]);
                }
                Some(rows)
            }
            "handle" => {
                Some(self.rows_for_known_defs(|def| handle_relation_row(def, self.handle(def))))
            }
            "span_key" => {
                let mut rows = Vec::new();
                for span in self.known_spans() {
                    let key = self
                        .span_keys
                        .get(&span)
                        .cloned()
                        .unwrap_or_else(|| deterministic_span_key(span));
                    rows.push(vec![
                        rv_span(span),
                        RuntimeValue::String(key.rel_path().to_string()),
                        RuntimeValue::Int(key.start().line() as i64),
                        RuntimeValue::Int(key.start().column() as i64),
                        RuntimeValue::Int(key.end().line() as i64),
                        RuntimeValue::Int(key.end().column() as i64),
                    ]);
                }
                Some(rows)
            }
            "typeref_id" => {
                let mut refs = self.known_typerefs();
                refs.extend(
                    self.known_functions()
                        .into_iter()
                        .map(|function| self.fn_return_for(function)),
                );
                let mut rows = Vec::new();
                for type_ref in refs {
                    rows.push(vec![
                        rv_typeref(type_ref),
                        RuntimeValue::String(self.typeref_id(type_ref).as_str().to_string()),
                    ]);
                }
                Some(rows)
            }
            "node_id" => {
                let mut nodes = BTreeSet::new();
                nodes.extend(self.node_keys.keys().copied());
                nodes.extend(self.node_records.keys().copied());
                let mut rows = Vec::new();
                for node in nodes {
                    rows.push(vec![
                        rv_node(node),
                        RuntimeValue::String(self.node_id(node).as_str().to_string()),
                    ]);
                }
                Some(rows)
            }
            "call_id" => {
                let mut rows = Vec::new();
                for (call, handle) in &self.call_keys {
                    rows.push(vec![
                        rv_call(*call),
                        RuntimeValue::String(handle.as_str().to_string()),
                    ]);
                }
                Some(rows)
            }
            "ref_id" => {
                let mut rows = Vec::new();
                for (r#ref, handle) in &self.ref_keys {
                    rows.push(vec![
                        rv_ref(*r#ref),
                        RuntimeValue::String(handle.as_str().to_string()),
                    ]);
                }
                Some(rows)
            }
            "impl_id" => {
                let mut rows = Vec::new();
                for (r#impl, handle) in &self.impl_keys {
                    rows.push(vec![
                        rv_impl(*r#impl),
                        RuntimeValue::String(handle.as_str().to_string()),
                    ]);
                }
                Some(rows)
            }
            _ => None,
        }
    }

    pub(crate) fn extern_relation_rows_for_predicate_result(
        &self,
        predicate: &str,
    ) -> Result<Option<Vec<Vec<RuntimeValue>>>, RaHostError> {
        if let Some(error) = self.extern_relation_errors.get(predicate) {
            return Err(error.clone());
        }
        Ok(self.extern_relation_rows_for_predicate(predicate))
    }

    fn record_runtime_note(&mut self, note: String) {
        self.runtime_notes.insert(note);
    }

    fn drain_runtime_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.runtime_notes)
            .into_iter()
            .collect()
    }
}

/// rust-analyzer-backed runtime adapter with optional workspace hot reload,
/// delegating host-contract behavior to [`DeterministicRaHost`].
#[derive(Debug)]
pub struct RaHostRuntime {
    analysis: Analysis,
    host: DeterministicRaHost,
    _proc_macro_client: Option<Box<dyn workspace_loader::ProcMacroClientHandle>>,
    workspace_root: Option<PathBuf>,
    manifest_path: Option<PathBuf>,
    init_mode: WorkspaceInitMode,
    hot_reload: HotReloadState,
}

#[derive(Debug, Clone)]
struct HotReloadState {
    enabled: bool,
    poll_interval: Duration,
    next_poll_after: Instant,
    workspace_fingerprint: Option<u64>,
    tracked_paths: Vec<PathBuf>,
}

impl HotReloadState {
    fn disabled() -> Self {
        Self {
            enabled: false,
            poll_interval: Duration::from_millis(750),
            next_poll_after: Instant::now(),
            workspace_fingerprint: None,
            tracked_paths: Vec::new(),
        }
    }
}

impl RaHostRuntime {
    pub(crate) fn new(analysis: Analysis, world_stamp: impl Into<WorldStamp>) -> Self {
        let mut host = DeterministicRaHost::new();
        host.set_world_stamp(world_stamp);
        Self {
            analysis,
            host,
            _proc_macro_client: None,
            workspace_root: None,
            manifest_path: None,
            init_mode: WorkspaceInitMode::Resilient,
            hot_reload: HotReloadState::disabled(),
        }
    }

    pub fn from_workspace_root(root: impl AsRef<Path>) -> Result<Self, RaHostInitError> {
        Self::from_workspace_root_with_mode(root, WorkspaceInitMode::Resilient)
    }

    pub fn from_workspace_root_with_mode(
        root: impl AsRef<Path>,
        mode: WorkspaceInitMode,
    ) -> Result<Self, RaHostInitError> {
        let loaded = workspace_loader::load_from_workspace_root(root.as_ref(), mode)?;
        Self::from_loaded_workspace(loaded, mode)
    }

    pub fn from_manifest_path(manifest: impl AsRef<Path>) -> Result<Self, RaHostInitError> {
        Self::from_manifest_path_with_mode(manifest, WorkspaceInitMode::Resilient)
    }

    pub fn from_manifest_path_with_mode(
        manifest: impl AsRef<Path>,
        mode: WorkspaceInitMode,
    ) -> Result<Self, RaHostInitError> {
        let loaded = workspace_loader::load_from_manifest_path(manifest.as_ref(), mode)?;
        Self::from_loaded_workspace(loaded, mode)
    }

    fn from_loaded_workspace(
        loaded: workspace_loader::LoadedWorkspace,
        mode: WorkspaceInitMode,
    ) -> Result<Self, RaHostInitError> {
        let workspace_loader::LoadedWorkspace {
            manifest_path,
            workspace_root,
            db,
            vfs,
            init_notes,
            proc_macro_client,
        } = loaded;
        let tracked_paths =
            collect_reload_tracked_paths(&vfs, manifest_path.as_path(), workspace_root.as_path());
        let host = workspace_snapshot::build_host_snapshot(&db, &vfs, workspace_root.as_path())?;
        let world_stamp =
            HostRuntime::world_stamp(&host).map_err(|err| RaHostInitError::SemanticBuild {
                details: err.to_string(),
            })?;
        let analysis = AnalysisHost::with_database(db).analysis();
        let mut runtime = Self::new(analysis, world_stamp);
        runtime.host = host;
        runtime._proc_macro_client = proc_macro_client;
        runtime.workspace_root = Some(workspace_root.clone());
        runtime.manifest_path = Some(manifest_path);
        runtime.init_mode = mode;
        runtime.hot_reload = HotReloadState {
            enabled: true,
            poll_interval: Duration::from_millis(750),
            next_poll_after: Instant::now() + Duration::from_millis(750),
            workspace_fingerprint: Some(workspace_reload_fingerprint(
                workspace_root.as_path(),
                tracked_paths.as_slice(),
            )),
            tracked_paths,
        };
        for note in init_notes {
            runtime.host.record_runtime_note(note);
        }
        Ok(runtime)
    }

    fn analysis(&self) -> &Analysis {
        &self.analysis
    }

    fn host(&self) -> &DeterministicRaHost {
        &self.host
    }

    fn host_mut(&mut self) -> &mut DeterministicRaHost {
        &mut self.host
    }

    pub fn analysis_status_ok(&self) -> bool {
        self.analysis().status(None).is_ok()
    }

    pub fn set_hot_reload_enabled(&mut self, enabled: bool) {
        self.hot_reload.enabled = enabled;
        self.hot_reload.next_poll_after = Instant::now();
        if enabled && self.hot_reload.workspace_fingerprint.is_none() {
            self.hot_reload.workspace_fingerprint = self
                .workspace_root
                .as_ref()
                .map(|root| {
                    workspace_reload_fingerprint(
                        root.as_path(),
                        self.hot_reload.tracked_paths.as_slice(),
                    )
                });
        }
    }

    pub fn set_hot_reload_poll_interval(&mut self, interval: Duration) {
        self.hot_reload.poll_interval = interval;
        self.hot_reload.next_poll_after = Instant::now() + interval;
    }

    pub fn reload_now(&mut self) -> Result<(), RaHostInitError> {
        let loaded = self.load_workspace_for_reload()?;
        self.apply_loaded_workspace(loaded)?;
        self.hot_reload.next_poll_after = Instant::now() + self.hot_reload.poll_interval;
        Ok(())
    }

    pub fn maybe_reload(&mut self) -> Result<bool, RaHostInitError> {
        if !self.hot_reload.enabled {
            return Ok(false);
        }
        if Instant::now() < self.hot_reload.next_poll_after {
            return Ok(false);
        }

        self.hot_reload.next_poll_after = Instant::now() + self.hot_reload.poll_interval;
        let Some(workspace_root) = self.workspace_root.as_ref() else {
            return Ok(false);
        };
        let fingerprint = workspace_reload_fingerprint(
            workspace_root.as_path(),
            self.hot_reload.tracked_paths.as_slice(),
        );
        if self.hot_reload.workspace_fingerprint == Some(fingerprint) {
            return Ok(false);
        }

        self.reload_now()?;
        self.hot_reload.workspace_fingerprint = Some(fingerprint);
        Ok(true)
    }

    fn load_workspace_for_reload(&self) -> Result<workspace_loader::LoadedWorkspace, RaHostInitError> {
        if let Some(manifest_path) = self.manifest_path.as_ref() {
            return workspace_loader::load_from_manifest_path(manifest_path.as_path(), self.init_mode);
        }
        if let Some(workspace_root) = self.workspace_root.as_ref() {
            return workspace_loader::load_from_workspace_root(workspace_root.as_path(), self.init_mode);
        }
        Err(RaHostInitError::SemanticBuild {
            details: "runtime reload unavailable without workspace initialization context".to_string(),
        })
    }

    fn apply_loaded_workspace(
        &mut self,
        loaded: workspace_loader::LoadedWorkspace,
    ) -> Result<(), RaHostInitError> {
        let runtime_scalar_options = self.host.runtime_scalar_options;
        let scalar_inputs = self.host.scalar_inputs.clone();
        let stable_overrides = self.host.stable_overrides.clone();
        let control_max_depth = self.host.control_max_depth;
        let runtime_notes = self.host.runtime_notes.clone();

        let workspace_loader::LoadedWorkspace {
            manifest_path,
            workspace_root,
            db,
            vfs,
            init_notes,
            proc_macro_client,
        } = loaded;
        let tracked_paths =
            collect_reload_tracked_paths(&vfs, manifest_path.as_path(), workspace_root.as_path());

        let mut host = workspace_snapshot::build_host_snapshot(&db, &vfs, workspace_root.as_path())?;
        let _ = HostRuntime::world_stamp(&host).map_err(|err| RaHostInitError::SemanticBuild {
            details: err.to_string(),
        })?;
        host.runtime_scalar_options = runtime_scalar_options;
        host.scalar_inputs = scalar_inputs;
        host.stable_overrides = stable_overrides;
        host.control_max_depth = control_max_depth;
        host.runtime_notes = runtime_notes;
        for note in init_notes {
            host.record_runtime_note(note);
        }

        self.analysis = AnalysisHost::with_database(db).analysis();
        self.host = host;
        self._proc_macro_client = proc_macro_client;
        self.workspace_root = Some(workspace_root.clone());
        self.manifest_path = Some(manifest_path);
        self.hot_reload.tracked_paths = tracked_paths;
        self.hot_reload.workspace_fingerprint = if self.hot_reload.enabled {
            Some(workspace_reload_fingerprint(
                workspace_root.as_path(),
                self.hot_reload.tracked_paths.as_slice(),
            ))
        } else {
            None
        };
        Ok(())
    }

    fn auto_reload_if_needed(&mut self) {
        match self.maybe_reload() {
            Ok(true) => {}
            Ok(false) => {}
            Err(err) => {
                self.hot_reload.enabled = false;
                self.host_mut().record_runtime_note(format!(
                    "workspace hot reload disabled after failure: {err}"
                ));
            }
        }
    }

    pub fn insert_handle(&mut self, def: DefId, handle: impl Into<StableHandle>) {
        self.host_mut().insert_handle(def, handle);
    }

    pub fn insert_type(&mut self, type_ref: TypeRefId, shape: TypeShape) {
        self.host_mut().insert_type(type_ref, shape);
    }

    pub fn insert_span(&mut self, span: SpanId, key: SpanKey) {
        self.host_mut().insert_span(span, key);
    }

    pub fn insert_node(
        &mut self,
        node: NodeId,
        kind: NodeKind,
        span: SpanId,
        parent: Option<NodeId>,
    ) {
        self.host_mut().insert_node(node, kind, span, parent);
    }

    pub fn set_world_stamp(&mut self, stamp: impl Into<WorldStamp>) {
        self.host_mut().set_world_stamp(stamp);
    }

    pub fn set_runtime_scalar_options(&mut self, options: RuntimeScalarOptions) {
        self.host_mut().set_runtime_scalar_options(options);
    }

    pub fn insert_scalar_input(&mut self, key: ScalarInputKey, value: ScalarValue) {
        self.host_mut().insert_scalar_input(key, value);
    }

    pub fn insert_stable_id_override(
        &mut self,
        namespace: impl Into<String>,
        logical_name: impl Into<String>,
        id: StableId,
    ) {
        self.host_mut()
            .insert_stable_id_override(namespace, logical_name, id);
    }

    pub fn intern_def_from_module_def(&mut self, def: ModuleDefId) -> DefId {
        let token = format!("{def:?}");
        self.host_mut().intern_def_from_token(token.as_str())
    }

    pub fn intern_def_from_body_def(&mut self, def: DefWithBodyId) -> DefId {
        let token = format!("{def:?}");
        self.host_mut().intern_def_from_token(token.as_str())
    }

    pub fn intern_typeref_from_token<T: fmt::Debug>(&mut self, token: &T) -> TypeRefId {
        self.host_mut().intern_typeref_from_token(token)
    }

    pub fn intern_node_from_syntax_ptr(&mut self, node: SyntaxNodePtr) -> NodeId {
        self.host_mut().intern_node_from_syntax_ptr(node)
    }

    pub fn intern_call_from_macro_call(&mut self, call: MacroCallId) -> CallId {
        self.host_mut().intern_call_from_macro_call(call)
    }

    pub fn intern_ref_from_token<T: fmt::Debug>(&mut self, token: &T) -> RefId {
        self.host_mut().intern_ref_from_token(token)
    }

    pub fn intern_impl_from_hir_impl(&mut self, impl_id: HirImplId) -> ImplId {
        self.host_mut().intern_impl_from_hir_impl(impl_id)
    }

    pub fn intern_span_from_text(
        &mut self,
        file_id: EditionedFileId,
        rel_path: impl Into<Box<str>>,
        source_text: &str,
        range: TextRange,
    ) -> Result<SpanId, RaHostError> {
        self.host_mut()
            .intern_span_from_text(file_id, rel_path, source_text, range)
    }

    pub fn parse_rust_and_intern_root_node(&mut self, text: &str, edition: Edition) -> NodeId {
        self.host_mut()
            .parse_rust_and_intern_root_node(text, edition)
    }
}

impl HostRuntime for RaHostRuntime {
    type Error = RaHostError;

    fn stable_id(&self, namespace: &str, logical_name: &str) -> Result<StableId, Self::Error> {
        self.host().stable_id(namespace, logical_name)
    }

    fn world_stamp(&self) -> Result<WorldStamp, Self::Error> {
        self.host().world_stamp()
    }

    fn handle(&self, def: DefId) -> Result<StableHandle, Self::Error> {
        self.host().handle(def)
    }

    fn span_key(&self, span: SpanId) -> Result<SpanKey, Self::Error> {
        self.host().span_key(span)
    }

    fn typeref_id(&self, type_ref: TypeRefId) -> Result<StableHandle, Self::Error> {
        Ok(self.host().typeref_id(type_ref))
    }

    fn node_id(&self, node: NodeId) -> Result<StableHandle, Self::Error> {
        Ok(self.host().node_id(node))
    }

    fn call_id(&self, call: CallId) -> Result<StableHandle, Self::Error> {
        self.host().call_id(call)
    }

    fn ref_id(&self, r#ref: RefId) -> Result<StableHandle, Self::Error> {
        self.host().ref_id(r#ref)
    }

    fn impl_id(&self, r#impl: ImplId) -> Result<StableHandle, Self::Error> {
        self.host().impl_id(r#impl)
    }

    fn runtime_scalar_options(&self) -> Result<RuntimeScalarOptions, Self::Error> {
        self.host().runtime_scalar_options()
    }

    fn scalar_input(&self, key: &ScalarInputKey) -> Result<Option<ScalarValue>, Self::Error> {
        self.host().scalar_input(key)
    }
}

impl EngineHostView for DeterministicRaHost {
    fn world_stamp(&mut self) -> String {
        match HostRuntime::world_stamp(self) {
            Ok(stamp) => stamp.as_str().to_string(),
            Err(err) => format!("ra-host:error:{err}"),
        }
    }

    fn stable_key(&mut self, value: &RuntimeValue) -> String {
        let resolution = runtime_value_stable_key_with_runtime(self, value);
        for note in resolution.runtime_notes {
            self.record_runtime_note(note);
        }
        resolution.key
    }

    fn take_runtime_notes(&mut self) -> Vec<String> {
        self.drain_runtime_notes()
    }

    fn set_control_max_depth(&mut self, depth: i64) {
        self.set_control_max_depth(depth.max(0) as u32);
    }

    fn extern_relation_rows(
        &mut self,
        predicate: &str,
    ) -> Result<Option<Vec<Vec<RuntimeValue>>>, EngineHostError> {
        self.extern_relation_rows_for_predicate_result(predicate)
            .map_err(|error| {
                EngineHostError::new(
                    "ra_host.extern_relation_rows_for_predicate",
                    error.to_string(),
                )
            })
    }
}

impl EngineHostView for RaHostRuntime {
    fn world_stamp(&mut self) -> String {
        self.auto_reload_if_needed();
        match HostRuntime::world_stamp(self) {
            Ok(stamp) => stamp.as_str().to_string(),
            Err(err) => format!("ra-host:error:{err}"),
        }
    }

    fn stable_key(&mut self, value: &RuntimeValue) -> String {
        self.auto_reload_if_needed();
        let resolution = runtime_value_stable_key_with_runtime(self, value);
        for note in resolution.runtime_notes {
            self.host_mut().record_runtime_note(note);
        }
        resolution.key
    }

    fn take_runtime_notes(&mut self) -> Vec<String> {
        self.host_mut().drain_runtime_notes()
    }

    fn set_control_max_depth(&mut self, depth: i64) {
        self.host_mut().set_control_max_depth(depth.max(0) as u32);
    }

    fn extern_relation_rows(
        &mut self,
        predicate: &str,
    ) -> Result<Option<Vec<Vec<RuntimeValue>>>, EngineHostError> {
        self.auto_reload_if_needed();
        self.host_mut()
            .extern_relation_rows_for_predicate_result(predicate)
            .map_err(|error| {
                EngineHostError::new(
                    "ra_host.extern_relation_rows_for_predicate",
                    error.to_string(),
                )
            })
    }
}

fn deterministic_span_key(span: SpanId) -> SpanKey {
    let raw = span.stable_id().as_u64();
    let rel_path = format!("ra/fallback/{raw:016x}.rs");
    let l0 = ((raw & 0xff) as u32) + 1;
    let c0 = ((raw >> 8) as u32) & 0x7f;
    let l1 = l0 + (((raw >> 16) as u32) & 0x07);
    let c1 = if l0 == l1 {
        c0 + (((raw >> 24) as u32) & 0x1f) + 1
    } else {
        ((raw >> 32) as u32) & 0x7f
    };
    SpanKey::new(rel_path, SpanCoord::new(l0, c0), SpanCoord::new(l1, c1))
}

fn deterministic_stable_id(namespace: &str, logical_name: &str) -> StableId {
    let mut hasher = FxHasher::default();
    namespace.hash(&mut hasher);
    0xff_u8.hash(&mut hasher);
    logical_name.hash(&mut hasher);
    StableId::new(hasher.finish())
}

fn collect_reload_tracked_paths(
    vfs: &vfs::Vfs,
    manifest_path: &Path,
    workspace_root: &Path,
) -> Vec<PathBuf> {
    let mut tracked = BTreeSet::new();

    tracked.insert(manifest_path.to_path_buf());
    tracked.insert(workspace_root.join("Cargo.lock"));
    tracked.insert(workspace_root.join("rust-toolchain"));
    tracked.insert(workspace_root.join("rust-toolchain.toml"));

    for (_, path) in vfs.iter() {
        let Some(abs) = path.as_path() else {
            continue;
        };
        let as_path = Path::new(abs.as_str());
        if !as_path.starts_with(workspace_root) || !should_track_reload_file(as_path) {
            continue;
        }
        tracked.insert(as_path.to_path_buf());
    }

    tracked.into_iter().collect()
}

fn workspace_reload_fingerprint(workspace_root: &Path, tracked_paths: &[PathBuf]) -> u64 {
    let mut hasher = FxHasher::default();
    for path in tracked_paths {
        hash_reload_path_metadata(&mut hasher, workspace_root, path.as_path());
    }
    tracked_paths.len().hash(&mut hasher);
    hasher.finish()
}

fn should_track_reload_file(path: &Path) -> bool {
    let file_name = path.file_name().and_then(|name| name.to_str());
    if file_name.is_some_and(|name| {
        matches!(
            name,
            "Cargo.toml" | "Cargo.lock" | "build.rs" | "rust-toolchain" | "rust-toolchain.toml"
        )
    }) {
        return true;
    }

    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "rs" | "toml"))
}

fn hash_reload_path_metadata(
    hasher: &mut FxHasher,
    workspace_root: &Path,
    path: &Path,
) {
    let rel_path = path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    rel_path.hash(hasher);

    match std::fs::metadata(path) {
        Ok(metadata) => {
            metadata.len().hash(hasher);
            if let Ok(modified) = metadata.modified()
                && let Ok(duration) = modified.duration_since(UNIX_EPOCH)
            {
                duration.as_secs().hash(hasher);
                duration.subsec_nanos().hash(hasher);
            }
        }
        Err(err) => {
            "metadata_unavailable".hash(hasher);
            err.kind().hash(hasher);
        }
    }
}

fn rv_def(def: DefId) -> RuntimeValue {
    RuntimeValue::Host {
        kind: HostValueKind::Def,
        id: def.stable_id().as_u64(),
    }
}

fn fallback_def_name(def: DefId) -> String {
    format!("def_{:016x}", def.stable_id().as_u64())
}

fn fallback_def_path(def: DefId) -> String {
    format!("raql::fallback::{}", fallback_def_name(def))
}

fn fallback_def_span(def: DefId) -> SpanId {
    SpanId::new(deterministic_stable_id(
        "def_span",
        format!("{:#018x}", def.stable_id().as_u64()).as_str(),
    ))
}

fn fallback_fn_return_typeref(function: DefId) -> TypeRefId {
    TypeRefId::new(deterministic_stable_id(
        "fn_return",
        format!("{:#018x}", function.stable_id().as_u64()).as_str(),
    ))
}

fn fallback_handle_text(def: DefId) -> String {
    format!("ra-host:fallback:handle:{:#018x}", def.stable_id().as_u64())
}

fn fallback_def_handle(def: DefId) -> StableHandle {
    StableHandle::new(fallback_handle_text(def))
}

fn error_provenance(error: &RaHostError) -> String {
    match error {
        RaHostError::InvalidSpanRange {
            rel_path,
            start,
            end,
            len,
        } => format!("invalid_span_range:{rel_path}:{start}:{end}:{len}"),
    }
}

fn fallback_handle_text_with_error(def: DefId, error: &RaHostError) -> String {
    format!(
        "{}|ra-host:error:{}",
        fallback_handle_text(def),
        error_provenance(error)
    )
}

fn fallback_span_key_text(span: SpanId) -> String {
    span_key_stable_text(&deterministic_span_key(span))
}

fn fallback_span_key_text_with_error(span: SpanId, error: &RaHostError) -> String {
    format!(
        "{}|ra-host:error:{}",
        fallback_span_key_text(span),
        error_provenance(error)
    )
}

fn handle_relation_row(
    def: DefId,
    resolved: Result<StableHandle, RaHostError>,
) -> Vec<RuntimeValue> {
    let handle = match resolved {
        Ok(handle) => handle,
        Err(error) => StableHandle::new(fallback_handle_text_with_error(def, &error)),
    };
    vec![
        rv_def(def),
        RuntimeValue::String(handle.as_str().to_string()),
    ]
}

fn rv_span(span: SpanId) -> RuntimeValue {
    RuntimeValue::Host {
        kind: HostValueKind::Span,
        id: span.stable_id().as_u64(),
    }
}

fn rv_typeref(type_ref: TypeRefId) -> RuntimeValue {
    RuntimeValue::Host {
        kind: HostValueKind::TypeRef,
        id: type_ref.stable_id().as_u64(),
    }
}

fn rv_node(node: NodeId) -> RuntimeValue {
    RuntimeValue::Host {
        kind: HostValueKind::Node,
        id: node.stable_id().as_u64(),
    }
}

fn rv_call(call: CallId) -> RuntimeValue {
    RuntimeValue::Host {
        kind: HostValueKind::Call,
        id: call.stable_id().as_u64(),
    }
}

fn rv_ref(r#ref: RefId) -> RuntimeValue {
    RuntimeValue::Host {
        kind: HostValueKind::Ref,
        id: r#ref.stable_id().as_u64(),
    }
}

fn rv_impl(r#impl: ImplId) -> RuntimeValue {
    RuntimeValue::Host {
        kind: HostValueKind::Impl,
        id: r#impl.stable_id().as_u64(),
    }
}

fn rv_option_node(node: Option<NodeId>) -> RuntimeValue {
    match node {
        Some(node) => RuntimeValue::Some(Box::new(rv_node(node))),
        None => RuntimeValue::None,
    }
}

fn rv_option_def(def: Option<DefId>) -> RuntimeValue {
    match def {
        Some(def) => RuntimeValue::Some(Box::new(rv_def(def))),
        None => RuntimeValue::None,
    }
}

fn rv_option_string(value: Option<&str>) -> RuntimeValue {
    match value {
        Some(value) => RuntimeValue::Some(Box::new(RuntimeValue::String(value.to_string()))),
        None => RuntimeValue::None,
    }
}

fn rv_dispatch_kind(dispatch: DispatchKind) -> RuntimeValue {
    let variant = match dispatch {
        DispatchKind::Direct => "DIRECT",
        DispatchKind::ThroughTrait => "THROUGH_TRAIT",
        DispatchKind::Dyn => "DYN",
        DispatchKind::Closure => "CLOSURE",
        DispatchKind::FnPointer => "FN_POINTER",
    };
    RuntimeValue::Enum {
        name: "DispatchKind".to_string(),
        variant: variant.to_string(),
    }
}

fn rv_def_kind(kind: DefKind) -> RuntimeValue {
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
    RuntimeValue::Enum {
        name: "DefKind".to_string(),
        variant: variant.to_string(),
    }
}

fn rv_mutability(mutability: Mutability) -> RuntimeValue {
    let variant = match mutability {
        Mutability::Shared => "IMM",
        Mutability::Mut => "MUT",
    };
    RuntimeValue::Enum {
        name: "Mutability".to_string(),
        variant: variant.to_string(),
    }
}

fn rv_node_kind(kind: NodeKind) -> RuntimeValue {
    let variant = match kind {
        NodeKind::If => "IF",
        NodeKind::Match => "MATCH",
        NodeKind::While => "WHILE",
        NodeKind::For => "FOR",
        NodeKind::Loop => "LOOP",
        NodeKind::Block => "BLOCK",
        NodeKind::Try => "TRY",
        NodeKind::Arm => "ARM",
        NodeKind::Other | NodeKind::Expr | NodeKind::Item => "OTHER",
    };
    RuntimeValue::Enum {
        name: "NodeKind".to_string(),
        variant: variant.to_string(),
    }
}

fn span_key_stable_text(key: &SpanKey) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        key.rel_path(),
        key.start().line(),
        key.start().column(),
        key.end().line(),
        key.end().column()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StableKeyResolution {
    key: String,
    runtime_notes: Vec<String>,
}

impl StableKeyResolution {
    fn plain(key: String) -> Self {
        Self {
            key,
            runtime_notes: Vec::new(),
        }
    }

    fn with_note(key: String, note: String) -> Self {
        Self {
            key,
            runtime_notes: vec![note],
        }
    }
}

fn span_key_anchor_text(key: &SpanKey) -> String {
    format!(
        "{}:{}:{}-{}:{}",
        key.rel_path(),
        key.start().line() + 1,
        key.start().column() + 1,
        key.end().line() + 1,
        key.end().column() + 1
    )
}

fn stable_key_hint(kind: HostValueKind, id: u64) -> String {
    let sid = StableId::new(id);
    match kind {
        HostValueKind::Def => {
            format!("handle hint `{}`", fallback_handle_text(DefId::new(sid)))
        }
        HostValueKind::Span => {
            let key = deterministic_span_key(SpanId::new(sid));
            format!(
                "span hint `{}` (anchor `{}`)",
                span_key_stable_text(&key),
                span_key_anchor_text(&key)
            )
        }
        HostValueKind::TypeRef => format!("label hint `typeref:{id:#018x}`"),
        HostValueKind::Node => format!("label hint `node:{id:#018x}`"),
        HostValueKind::Call => format!("label hint `call:{id:#018x}`"),
        HostValueKind::Ref => format!("label hint `ref:{id:#018x}`"),
        HostValueKind::Impl => format!("label hint `impl:{id:#018x}`"),
    }
}

fn runtime_error_source_hint(error: &RaHostError) -> String {
    match error {
        RaHostError::InvalidSpanRange {
            rel_path,
            start,
            end,
            len,
        } => format!("host source `{rel_path}` bytes {start}..{end} (len {len})"),
    }
}

fn stable_key_fallback_note(
    kind: HostValueKind,
    id: u64,
    operation: &str,
    fallback_key: &str,
    error: &RaHostError,
) -> String {
    let hint = stable_key_hint(kind, id);
    let source_hint = runtime_error_source_hint(error);
    format!(
        "host stable-key fallback for `{}` value `{id:#018x}`: `{operation}` failed ({error}); {hint}; {source_hint}; using deterministic fallback key `{fallback_key}`.",
        kind.label()
    )
}

fn runtime_value_stable_key_with_runtime(
    runtime: &impl HostRuntime<Error = RaHostError>,
    value: &RuntimeValue,
) -> StableKeyResolution {
    match value {
        RuntimeValue::Int(v) => StableKeyResolution::plain(format!("i:{v:020}")),
        RuntimeValue::String(v) => StableKeyResolution::plain(format!("s:{v}")),
        RuntimeValue::Bool(v) => {
            StableKeyResolution::plain(format!("b:{}", if *v { 1 } else { 0 }))
        }
        RuntimeValue::Enum { name, variant } => {
            StableKeyResolution::plain(format!("e:{name}::{variant}"))
        }
        RuntimeValue::Host { kind, id } => {
            let sid = StableId::new(*id);
            match kind {
                HostValueKind::Def => match runtime.handle(DefId::new(sid)) {
                    Ok(handle) => StableKeyResolution::plain(handle.as_str().to_string()),
                    Err(error) => {
                        let fallback_key = fallback_handle_text_with_error(DefId::new(sid), &error);
                        StableKeyResolution::with_note(
                            fallback_key.clone(),
                            stable_key_fallback_note(*kind, *id, "handle/2", &fallback_key, &error),
                        )
                    }
                },
                HostValueKind::Span => match runtime.span_key(SpanId::new(sid)) {
                    Ok(key) => StableKeyResolution::plain(span_key_stable_text(&key)),
                    Err(error) => {
                        let fallback_key =
                            fallback_span_key_text_with_error(SpanId::new(sid), &error);
                        StableKeyResolution::with_note(
                            fallback_key.clone(),
                            stable_key_fallback_note(
                                *kind,
                                *id,
                                "span_key/6",
                                &fallback_key,
                                &error,
                            ),
                        )
                    }
                },
                HostValueKind::TypeRef => match runtime.typeref_id(TypeRefId::new(sid)) {
                    Ok(handle) => StableKeyResolution::plain(handle.as_str().to_string()),
                    Err(error) => {
                        let fallback_key = format!("typeref:{id:#018x}");
                        StableKeyResolution::with_note(
                            fallback_key.clone(),
                            stable_key_fallback_note(
                                *kind,
                                *id,
                                "typeref_id/2",
                                &fallback_key,
                                &error,
                            ),
                        )
                    }
                },
                HostValueKind::Node => match runtime.node_id(NodeId::new(sid)) {
                    Ok(handle) => StableKeyResolution::plain(handle.as_str().to_string()),
                    Err(error) => {
                        let fallback_key = format!("node:{id:#018x}");
                        StableKeyResolution::with_note(
                            fallback_key.clone(),
                            stable_key_fallback_note(
                                *kind,
                                *id,
                                "node_id/2",
                                &fallback_key,
                                &error,
                            ),
                        )
                    }
                },
                HostValueKind::Call => match runtime.call_id(CallId::new(sid)) {
                    Ok(handle) => StableKeyResolution::plain(handle.as_str().to_string()),
                    Err(error) => {
                        let fallback_key = format!("call:{id:#018x}");
                        StableKeyResolution::with_note(
                            fallback_key.clone(),
                            stable_key_fallback_note(
                                *kind,
                                *id,
                                "call_id/2",
                                &fallback_key,
                                &error,
                            ),
                        )
                    }
                },
                HostValueKind::Ref => match runtime.ref_id(RefId::new(sid)) {
                    Ok(handle) => StableKeyResolution::plain(handle.as_str().to_string()),
                    Err(error) => {
                        let fallback_key = format!("ref:{id:#018x}");
                        StableKeyResolution::with_note(
                            fallback_key.clone(),
                            stable_key_fallback_note(*kind, *id, "ref_id/2", &fallback_key, &error),
                        )
                    }
                },
                HostValueKind::Impl => match runtime.impl_id(ImplId::new(sid)) {
                    Ok(handle) => StableKeyResolution::plain(handle.as_str().to_string()),
                    Err(error) => {
                        let fallback_key = format!("impl:{id:#018x}");
                        StableKeyResolution::with_note(
                            fallback_key.clone(),
                            stable_key_fallback_note(
                                *kind,
                                *id,
                                "impl_id/2",
                                &fallback_key,
                                &error,
                            ),
                        )
                    }
                },
            }
        }
        RuntimeValue::None => StableKeyResolution::plain("o:none".to_string()),
        RuntimeValue::Some(inner) => {
            let inner = runtime_value_stable_key_with_runtime(runtime, inner);
            StableKeyResolution {
                key: format!("o:some:{}", inner.key),
                runtime_notes: inner.runtime_notes,
            }
        }
        RuntimeValue::List(items) => {
            let mut rendered = Vec::new();
            let mut runtime_notes = Vec::new();
            for item in items {
                let item_resolution = runtime_value_stable_key_with_runtime(runtime, item);
                rendered.push(item_resolution.key);
                runtime_notes.extend(item_resolution.runtime_notes);
            }
            StableKeyResolution {
                key: format!("l:[{}]", rendered.join("\u{1f}")),
                runtime_notes,
            }
        }
    }
}

fn is_control_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::If | NodeKind::Match | NodeKind::While | NodeKind::For | NodeKind::Loop
    )
}

fn is_function_kind(kind: DefKind) -> bool {
    matches!(kind, DefKind::Fn | DefKind::Method)
}

fn stable_node_key(host: &DeterministicRaHost, node: NodeId) -> StableHandle {
    host.node_id(node)
}

fn span_specificity(span: &SpanKey) -> (u32, u32, u32, u32) {
    let start = span.start();
    let end = span.end();
    let line_width = end.line().saturating_sub(start.line());
    let col_width = if line_width == 0 {
        end.column().saturating_sub(start.column())
    } else {
        u32::MAX
    };
    (line_width, col_width, start.line(), start.column())
}

fn span_contains(container: &SpanKey, candidate: &SpanKey) -> bool {
    if container.rel_path() != candidate.rel_path() {
        return false;
    }
    coord_leq(container.start(), candidate.start()) && coord_leq(candidate.end(), container.end())
}

fn coord_leq(a: SpanCoord, b: SpanCoord) -> bool {
    (a.line(), a.column()) <= (b.line(), b.column())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{DeterministicRaHost, RaHostError, RaHostRuntime};
    use raql_engine::{EngineHostView, HostValueKind, RuntimeValue};
    use raql_host::{HostRuntime, RuntimeScalarOptions, ScalarInputKey, WorldStamp};
    use raql_ir::{ScalarValue, StableId};
    use span::{TextRange, TextSize};

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace(package_name: &str, lib_rs: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "raql-host-ra-lib-test-{package_name}-{}-{:#x}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create temp workspace src dir");
        fs::write(
            root.join("Cargo.toml"),
            format!(
                r#"[package]
name = "{package_name}"
version = "0.0.0"
edition = "2021"
"#
            ),
        )
        .expect("write temp Cargo.toml");
        fs::write(root.join("src/lib.rs"), lib_rs).expect("write temp src/lib.rs");
        root
    }

    fn injected_lookup_error() -> RaHostError {
        RaHostError::InvalidSpanRange {
            rel_path: "src/fail.rs".into(),
            start: 0,
            end: 8,
            len: 3,
        }
    }

    struct ErroringLookupRuntime {
        inner: DeterministicRaHost,
        runtime_notes: BTreeSet<String>,
    }

    impl Default for ErroringLookupRuntime {
        fn default() -> Self {
            Self {
                inner: DeterministicRaHost::new(),
                runtime_notes: BTreeSet::new(),
            }
        }
    }

    impl HostRuntime for ErroringLookupRuntime {
        type Error = RaHostError;

        fn stable_id(&self, namespace: &str, logical_name: &str) -> Result<StableId, Self::Error> {
            HostRuntime::stable_id(&self.inner, namespace, logical_name)
        }

        fn world_stamp(&self) -> Result<super::WorldStamp, Self::Error> {
            HostRuntime::world_stamp(&self.inner)
        }

        fn handle(&self, _def: super::DefId) -> Result<super::StableHandle, Self::Error> {
            Err(injected_lookup_error())
        }

        fn span_key(&self, _span: super::SpanId) -> Result<super::SpanKey, Self::Error> {
            Err(injected_lookup_error())
        }

        fn typeref_id(
            &self,
            type_ref: super::TypeRefId,
        ) -> Result<super::StableHandle, Self::Error> {
            HostRuntime::typeref_id(&self.inner, type_ref)
        }

        fn node_id(&self, node: super::NodeId) -> Result<super::StableHandle, Self::Error> {
            HostRuntime::node_id(&self.inner, node)
        }

        fn call_id(&self, call: super::CallId) -> Result<super::StableHandle, Self::Error> {
            HostRuntime::call_id(&self.inner, call)
        }

        fn ref_id(&self, r#ref: super::RefId) -> Result<super::StableHandle, Self::Error> {
            HostRuntime::ref_id(&self.inner, r#ref)
        }

        fn impl_id(&self, r#impl: super::ImplId) -> Result<super::StableHandle, Self::Error> {
            HostRuntime::impl_id(&self.inner, r#impl)
        }

        fn runtime_scalar_options(&self) -> Result<RuntimeScalarOptions, Self::Error> {
            HostRuntime::runtime_scalar_options(&self.inner)
        }

        fn scalar_input(&self, key: &ScalarInputKey) -> Result<Option<ScalarValue>, Self::Error> {
            HostRuntime::scalar_input(&self.inner, key)
        }
    }

    impl EngineHostView for ErroringLookupRuntime {
        fn world_stamp(&mut self) -> String {
            HostRuntime::world_stamp(&self.inner)
                .expect("world_stamp")
                .as_str()
                .to_string()
        }

        fn stable_key(&mut self, value: &RuntimeValue) -> String {
            let resolution = super::runtime_value_stable_key_with_runtime(self, value);
            self.runtime_notes.extend(resolution.runtime_notes);
            resolution.key
        }

        fn take_runtime_notes(&mut self) -> Vec<String> {
            std::mem::take(&mut self.runtime_notes)
                .into_iter()
                .collect()
        }
    }

    #[test]
    fn ra_runtime_exposes_analysis_snapshot() {
        let root = temp_workspace("runtime_snapshot", "pub fn marker() -> i32 { 1 }\n");
        let runtime = RaHostRuntime::from_workspace_root(root.as_path()).expect("runtime");
        assert!(
            runtime
                .world_stamp()
                .expect("stamp")
                .as_str()
                .starts_with("ra-workspace:")
        );
        assert!(runtime.analysis_status_ok());
    }

    #[test]
    fn span_registration_uses_ra_span_text_range() {
        let root = temp_workspace("span_registration", "pub fn marker() {}\n");
        let mut runtime = RaHostRuntime::from_workspace_root(root.as_path()).expect("runtime");
        let file = span::EditionedFileId::current_edition(vfs::FileId::from_raw(0));
        let range = TextRange::new(TextSize::from(0), TextSize::from(9));
        let span = runtime
            .intern_span_from_text(file, "src/main.rs", "fn main() {\n  let x = 1;\n}\n", range)
            .expect("span");
        let key = runtime.span_key(span).expect("span key");
        assert_eq!(key.rel_path(), "src/main.rs");
        assert_eq!(key.start().line(), 0);
        assert_eq!(key.end().line(), 0);
    }

    #[test]
    fn deterministic_host_matches_required_defaults() {
        let host = DeterministicRaHost::new();
        assert_eq!(host.control_max_depth(), 32);
        assert_eq!(
            host.typeref_id(super::TypeRefId::new(StableId::new(0x7)))
                .as_str(),
            "typeref_id:0x0000000000000007"
        );
        assert_eq!(
            host.node_id(super::NodeId::new(StableId::new(0x8)))
                .as_str(),
            "node_id:0x0000000000000008"
        );
    }

    #[test]
    fn handle_row_falls_back_instead_of_disappearing_on_single_error() {
        let ok_def = super::DefId::new(StableId::new(0x10));
        let failing_def = super::DefId::new(StableId::new(0x11));

        let mut rows = Vec::new();
        rows.push(super::handle_relation_row(
            ok_def,
            Ok(super::StableHandle::new("def://ok")),
        ));
        rows.push(super::handle_relation_row(
            failing_def,
            Err(injected_lookup_error()),
        ));

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1], RuntimeValue::String("def://ok".to_string()));
        assert_eq!(
            rows[1][1],
            RuntimeValue::String(
                "ra-host:fallback:handle:0x0000000000000011|ra-host:error:invalid_span_range:src/fail.rs:0:8:3"
                    .to_string()
            )
        );
    }

    #[test]
    fn stable_key_fallbacks_are_deterministic_and_visible() {
        let def = StableId::new(0x42);
        let span = StableId::new(0x43);
        let def_value = RuntimeValue::Host {
            kind: HostValueKind::Def,
            id: def.as_u64(),
        };
        let span_value = RuntimeValue::Host {
            kind: HostValueKind::Span,
            id: span.as_u64(),
        };

        let host_a = DeterministicRaHost::new();
        let host_b = DeterministicRaHost::new();
        let def_key_a = super::runtime_value_stable_key_with_runtime(&host_a, &def_value);
        let def_key_b = super::runtime_value_stable_key_with_runtime(&host_b, &def_value);
        let span_key_a = super::runtime_value_stable_key_with_runtime(&host_a, &span_value);
        let span_key_b = super::runtime_value_stable_key_with_runtime(&host_b, &span_value);

        assert_eq!(def_key_a.key.as_str(), def_key_b.key.as_str());
        assert_eq!(span_key_a.key.as_str(), span_key_b.key.as_str());
        assert_eq!(
            def_key_a.key.as_str(),
            "ra-host:fallback:handle:0x0000000000000042"
        );
        assert!(
            span_key_a
                .key
                .starts_with("ra/fallback/0000000000000043.rs:")
        );
        assert!(def_key_a.runtime_notes.is_empty());
        assert!(span_key_a.runtime_notes.is_empty());
    }

    #[test]
    fn stable_key_fallback_includes_error_provenance_when_lookup_fails() {
        let runtime = ErroringLookupRuntime::default();
        let def_value = RuntimeValue::Host {
            kind: HostValueKind::Def,
            id: 0x51,
        };
        let span_value = RuntimeValue::Host {
            kind: HostValueKind::Span,
            id: 0x52,
        };

        let def_key = super::runtime_value_stable_key_with_runtime(&runtime, &def_value);
        let span_key = super::runtime_value_stable_key_with_runtime(&runtime, &span_value);

        assert_eq!(
            def_key.key.as_str(),
            "ra-host:fallback:handle:0x0000000000000051|ra-host:error:invalid_span_range:src/fail.rs:0:8:3"
        );
        let expected_span_key = format!(
            "{}|ra-host:error:invalid_span_range:src/fail.rs:0:8:3",
            super::fallback_span_key_text(super::SpanId::new(StableId::new(0x52)))
        );
        assert_eq!(span_key.key.as_str(), expected_span_key.as_str());
        assert!(def_key.runtime_notes.iter().any(|note| {
            note.contains("host stable-key fallback for `Def` value `0x0000000000000051`")
                && note.contains("handle/2")
                && note.contains("handle hint `ra-host:fallback:handle:0x0000000000000051`")
                && note.contains("host source `src/fail.rs` bytes 0..8")
                && note.contains(
                    "using deterministic fallback key `ra-host:fallback:handle:0x0000000000000051|ra-host:error:invalid_span_range:src/fail.rs:0:8:3`"
                )
        }));
        assert!(span_key.runtime_notes.iter().any(|note| {
            note.contains("host stable-key fallback for `Span` value `0x0000000000000052`")
                && note.contains("span_key/6")
                && note.contains("span hint `")
                && note.contains("anchor `ra/fallback/")
                && note.contains("host source `src/fail.rs` bytes 0..8")
                && note.contains("using deterministic fallback key")
        }));
    }

    #[test]
    fn engine_host_view_exposes_stable_key_fallback_runtime_notes() {
        let mut runtime = ErroringLookupRuntime::default();
        let value = RuntimeValue::Host {
            kind: HostValueKind::Def,
            id: 0x61,
        };

        let key = EngineHostView::stable_key(&mut runtime, &value);
        assert!(key.contains("|ra-host:error:invalid_span_range:src/fail.rs:0:8:3"));

        let notes = EngineHostView::take_runtime_notes(&mut runtime);
        assert!(notes.iter().any(|note| {
            note.contains("host stable-key fallback for `Def` value `0x0000000000000061`")
                && note.contains("handle/2")
                && note.contains("handle hint `ra-host:fallback:handle:0x0000000000000061`")
                && note.contains("host source `src/fail.rs` bytes 0..8")
                && note.contains("using deterministic fallback key")
        }));
        assert!(EngineHostView::take_runtime_notes(&mut runtime).is_empty());
    }

    #[test]
    fn engine_host_view_extern_relation_boundary_distinguishes_error_from_no_data() {
        let mut host = DeterministicRaHost::new();
        host.insert_extern_relation_error("ty_app", injected_lookup_error());

        let err = EngineHostView::extern_relation_rows(&mut host, "ty_app").expect_err("error");
        assert_eq!(
            err.operation(),
            "ra_host.extern_relation_rows_for_predicate"
        );
        assert!(err.message().contains("span range 0..8 is invalid"));

        host.clear_extern_relation_error("ty_app");
        assert!(
            EngineHostView::extern_relation_rows(&mut host, "unknown_predicate")
                .expect("ok none")
                .is_none()
        );
    }

    #[test]
    fn span_key_relation_keeps_node_rows_with_fallback_keys() {
        let mut host = DeterministicRaHost::new();
        let span = super::SpanId::new(StableId::new(0x90));
        host.insert_node(
            super::NodeId::new(StableId::new(0x91)),
            super::NodeKind::Expr,
            span,
            None,
        );

        let rows = host
            .extern_relation_rows_for_predicate("span_key")
            .expect("span_key rows");
        let key = host.span_key(span).expect("fallback key");

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0],
            vec![
                RuntimeValue::Host {
                    kind: HostValueKind::Span,
                    id: span.stable_id().as_u64()
                },
                RuntimeValue::String(key.rel_path().to_string()),
                RuntimeValue::Int(key.start().line() as i64),
                RuntimeValue::Int(key.start().column() as i64),
                RuntimeValue::Int(key.end().line() as i64),
                RuntimeValue::Int(key.end().column() as i64),
            ]
        );
        assert!(key.rel_path().contains("/fallback/"));
    }

    #[test]
    fn scalar_options_and_inputs_are_reported() {
        let root = temp_workspace("scalar_options", "pub fn marker() {}\n");
        let mut runtime = RaHostRuntime::from_workspace_root(root.as_path()).expect("runtime");
        runtime.set_runtime_scalar_options(
            RuntimeScalarOptions::new()
                .with_path_limit(7)
                .with_path_max_depth(11)
                .with_max_iters(256),
        );
        runtime.set_world_stamp(WorldStamp::new("cfg:test"));

        let source = StableId::new(9);
        let key = ScalarInputKey::new(source, "path_limit");
        runtime.insert_scalar_input(key.clone(), ScalarValue::I64(7));

        assert_eq!(
            runtime.runtime_scalar_options().expect("opts").path_limit(),
            Some(7)
        );
        assert_eq!(
            runtime.scalar_input(&key).expect("scalar"),
            Some(ScalarValue::I64(7))
        );
        assert_eq!(runtime.world_stamp().expect("stamp").as_str(), "cfg:test");
    }
}
