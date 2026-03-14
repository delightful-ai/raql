# Host/Engine Pushdown for RA-Native Cold-Path Performance

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate universe-shaped host materialization for supported extern predicates by pushing bound-argument lookups through the RAQL engine/host boundary, so cold repo queries stop paying `world_symbols("")` for exact or narrow lookups.

**Why now:** Current measurements on `/Users/darin/Projects/raql` show the supported in-process warm path is already acceptable (`~69ms`), but the cold path is catastrophically dominated by `workspace_service.populate_defs_from_symbols.world_symbols` (`~57.4s`). That means the next architectural move is not more warm-path tuning. We need the engine to ask the host for the rows it actually needs instead of forcing whole-relation materialization.

**Architecture:** Keep the daemon-backed incremental rust-analyzer runtime as the only supported execution path. Extend the host contract so extern functions and selected relations can answer lookup-shaped requests from bound arguments. The compiler/planner extracts pushdown-safe lookup shapes from the query plan, the engine issues lookup requests instead of scanning full extern relations, and `raql-host-ra` answers those lookups with RA-native indexed queries. Unsupported or non-pushdownable cases continue to use honest capability gating or explicit fallback-to-materialize within the supported runtime only when semantics are still RA-native.

**Tech Stack:** Rust, RAQL compiler/engine/host crates, rust-analyzer HIR/IDE symbol index, daemon runtime, beads tracker, jj.

---

## Chunk 1: Lock the pushdown contract

### Task 1: Define the host lookup contract and pushdown-safe shapes

**Files:**
- Modify: `crates/raql-host/src/lib.rs`
- Modify: `docs/superpowers/specs/2026-04-09-ra-native-audit-matrix.md`
- Create: `docs/superpowers/specs/2026-04-09-host-engine-pushdown-design.md`

- [ ] **Step 1: Specify the lookup request/response contract**
- Define the smallest host API that can answer bound-argument extern lookups without materializing the full relation.
- Include exact-match function lookups first; leave broader relation pushdown as an explicit future extension.

- [ ] **Step 2: Enumerate pushdown-safe shapes**
- Identify the first supported shapes, starting with exact bound-argument bindings for function externs and tightly bounded relation probes where semantics are unambiguous.

- [ ] **Step 3: Record the contract in the audit matrix**
Run: `rg -n "def_name|call_edge|field|search|contains\(|starts_with\(" docs/superpowers/specs/2026-04-09-ra-native-audit-matrix.md crates`
Expected: every candidate pushdown family has an explicit audit entry and support stance.

### Task 2: Add failing engine/host tests for lookup-shaped extern evaluation

**Files:**
- Modify: `crates/raql-engine/src/tests.rs`
- Modify: `crates/raql-host/src/lib.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`

- [ ] **Step 1: Add engine unit tests for bound-argument function pushdown**
- Cover exact bound-argument lookups, constant-output filtering, repeated evaluation, and no-result cases.

- [ ] **Step 2: Add host-ra repo probe tests that prove full materialization is still happening today**
- Use the existing repo-scale defs probe and a narrower exact-name probe as red tests for the old path.

- [ ] **Step 3: Verify red**
Run: `cargo test -p raql-engine pushdown -- --nocapture && RAQL_TRACE_TIMINGS=1 cargo test -p raql-host-ra workspace_service_repo_defs_probe_smoke -- --ignored --nocapture`
Expected: engine tests fail for missing lookup support and the repo probe still shows `world_symbols("")` dominating cold runs.

## Chunk 2: Compiler and engine pushdown plumbing

### Task 3: Teach the compiler/planner to extract lookup shapes from bound extern calls

**Files:**
- Modify: `crates/raql-compiler/src/lib.rs`
- Modify: `crates/raql-ir` if needed
- Modify: `crates/raql-protocol/src/lib.rs` if daemon payloads need new plan metadata

- [ ] **Step 1: Extract pushdown-eligible extern call shapes from planned queries**
- Track predicate, bound positions, literal equality constraints, and any other bindings needed for a safe lookup.

- [ ] **Step 2: Keep the plan honest**
- Unsupported shapes must stay non-pushdown and explicit; do not silently widen them into partial scans.

- [ ] **Step 3: Verify green**
Run: `cargo test -p raql-compiler -- --nocapture`
Expected: plan metadata includes pushdown shapes only where the query actually guarantees them.

### Task 4: Add engine support for host lookup requests before relation scans

**Files:**
- Modify: `crates/raql-engine/src/lib.rs`
- Modify: `crates/raql-engine/src/tests.rs`
- Modify: `crates/raql-host/src/lib.rs`

- [ ] **Step 1: Implement lookup-first evaluation for pushdown-marked extern goals**
- The engine should call the host with bound arguments and consume only the returned rows.

- [ ] **Step 2: Preserve existing semantics for non-pushdown cases**
- If a goal is not pushdown-safe, the engine continues through the existing supported path rather than inventing new semantics.

- [ ] **Step 3: Verify green**
Run: `cargo test -p raql-engine -- --nocapture`
Expected: pushdown cases no longer scan the entire host relation and existing non-pushdown coverage still passes.

## Chunk 3: RA-native host implementations

### Task 5: Implement a query-shaped `def` / `def_name` provider over RA symbol search

**Files:**
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Reference: `tmp/rust-analyzer`

- [ ] **Step 1: Replace exact-name `def_name` cold-path materialization with indexed lookup**
- Use rust-analyzer symbol search APIs with the actual requested name instead of `world_symbols("")`.

- [ ] **Step 2: Derive matching `def` rows from the looked-up symbol set**
- Do not enumerate the whole workspace when the query only asks for a bound name.

- [ ] **Step 3: Verify green**
Run: `cargo test -p raql-host-ra workspace_service_reports_supported_def_paths -- --nocapture && RAQL_TRACE_TIMINGS=1 cargo test -p raql-host-ra workspace_service_repo_defs_probe_smoke -- --ignored --nocapture`
Expected: exact-name cold probe no longer spends tens of seconds in `world_symbols("")`; warm probe remains correct.

### Task 6: Extend pushdown to the next honest extern families only if RA-native

**Files:**
- Modify: `crates/raql-host-ra/src/workspace_service.rs`
- Modify: `crates/raql-host-ra/src/capability.rs`
- Modify: `crates/raql-host-ra/src/workspace_service_tests.rs`
- Modify: `tests/cli_daemon_cutover.rs`

- [ ] **Step 1: Audit candidate families**
- Evaluate `field`, `search`, and other bound-argument externs for real RA-native lookup support.

- [ ] **Step 2: Implement only the honest ones**
- Any family that still needs approximate semantics stays disabled.

- [ ] **Step 3: Verify green**
Run: `cargo test -p raql-host-ra -- --nocapture && cargo test --test cli_daemon_cutover -- --nocapture`
Expected: supported pushdown families work end to end; approximate families remain rejected.

## Chunk 4: Daemon reuse, measurements, and repo-scale gates

### Task 7: Wire daemon-side plan/lookup reuse and measure cold vs warm paths

**Files:**
- Modify: `crates/raql-daemon/src/lib.rs`
- Modify: `tests/cli_daemon_cutover.rs`
- Modify: `docs/superpowers/specs/2026-04-09-host-engine-pushdown-design.md`

- [ ] **Step 1: Reuse pushdown-capable planned queries inside the daemon**
- Keep the existing planned-query cache honest when lookup metadata is part of the plan.

- [ ] **Step 2: Add end-to-end latency probes for the supported daemon path**
- Measure cold first query, immediate warm rerun, and an unchanged third run.

- [ ] **Step 3: Verify green**
Run: `cargo test --test cli_daemon_cutover -- --nocapture`
Expected: exact-name cold queries improve materially and warm queries stay sub-second in the repo-scale probe.

### Task 8: Lock repo-scale verification gates before closing the epic

**Files:**
- Modify: `docs/superpowers/specs/2026-04-09-ra-native-audit-corpus-results.md` if needed
- Modify: `views/stdlib_callers_load_and_plan.raql` only if semantics require an honest query rewrite
- Modify: `views/stdlib_dispatch_hotspots.raql` only if semantics require an honest query rewrite

- [ ] **Step 1: Run the representative repo probes end to end**
- Include the minimal exact-name defs probe and at least one real stdlib query against `/Users/darin/Projects/raql`.

- [ ] **Step 2: Record measured outcomes against the budget**
- Cold exact-name query target: materially below the old `~57s` wall.
- Warm exact-name query target: remain within the existing `~69ms` ballpark.
- Stretch target: move cold exact-name repo query into the `<5s` bucket.

- [ ] **Step 3: Verify closeout gate**
Run: the documented probe commands plus `cargo test -p raql-engine -- --nocapture && cargo test -p raql-host-ra -- --nocapture && cargo test --test cli_daemon_cutover -- --nocapture`
Expected: correctness is preserved, approximate families stay disabled unless truly RA-native, and the repo-scale cold exact-name probe is no longer dominated by full universe symbol enumeration.
