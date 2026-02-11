//! Host boundary abstractions for RAQL.
#![forbid(unsafe_code)]

use std::{collections::BTreeMap, convert::Infallible, error::Error};

use raql_ir::{ScalarValue, StableId};

/// Key used to request a scalar input value from the host runtime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScalarInputKey {
    source: StableId,
    name: Box<str>,
}

impl ScalarInputKey {
    /// Creates a new scalar input key.
    pub fn new(source: StableId, name: impl Into<Box<str>>) -> Self {
        Self {
            source,
            name: name.into(),
        }
    }

    /// Returns the stable source ID.
    pub const fn source(&self) -> StableId {
        self.source
    }

    /// Returns the scalar input name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Stable textual key for host objects used by deterministic ordering.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StableHandle(Box<str>);

impl StableHandle {
    /// Creates a stable handle.
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// Returns the textual handle key.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for StableHandle {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for StableHandle {
    fn from(value: String) -> Self {
        Self::new(value.into_boxed_str())
    }
}

/// Stable identifier for the analysis world snapshot used during evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorldStamp(Box<str>);

impl WorldStamp {
    /// Creates a new world stamp.
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// Returns the stamp as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for WorldStamp {
    fn default() -> Self {
        Self::new("mock-world")
    }
}

impl From<&str> for WorldStamp {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for WorldStamp {
    fn from(value: String) -> Self {
        Self::new(value.into_boxed_str())
    }
}

macro_rules! stable_entity_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(StableId);

        impl $name {
            /// Creates a typed host-entity ID.
            pub const fn new(id: StableId) -> Self {
                Self(id)
            }

            /// Returns the underlying stable ID.
            pub const fn stable_id(self) -> StableId {
                self.0
            }
        }

        impl From<StableId> for $name {
            fn from(id: StableId) -> Self {
                Self::new(id)
            }
        }

        impl From<$name> for StableId {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

stable_entity_id!(
    /// Stable identifier of a host `Def` value.
    DefId
);
stable_entity_id!(
    /// Stable identifier of a host `Span` value.
    SpanId
);
stable_entity_id!(
    /// Stable identifier of a host `TypeRef` value.
    TypeRefId
);
stable_entity_id!(
    /// Stable identifier of a host `Node` value.
    NodeId
);
stable_entity_id!(
    /// Stable identifier of a host `Call` value.
    CallId
);
stable_entity_id!(
    /// Stable identifier of a host `Ref` value.
    RefId
);
stable_entity_id!(
    /// Stable identifier of a host `Impl` value.
    ImplId
);

/// 0-based line/column coordinate in a span key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpanCoord {
    line: u32,
    column: u32,
}

impl SpanCoord {
    /// Creates a line/column coordinate.
    pub const fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }

    /// Returns the line number.
    pub const fn line(self) -> u32 {
        self.line
    }

    /// Returns the column number.
    pub const fn column(self) -> u32 {
        self.column
    }
}

/// Stable ordering key for spans.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpanKey {
    rel_path: Box<str>,
    start: SpanCoord,
    end: SpanCoord,
}

impl SpanKey {
    /// Creates a span key tuple `(RelPath, L0, C0, L1, C1)`.
    pub fn new(rel_path: impl Into<Box<str>>, start: SpanCoord, end: SpanCoord) -> Self {
        Self {
            rel_path: rel_path.into(),
            start,
            end,
        }
    }

    /// Returns the relative path component.
    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }

    /// Returns the start coordinate.
    pub const fn start(&self) -> SpanCoord {
        self.start
    }

    /// Returns the end coordinate.
    pub const fn end(&self) -> SpanCoord {
        self.end
    }
}

/// Scalar inputs used by runtime semantics in the language spec.
///
/// `None` means the runtime should inject the spec default:
/// - `path_limit = 1`
/// - `path_max_depth = 8`
/// - `opt_max_iters = none`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuntimeScalarOptions {
    path_limit: Option<u32>,
    path_max_depth: Option<u32>,
    max_iters: Option<u32>,
}

impl RuntimeScalarOptions {
    /// Creates unset scalar options (runtime injects defaults).
    pub const fn new() -> Self {
        Self {
            path_limit: None,
            path_max_depth: None,
            max_iters: None,
        }
    }

    /// Sets `path_limit`.
    pub const fn with_path_limit(mut self, value: u32) -> Self {
        self.path_limit = Some(value);
        self
    }

    /// Sets `path_max_depth`.
    pub const fn with_path_max_depth(mut self, value: u32) -> Self {
        self.path_max_depth = Some(value);
        self
    }

    /// Sets `opt_max_iters = some(value)`.
    pub const fn with_max_iters(mut self, value: u32) -> Self {
        self.max_iters = Some(value);
        self
    }

    /// Returns `path_limit`.
    pub const fn path_limit(self) -> Option<u32> {
        self.path_limit
    }

    /// Returns `path_max_depth`.
    pub const fn path_max_depth(self) -> Option<u32> {
        self.path_max_depth
    }

    /// Returns `opt_max_iters`.
    pub const fn max_iters(self) -> Option<u32> {
        self.max_iters
    }
}

/// Runtime hooks used by RAQL planning and execution boundaries.
pub trait HostRuntime {
    /// Error type returned by runtime hooks.
    type Error: Error + Send + Sync + 'static;

    /// Returns a deterministic, stable ID for a logical entity in a namespace.
    fn stable_id(&self, namespace: &str, logical_name: &str) -> Result<StableId, Self::Error>;

    /// Returns the analysis world stamp (`world_stamp/1`).
    fn world_stamp(&self) -> Result<WorldStamp, Self::Error>;

    /// Returns the stable `handle/2` key for `Def`.
    fn handle(&self, def: DefId) -> Result<StableHandle, Self::Error>;

    /// Returns the stable `span_key/6` tuple key for `Span`.
    fn span_key(&self, span: SpanId) -> Result<SpanKey, Self::Error>;

    /// Returns the stable `typeref_id/2` key.
    fn typeref_id(&self, type_ref: TypeRefId) -> Result<StableHandle, Self::Error>;

    /// Returns the stable `node_id/2` key.
    fn node_id(&self, node: NodeId) -> Result<StableHandle, Self::Error>;

    /// Returns the stable `call_id/2` key.
    fn call_id(&self, call: CallId) -> Result<StableHandle, Self::Error>;

    /// Returns the stable `ref_id/2` key.
    fn ref_id(&self, r#ref: RefId) -> Result<StableHandle, Self::Error>;

    /// Returns the stable `impl_id/2` key.
    fn impl_id(&self, r#impl: ImplId) -> Result<StableHandle, Self::Error>;

    /// Returns scalar options used by runtime semantics (`path_limit`,
    /// `path_max_depth`, `opt_max_iters`).
    fn runtime_scalar_options(&self) -> Result<RuntimeScalarOptions, Self::Error> {
        Ok(RuntimeScalarOptions::default())
    }

    /// Resolves an arbitrary scalar input value from the host.
    fn scalar_input(&self, key: &ScalarInputKey) -> Result<Option<ScalarValue>, Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum StableKeyKind {
    Handle,
    TypeRefId,
    NodeId,
    CallId,
    RefId,
    ImplId,
}

impl StableKeyKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Handle => "handle",
            Self::TypeRefId => "typeref_id",
            Self::NodeId => "node_id",
            Self::CallId => "call_id",
            Self::RefId => "ref_id",
            Self::ImplId => "impl_id",
        }
    }
}

/// Deterministic in-memory host runtime for tests and local wiring.
#[derive(Debug, Clone, Default)]
pub struct MockHostRuntime {
    world_stamp: WorldStamp,
    stable_overrides: BTreeMap<(String, String), StableId>,
    scalar_inputs: BTreeMap<ScalarInputKey, ScalarValue>,
    stable_keys: BTreeMap<(StableKeyKind, StableId), StableHandle>,
    span_keys: BTreeMap<SpanId, SpanKey>,
    runtime_scalar_options: RuntimeScalarOptions,
}

impl MockHostRuntime {
    /// Creates a new empty runtime.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the analysis world stamp and returns `self` for chaining.
    pub fn with_world_stamp(mut self, stamp: impl Into<WorldStamp>) -> Self {
        self.set_world_stamp(stamp);
        self
    }

    /// Sets the analysis world stamp.
    pub fn set_world_stamp(&mut self, stamp: impl Into<WorldStamp>) {
        self.world_stamp = stamp.into();
    }

    /// Adds an explicit stable ID override and returns `self` for chaining.
    pub fn with_stable_id(
        mut self,
        namespace: impl Into<String>,
        logical_name: impl Into<String>,
        id: StableId,
    ) -> Self {
        self.insert_stable_id(namespace, logical_name, id);
        self
    }

    /// Inserts/overrides a stable ID mapping.
    pub fn insert_stable_id(
        &mut self,
        namespace: impl Into<String>,
        logical_name: impl Into<String>,
        id: StableId,
    ) {
        self.stable_overrides
            .insert((namespace.into(), logical_name.into()), id);
    }

    /// Adds an explicit scalar input and returns `self` for chaining.
    pub fn with_scalar_input(mut self, key: ScalarInputKey, value: ScalarValue) -> Self {
        self.insert_scalar_input(key, value);
        self
    }

    /// Inserts/overrides a scalar input mapping.
    pub fn insert_scalar_input(&mut self, key: ScalarInputKey, value: ScalarValue) {
        self.scalar_inputs.insert(key, value);
    }

    /// Sets runtime scalar option overrides and returns `self` for chaining.
    pub fn with_runtime_scalar_options(mut self, options: RuntimeScalarOptions) -> Self {
        self.set_runtime_scalar_options(options);
        self
    }

    /// Sets runtime scalar option overrides.
    pub fn set_runtime_scalar_options(&mut self, options: RuntimeScalarOptions) {
        self.runtime_scalar_options = options;
    }

    /// Adds a `handle/2` override and returns `self` for chaining.
    pub fn with_handle(mut self, def: DefId, handle: impl Into<StableHandle>) -> Self {
        self.insert_handle(def, handle);
        self
    }

    /// Inserts/overrides a `handle/2` key.
    pub fn insert_handle(&mut self, def: DefId, handle: impl Into<StableHandle>) {
        self.insert_contract_key(StableKeyKind::Handle, def.stable_id(), handle);
    }

    /// Adds a `span_key/6` override and returns `self` for chaining.
    pub fn with_span_key(mut self, span: SpanId, key: SpanKey) -> Self {
        self.insert_span_key(span, key);
        self
    }

    /// Inserts/overrides a `span_key/6` key.
    pub fn insert_span_key(&mut self, span: SpanId, key: SpanKey) {
        self.span_keys.insert(span, key);
    }

    /// Adds a `typeref_id/2` override and returns `self` for chaining.
    pub fn with_typeref_id(mut self, type_ref: TypeRefId, handle: impl Into<StableHandle>) -> Self {
        self.insert_typeref_id(type_ref, handle);
        self
    }

    /// Inserts/overrides a `typeref_id/2` key.
    pub fn insert_typeref_id(&mut self, type_ref: TypeRefId, handle: impl Into<StableHandle>) {
        self.insert_contract_key(StableKeyKind::TypeRefId, type_ref.stable_id(), handle);
    }

    /// Adds a `node_id/2` override and returns `self` for chaining.
    pub fn with_node_id(mut self, node: NodeId, handle: impl Into<StableHandle>) -> Self {
        self.insert_node_id(node, handle);
        self
    }

    /// Inserts/overrides a `node_id/2` key.
    pub fn insert_node_id(&mut self, node: NodeId, handle: impl Into<StableHandle>) {
        self.insert_contract_key(StableKeyKind::NodeId, node.stable_id(), handle);
    }

    /// Adds a `call_id/2` override and returns `self` for chaining.
    pub fn with_call_id(mut self, call: CallId, handle: impl Into<StableHandle>) -> Self {
        self.insert_call_id(call, handle);
        self
    }

    /// Inserts/overrides a `call_id/2` key.
    pub fn insert_call_id(&mut self, call: CallId, handle: impl Into<StableHandle>) {
        self.insert_contract_key(StableKeyKind::CallId, call.stable_id(), handle);
    }

    /// Adds a `ref_id/2` override and returns `self` for chaining.
    pub fn with_ref_id(mut self, r#ref: RefId, handle: impl Into<StableHandle>) -> Self {
        self.insert_ref_id(r#ref, handle);
        self
    }

    /// Inserts/overrides a `ref_id/2` key.
    pub fn insert_ref_id(&mut self, r#ref: RefId, handle: impl Into<StableHandle>) {
        self.insert_contract_key(StableKeyKind::RefId, r#ref.stable_id(), handle);
    }

    /// Adds an `impl_id/2` override and returns `self` for chaining.
    pub fn with_impl_id(mut self, r#impl: ImplId, handle: impl Into<StableHandle>) -> Self {
        self.insert_impl_id(r#impl, handle);
        self
    }

    /// Inserts/overrides an `impl_id/2` key.
    pub fn insert_impl_id(&mut self, r#impl: ImplId, handle: impl Into<StableHandle>) {
        self.insert_contract_key(StableKeyKind::ImplId, r#impl.stable_id(), handle);
    }

    /// Deterministic stable ID derivation used when no explicit override exists.
    fn deterministic_stable_id(namespace: &str, logical_name: &str) -> StableId {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

        let mut hash = FNV_OFFSET;
        for segment in [namespace.as_bytes(), logical_name.as_bytes()] {
            for byte in segment {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
            // Segment delimiter to avoid accidental concatenation collisions.
            hash ^= 0xff;
            hash = hash.wrapping_mul(FNV_PRIME);
        }

        StableId::new(hash)
    }

    /// Deterministic stable key derivation for typed host contract IDs.
    fn deterministic_stable_key(kind: StableKeyKind, id: StableId) -> StableHandle {
        StableHandle::new(format!("{}:{id}", kind.label()))
    }

    /// Deterministic span key derivation.
    fn deterministic_span_key(span: SpanId) -> SpanKey {
        let raw = span.stable_id().as_u64();
        let rel_path = format!("mock/{raw:016x}.rs");
        let l0 = ((raw & 0xff) as u32) + 1;
        let c0 = ((raw >> 8) as u32) & 0x7f;
        let l1 = l0 + ((raw >> 15) as u32 & 0x07);
        let c1 = if l0 == l1 {
            c0 + (((raw >> 22) as u32) & 0x1f) + 1
        } else {
            ((raw >> 27) as u32) & 0x7f
        };
        SpanKey::new(rel_path, SpanCoord::new(l0, c0), SpanCoord::new(l1, c1))
    }

    fn insert_contract_key(
        &mut self,
        kind: StableKeyKind,
        id: StableId,
        handle: impl Into<StableHandle>,
    ) {
        self.stable_keys.insert((kind, id), handle.into());
    }

    fn contract_key(&self, kind: StableKeyKind, id: StableId) -> StableHandle {
        self.stable_keys
            .get(&(kind, id))
            .cloned()
            .unwrap_or_else(|| Self::deterministic_stable_key(kind, id))
    }
}

impl HostRuntime for MockHostRuntime {
    type Error = Infallible;

    fn stable_id(&self, namespace: &str, logical_name: &str) -> Result<StableId, Self::Error> {
        if let Some(id) = self
            .stable_overrides
            .get(&(namespace.to_owned(), logical_name.to_owned()))
        {
            return Ok(*id);
        }

        Ok(Self::deterministic_stable_id(namespace, logical_name))
    }

    fn world_stamp(&self) -> Result<WorldStamp, Self::Error> {
        Ok(self.world_stamp.clone())
    }

    fn handle(&self, def: DefId) -> Result<StableHandle, Self::Error> {
        Ok(self.contract_key(StableKeyKind::Handle, def.stable_id()))
    }

    fn span_key(&self, span: SpanId) -> Result<SpanKey, Self::Error> {
        Ok(self
            .span_keys
            .get(&span)
            .cloned()
            .unwrap_or_else(|| Self::deterministic_span_key(span)))
    }

    fn typeref_id(&self, type_ref: TypeRefId) -> Result<StableHandle, Self::Error> {
        Ok(self.contract_key(StableKeyKind::TypeRefId, type_ref.stable_id()))
    }

    fn node_id(&self, node: NodeId) -> Result<StableHandle, Self::Error> {
        Ok(self.contract_key(StableKeyKind::NodeId, node.stable_id()))
    }

    fn call_id(&self, call: CallId) -> Result<StableHandle, Self::Error> {
        Ok(self.contract_key(StableKeyKind::CallId, call.stable_id()))
    }

    fn ref_id(&self, r#ref: RefId) -> Result<StableHandle, Self::Error> {
        Ok(self.contract_key(StableKeyKind::RefId, r#ref.stable_id()))
    }

    fn impl_id(&self, r#impl: ImplId) -> Result<StableHandle, Self::Error> {
        Ok(self.contract_key(StableKeyKind::ImplId, r#impl.stable_id()))
    }

    fn runtime_scalar_options(&self) -> Result<RuntimeScalarOptions, Self::Error> {
        Ok(self.runtime_scalar_options)
    }

    fn scalar_input(&self, key: &ScalarInputKey) -> Result<Option<ScalarValue>, Self::Error> {
        Ok(self.scalar_inputs.get(key).cloned())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        CallId, DefId, HostRuntime, MockHostRuntime, RuntimeScalarOptions, ScalarInputKey,
        SpanCoord, SpanId, SpanKey, StableHandle, TypeRefId, WorldStamp,
    };
    use raql_ir::{ScalarValue, StableId};

    #[test]
    fn deterministic_ids_match_across_instances() {
        let a = MockHostRuntime::new();
        let b = MockHostRuntime::new();

        let id_a = a
            .stable_id("catalog", "users")
            .expect("infallible stable id");
        let id_b = b
            .stable_id("catalog", "users")
            .expect("infallible stable id");

        assert_eq!(id_a, id_b);
    }

    #[test]
    fn explicit_stable_id_override_wins() {
        let runtime = MockHostRuntime::new().with_stable_id(
            "catalog",
            "users",
            StableId::new(0xdead_beef_u64),
        );

        let id = runtime
            .stable_id("catalog", "users")
            .expect("infallible stable id");
        assert_eq!(id, StableId::new(0xdead_beef_u64));
    }

    #[test]
    fn scalar_inputs_are_keyed_by_source_and_name() {
        let runtime = MockHostRuntime::new();
        let source = runtime
            .stable_id("catalog", "orders")
            .expect("infallible stable id");

        let key = ScalarInputKey::new(source, "limit");
        let runtime = runtime.with_scalar_input(key.clone(), ScalarValue::I64(25));

        assert_eq!(
            runtime.scalar_input(&key).expect("infallible scalar input"),
            Some(ScalarValue::I64(25))
        );
        assert_eq!(
            runtime
                .scalar_input(&ScalarInputKey::new(source, "offset"))
                .expect("infallible scalar input"),
            None
        );
    }

    #[test]
    fn world_stamp_is_exposed_and_overridable() {
        let runtime = MockHostRuntime::new();
        assert_eq!(
            runtime.world_stamp().expect("infallible world stamp"),
            WorldStamp::new("mock-world")
        );

        let runtime = runtime.with_world_stamp("cfg:debug,target:x86_64-unknown-linux-gnu");
        assert_eq!(
            runtime
                .world_stamp()
                .expect("infallible world stamp")
                .as_str(),
            "cfg:debug,target:x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn stable_key_functions_are_typed_and_overridable() {
        let def = DefId::new(StableId::new(0x1));
        let type_ref = TypeRefId::new(StableId::new(0x2));
        let call = CallId::new(StableId::new(0x3));

        let runtime = MockHostRuntime::new();
        assert_eq!(
            runtime.handle(def).expect("infallible handle").as_str(),
            "handle:0x0000000000000001"
        );
        assert_eq!(
            runtime
                .typeref_id(type_ref)
                .expect("infallible typeref_id")
                .as_str(),
            "typeref_id:0x0000000000000002"
        );

        let runtime = runtime
            .with_handle(def, StableHandle::new("def://users"))
            .with_call_id(call, "call://main#12");
        assert_eq!(
            runtime.handle(def).expect("infallible handle"),
            StableHandle::new("def://users")
        );
        assert_eq!(
            runtime.call_id(call).expect("infallible call_id"),
            StableHandle::new("call://main#12")
        );
    }

    #[test]
    fn span_keys_and_runtime_scalar_options_are_configurable() {
        let span = SpanId::new(StableId::new(0x42));
        let a = MockHostRuntime::new();
        let b = MockHostRuntime::new();
        assert_eq!(
            a.span_key(span).expect("infallible span_key"),
            b.span_key(span).expect("infallible span_key")
        );

        let custom = SpanKey::new("src/lib.rs", SpanCoord::new(1, 0), SpanCoord::new(1, 10));
        let options = RuntimeScalarOptions::new()
            .with_path_limit(5)
            .with_path_max_depth(16)
            .with_max_iters(512);
        let runtime = a
            .with_span_key(span, custom.clone())
            .with_runtime_scalar_options(options);

        assert_eq!(runtime.span_key(span).expect("infallible span_key"), custom);
        assert_eq!(
            runtime
                .runtime_scalar_options()
                .expect("infallible scalar options"),
            options
        );
    }
}
