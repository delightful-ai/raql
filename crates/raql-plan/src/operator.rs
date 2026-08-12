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
    /// `def(-D)` — scan: enumerate every definition in scope.
    DefsScan,
    /// `fn_def(-D)` — scan: `def` filtered to functions during enumeration.
    FnDefsScan,
    /// `def_name(-D, -Name)` — scan: enumeration × name projection.
    DefNamesScan,
    /// `call_edge(-,-,-,-)` — scan: the SPEC §8.5 rewrite
    /// `fn_def(C), callee(C, K, S, D)`. Never routes through reference
    /// search.
    CallEdgesScan,
    /// `is_public(+D)` — filter: the definition's visibility is `pub`.
    IsPublicFilter,
    /// `in_test(+D)` — filter: the definition is test code.
    InTestFilter,
    /// `span_allowed(+S)` — filter: the span is in the request scope.
    SpanAllowedFilter,
    /// `handle(+D, -H)` — the §13.2 handle projection as a predicate.
    HandleOfDef,
    /// `span_key(+S, -Path, -L0, -C0, -L1, -C1)` — workspace-relative
    /// location projection of a span.
    SpanKeyOfSpan,
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
            OperatorId::DefsScan => "def/defs-scan",
            OperatorId::FnDefsScan => "fn_def/fn-defs-scan",
            OperatorId::DefNamesScan => "def_name/def-names-scan",
            OperatorId::CallEdgesScan => "call_edge/scan",
            OperatorId::IsPublicFilter => "is_public/visibility-filter",
            OperatorId::InTestFilter => "in_test/test-scope-filter",
            OperatorId::SpanAllowedFilter => "span_allowed/scope-filter",
            OperatorId::HandleOfDef => "handle/handle-of-def",
            OperatorId::SpanKeyOfSpan => "span_key/span-key-of-span",
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
