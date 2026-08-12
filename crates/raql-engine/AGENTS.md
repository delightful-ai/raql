# raql-engine

Demand-driven row execution over the catalog operator boundary. Contract:
`docs/SPEC.md` §9.2, §11, §17.1 (raql-engine row).

## Ownership boundary

- Owns: joins, stratified negation, aggregation, `choose_topk`,
  `witness_path`/`path_hop`, per-request demand memoization, iteration
  caps, the runtime error taxonomy (RAQL09xx), result assembly
  (`out_status`, coded notes).
- Must not own: Rust semantic discovery, any RA API, any access-path
  choice. Extern goals are `raql_plan::OperatorSet` invocations, full
  stop; the value type is generic through `raql_plan::EngineValue`.

## The shape (don't re-derive)

- Execution starts at the plan's demand roots and evaluates
  specializations memoized per `(predicate, pattern, seed)` (SPEC §9.2).
  Recursion is naive iteration over the demanded subset: a frame that
  reads its own in-progress partial loops to fixpoint; one that read an
  *ancestor's* partial returns provisionally without memoizing and the
  ancestor's loop re-runs it (`eval.rs::eval_spec`).
- The engine executes from the source AST: `source_index` → provenance
  `GoalPath` → `raql_syntax::Goal`; the access says how (operator,
  input rows, demanded spec, builtin, binder). Positional atoms pair
  logic args 1:1 with source terms; engine builtins go through the
  flattened-variable path (`terms.rs` name-based unification).
- Binder goals evaluate their synthesized sub-body specialization
  (correlated seeds, memoized) and apply aggregate/topk semantics
  engine-side; an aggregate spec's head drops the unbound output column.
- No ordering of semantic handles: `plain_cmp` refuses them; min/max and
  comparisons over handles are runtime type errors; `choose_topk` ties
  keep insertion order (deterministic per snapshot).
- Operator errors are errors (RAQL0909) — never degrade to an empty
  relation. Runtime failures degrade the result to `Partial` with coded
  notes; partials are labeled, never silent (SPEC §4.4).
- Unwind safety: all state is request-local; Salsa cancellation panics
  unwind through `execute` for the caller's `catch_unwind` (§11.2).

## Bait / keep out

- No bulk relation materialization, no `extern_relation_rows`-style
  pull, no fallback access paths, no host-value fabrication.
- Do not reintroduce a host-facing trait beyond `OperatorSet` +
  `EngineValue`. The old `EngineHostView` is the canonical example.
- Join indexing / semi-naive recursion are welcome as quality work
  (§11.1) but must not change set semantics.

## Verify

- `cargo test -p raql-engine` — full-pipeline suite (programs compile
  through the real resolve/typecheck/plan and execute against a canned
  `OperatorSet` with an unorderable handle type): joins, recursion +
  cap, negation, binders, builtins, witness paths, catalog mode
  selection, per-seed demand memoization via operator call counts.
