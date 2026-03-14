# RA daemon-backed cutover Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move supported `raql lang run` execution onto a daemon-backed incremental rust-analyzer runtime with capability-gated stdlib support and a quarantined dev-only direct-runtime path.

**Architecture:** Keep the language/compiler/engine crates intact, add a protocol and daemon layer, make the public CLI a thin client, and replace eager snapshot-backed public execution with a smaller RA-native lazy provider core. Unsupported stdlib-backed externs fail explicitly at planning time instead of silently degrading.

**Tech Stack:** Rust 2024, clap, rust-analyzer crates, Unix domain sockets, serde/serde_json, existing RAQL compiler/engine/host crates

---

## File map

### Workspace and packaging

- Modify: `/Users/darin/Projects/raql/Cargo.toml`
- Modify: `/Users/darin/Projects/raql/src/main.rs`

### Public CLI and daemon boundary

- Modify: `/Users/darin/Projects/raql/crates/raql-cli/Cargo.toml`
- Create: `/Users/darin/Projects/raql/crates/raql-cli/src/lib.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-cli/src/main.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-protocol/Cargo.toml`
- Create: `/Users/darin/Projects/raql/crates/raql-protocol/src/lib.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-daemon/Cargo.toml`
- Create: `/Users/darin/Projects/raql/crates/raql-daemon/src/lib.rs`

### Host contract and RA backend

- Modify: `/Users/darin/Projects/raql/crates/raql-host/Cargo.toml`
- Modify: `/Users/darin/Projects/raql/crates/raql-host/src/lib.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/Cargo.toml`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/capability.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/lazy_runtime.rs`

### Compiler/runtime capability wiring

- Modify: `/Users/darin/Projects/raql/crates/raql-compiler/src/lib.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-engine/src/lib.rs`

### Tests

- Modify: `/Users/darin/Projects/raql/tests/cli_strict_runtime.rs`
- Create: `/Users/darin/Projects/raql/tests/cli_daemon_cutover.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/tests/capability_gating.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/tests/workspace_service.rs`

### Policy/docs

- Modify: `/Users/darin/Projects/raql/AGENTS.md`

## Chunk 1: Establish the public CLI, protocol, and daemon crate boundaries

**Files:**
- Modify: `/Users/darin/Projects/raql/Cargo.toml`
- Modify: `/Users/darin/Projects/raql/src/main.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-cli/Cargo.toml`
- Create: `/Users/darin/Projects/raql/crates/raql-cli/src/lib.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-cli/src/main.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-protocol/Cargo.toml`
- Create: `/Users/darin/Projects/raql/crates/raql-protocol/src/lib.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-daemon/Cargo.toml`
- Create: `/Users/darin/Projects/raql/crates/raql-daemon/src/lib.rs`
- Test: `/Users/darin/Projects/raql/tests/cli_daemon_cutover.rs`

- [ ] **Step 1: Write the failing daemon-cutover CLI test**

Add a CLI integration test that:
- writes a tiny workspace
- writes a tiny RAQL query using only day-one supported core facts
- runs `raql lang run <query> --rust-file <workspace>`
- asserts the command succeeds through the daemon-backed path
- asserts a second run reuses the warm daemon instead of reporting cold init behavior

- [ ] **Step 2: Run the new CLI test and watch it fail**

Run: `cargo test --test cli_daemon_cutover -- --nocapture`
Expected: FAIL because there is no daemon/protocol/public CLI wiring yet.

- [ ] **Step 3: Create the protocol crate**

Implement request/response/event types in `raql-protocol`:
- workspace identity
- run request payload
- daemon hello/status payloads
- streamed event envelope
- CLI-renderable final result payload

Keep the protocol versioned and JSON-serializable.

- [ ] **Step 4: Create the daemon crate**

Implement `raql-daemon` as a library that:
- owns one workspace session per process
- listens on a Unix socket
- handles version handshake
- loads or reuses a `WorkspaceService`
- serves one-shot run requests with streamed events

- [ ] **Step 5: Move public CLI behavior into `raql-cli`**

Replace the stub `raql-cli` binary with real command handling:
- `lang check` remains local compile-only behavior
- `lang run` becomes daemon-backed
- hidden internal daemon serving subcommand is allowed if it routes into `raql-daemon`

`/Users/darin/Projects/raql/src/main.rs` should become a thin wrapper into `raql_cli`.

- [ ] **Step 6: Run the CLI daemon-cutover test and make it pass**

Run: `cargo test --test cli_daemon_cutover -- --nocapture`
Expected: PASS with both first-run and warm-daemon assertions.

## Chunk 2: Add capability planning and explicit unsupported diagnostics

**Files:**
- Modify: `/Users/darin/Projects/raql/crates/raql-host/Cargo.toml`
- Modify: `/Users/darin/Projects/raql/crates/raql-host/src/lib.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-compiler/src/lib.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-engine/src/lib.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/capability.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/tests/capability_gating.rs`

- [ ] **Step 1: Write the failing capability-gating test**

Add a focused test that compiles a query touching one unsupported stdlib-backed extern from the non-core set, then asserts planning/execution reports a deterministic unsupported-capability diagnostic before runtime evaluation proceeds.

- [ ] **Step 2: Run the capability-gating test and watch it fail**

Run: `cargo test -p raql-host-ra --test capability_gating -- --nocapture`
Expected: FAIL because unsupported externs are still treated as host-missing rows or partial notes.

- [ ] **Step 3: Define host capability types**

In `raql-host` add:
- stable capability identifiers
- capability sets
- structured unsupported-capability diagnostics

Do not put rust-analyzer-specific logic in this crate.

- [ ] **Step 4: Teach the compiler/planner to collect required extern capabilities**

Add a planning helper in `raql-compiler` that inspects referenced extern predicates/functions and returns the required capability set for a compiled program. Exclude engine-managed externs and scalar-input pseudo-relations.

- [ ] **Step 5: Fail explicitly on missing capabilities**

Wire the execution entrypoint so:
- required capabilities are computed before public execution
- daemon-backed RA runtime reports its supported capability set
- missing capabilities become explicit planning-time failure output
- `extern_relation_rows` no longer silently papers over unsupported RA-backed externs in the public path

- [ ] **Step 6: Re-run capability tests**

Run:
- `cargo test -p raql-host-ra --test capability_gating -- --nocapture`
- `cargo test -p raql-engine`

Expected: PASS, with engine behavior still valid for unit tests and the public RA path now failing unsupported capabilities explicitly.

## Chunk 3: Build the daemon-owned workspace service and day-one lazy RA-native core

**Files:**
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/Cargo.toml`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/src/lazy_runtime.rs`
- Create: `/Users/darin/Projects/raql/crates/raql-host-ra/tests/workspace_service.rs`

- [ ] **Step 1: Write failing workspace-service tests**

Add tests that assert:
- a workspace loads once and can serve multiple queries
- ordinary source edits update query results without building a new process
- workspace-shape changes trigger a controlled reload path

- [ ] **Step 2: Run the workspace-service tests and watch them fail**

Run: `cargo test -p raql-host-ra --test workspace_service -- --nocapture`
Expected: FAIL because the current runtime is still snapshot-owned and reloads by rebuilding the whole host model.

- [ ] **Step 3: Introduce `WorkspaceService`**

Implement a service that owns:
- live rust-analyzer state
- workspace metadata and identity
- reload stamps
- provider-family caches

Expose narrow methods for:
- initialization
- run preparation
- incremental content update
- controlled workspace reload

- [ ] **Step 4: Introduce a lazy RA runtime for the day-one core**

Implement a new runtime path that serves only the day-one supported capabilities lazily from live RA state. Day-one support should cover:
- `def`
- `def_name`
- `def_kind`
- `def_span`
- `def_path`
- `method_of`
- `fn_return_type`
- `handle`
- `span_key`
- `span_allowed`
- `is_public`
- `in_test`

Everything else should be absent from the supported capability set until rebuilt properly.

- [ ] **Step 5: Make public daemon execution use the lazy runtime**

`raql-daemon` should route public `lang run` requests through `WorkspaceService` plus the lazy day-one RA runtime, not through eager `RaHostRuntime::from_*` snapshot construction.

- [ ] **Step 6: Re-run workspace-service tests**

Run: `cargo test -p raql-host-ra --test workspace_service -- --nocapture`
Expected: PASS, demonstrating warm state reuse and explicit reload behavior.

## Chunk 4: Quarantine the old direct runtime path and harden the public CLI contract

**Files:**
- Modify: `/Users/darin/Projects/raql/src/main.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-cli/src/lib.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-cli/src/main.rs`
- Modify: `/Users/darin/Projects/raql/tests/cli_strict_runtime.rs`
- Modify: `/Users/darin/Projects/raql/AGENTS.md`

- [ ] **Step 1: Write the failing public/dev surface tests**

Add or update tests asserting:
- public `lang run` requires the daemon-backed RA path
- the old direct runtime path is not reachable through supported CLI behavior
- strict runtime-init failures still surface clearly

- [ ] **Step 2: Run the CLI tests and watch the new assertions fail**

Run:
- `cargo test --test cli_strict_runtime -- --nocapture`
- `cargo test --test cli_daemon_cutover -- --nocapture`

Expected: FAIL until the public/dev boundary is fully enforced.

- [ ] **Step 3: Quarantine the direct runtime surface**

Move old direct-runtime behavior behind an explicit dev-only command or hidden internal harness. Ensure no supported CLI path calls `RaHostRuntime::from_workspace_root*` directly.

- [ ] **Step 4: Codify the AGENTS rule**

Update `/Users/darin/Projects/raql/AGENTS.md` with this exact policy:

`Any supported RAQL query execution path must go through the daemon-backed, incremental rust-analyzer runtime. Any direct runtime path is dev-only, quarantined, and must not be wired into public CLI behavior.`

- [ ] **Step 5: Run the public CLI tests again**

Run:
- `cargo test --test cli_strict_runtime -- --nocapture`
- `cargo test --test cli_daemon_cutover -- --nocapture`

Expected: PASS with the quarantined dev surface and preserved strict init diagnostics.

## Chunk 5: End-to-end regression coverage for the hard cutover

**Files:**
- Modify: `/Users/darin/Projects/raql/tests/cli_daemon_cutover.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/tests/workspace_service.rs`
- Modify: `/Users/darin/Projects/raql/crates/raql-host-ra/tests/capability_gating.rs`

- [ ] **Step 1: Add end-to-end regression cases**

Cover:
- supported core query succeeds through daemon-backed execution
- unsupported stdlib capability fails explicitly and names the missing capability
- two consecutive runs reuse warm daemon state
- source edit changes results without requiring a new daemon process

- [ ] **Step 2: Run the focused regression suite**

Run:
- `cargo test --test cli_daemon_cutover -- --nocapture`
- `cargo test -p raql-host-ra --test capability_gating -- --nocapture`
- `cargo test -p raql-host-ra --test workspace_service -- --nocapture`

Expected: PASS.

- [ ] **Step 3: Run the final package-level verification set**

Run:
- `cargo test -p raql-cli`
- `cargo test -p raql-daemon`
- `cargo test -p raql-host-ra`
- `cargo test --test cli_strict_runtime`
- `cargo test --test cli_daemon_cutover`

Expected: PASS.
