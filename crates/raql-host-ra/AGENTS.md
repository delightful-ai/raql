# raql-host-ra

Workspace lifecycle for the daemon, plus the output-projection boundary of
the new runtime. The legacy semantic machinery (`DeterministicRaHost`,
providers, lookup caches) was deleted per SPEC §17.1 — semantic truth
lives in `crates/raql-ra/` now. This crate is scheduled to dissolve into
`raql-server` at step 5 (SPEC §12); until then it is the working shell.

## Ownership boundary

- Owns: loading a Cargo workspace into an `AnalysisHost` + VFS
  (`workspace_loader.rs`), watching and syncing it (`workspace_service/`,
  incl. `sync_workspace` — the known-bad warm-path sweep §12.2 deletes),
  warmup, workspace epoch/content revision, the supported-capability set
  (`capability.rs`), and running a planned program: `run_planned` =
  sync → `raql_engine::execute` over `raql_ra::SnapshotOperators` →
  §13.1 projection (`projection.rs` → `ProjectedRunResult`).
- Must not own: semantic extraction (raql-ra's), execution semantics
  (raql-engine's), plan knowledge (raql-plan's), CLI/daemon lifecycle
  (raql-daemon's).

## Keep out

- Do not re-grow providers, lookup caches, or any extraction here; a new
  semantic family means a catalog entry + operator in `raql-ra`.
- `supported_capabilities` must track what `SnapshotOperators` actually
  implements (the catalog's non-disabled predicates) — update both sides
  together.
- Projection never fabricates: unprojectable values render as
  `<unprojectable:reason>` (SPEC §4.4), through `raql_ra::project_def` /
  `project_file_range` only.

## Verify

- `cargo test -p raql-host-ra` — lifecycle + the end-to-end fixture run
  through `run_planned` (projected-row assertions, bound call family,
  incremental-edit visibility, capability coverage).
- The public latency probe: release binary,
  `raql lang run views/stdlib_callers_load_and_plan.raql --rust-file .`
  (daemon-backed; cold ≈3.2s / warm ≈120ms as of 2026-08-11).
