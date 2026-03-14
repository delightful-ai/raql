use std::cell::RefCell;
use std::rc::Rc;

use raql_engine::{EngineHostError, EngineHostView, RuntimeValue};
use raql_host::{
    CallId, DefId, HostRuntime, ImplId, NodeId, RefId, RuntimeScalarOptions, ScalarInputKey,
    SpanId, SpanKey, StableHandle, TypeRefId, WorldStamp,
};

use crate::{
    RaHostError, ScalarValue, StableId, WorkspaceService, runtime_value_stable_key_with_runtime,
};

pub struct LazyRaRuntime<'a> {
    service: Rc<RefCell<&'a mut WorkspaceService>>,
}

impl<'a> LazyRaRuntime<'a> {
    pub fn new(service: Rc<RefCell<&'a mut WorkspaceService>>) -> Self {
        Self { service }
    }
}

impl HostRuntime for LazyRaRuntime<'_> {
    type Error = RaHostError;

    fn stable_id(&self, namespace: &str, logical_name: &str) -> Result<StableId, Self::Error> {
        HostRuntime::stable_id(self.service.borrow_mut().ensure_core_host()?, namespace, logical_name)
    }

    fn world_stamp(&self) -> Result<WorldStamp, Self::Error> {
        HostRuntime::world_stamp(self.service.borrow_mut().ensure_core_host()?)
    }

    fn handle(&self, def: DefId) -> Result<StableHandle, Self::Error> {
        HostRuntime::handle(self.service.borrow_mut().ensure_core_host()?, def)
    }

    fn span_key(&self, span: SpanId) -> Result<SpanKey, Self::Error> {
        HostRuntime::span_key(self.service.borrow_mut().ensure_core_host()?, span)
    }

    fn typeref_id(&self, type_ref: TypeRefId) -> Result<StableHandle, Self::Error> {
        HostRuntime::typeref_id(self.service.borrow_mut().ensure_core_host()?, type_ref)
    }

    fn node_id(&self, node: NodeId) -> Result<StableHandle, Self::Error> {
        HostRuntime::node_id(self.service.borrow_mut().ensure_core_host()?, node)
    }

    fn call_id(&self, call: CallId) -> Result<StableHandle, Self::Error> {
        HostRuntime::call_id(self.service.borrow_mut().ensure_core_host()?, call)
    }

    fn ref_id(&self, r#ref: RefId) -> Result<StableHandle, Self::Error> {
        HostRuntime::ref_id(self.service.borrow_mut().ensure_core_host()?, r#ref)
    }

    fn impl_id(&self, r#impl: ImplId) -> Result<StableHandle, Self::Error> {
        HostRuntime::impl_id(self.service.borrow_mut().ensure_core_host()?, r#impl)
    }

    fn runtime_scalar_options(&self) -> Result<RuntimeScalarOptions, Self::Error> {
        HostRuntime::runtime_scalar_options(self.service.borrow_mut().ensure_core_host()?)
    }

    fn scalar_input(&self, key: &ScalarInputKey) -> Result<Option<ScalarValue>, Self::Error> {
        HostRuntime::scalar_input(self.service.borrow_mut().ensure_core_host()?, key)
    }
}

impl EngineHostView for LazyRaRuntime<'_> {
    fn world_stamp(&mut self) -> String {
        match HostRuntime::world_stamp(self) {
            Ok(stamp) => stamp.as_str().to_string(),
            Err(err) => format!("ra-host:error:{err}"),
        }
    }

    fn stable_key(&mut self, value: &RuntimeValue) -> String {
        runtime_value_stable_key_with_runtime(self, value).key
    }

    fn take_runtime_notes(&mut self) -> Vec<String> {
        self.service
            .borrow_mut()
            .ensure_core_host()
            .map(|host| host.drain_runtime_notes())
            .unwrap_or_default()
    }

    fn set_control_max_depth(&mut self, depth: i64) {
        if let Ok(host) = self.service.borrow_mut().ensure_core_host() {
            host.set_control_max_depth(depth.max(0) as u32);
        }
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
            .ensure_core_host()
            .map_err(|error| EngineHostError::new("ra_host.lazy_runtime", error.to_string()))?
            .extern_relation_rows_for_predicate_result(predicate)
            .map_err(|error| EngineHostError::new("ra_host.lazy_runtime", error.to_string()))
    }
}
