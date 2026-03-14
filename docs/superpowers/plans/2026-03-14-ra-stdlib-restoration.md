# RA-backed Stdlib Restoration Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore the remaining `std.raql` extern families on the supported daemon-backed incremental rust-analyzer runtime without changing the stdlib surface.

**Architecture:** Keep `std.raql` source-stable and rebuild support underneath it by expanding `WorkspaceService` population and capability gating. Start with families where `DeterministicRaHost` already has concrete row generation and only lacks capability exposure, then add RA-native extraction in `CoreFactsBuilder` for the families that currently have empty storage.

**Tech Stack:** Rust, rust-analyzer `hir`/`ide` APIs, RAQL compiler/engine, beads tracker, `jj`.

---

## File map

- Modify: `crates/raql-host-ra/src/capability.rs`
  - Source of truth for supported daemon capabilities.
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
  - `CoreFactsBuilder` and workspace-backed fact population.
- Modify: `crates/raql-host-ra/src/lib.rs`
  - Host storage helpers already exist; only touch if a family needs a new insertion/helper seam that `WorkspaceService` cannot express cleanly.
- Modify: `crates/raql-host-ra/src/capability_gating_tests.rs`
  - Capability gating expectations as families come online.
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
  - In-process supported-path coverage for populated families.
- Modify: `tests/cli_daemon_cutover.rs`
  - Public daemon-backed regression coverage.
- Modify: `tests/cli_strict_runtime.rs`
  - Public failure-path coverage as unsupported families disappear.

## Chunk 1: Cheap enablement and foundational population

### Task 1: `raql-dc3.2` Restore `search/3`

**Files:**
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/capability_gating_tests.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] Write a failing capability-gating test that proves `search` is currently unsupported.
- [ ] Write a failing workspace-service test that expects `search("alpha", ...)` to return the known `alpha` def for a simple workspace.
- [ ] Write a failing daemon-backed CLI test that runs a `search/3` query through `raql lang run` and expects a hit.
- [ ] Add `search` to `DAY_ONE_SUPPORTED_CAPABILITIES` in `crates/raql-host-ra/src/capability.rs`.
- [ ] Run the targeted tests and make them pass without touching `std.raql`.
- [ ] Verify with:
  - `cargo test -p raql-host-ra capability_gating_tests::unsupported_stdlib_capabilities_fail_explicitly -- --nocapture`
  - `cargo test -p raql-host-ra workspace_service_search -- --nocapture`
  - `cargo test --test cli_daemon_cutover search -- --nocapture`

### Task 2: `raql-dc3.5` Restore structure and trait family

**Files:**
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`
- Modify: `tests/cli_strict_runtime.rs`

- [ ] Write failing tests for `field/3`, `variant/3`, `implements/3`, and `from_impl/3` on a small workspace fixture.
- [ ] Extend `CoreFactsBuilder::process_adt` to emit field and variant records using existing host insertors.
- [ ] Extend impl extraction to emit `implements/3` and `from_impl/3` rows for trait impls, especially `From` impls.
- [ ] Add `method`, `trait_method`, `field`, `variant`, `implements`, and `from_impl` to capability gating once their tests pass.
- [ ] Add one daemon-backed CLI regression that exercises a trait/impl query through `std.raql`.
- [ ] Verify with targeted host-ra tests, then the public CLI suites.

### Task 3: `raql-dc3.3` Restore type surface family

**Files:**
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] Write failing tests for `fn_return_type`, `fn_error_type`, and representative `ty_*` predicates (`ty_app`, `ty_arg`, `ty_ref`, `ty_ptr`, `ty_tuple`, `ty_slice`, `ty_param`, `ty_prim`, `ty_unknown`).
- [ ] Add a `CoreFactsBuilder` type-extraction pass that interns `TypeRefId`s for function returns, field types, and the type decomposition tree using existing host `insert_type`, `set_fn_return_type`, `set_fn_error_type`, and `insert_typeref_id` helpers.
- [ ] Keep the extraction centered on `hir::Type`/`Semantics` entry points from the bundled rust-analyzer sources; do not add fallback string parsing.
- [ ] Add the type-family capabilities to `capability.rs` only after the population tests pass.
- [ ] Add a daemon-backed CLI regression that proves `std.raql` type helpers work on a real workspace.
- [ ] Verify targeted tests, then full daemon/host/CLI suites.

## Chunk 2: Syntax/call/reference semantics on top of the foundations

### Task 4: `raql-dc3.6` Restore syntax-tree and control-context family

**Files:**
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] Write failing tests for `node_at`, `node_kind`, `node_span`, `node_parent`, `enclosing_control`, and `node_id`.
- [ ] Add a syntax-tree walk in `CoreFactsBuilder` that interns nodes for tracked files, records parent relationships, and classifies control nodes into the `NodeKind` enum.
- [ ] Reuse existing span interning so `node_at` works over supported `Span` values instead of inventing parallel coordinates.
- [ ] Enable the node/control capabilities only after the node population tests pass.
- [ ] Add a daemon-backed CLI regression using a control-context query from `std.raql`.
- [ ] Verify targeted tests, then full suites.

### Task 5: `raql-dc3.1` Restore call graph provider family

**Files:**
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`
- Modify: `tests/cli_strict_runtime.rs`

- [ ] Write failing tests for `call_edge/4`, `call_id/2`, and `dispatch_str/2` on direct calls and method calls.
- [ ] Implement call extraction using the RA call hierarchy / `Semantics` callable resolution patterns from `tmp/rust-analyzer/crates/ide/src/call_hierarchy.rs`.
- [ ] Classify dispatch into the existing `DispatchKind` values conservatively; prefer correct `DIRECT` / `THROUGH_TRAIT` coverage first and add richer classifications only when directly derivable.
- [ ] Populate stable call IDs with existing host helpers.
- [ ] Enable `call_edge`, `call_id`, and keep `dispatch_str` supported.
- [ ] Add a daemon-backed CLI regression for `caller/4` and `callee/4` through `std.raql`.
- [ ] Verify targeted tests, then full suites.

### Task 6: `raql-dc3.4` Restore reference-event family

**Files:**
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] Write failing tests for `compares/4`, `writes/3`, and `ref_id/2`.
- [ ] Implement reference extraction on top of RA usage/reference search plus syntax classification for compare/write events.
- [ ] Emit stable `RefId`s for collected reference events.
- [ ] Add the reference-event capabilities to `capability.rs` once populated.
- [ ] Add a daemon-backed CLI regression using `std.raql` compare/write queries.
- [ ] Verify targeted tests, then full suites.

## Chunk 3: Error-flow family and epic closeout

### Task 7: `raql-dc3.7` Restore error-flow family

**Files:**
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] Write failing tests for `constructs/4`, `propagates/3`, `converts/4`, and `handles/4` against focused error-handling fixtures.
- [ ] Build error-flow extraction on top of the restored call/type/impl/reference foundations rather than bespoke string heuristics.
- [ ] Reuse `from_impl/3` and `fn_error_type/2` where possible so error-flow logic composes from earlier families.
- [ ] Enable the error-flow capabilities only after all four predicates are populated.
- [ ] Add a daemon-backed CLI regression for `error_edge/4` derived behavior through `std.raql`.
- [ ] Verify targeted tests, then full suites.

### Task 8: Epic verification and closeout

**Files:**
- Modify: tracker only unless review finds code gaps.

- [ ] Run the full verification set:
  - `cargo test -p raql-daemon`
  - `cargo test -p raql-host-ra`
  - `cargo test --test cli_daemon_cutover -- --nocapture`
  - `cargo test --test cli_strict_runtime -- --nocapture`
- [ ] Run adversarial review over daemon lifecycle, workspace-service semantics, and supported-path stdlib coverage.
- [ ] Turn any real findings into new `raql-dc3.*` beads immediately and fix them before closing the epic.
- [ ] Add audit comments and close child beads honestly.
- [ ] Close `raql-dc3` only after tracker state, tests, and supported daemon behavior agree.
