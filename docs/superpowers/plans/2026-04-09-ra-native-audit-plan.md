# RA-Native Audit and Correctness-First Hardening Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Audit every semantic and frontend path that should defer to rust-analyzer, disable approximate stdlib families, and replace custom logic with RA-native truth while establishing measurable latency budgets.

**Architecture:** The daemon-backed runtime remains the only supported execution path. Each semantic family is audited against the RA-native rule, then either kept, rewritten as a thin deterministic wrapper over RA truth, or disabled through capability gating. Query frontend work is cached inside the daemon, and workspace truth shifts from raw filesystem scans to RA/Cargo/VFS-backed state.

**Tech Stack:** Rust, rust-analyzer HIR/IDE/VFS/Cargo state, RAQL compiler/daemon/runtime crates, beads tracker, jj.

---

## Chunk 1: Audit Matrix and Truth Inventory

### Task 1: Write the audit matrix document in-repo

**Files:**
- Modify: `docs/superpowers/specs/2026-04-09-ra-native-audit-design.md`
- Create: `docs/superpowers/specs/2026-04-09-ra-native-audit-matrix.md`

- [ ] **Step 1: Write the initial audit matrix**
- Enumerate every supported stdlib family and every daemon-path runtime/frontend responsibility.
- Record current source of truth, RA-native target, action, and ship state.

- [ ] **Step 2: Add explicit known-gap rows**
- Include call graph, reference events, error flow, workspace scope, reload/invalidation, syntax nodes, query planning cache, and stable IDs.

- [ ] **Step 3: Review the matrix against current code**
Run: `rg -n "call_edge|compares|writes|constructs|node_at|sync_workspace|load_and_plan" crates std.raql tests`
Expected: every matching family has a matrix row.

### Task 2: Add audit-state enforcement comments/TODO markers

**Files:**
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-daemon/src/lib.rs`
- Modify: `crates/raql-host-ra/src/capability.rs`

- [ ] **Step 1: Mark known approximate families inline**
- Add TODO comments where current logic is knowingly heuristic and must be replaced or disabled.

- [ ] **Step 2: Mark non-RA-native workspace and planning paths inline**
- Add TODO comments to raw scan/rebuild/planning hotspots.

- [ ] **Step 3: Verify only intended files changed**
Run: `jj status`
Expected: only the audit/docs/source files above are modified.

## Chunk 2: Disable Approximate Families Until RA-Native

### Task 3: Disable reference-event family if it remains approximate

**Files:**
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/capability_gating_tests.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] **Step 1: Write failing capability tests for disabled approximate reference family**
- Assert `compares`, `writes`, and `ref_id` are rejected if still backed by approximate summaries.

- [ ] **Step 2: Verify red**
Run: `cargo test -p raql-host-ra reference -- --nocapture`
Expected: failure showing the family is still incorrectly enabled.

- [ ] **Step 3: Disable through capability gating**
- Remove/withhold capabilities until the family is RA-native.

- [ ] **Step 4: Verify green**
Run: `cargo test -p raql-host-ra capability_gating_tests::workspace_service_rejects_unknown_capabilities_during_run -- --nocapture`
Expected: explicit unsupported diagnostics.

### Task 4: Narrow or disable error-flow claims that are not RA-native

**Files:**
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/capability_gating_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] **Step 1: Write failing truth tests for unsupported carrier/conversion cases**
- Include alias/wrapper/manual-conversion cases that current Result/From heuristics miss.

- [ ] **Step 2: Verify red**
Run: `cargo test -p raql-host-ra error_flow -- --nocapture`
Expected: failure proving overclaim.

- [ ] **Step 3: Reduce the enabled surface to honest cases**
- Either disable the family temporarily or gate a narrower subset with explicit semantics.

- [ ] **Step 4: Verify green**
Run: `cargo test -p raql-host-ra error_flow -- --nocapture`
Expected: all enabled behavior is honest and tested.

## Chunk 3: Replace Non-RA Workspace Truth

### Task 5: Rewrite workspace membership/scope to use RA/Cargo/VFS truth only

**Files:**
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_loader.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`

- [ ] **Step 1: Write failing tests for stray-file churn and non-member file influence**
- Add cases where stray `.rs`/Cargo-ish files under scan roots must not trigger reload or alter observed scope.

- [ ] **Step 2: Verify red**
Run: `cargo test -p raql-host-ra workspace_service_ -- --nocapture`
Expected: failures showing raw filesystem scope leakage.

- [ ] **Step 3: Replace raw scan-root truth with RA/Cargo/VFS-backed scope**
- Remove filesystem membership as semantic truth.
- Use loaded workspace/VFS/Cargo model as the authoritative set.

- [ ] **Step 4: Verify green**
Run: `cargo test -p raql-host-ra workspace_service_ -- --nocapture`
Expected: stray files do not perturb steady-state scope or reload behavior.

### Task 6: Tighten build-script invalidation to honest watched inputs only

**Files:**
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`

- [ ] **Step 1: Write failing tests for `rerun-if-env-changed` and conditional build inputs**
- Add explicit cases that should invalidate or stay stable.

- [ ] **Step 2: Verify red**
Run: `cargo test -p raql-host-ra generated_build -- --nocapture`
Expected: failure exposing stale or overbroad invalidation.

- [ ] **Step 3: Replace heuristic invalidation with tighter watched-input state**
- Use the strongest RA/Cargo-derived truth available, with minimal wrappers.

- [ ] **Step 4: Verify green**
Run: `cargo test -p raql-host-ra generated_build -- --nocapture`
Expected: only declared relevant changes trigger reload.

## Chunk 4: Make Supported Semantic Families More RA-Native

### Task 7: Fix call-graph attribution boundaries

**Files:**
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`
- Reference: `tmp/rust-analyzer`

- [ ] **Step 1: Write failing truth tests for nested items, closures, and async blocks**
- Separate outer caller attribution from closure/nested-body attribution.

- [ ] **Step 2: Verify red**
Run: `cargo test -p raql-host-ra call_graph -- --nocapture`
Expected: failures showing over-attribution or dropped bodies.

- [ ] **Step 3: Rewrite extraction against RA-owned callable boundaries**
- Prefer RA/HIR ownership and callable structure over raw descendant walks.

- [ ] **Step 4: Verify green**
Run: `cargo test -p raql-host-ra call_graph -- --nocapture && cargo test --test cli_daemon_cutover lang_run_supports_call_graph_queries_on_the_daemon_path -- --nocapture`
Expected: accurate caller attribution in supported cases.

### Task 8: Replace raw-text syntax extraction with RA-backed syntax truth

**Files:**
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] **Step 1: Write failing tests for node/control attribution that depend on the retained RA parse tree**
- Include edge cases near macros/expansion or nested control structures where raw parse duplication is risky.

- [ ] **Step 2: Verify red**
Run: `cargo test -p raql-host-ra syntax_control -- --nocapture`
Expected: failures showing mismatch with RA-backed structure.

- [ ] **Step 3: Rebuild syntax/node extraction over RA-backed parse state**
- Eliminate the duplicate raw `syntax::SourceFile::parse` pass when possible.

- [ ] **Step 4: Verify green**
Run: `cargo test -p raql-host-ra syntax_control -- --nocapture`
Expected: node/control facts come from RA-backed syntax truth.

## Chunk 5: Daemon Frontend and Hot-Path Lifecycle

### Task 9: Cache compiled/planned queries in the daemon

**Files:**
- Modify: `crates/raql-daemon/src/lib.rs`
- Modify: `crates/raql-protocol/src/lib.rs` if needed
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] **Step 1: Write failing latency-oriented tests for repeated identical queries**
- Add instrumentation or assertions proving the compile pipeline reruns today.

- [ ] **Step 2: Verify red**
Run: `cargo test --test cli_daemon_cutover lang_run_is_daemon_backed_and_reuses_warm_session -- --nocapture`
Expected: evidence of repeated frontend compilation or lack of reuse.

- [ ] **Step 3: Add daemon-local cache keyed by query content, include graph, and options**
- Reuse parse/resolve/typecheck/plan results safely.

- [ ] **Step 4: Verify green**
Run: `cargo test --test cli_daemon_cutover -- --nocapture`
Expected: repeated queries reuse planned state without semantic drift.

### Task 10: Stop rebuilding the deterministic host on ordinary warm-path sync

**Files:**
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`

- [ ] **Step 1: Write failing latency/correctness tests around one-line source edits and no-op runs**
- Distinguish no-op queries, minor edits, and workspace-shape changes.

- [ ] **Step 2: Verify red**
Run: `cargo test -p raql-host-ra workspace_service_tracks_incremental_rust_file_edits -- --nocapture`
Expected: evidence that current path rebuilds too aggressively.

- [ ] **Step 3: Narrow invalidation and preserve host state where safe**
- Avoid dropping `core_host` on changes that can be reconciled incrementally.

- [ ] **Step 4: Verify green**
Run: `cargo test -p raql-host-ra workspace_service_tracks_incremental_rust_file_edits -- --nocapture`
Expected: correctness preserved with materially less rebuild churn.

## Chunk 6: Latency Budget and Corpus Evidence

### Task 11: Add bounded latency sampling for supported daemon-path queries

**Files:**
- Create: `tests/latency_smoke.rs` or `xtask`/script equivalent
- Modify: `docs/superpowers/specs/2026-04-09-ra-native-audit-matrix.md`

- [ ] **Step 1: Record baseline latency against the four target buckets**
- Warm query
- Cold first query
- Single-file edit refresh
- Workspace-shape reload

- [ ] **Step 2: Add a repeatable measurement harness**
- Keep it bounded and CI-safe; it may report metrics rather than hard-fail initially.

- [ ] **Step 3: Verify output is readable and attributable**
Run: the chosen latency harness command
Expected: concrete numbers for each bucket.

### Task 12: Run representative stdlib corpus queries against `raql` and `tmp/rust-analyzer`

**Files:**
- Modify: `views/stdlib_callers_load_and_plan.raql` if needed only for honest semantics
- Modify: `views/stdlib_dispatch_hotspots.raql` if needed only for honest semantics
- Modify: `views/stdlib_error_conversions.raql` if needed only for honest semantics
- Create: `docs/superpowers/specs/2026-04-09-ra-native-audit-corpus-results.md`

- [ ] **Step 1: Run the representative corpus on `raql`**
- Capture status, timing, and result shape.

- [ ] **Step 2: Run the same corpus on `tmp/rust-analyzer`**
- Capture status, timing, and result shape.

- [ ] **Step 3: Record failures as either disabled surfaces or correctness bugs**
- No hand-waving and no silent approximation.

## Chunk 7: Review and Closeout

### Task 13: Run adversarial review on the audited state

**Files:**
- No code changes required unless findings demand them

- [ ] **Step 1: Dispatch adversarial reviewers for latency and semantic correctness**
- Focus on remaining custom logic that RA should own.

- [ ] **Step 2: Fix any confirmed high-severity findings**
- Keep the audit matrix updated with outcomes.

- [ ] **Step 3: Re-run full verification**
Run: `cargo test -p raql-host-ra && cargo test --test cli_daemon_cutover -- --nocapture && cargo test --test cli_strict_runtime -- --nocapture`
Expected: all green.

### Task 14: Sync tracker and summarize audited surface

**Files:**
- Modify: `docs/superpowers/specs/2026-04-09-ra-native-audit-matrix.md`

- [ ] **Step 1: Mark each family final state in the audit matrix**
- enabled RA-native
- enabled thin wrapper
- disabled pending rewrite

- [ ] **Step 2: Update beads state for the epic and children**
Run: `bd show <epic>` and close completed beads.
Expected: tracker matches actual repo state.

- [ ] **Step 3: Final verification and status summary**
Run: `jj status`
Expected: only intended files remain modified.
