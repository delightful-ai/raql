# RA-Native Provider Split Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the remaining custom semantic rediscovery in `raql-host-ra` with RA-native providers and split `workspace_service.rs` into focused modules without regressing the daemon-backed public path.

**Architecture:** Supported lookup and extraction paths should be driven by rust-analyzer identity (`hir::ModuleDef`, `hir::Function`, HIR types, RA syntax ranges) plus thin deterministic wrappers for `DefId`, `SpanId`, and wire formats. `WorkspaceService` should become orchestration only: provider modules own discovery and query-shaped lookups, the daemon owns lifecycle, and the engine owns RAQL execution semantics. Bound lookups must hit RA-built indexes instead of reverse search.

**Tech Stack:** Rust, rust-analyzer HIR/IDE/VFS/Cargo APIs, RAQL daemon/runtime, `jj`, `bd`

---

## AGENTS contract to codify

These are the exact phrases the repo should now treat as law:

> "given only the relevant code plus the stacked `agents.md` files from root to the working directory, a smart stranger should be able to make the right change in the right place, in the house style, without copying legacy nonsense, and with a cheap way to prove they didn’t break the wrong thing."

> "The hierarchy rule is highest stable truth, not highest possible truth."

> "Child files should mostly contain delta, not duplication."

> "On-touch rule: if you change a directory's boundary, canonical pattern, verification command, or known hazard, update the nearest relevant `AGENTS.md` in the same change."

Repo-specific verbatim rules that should now anchor implementation work:

> "Semantic truth for supported surfaces comes from rust-analyzer state: HIR, IDE, VFS, and Cargo/workspace model."

> "Thin deterministic wrappers are allowed only for stable IDs, normalization, and transport. They must not invent semantic truth."

> "If a primitive is not RA-native and honest, disable it rather than keep an approximate custom extractor alive."

> "Do not keep adding responsibilities to `crates/raql-host-ra/src/workspace_service.rs`. If a change adds a new semantic family or proof model, split it into a focused provider/module instead of extending the blob."

## File map

### Root guidance and hierarchy

- Modify: `/Users/darin/Projects/raql/AGENTS.md`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/AGENTS.md`
- Create: `/Users/darin/Projects/raql/crates/raql-daemon/AGENTS.md`
- Create: `/Users/darin/Projects/raql/crates/raql-engine/AGENTS.md`

### Host/provider split

- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/lazy_runtime.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/identity.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/mod.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/defs.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/calls.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/core_index.rs`

### Follow-on provider split

- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/structure.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/types.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/syntax.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/search.rs`

### Tests and probes

- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `/Users/darin/Projects/raql/tests/cli_daemon_cutover.rs`
- Use: `/Users/darin/Projects/raql/views/stdlib_callers_load_and_plan.raql`

## P0: Kill the remaining non-RA truth in the hot path

### Task P0.1: Codify the hierarchy and ownership rules

**Files:**
- Modify: `/Users/darin/Projects/raql/AGENTS.md`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/AGENTS.md`
- Create: `/Users/darin/Projects/raql/crates/raql-daemon/AGENTS.md`
- Create: `/Users/darin/Projects/raql/crates/raql-engine/AGENTS.md`

- [ ] Write the child `AGENTS.md` files as local delta, not root duplication.
- [ ] Make `crates/raql-host-ra/AGENTS.md` say that provider modules own semantic families and `workspace_service.rs` is orchestration only.
- [ ] Make `crates/raql-daemon/AGENTS.md` say lifecycle/warmup/protocol belong there and semantic extraction does not.
- [ ] Make `crates/raql-engine/AGENTS.md` say execution semantics and lookup contracts belong there and Rust semantic discovery does not.
- [ ] Verify referenced paths/commands still exist and still reflect the current workflow.
- [ ] `jj describe` this hierarchy slice and `jj new`.

### Task P0.2: Extract shared RA identity and bound lookup indexes

**Files:**
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/identity.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/core_index.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs`

- [ ] Write a failing test proving bound `Def` lookups must answer without reverse search.
- [ ] Move RA identity types (`ModuleDef`-backed entity, span wrapper, lookup record) into `identity.rs`.
- [ ] Add a core-host-backed bound lookup index for `DefId -> name/path/kind/public/test` in `provider/core_index.rs`.
- [ ] Teach the supported `def_name(+Def, -string)` and `def_path(+Def, -string)` paths to answer from that index first.
- [ ] Delete any remaining bound lookup code that re-searches by exact name or text once a `DefId` is already known.
- [ ] Re-run the focused bound lookup tests and update any stale notes/TODOs.
- [ ] `jj describe` this slice and `jj new`.

### Task P0.3: Split `DefProvider` out of `workspace_service.rs`

**Files:**
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/defs.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`

- [ ] Write a failing test for any still-supported exact-name path that falls through text scans or AST rediscovery.
- [ ] Move exact-name `def`, `def_name`, `def_kind`, `def_span`, and `def_path` lookup code into `provider/defs.rs`.
- [ ] Make exact-name discovery RA-symbol/HIR-backed end to end for supported defs.
- [ ] Keep only thin deterministic wrappers for `DefId` and `SpanId`.
- [ ] Delete the tracked-file text scan and AST fallback path for supported exact-name def lookup.
- [ ] Verify:
Run: `cargo test -p raql-host-ra workspace_service_reports_supported_def_paths_for_exact_name_seeded_structs -- --nocapture`
Expected: PASS
- [ ] Verify:
Run: `cargo test -p raql-host-ra workspace_service_supports_structure_and_trait_rows -- --nocapture`
Expected: PASS
- [ ] `jj describe` this slice and `jj new`.

### Task P0.4: Split `CallProvider` out of `workspace_service.rs`

**Files:**
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/calls.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/lazy_runtime.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `/Users/darin/Projects/raql/tests/cli_daemon_cutover.rs`

- [ ] Write a failing test for the supported caller/callee path that still relies on synthetic recovery or non-RA identity.
- [ ] Move bound caller/callee lookup and call-edge collection into `provider/calls.rs`.
- [ ] Keep supported `call_edge` paths strictly on RA function identity plus RA usages/semantic resolution.
- [ ] Delete any remaining supported-path function recovery from spans, names, or text.
- [ ] Verify:
Run: `cargo test -p raql-host-ra workspace_service_looks_up_call_edges_for_bound_callee -- --nocapture`
Expected: PASS
- [ ] Verify:
Run: `cargo test --test cli_daemon_cutover lang_run_supports_stdlib_caller_queries_on_the_daemon_path -- --nocapture`
Expected: PASS
- [ ] Measure:
Run the release binary against `/Users/darin/Projects/raql/views/stdlib_callers_load_and_plan.raql`
Expected: warm path stays in the current `~50-70ms` band
- [ ] `jj describe` this slice and `jj new`.

### Task P0.5: Stop using broad filesystem truth for supported workspace sync

**Files:**
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`

- [ ] Write a failing test for a workspace-change scenario that currently depends on tracked-file rescans instead of RA/VFS/project-folder truth.
- [ ] Narrow supported sync/invalidation to RA watch entries, VFS file ids, and explicit workspace-shape sentinels.
- [ ] Keep raw filesystem scans only where there is no RA/Cargo alternative yet, and mark those seams explicitly.
- [ ] Verify:
Run: `cargo test -p raql-host-ra workspace_service_tracks_incremental_rust_file_edits -- --nocapture`
Expected: PASS
- [ ] Verify:
Run: `cargo test -p raql-host-ra workspace_service_from_member_manifest_detects_new_sibling_member_files -- --nocapture`
Expected: PASS
- [ ] `jj describe` this slice and `jj new`.

## P1: Finish the provider split and kill the blob

### Task P1.1: Split structure and type families into providers

**Files:**
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/structure.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/types.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`

- [ ] Move `field`, `variant`, `method`, `trait_method`, `implements`, `from_impl`, `fn_return_type`, `fn_error_type`, and the `ty_*` family into provider modules.
- [ ] Make provider inputs/outputs `ModuleDef`/HIR-type-based instead of ad hoc per-call glue.
- [ ] Verify:
Run: `cargo test -p raql-host-ra workspace_service_supports_structure_and_trait_rows -- --nocapture`
Expected: PASS
- [ ] Verify:
Run: `cargo test -p raql-host-ra workspace_service_supports_type_surface_rows -- --nocapture`
Expected: PASS
- [ ] `jj describe` this slice and `jj new`.

### Task P1.2: Split syntax/search families into providers

**Files:**
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/syntax.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/provider/search.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `/Users/darin/Projects/raql/tests/cli_daemon_cutover.rs`

- [ ] Keep syntax answers on RA parse tree identity only.
- [ ] Keep search answers on RA symbol/search substrate only.
- [ ] Delete any remaining node/search path that re-parses or re-scans text as the supported truth.
- [ ] Verify:
Run: `cargo test -p raql-host-ra workspace_service_supports_syntax_control_rows -- --nocapture`
Expected: PASS
- [ ] Verify:
Run: `cargo test --test cli_daemon_cutover lang_run_supports_syntax_control_queries_on_the_daemon_path -- --nocapture`
Expected: PASS
- [ ] `jj describe` this slice and `jj new`.

### Task P1.3: Shrink `workspace_service.rs` to orchestration only

**Files:**
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs`

- [ ] Remove provider-internal helpers from `workspace_service.rs`.
- [ ] Keep only:
  - workspace/session orchestration
  - cache ownership
  - provider dispatch
  - daemon-facing lifecycle hooks
- [ ] Update any now-stale TODO markers or comments that still assume the blob owns semantic logic.
- [ ] Run a release build and keep the public probe in the current cold/warm envelope or better.
- [ ] `jj describe` this slice and `jj new`.

### Task P1.4: Final verification and tracker hygiene

**Files:**
- Modify only if needed for fixes from verification

- [ ] Verify:
Run: `cargo test -p raql-host-ra -- --nocapture`
Expected: PASS
- [ ] Verify:
Run: `cargo test --test cli_daemon_cutover -- --nocapture`
Expected: PASS
- [ ] Verify:
Run: `cargo build --release --bin raql`
Expected: PASS
- [ ] Measure:
Run the release binary against `/Users/darin/Projects/raql/views/stdlib_callers_load_and_plan.raql`
Expected: warm remains in budget and cold does not regress materially from the current `~4.2s` band
- [ ] If any follow-on work appears, file a `bd` bead immediately instead of hiding it in commit messages.
- [ ] Close the relevant bead(s) only after the acceptance gates above are actually green.

## Execution notes

- Use `jj` checkpoints aggressively. Do not stack another long experiment on top of a dirty rewrite state.
- If a provider answer cannot be made RA-native and honest in the slice you are touching, disable that surface instead of keeping a custom approximation alive.
- If the same AGENTS rule becomes true for `crates/raql-host-ra`, `crates/raql-daemon`, and `crates/raql-engine`, promote it; otherwise keep it in the child file as local delta.

Plan complete and saved to `/Users/darin/Projects/raql/docs/superpowers/plans/2026-04-11-ra-native-provider-split-plan.md`. Ready to execute?
