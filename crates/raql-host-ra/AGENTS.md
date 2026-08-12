## Boundary

This crate owns the rust-analyzer-backed host runtime: RA-native identity, workspace integration, semantic provider logic, and the lookup/index boundary between RAQL and rust-analyzer.

`src/workspace_service.rs` is orchestration, cache ownership, and daemon-facing lifecycle glue. It must not keep absorbing new semantic families.

## Route work

- Put RA identity types, canonical wrappers, and shared lookup record logic in focused modules, not inline in `src/workspace_service.rs`.
- Put exact-name/bound-def logic in a defs-oriented provider module.
- Put caller/callee and dispatch logic in a call-oriented provider module.
- Put structure, type, syntax, and search logic in their own provider modules once they have distinct proof loops.
- Use `vendor/rust-analyzer` to copy RA integration patterns before inventing local substitutes.

## Keep out

- Do not use tracked-file text scans, raw filesystem sweeps, path-prefix guessing, or AST rediscovery as supported semantic truth when RA already has the answer.
- Do not answer bound lookups by turning them back into reverse search.
- Do not grow new public CLI or daemon lifecycle logic here. That belongs in `crates/raql-daemon/` and `crates/raql-cli/`.
- If a surface is not RA-native and honest yet, disable it instead of preserving a custom approximation.

## Verification

- `cargo test -p raql-host-ra workspace_service_reports_supported_def_paths_for_exact_name_seeded_structs -- --nocapture`
  proves exact-name seeded non-function defs still resolve on the supported path.
- `cargo test -p raql-host-ra workspace_service_supports_structure_and_trait_rows -- --nocapture`
  proves bound `Def` joins still line up with the RA-built core host.
- `cargo test -p raql-host-ra workspace_service_looks_up_call_edges_for_bound_callee -- --nocapture`
  proves the bound-callee call path still works through RA-backed identity.
- `cargo test -p raql-host-ra -- --nocapture`
  is the crate-wide proof loop after touching shared provider or workspace logic.

