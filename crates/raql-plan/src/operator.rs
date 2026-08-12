//! The physical-operator contract between planner/engine and the semantic
//! host (SPEC §5.1, §8.1).

/// The closed vocabulary of physical operators, one per catalog-declared
/// (predicate, mode) pair.
///
/// This is an enum on purpose: the catalog is the single source of truth
/// (SPEC §8.1), so the operator set is closed, and host dispatch must be
/// exhaustive — adding a catalog entry adds a variant here, and every
/// `OperatorSet` implementation fails to compile until it handles it. No
/// registry-walking test, no drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OperatorId {
    /// `def_name(+D, -Name)` — name projection of one def.
    NameOfDef,
    /// `def_name(-D, +Name)` — exact-name seed via the symbol index.
    DefsByExactName,
    /// `def_kind(+D, -K)` — kind projection of one def.
    KindOfDef,
    /// `def_path(+D, -P)` — canonical module path of one def.
    CanonicalPathOfDef,
    /// `def_span(+D, -S)` — primary-source span of one def.
    SpanOfDef,
    /// `def_at(+File, +Pos, -D)` — definition at a file position.
    DefAtPosition,
    /// `callee(+F, -Callee, -Site, -Disp)` — outgoing call edges of a body.
    CalleesOfFn,
    /// `caller(+F, -CallerFn, -Site, -Disp)` — incoming call edges via
    /// reference search.
    CallersOfFn,
    /// `call_edge(+Caller, -Callee, -Site, -Disp)` — composition over the
    /// outgoing direction.
    CallEdgesByCaller,
    /// `call_edge(-Caller, +Callee, -Site, -Disp)` — composition over the
    /// incoming direction.
    CallEdgesByCallee,
}

impl OperatorId {
    /// Stable name used in `explain` and capabilities output.
    pub fn name(self) -> &'static str {
        match self {
            OperatorId::NameOfDef => "def_name/name-of-def",
            OperatorId::DefsByExactName => "def_name/defs-by-exact-name",
            OperatorId::KindOfDef => "def_kind/kind-of-def",
            OperatorId::CanonicalPathOfDef => "def_path/canonical-path-of-def",
            OperatorId::SpanOfDef => "def_span/span-of-def",
            OperatorId::DefAtPosition => "def_at/classify-at-position",
            OperatorId::CalleesOfFn => "callee/callees-of-fn",
            OperatorId::CallersOfFn => "caller/callers-of-fn",
            OperatorId::CallEdgesByCaller => "call_edge/by-caller",
            OperatorId::CallEdgesByCallee => "call_edge/by-callee",
        }
    }
}

/// The physical-operator invocation contract.
///
/// Contract, per SPEC §8:
/// - `operator` is the catalog-declared binding of one (predicate, mode).
/// - `inputs` are the values of the mode's `+` arguments, in predicate
///   declaration order.
/// - The result rows are full tuples over all predicate arguments, in
///   declaration order (bound positions echo their input value).
/// - Operator errors are errors: implementations must never degrade to a
///   different access path, and callers must never treat an error as an
///   empty relation.
///
/// `invoke` takes `&mut self`: hosts use exclusive access to encode
/// invocation-scoped context discipline in the type system (e.g. raql-ra's
/// thread-local database attachment scopes), and evaluation is
/// single-threaded per request.
pub trait OperatorSet {
    type Value;
    type Error;

    fn invoke(
        &mut self,
        operator: OperatorId,
        inputs: &[Self::Value],
    ) -> Result<Vec<Vec<Self::Value>>, Self::Error>;
}
