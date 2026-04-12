use std::cell::{Cell, RefCell};
use std::rc::Rc;

use raql_engine::{EngineHostError, EngineHostView, RuntimeValue};
use raql_host::{
    CallId, DefId, ExternLookupRequest, ExternLookupValue, HostRuntime, ImplId, NodeId, RefId,
    RuntimeScalarOptions, ScalarInputKey, SpanId, SpanKey, StableHandle, TypeRefId, WorldStamp,
};

use crate::workspace_service::{CoreHostBuildSpec, WorkspaceService};
use crate::{RaHostError, ScalarValue, StableId, runtime_value_stable_key_with_runtime};

pub struct LazyRaRuntime<'a> {
    service: Rc<RefCell<&'a mut WorkspaceService>>,
    core_host_spec: CoreHostBuildSpec,
    control_max_depth: Cell<u32>,
}

impl<'a> LazyRaRuntime<'a> {
    pub fn new(service: Rc<RefCell<&'a mut WorkspaceService>>, core_host_spec: CoreHostBuildSpec) -> Self {
        Self {
            service,
            core_host_spec,
            control_max_depth: Cell::new(32),
        }
    }

    fn with_core_host<T>(
        &self,
        f: impl FnOnce(&mut crate::DeterministicRaHost) -> T,
    ) -> Result<T, RaHostError> {
        let mut service = self.service.borrow_mut();
        let host = service.ensure_core_host(&self.core_host_spec)?;
        host.set_control_max_depth(self.control_max_depth.get());
        Ok(f(host))
    }

    fn lookup_supported(&self, predicate: &str) -> bool {
        matches!(
            predicate,
            "def_name"
                | "def"
                | "def_kind"
                | "def_span"
                | "def_path"
                | "is_public"
                | "in_test"
                | "span_allowed"
                | "span_key"
                | "call_edge"
        )
    }

    fn lookup_only_enabled(&self, predicate: &str) -> bool {
        self.core_host_spec.supports_lookup_only_fast_path() && self.lookup_supported(predicate)
    }
}

impl HostRuntime for LazyRaRuntime<'_> {
    type Error = RaHostError;

    fn stable_id(&self, namespace: &str, logical_name: &str) -> Result<StableId, Self::Error> {
        self.with_core_host(|host| HostRuntime::stable_id(host, namespace, logical_name))?
    }

    fn world_stamp(&self) -> Result<WorldStamp, Self::Error> {
        self.with_core_host(|host| HostRuntime::world_stamp(host))?
    }

    fn handle(&self, def: DefId) -> Result<StableHandle, Self::Error> {
        self.with_core_host(|host| HostRuntime::handle(host, def))?
    }

    fn span_key(&self, span: SpanId) -> Result<SpanKey, Self::Error> {
        self.with_core_host(|host| HostRuntime::span_key(host, span))?
    }

    fn typeref_id(&self, type_ref: TypeRefId) -> Result<StableHandle, Self::Error> {
        self.with_core_host(|host| HostRuntime::typeref_id(host, type_ref))?
    }

    fn node_id(&self, node: NodeId) -> Result<StableHandle, Self::Error> {
        self.with_core_host(|host| HostRuntime::node_id(host, node))?
    }

    fn call_id(&self, call: CallId) -> Result<StableHandle, Self::Error> {
        self.with_core_host(|host| HostRuntime::call_id(host, call))?
    }

    fn ref_id(&self, r#ref: RefId) -> Result<StableHandle, Self::Error> {
        self.with_core_host(|host| HostRuntime::ref_id(host, r#ref))?
    }

    fn impl_id(&self, r#impl: ImplId) -> Result<StableHandle, Self::Error> {
        self.with_core_host(|host| HostRuntime::impl_id(host, r#impl))?
    }

    fn runtime_scalar_options(&self) -> Result<RuntimeScalarOptions, Self::Error> {
        self.with_core_host(|host| HostRuntime::runtime_scalar_options(host))?
    }

    fn scalar_input(&self, key: &ScalarInputKey) -> Result<Option<ScalarValue>, Self::Error> {
        self.with_core_host(|host| HostRuntime::scalar_input(host, key))?
    }
}

impl EngineHostView for LazyRaRuntime<'_> {
    fn world_stamp(&mut self) -> String {
        self.service
            .borrow()
            .current_world_stamp()
            .as_str()
            .to_string()
    }

    fn stable_key(&mut self, value: &RuntimeValue) -> String {
        runtime_value_stable_key_with_runtime(self, value).key
    }

    fn take_runtime_notes(&mut self) -> Vec<String> {
        self.service.borrow_mut().take_runtime_notes_if_ready()
    }

    fn set_control_max_depth(&mut self, depth: i64) {
        let depth = depth.max(0) as u32;
        self.control_max_depth.set(depth);
        self.service.borrow_mut().set_control_max_depth_if_ready(depth);
    }

    fn extern_relation_rows(
        &mut self,
        predicate: &str,
    ) -> Result<Option<Vec<Vec<RuntimeValue>>>, EngineHostError> {
        if !self.service.borrow().supports_extern_predicate(predicate) {
            return Err(EngineHostError::new(
                "ra_host.lazy_runtime",
                format!("unsupported rust-analyzer runtime capability: {predicate}"),
            ));
        }
        self.service
            .borrow_mut()
            .ensure_core_host(&self.core_host_spec)
            .map(|host| {
                host.set_control_max_depth(self.control_max_depth.get());
                host
            })
            .map_err(|error| EngineHostError::new("ra_host.lazy_runtime", error.to_string()))?
            .extern_relation_rows_for_predicate_result(predicate)
            .map_err(|error| EngineHostError::new("ra_host.lazy_runtime", error.to_string()))
    }

    fn extern_lookup(
        &mut self,
        request: &ExternLookupRequest,
    ) -> Result<Option<Vec<Vec<ExternLookupValue>>>, EngineHostError> {
        if !self.lookup_supported(request.predicate()) {
            return Ok(None);
        }
        if !self.service.borrow().supports_extern_predicate(request.predicate()) {
            return Err(EngineHostError::new(
                "ra_host.lazy_runtime",
                format!(
                    "unsupported rust-analyzer runtime capability: {}",
                    request.predicate()
                ),
            ));
        }
        self.service
            .borrow_mut()
            .extern_lookup_rows(request)
            .map_err(|error| EngineHostError::new("ra_host.lazy_runtime", error.to_string()))
    }

    fn prefers_lookup_only(&mut self, predicate: &str) -> bool {
        self.lookup_only_enabled(predicate)
    }
}
