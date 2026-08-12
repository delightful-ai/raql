# raql-compiler

The lang layer: parse-adjacent resolution, typechecking, stratification,
and lowering to the planner's logical IR. Contract: `docs/SPEC.md` §5.1
(lang row), §9 front half, §17.1.

## Ownership boundary

- Owns: name resolution (`resolve.rs`), type inference and the reserved
  engine contracts (`typecheck.rs`), disjunction flattening (`expand.rs`),
  stratification (`strata.rs`), the lowering to `raql_plan::logic` with
  its provenance (`lower.rs`), catalog/engine-builtin declaration
  injection (`externs.rs`), and the plan entry (`plan.rs`) that routes
  through `raql_plan::plan`.
- Must not own: goal ordering, access-path selection, cost knowledge
  (raql-plan's), execution (raql-engine's), RA types.

## The shape (don't re-derive)

- Extern predicates exist only in the catalog. `resolve` injects their
  declarations (`externs.rs`); programs cannot declare externs
  (RAQL0105), redeclare catalog names (RAQL0101), or put `.mode` on
  externs (RAQL0105) / inputs (RAQL0106). Exception: `fmt`/`coalesce`
  keep use-site declarations (their schemas are generic) and are
  validated by the reserved-shape checks.
- Every top-level body goal lowers to exactly one logic goal at the same
  index, so `PlannedGoal::source_index` is a source body index; the
  `GoalPath` provenance handles binder sub-bodies. The engine executes
  from the source AST through this mapping — change the lowering and the
  engine's assumptions together.
- Aggregates/`choose_topk` lower as synthesized derived defs (head =
  bound slots ++ correlated ++ locals ++ outputs, declared mode bound/
  free split) so the planner orders sub-bodies honestly and demand
  specialization plans them. A sibling binder exposes only its outputs
  (+ `k`/`group` vars) to correlation — reused local names do not
  correlate.
- Constraints lower to per-goal multi-pattern builtins (`=` runs with
  either side ground). Engine-builtin atoms lower over their flattened
  variable list, so compound arguments (`fmt("{}", [N], M)`) plan.
- Only rules reachable from the demand roots lower: stdlib rules over
  `disabled` families compile until demanded (then RAQL0302 with span).
  Roots: root-file outputs, else root-file rule heads/decls, else every
  rule head. `witness_path` reachability implicitly demands `graph_edge`.
- RAQL0301 text is raql-plan's verbatim §10.3 rendering; the span comes
  from `GoalLocation` through the provenance. Do not re-add local plan
  error prose.
- Helper-rule inlining is gone deliberately: it bypassed §9.1 declared-
  mode contracts, and demand specialization subsumes it.

## Verify

- `cargo test -p raql-compiler` — pipeline, diagnostics contracts, and
  the view compilation proofs (P1 seeded/no-enumeration, hotspots on the
  declared scan, callers_demo binder lowering, error-conversions dark).
