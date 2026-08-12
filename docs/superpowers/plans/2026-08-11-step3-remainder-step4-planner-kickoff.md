# Kickoff: finish step 3 (remaining v0 predicates) + step 4 (binding-aware planner)

> **SUPERSEDED 2026-08-11 (same day, second session):** items 1–2 of "Next
> work" are done (v0 catalog complete, planner built, 46 tests green).
> Read `2026-08-11-step4-integration-kickoff.md` instead; this file stays
> as the historical record of the earlier decisions and landmines.

Session handoff written 2026-08-11 by the session that executed the RA bump, the
step-2 spike (G1–G6 all green), and slices A/B of step 3. Carries what the SPEC
and code deliberately don't.

## Read first

1. `docs/SPEC.md` — the contract. §8–§10 are now the load-bearing sections
   (planner is next); §18.8 records the tracked-`caller` blocker.
2. `AGENTS.md` (root) + `crates/raql-ra/AGENTS.md` + `crates/raql-plan/AGENTS.md`
   — ownership + the attachment landmine (typed; see below).
3. `git log` from `bf7ddbd` — the four commits carry gate evidence, timings,
   and migration notes in their messages.
4. This file.

## State of the world

- Branch `native-native/step2-spike-slice-a`, 5 commits ahead of the old HEAD
  (`6c4f650`). Working tree clean. `cargo test -p raql-ra -p raql-plan`: 21/21.
- RA pin is `b2d445b22a` (2026-08-11 master), salsa 0.28.2, rustc 1.97.1,
  reference clone at `vendor/rust-analyzer` (gitignored; full git clone — old
  revs available via `git -C vendor/rust-analyzer show <rev>:<path>`).
- New crates: `raql-plan` (catalog, no RA deps, closed `OperatorId` enum),
  `raql-ra` (spike query + slice A def family + slice B call family, all
  driven through `raql_plan::OperatorSet` in tests).
- Old host path (`raql-host-ra`, daemon, CLI) untouched and still the only
  public execution path. Parallel scaffolding per plan; nothing deleted.
- `cargo test -p raql-host-ra`: 57 pass / 7 fail — **all 7 pre-existing**
  before the RA bump (verified against a pre-bump baseline build). One
  root-cause is known: `workspace_service.rs` unbound-`def` branch calls
  `ensure_core_index(&CoreHostBuildSpec::default())` (no `def_spans`), so
  `def_span` under the lookup-only fast path returns 0 rows → RAQL0907.
  Fix would be passing the planned spec instead of `default()` — but it's
  legacy-path code; weigh against cutover before spending time.

## Decisions made (don't relitigate)

- Extension idiom: free fn wrapping `#[salsa::interned]` key +
  `#[salsa::tracked]` fn (upstream deleted `query_group`). SPEC §6.1/§18.1.
- `caller` is an **untracked** operator (SPEC §18.8): ide-db `usages` is typed
  over concrete `RootDatabase`; tracked bodies are dyn-typed. Don't try to
  trick this — it's an upstream-patch-or-reimplement decision for later.
- Attachment discipline is **typed** (`raql-ra/src/snapshot.rs`):
  `Snapshot::attached(&mut self)` → `Attached` witness (!Send, non-escaping);
  unattached-only APIs take `&mut Snapshot`; `hir::attach_db` may appear in
  `snapshot.rs` only (test-enforced in `spike_gates.rs::g5`). Never hoist
  attachment to a request boundary.
- `OperatorSet::invoke` takes `&mut self` on purpose (encodes the above).
- Closed `OperatorId` enum: adding a catalog mode without a host
  implementation is a compile error. Keep it that way.
- No synthetic defs, no fallback IDs ported from the old host, ever.
- Canonical paths are owner-qualified for assoc items / variants / fields
  (`crate::mod::Owner::member`) — RA's `canonical_path` alone is module-level
  and was wrong for selectors; see `Def::canonical_path`.

## Landmines (verified this session)

- `world_symbols` (and `parallel_prime_caches`) fan out over db clones on the
  calling thread → panic if attached. Typed away in raql-ra, but **server
  warmup (step 5) and selector resolution must respect it too** — shape
  selector resolution like RA's `Analysis::symbol_search` (search unattached,
  attach for classification).
- RA reference search cannot see callsites whose tokens live in a
  `macro_rules!` **definition body** — absent in both call directions (RA's
  own call hierarchy has the same hole). Catalog caveat
  `macro_definition_body_callsites_absent`; fixtures assert it. Don't "fix".
- `hir::Variant` is a silent name swap on this rev: it's the old `VariantDef`
  ({Struct, Union, EnumVariant}); the old variant type is `hir::EnumVariant`.
  Code using `hir::Variant` still compiles and means something else.
- salsa version in workspace `Cargo.toml` must track RA's pin exactly.
- On every RA bump: re-check which APIs parallelize internally, and re-run the
  gate tests — they caught everything this time within seconds.

## Next work, in order

1. **Step 3 remainder** — v0 predicates still missing from the catalog:
   `def`/`fn_def` crate+workspace scans (first `AccessKind::Scan` modes; the
   §8.5 visibility contract needs a first consumer), `is_public`, `in_test`
   (port from `raql-host-ra/src/provider/defs.rs` — `module_def_is_public`,
   `module_def_in_test`; drop nothing, they're honest), `span_allowed`,
   `handle` (needs impl ordinals, §13.2), `span_key`. Each with §16.1 truth
   fixtures. P2-shaped probe: extend `examples/spike_timings.rs` or add a
   probe example that seeds by name + projections and reports p95.
2. **Step 4 planner** (SPEC §9–§10): demand propagation / derived-mode
   inference, backtracking reorderer, RAQL0301/0310 message contracts,
   `explain`. Seed exists: `PredicateDef::satisfiable_modes` (cheapest-first,
   scans-last) + unit tests in `raql-plan/src/predicate.rs`. The compiler's
   current reorderer is `crates/raql-compiler/src/lib.rs:1368-1470` (verify
   drift), `goal_runnable` `:3125`, `planned_extern_lookup` `:3220-3275`,
   zero-bound allowlist `:3277-3283` — the catalog replaces that allowlist.
3. Engine bridging happens per kickoff decision #1 (new trait + small
   adapter); the explorer's seam report (2026-08-11 session) found the whole
   coupling is `eval_goal`'s Atom arm `raql-engine/src/lib.rs:1682-1761` plus
   the `MissingRelation` precondition at `:1682-1691`. Don't implement
   `EngineHostView` from raql-ra.

## Latency reference points (release, this repo, M-series)

Spike probe: load 647ms · first whole-workspace def-map walk 4.96s · cold
`raql_callees` 1.28s (53 sites) · warm memo hit p50/max <1µs ×100.
Old-path reference (pre-rewrite): warm ~59–69ms, cold ~4.1s (SPEC §15).

## Verify before trusting

- `cargo test -p raql-ra -p raql-plan` (21 tests, ~2s after build).
- `cargo test -p raql-host-ra` only when touching the legacy path (minutes;
  expect the 7 known failures + occasional watcher-timing flake in
  `from_member_manifest_detects_new_sibling_member_files`).
