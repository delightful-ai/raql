# raql-plan

The predicate catalog and (from build step 4) the binding-aware planner.
Contract: `docs/SPEC.md` §8–§10.

## Ownership boundary

- Owns: the catalog registry (single source of truth for extern predicates:
  schema, modes, costs, completeness, operator bindings), mode-satisfaction
  logic, capabilities rendering, and — later — demand propagation and
  physical planning.
- Must not own: execution of anything; any rust-analyzer type or dependency.
  The `[dependencies]` section is empty on purpose and should stay that way.
  The host side is reached only through the `OperatorSet` trait, generic over
  the host's value type.

## Bait / keep out

- No handwritten capability lists anywhere else in the workspace — render
  from the catalog (`Catalog::capabilities_text`).
- `OperatorId` is a closed enum so host dispatch is exhaustive. Add a
  variant only together with its catalog mode; never add a stringly-typed
  escape hatch.
- A predicate not in the registry does not exist; "approximately complete"
  is `Completeness::Disabled`, not a fourth class (SPEC §4.3).
- Catalog entries land only together with their operator implementation and
  §16 proof matrix (truth fixtures in `raql-ra`).

## Verify

- `cargo test -p raql-plan` and the registry-driven truth tests in
  `raql-ra` (`cargo test -p raql-ra --test slice_a_defs`).
