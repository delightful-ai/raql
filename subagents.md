# Subagent Orchestration Log

## Session
- Date: 2026-02-13
- Repo: /Users/darin/Projects/raql
- Parent checkpoint change: `mmuwkssr` (`wip: baseline RA host snapshot before gap-closure pass`)
- Active implementation change: `knlsvqzx` (`feat: close rust-analyzer semantic/runtime gaps (ordered pass)`)

## Workflow Rules
- For each gap (in user-provided order):
  - Spawn planning subagent
  - Implement directly in main agent
  - Spawn review subagent
  - Validate findings and fix valid issues
  - Re-run review until no material findings
  - Close subagents before moving to next gap

## Gap Queue (Ordered)
1. Semantic call graph fidelity (`workspace_snapshot.rs:685`)
2. Populate error-flow predicates (`lib.rs:1259` + extraction)
3. Tighten type/error extraction seams (`workspace_snapshot.rs:915`)
4. Complete world identity (`workspace_snapshot.rs:1123`)
5. Expand beyond local-root-only coverage (`workspace_snapshot.rs:1156`)
6. Add runtime incremental/hot-reload model (`lib.rs:1656`)
7. Reduce strict init brittleness with robust modes (`workspace_loader.rs:80`)
8. Build real-world conformance corpus gate

## Activity Log
- Initialized orchestration log.
- [Gap 1][Planning] Spawned explorer subagent `019c5666-2844-7a33-8138-ca1dd4841df3` for semantic call-edge plan.
- [Gap 1][Planning] Plan received: switch call target resolution to semantic callable APIs, add explicit fn-pointer dispatch variant, keep path fallback for unresolved cases, extend tests.
- [Gap 1][Execution] Implemented semantic callable classification in `workspace_snapshot.rs`; added `DispatchKind::FnPointer`; updated dispatch type declarations and added callable-dispatch integration test in `workspace_init.rs`.
- [Gap 1][Review] Spawned explorer subagent `019c566a-37bf-7561-a16c-6732e117fdab` for adversarial code review.
- [Gap 1][Review] Finding accepted as valid: method-call callee precision regressed by early return on `resolve_method_call_as_callable`.
- [Gap 1][Fix] Updated method-call semantic resolver to keep callee from `sema.resolve_method_call` and use callable API only for dispatch classification.
- [Gap 1][Review] Re-review result: no actionable findings remain.
- [Gap 2][Planning] Spawned explorer subagent `019c5670-cfca-7c03-af63-94e6b95efe7d` for error-flow predicate extraction plan.
- [Gap 2][Planning] Plan received: populate constructs/propagates/converts/handles/compares/writes from AST+semantic context, anchored to function error type, with integration test coverage.
- [Gap 2][Execution] Added `extract_error_flow_for_file` pass in `workspace_snapshot.rs` and wired it into snapshot extraction. Added semantic population for:
  - `constructs` from enum-variant constructors in call/record expressions
  - `propagates` from postfix `?`
  - `converts` from `?` when source error != function error
  - `handles` from match-arm variant patterns
  - `compares` from comparison binops touching function error type
  - `writes` from assignment binops touching function error type
- [Gap 2][Execution] Added `error_flow_predicates_are_semantically_populated` integration test in `workspace_init.rs`.
- [Gap 2][Review] Spawned explorer subagent `019c5678-6868-7623-898e-36c65996b9d4` for adversarial review.
- [Gap 2][Review] Findings accepted as valid:
  - Async functions did not populate error-flow predicates because function error detection only inspected direct return type.
  - Unit-like enum variant constructions were missed because only call/record constructors were scanned.
- [Gap 2][Fix] Added async-aware function error helper (`async_ret_type` fallback) and `PathExpr` unit-variant construct extraction.
- [Gap 2][Fix] Expanded integration test with async propagation and unit-variant construct assertions.
- [Gap 2][Review] Re-review result: no actionable findings remain.
- [Gap 3][Planning] Spawned explorer subagent `019c5680-b6d4-7163-8390-96574410ee81` for type/error extraction hardening plan.
- [Gap 3][Planning] Plan received: move result-error inference to semantic type argument APIs, enrich generic type-shape extraction, and add alias/generic regression tests.
- [Gap 3][Execution] Hardened type/error extraction in `workspace_snapshot.rs`:
  - `best_effort_result_error_def` now prioritizes semantic type arguments and layered fallbacks.
  - Added helpers for result-head detection and deterministic synthetic type-head IDs.
  - `TypeShape::App` now captures type generic arguments for ADT/dyn trait shapes.
  - `type_fingerprint` now includes generic argument fingerprints.
- [Gap 3][Execution] Prevented metadata clobber in `DeterministicRaHost::insert_def` (`lib.rs`) so repeated def registration no longer wipes `fn_error_type` / return-type metadata.
- [Gap 3][Execution] Added `fn_error_type_and_ty_arg_handle_result_aliases_and_generics` integration test in `workspace_init.rs`.
- [Gap 3][Review] Spawned explorer subagent `019c5693-3e26-7573-b36e-b71cb663860b` for adversarial review.
- [Gap 3][Review] Findings accepted as valid:
  - Result inference was too broad with unconditional second-generic fallback.
  - Fingerprint encoding could collide across different generic argument boundaries.
  - Alias fallback initially over-eager for non-Result aliases.
- [Gap 3][Fix] Tightened result-like guards, added semantics-safe alias path handling from `sema.parse` return-type syntax, and encoded fingerprint parts with length delimiters.
- [Gap 3][Fix] `process_function` now clears and recomputes `fn_error_type` each pass.
- [Gap 3][Review] Re-review result: no actionable findings remain.
- [Gap 4][Planning] Spawned explorer subagent `019c569c-41af-7080-a492-27b79f46969f` for world identity expansion plan.
- [Gap 4][Planning] Plan received: include semantic config/dependency/toolchain inputs in world-stamp identity and prove manifest/config changes flip the stamp.
- [Gap 4][Execution] Expanded world identity in `workspace_snapshot.rs`:
  - Added semantic identity fingerprint over resolved crate graph metadata, cfg/env axes, toolchain command outputs, and workspace config file hashes.
  - Folded semantic identity into `compute_world_stamp` hashing input.
- [Gap 4][Execution] Added `world_stamp_changes_when_manifest_configuration_changes` integration test in `workspace_init.rs`.
- [Gap 4][Review] Spawned explorer subagent `019c56a2-7684-7451-b078-aba39924dc04` for adversarial review.
- [Gap 4][Review] Findings accepted as valid:
  - Toolchain command outputs were CWD-sensitive and could miss workspace rust-toolchain context.
  - Cargo.lock hashing leaked absolute `path+file://` paths and broke portability determinism.
- [Gap 4][Fix] Ran toolchain commands with `current_dir(workspace_root)` and normalized Cargo.lock path sources before hashing.
- [Gap 4][Review] Re-review result: no actionable findings remain.
- [Gap 5][Planning] Spawned explorer subagent `019c56b9-dc09-7a40-8c39-30649fd8a408` for cross-crate source-root expansion/perf-safety plan.
- [Gap 5][Planning] Plan validated direction: expand selection to dependency roots via crate graph, preserve path normalization, add deterministic/per-root quotas and failure safeguards.
- [Gap 5][Execution] Completed local-root-only removal with bounded dependency-root coverage in `workspace_snapshot.rs`:
  - Added `selected_source_root_ids` seeded from local/workspace crates and transitive dependencies (excluding lang crates).
  - `collect_local_files` now restricts scanning to selected source roots.
  - Added panic-safe wrappers in impl/function extraction paths (`process_impl`/`process_function`) to skip pathological RA type panics instead of failing init.
  - Added deterministic library quotas: global + per-source-root limits and stable candidate sorting before truncation.
  - Hardened external path normalization fallback with hashed prefixes to avoid collisions without leaking absolute paths.
- [Gap 5][Validation] `cargo test -p raql-host-ra --test workspace_init` passed (15/15), including `dependency_library_source_roots_are_included_in_snapshot`.
- [Gap 5][Review] Spawned explorer subagent `019c56bc-068c-7913-98da-e0f10a2298e8` for adversarial review.
- [Gap 5][Review] Finding accepted as valid: nondeterministic quota truncation due VFS iteration order. Fixed by sorting file candidates before quota application.
- [Gap 5][Review] Re-review returned one unrelated startup-performance note in semantic identity computation (gap 4 scope); not accepted as a gap-5 blocker.
- [Gap 5][Review] No remaining actionable gap-5 findings.
- [Gap 6][Planning] Spawned explorer subagent `019c56c2-758a-7e63-96fa-2c48c01d6bcb` for runtime hot-reload pipeline plan.
- [Gap 6][Planning] Plan adopted: add reload-capable runtime state (workspace context + change detection), explicit reload API, and query-boundary auto-refresh hooks.
- [Gap 6][Execution] Implemented reload pipeline in `raql-host-ra`:
  - `workspace_loader::LoadedWorkspace` now carries `manifest_path` so reload has stable loader context.
  - `RaHostRuntime` now stores workspace/manifest context and hot-reload state (`enabled`, poll interval, next poll, fingerprint).
  - Added `set_hot_reload_enabled`, `set_hot_reload_poll_interval`, `reload_now`, and `maybe_reload`.
  - Added workspace metadata fingerprinting for change detection and query-boundary reload checks in `EngineHostView` (`world_stamp`, `stable_key`, `extern_relation_rows`).
  - Reload preserves runtime scalar options/inputs/stable ID overrides/control depth and runtime notes across snapshot rebuilds.
- [Gap 6][Execution] Added integration coverage in `workspace_init.rs`:
  - `runtime_reload_now_refreshes_snapshot_after_file_change`
  - `runtime_auto_reload_refreshes_on_query_boundary`
  - `runtime_reload_preserves_runtime_scalar_configuration`
- [Gap 6][Validation] `cargo test -p raql-host-ra --test workspace_init` passed (18/18).
- [Gap 6][Review] Spawned explorer subagent `019c56cc-03ae-72d0-b7dd-2d5b6ca7451d` for adversarial review.
- [Gap 6][Review] Findings accepted as valid:
  - Fingerprint hard cutoff could freeze change detection on very large workspaces.
  - Auto-reload failure handling needed explicit disable behavior to avoid repeated stale polling.
- [Gap 6][Fix] Removed fingerprint hard cutoff and disable hot reload after failure while recording a runtime note.
- [Gap 6][Review] Re-review result: no actionable findings remain.
- [Gap 7][Planning] Spawned explorer subagent `019c56d7-eb84-7242-b313-c9ff171f58cb` for strict-vs-resilient initialization strategy.
- [Gap 7][Planning] Plan confirmed direction: preserve strict semantics, add resilient fallback behavior and explicit degraded-mode signaling.
- [Gap 7][Execution] Implemented robust init modes:
  - Added public `WorkspaceInitMode` (`Strict`, `Resilient`) and mode-aware constructors (`from_workspace_root_with_mode`, `from_manifest_path_with_mode`).
  - Switched default constructors to `Resilient`.
  - `workspace_loader` now accepts mode and can retry with resilient load config (`load_out_dirs_from_check=false`, `proc_macro_server=None`) when strict load fails.
  - `LoadedWorkspace` now carries `init_notes` and optional proc-macro client; notes are propagated into runtime notes during init/reload.
  - Runtime reload now preserves chosen init mode across refresh cycles.
- [Gap 7][Execution] Added integration coverage:
  - `resilient_init_falls_back_when_build_scripts_fail`
  - `re_enabling_hot_reload_applies_changes_made_while_disabled`
- [Gap 7][Validation] `cargo test -p raql-host-ra --test workspace_init` passed (20/20).
- [Gap 7][Review] Spawned explorer subagent `019c56d8-819d-70e2-aa2e-25bd8432abae` for adversarial review.
- [Gap 7][Review] Finding accepted as valid: re-enabling hot reload reset fingerprint baseline and could miss edits made while disabled.
- [Gap 7][Fix] `set_hot_reload_enabled(true)` now preserves existing fingerprint baseline unless unset, so pending edits are reloaded.
- [Gap 7][Review] Final re-review via explorer subagent `019c56e0-b6b8-7a20-a422-abd328b2c1ba`: no actionable findings remain.
- [Gap 8][Planning] Spawned explorer subagent `019c56e3-e232-7020-9b3b-9c3cadcfbe69` for conformance-corpus release-gate structure review.
- [Gap 8][Planning] Plan applied: add pinned corpus manifest, invariant tests always-on, and ignored release-gate runtime test over real repositories.
- [Gap 8][Execution] Added conformance corpus artifacts:
  - `conformance/corpus.toml` with pinned commits for `no_std`, `proc_macro_heavy`, `huge_monorepo`, and `feature_matrix` repositories.
  - `crates/raql-host-ra/tests/conformance_corpus.rs` with:
    - always-on manifest validation tests,
    - ignored `conformance_corpus_runtime_gate` (enabled by `RAQL_CONFORMANCE=1`) that materializes repos and runs runtime/query checks,
    - case-level symbol+exact-path probes.
  - `scripts/run_conformance_gate.sh` release-gate script invoking ignored conformance test in `--release`.
  - `crates/raql-host-ra/Cargo.toml` dev-deps for corpus manifest parsing (`serde`, `toml`).
- [Gap 8][Validation] `cargo test -p raql-host-ra conformance_manifest -- --nocapture` passed (manifest tests).
- [Gap 8][Review] Spawned explorer subagent `019c56e5-4a47-71f3-973d-b278f444bfda` for adversarial review.
- [Gap 8][Review] Findings accepted as valid:
  - Symbol/path probe initially allowed false positives via substring matching.
  - Cache root needed safer per-run default to avoid clone race collisions.
  - Gate script should run in release mode.
- [Gap 8][Fix] Tightened probe to exact expected path matches; added per-run cache root default with env override (`RAQL_CONFORMANCE_CACHE_ROOT`); switched gate script to `cargo test --release`.
- [Gap 8][Review] Final re-review result: no actionable findings remain.
