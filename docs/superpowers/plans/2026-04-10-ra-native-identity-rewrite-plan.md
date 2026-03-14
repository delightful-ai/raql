# RA-Native Identity Rewrite Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the supported hot-path lookup layer with RA-native identity-backed `DefProvider` and `CallProvider`, removing synthetic function recovery and named-target function caller fallback from the daemon-backed runtime.

**Architecture:** Introduce a shared RA-native identity/cache model in the rust-analyzer host, then rebuild exact-name/bound-def lookup and `call_edge` lookup on top of that model. Keep wrappers deterministic, keep disabled families disabled, and preserve the daemon-backed execution boundary.

**Tech Stack:** Rust, rust-analyzer HIR/IDE APIs, RAQL compiler/engine daemon runtime, release-mode repo-scale verification.

---

## Chunk 1: Restore the best known baseline

### Task 1: Revert the current regressing `def_name` experiment

**Files:**
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Test: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`
- Test: `/Users/darin/Projects/raql/tests/cli_daemon_cutover.rs`

- [ ] **Step 1: Remove the HIR-first exact-name collector from `lookup_def_name_rows`**
- [ ] **Step 2: Restore the previous best-known release behavior for exact-name seeded lookup**
- [ ] **Step 3: Run the release repo probe**

Run: `RAQL_TRACE_TIMINGS=1 cargo test --release -p raql-host-ra workspace_service_repo_callers_probe_smoke -- --ignored --nocapture`
Expected: pass, with cold path back near the pre-regression state

- [ ] **Step 4: Run the real release cold/warm query**

Run:
```bash
target/release/raql lang run /Users/darin/Projects/raql/views/stdlib_callers_load_and_plan.raql --rust-file /Users/darin/Projects/raql/Cargo.toml
```
Expected: cold path returns to the current best band; warm path stays sub-100ms

## Chunk 2: Introduce RA-native identity entries

### Task 2: Add the identity/cache model

**Files:**
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Test: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`

- [ ] **Step 1: Add `RaEntity`, `RaSpan`, and `LookupEntry` types**
- [ ] **Step 2: Replace current lookup cache values so synthetic IDs map to `LookupEntry`**
- [ ] **Step 3: Keep deterministic wrapper IDs stable for the daemon path**
- [ ] **Step 4: Add focused tests proving function-backed entries carry `hir::Function` directly**
- [ ] **Step 5: Run the focused host tests**

Run: `cargo test -p raql-host-ra workspace_service_ -- --nocapture`
Expected: targeted lookup/cache tests pass

## Chunk 3: Rebuild `DefProvider`

### Task 3: Rewrite exact-name and bound-def lookup over RA identity

**Files:**
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Test: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`
- Test: `/Users/darin/Projects/raql/tests/cli_daemon_cutover.rs`

- [ ] **Step 1: Implement exact-name function lookup using RA-native identities**
- [ ] **Step 2: Make `def_name`, `def_kind`, `def_span`, and `def_path` derive from `LookupEntry`**
- [ ] **Step 3: Remove supported-path function dependence on rel-path/span/name recovery**
- [ ] **Step 4: Add/fix exact-name seeded daemon-path regressions**
- [ ] **Step 5: Run release verification**

Run:
```bash
RAQL_TRACE_TIMINGS=1 cargo test --release -p raql-host-ra workspace_service_repo_callers_probe_smoke -- --ignored --nocapture
cargo test --test cli_daemon_cutover lang_run_supports_stdlib_exact_name_seed_queries_on_the_daemon_path -- --nocapture
```
Expected: correctness passes; warm path does not regress

## Chunk 4: Rebuild `CallProvider`

### Task 4: Remove named-target function fallback from supported `call_edge`

**Files:**
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Test: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service_tests.rs`
- Test: `/Users/darin/Projects/raql/tests/cli_daemon_cutover.rs`

- [ ] **Step 1: Make bound-callee and bound-caller `call_edge` operate only on `RaEntity::Function`**
- [ ] **Step 2: Remove supported-path use of `lookup_function_from_record` for functions**
- [ ] **Step 3: Remove supported-path use of `collect_lookup_callers_for_named_target` for function edges**
- [ ] **Step 4: Preserve alias-based caller correctness through RA-native reference search**
- [ ] **Step 5: Run release verification**

Run:
```bash
cargo test -p raql-host-ra workspace_service_looks_up_call_edges_for_bound_callee_through_alias_reference -- --nocapture
cargo test --test cli_daemon_cutover lang_run_supports_stdlib_caller_queries_on_the_daemon_path -- --nocapture
RAQL_TRACE_TIMINGS=1 cargo test --release -p raql-host-ra workspace_service_repo_callers_probe_smoke -- --ignored --nocapture
```
Expected: correctness passes; cold path improves or at least centralizes around one honest RA primitive

## Chunk 5: Cleanup and audit update

### Task 5: Delete dead recovery code from the supported path and update the audit

**Files:**
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/docs/superpowers/specs/2026-04-09-ra-native-audit-matrix.md`

- [ ] **Step 1: Delete or quarantine synthetic function recovery that is no longer used on the supported path**
- [ ] **Step 2: Update `TODO(ra-native-audit)` markers to reflect what remains**
- [ ] **Step 3: Update the audit matrix status for the `P0` hot path**
- [ ] **Step 4: Run the final release verification**

Run:
```bash
RAQL_TRACE_TIMINGS=1 cargo test --release -p raql-host-ra workspace_service_repo_callers_probe_smoke -- --ignored --nocapture
cargo test --test cli_daemon_cutover -- --nocapture
```
Expected: supported daemon path remains correct; audit reflects the new truth

## Final release gate

- [ ] **Step 1: Build the release binary**

Run: `cargo build --release --bin raql`
Expected: success

- [ ] **Step 2: Run the real release cold/warm query**

Run:
```bash
target/release/raql lang run /Users/darin/Projects/raql/views/stdlib_callers_load_and_plan.raql --rust-file /Users/darin/Projects/raql/Cargo.toml
target/release/raql lang run /Users/darin/Projects/raql/views/stdlib_callers_load_and_plan.raql --rust-file /Users/darin/Projects/raql/Cargo.toml
```
Expected:
- warm path does not regress from the current best `~59ms`
- cold path improves from the current best `~4.1s`, or at minimum the remaining cost is isolated to one honest RA primitive
