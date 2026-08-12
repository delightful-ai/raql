# raql-plan

The predicate catalog and the binding-aware planner.
Contract: `docs/SPEC.md` §8–§10.

## Ownership boundary

- Owns: the catalog registry (single source of truth for extern predicates:
  schema, modes, costs, completeness, operator bindings), mode-satisfaction
  logic, capabilities rendering, and the planner — its logical input IR
  (`logic.rs`), derived-mode inference / demand propagation / the
  backtracking reorderer (`planner.rs`), physical plan + explain
  (`plan.rs`), and the RAQL0301/0310-family error contracts (`error.rs`).
- Must not own: execution of anything; any rust-analyzer type or dependency.
  The `[dependencies]` section is empty on purpose and should stay that way.
  The host side is reached only through the `OperatorSet` trait, generic over
  the host's value type.

## Planner shape (don't re-derive)

- Planner input is `logic::Program` — the lang layer lowers to it; the
  planner never sees compiler IR. Selector bindings are input relations.
- Derived-mode inference is a greatest fixpoint per call-graph SCC
  (SPEC §9.1); declared `.mode` assertions are checked against the inferred
  set and then *replace* it as the public contract.
- The reorderer backtracks over goal order only — mode choice is pure cost
  selection because any mode binds all of a goal's variables.
- Ordering costs derived goals with a C1 heuristic (C4 when unseeded);
  *reported* plan costs are exact, computed transitively after planning.
- Error codes: 0301/0310 are SPEC-normative; 0302 (disabled), 0303
  (declared mode not inferable), 0304 (malformed input), 0311 (scan under
  negation) are assigned in `error.rs`. The RAQL0301 message contract
  (SPEC §10.3) is asserted verbatim in `tests/planner.rs` — change it
  deliberately or not at all.

## Bait / keep out

- No handwritten capability lists anywhere else in the workspace — render
  from the catalog (`Catalog::capabilities_text`). Same for the RAQL0301
  seed hint: derived from the catalog, never hardcoded.
- `OperatorId` is a closed enum so host dispatch is exhaustive. Add a
  variant only together with its catalog mode; never add a stringly-typed
  escape hatch.
- A predicate not in the registry does not exist; "approximately complete"
  is `Completeness::Disabled`, not a fourth class (SPEC §4.3). The
  `disabled` roadmap families are the one exception to "entries land with
  their operator": they exist mode-less so capabilities show the roadmap.
- Catalog entries land only together with their operator implementation and
  §16 proof matrix (truth fixtures in `raql-ra`).
- Scans are declared modes, never inferred (SPEC §8.5). A mode whose access
  path cannot cover part of the predicate's domain declares a per-mode
  caveat (`ModeDef::caveats`, e.g. `fields_not_in_symbol_index`).

## Verify

- `cargo test -p raql-plan` — catalog invariants plus the planner
  conformance suite (`tests/planner.rs`: registry-walked mode matrix, the
  step-4 no-enumeration gate, recursion, negation, error contracts).
- Registry-driven truth tests live in `raql-ra`
  (`cargo test -p raql-ra --test slice_a_defs --test slice_b_calls --test slice_c_scans`).
