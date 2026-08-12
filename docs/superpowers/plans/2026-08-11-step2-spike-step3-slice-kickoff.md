# Kickoff: build step 2 (RA/Salsa spike) + step 3 (minimal vertical slice)

Session handoff for executing `docs/SPEC.md` §17.2 steps 2–3. Written 2026-08-11 by the
session that authored the SPEC, to carry over context the SPEC deliberately doesn't hold.

## Read first

1. `docs/SPEC.md` — the contract. Load-bearing sections for this work: §6 (raql-ra +
   spike gates G1–G6), §7 (Value model), §8.6 (v0 catalog — normative), §9–§10 (only to
   keep the slice's seams compatible; full planner work is step 4), §17.1 (disposition map).
2. `AGENTS.md` — invariants; latency claims are release-mode daemon-backed only.
3. This file.

## Ordering (hard)

Step 2 before step 3. The spike is small and bounded (one tracked query, G1–G6); if G1 or
G6 fails, the raql-ra design changes and the slice must not have started. Do the spike,
commit it with a gate-results note, then start the slice in the same or next session.

Sizing honesty: step 3 is the largest single step in the sequence. Recommended split with
a commit between each:
- **Slice A**: catalog skeleton + operator trait + `def_name`/`def_kind`/`def_path`/
  `def_span`/`def_at` + projection + truth fixtures.
- **Slice B**: `callee`/`caller`/`call_edge` bound modes + dispatch classification port +
  P2/P1 probe wiring.

## Treasure map (from the 2026-08-11 survey; verify line numbers before relying on them)

Portable logic (SPEC §17.1 "port into raql-ra") — read these before writing operators:
- Keyed lookup dispatch: `crates/raql-host-ra/src/workspace_service.rs:351-476`
  (`extern_lookup_rows`, the 17-predicate match — the shape of the operator set).
- Call lookup + dispatch classification: `crates/raql-host-ra/src/provider/call_lookup.rs`
  (644 lines; recent commits moved this to a single RA ancestor walk — keep that).
- Exact-name seeding via public `world_symbols`: `crates/raql-host-ra/src/provider/def_name.rs`
  and recent commits `6c4f650`/`f204c69`.
- Def metadata lowering: `crates/raql-host-ra/src/provider/defs.rs`.
- Lazy runtime (what "never materialize" looks like today): `crates/raql-host-ra/src/lazy_runtime.rs`.

Engine seams (touch minimally in step 3; full refit is step 6):
- Host view contract: `crates/raql-engine/src/lib.rs:231-255`.
- Bulk-injection + all-or-nothing lookup coverage (the thing that eventually dies):
  `crates/raql-engine/src/lib.rs:630-733`.

Compiler mode machinery (step 4 territory; read-only for now):
- Reorderer `crates/raql-compiler/src/lib.rs:1368-1470`, `goal_runnable` `:3125`,
  `planned_extern_lookup` `:3220-3275`, zero-bound allowlist `:3277-3283`.

Daemon: `PlanCache` at `crates/raql-daemon/src/lib.rs:700-756` (seed of SPEC §5.2 cache).

RA facts already verified (don't re-derive): `RootDatabase` is new-Salsa and third-party
extensible (`vendor/rust-analyzer/crates/load-cargo/src/lib.rs:96-99`); `query_group` macro
re-exported at `vendor/rust-analyzer/crates/base-db/src/lib.rs:35`; `hir::Function` is
`Clone,Copy,Eq,Hash` (`vendor/rust-analyzer/crates/hir/src/lib.rs:2288`); IDE call hierarchy
is identity-lossy (`NavigationTarget`) so operators adapt its *implementation*
(`vendor/rust-analyzer/crates/ide/src/call_hierarchy.rs`) keeping `hir` handles.

## Decisions already made (don't relitigate)

- Everything in `docs/SPEC.md`, including: scans explicit + visible (§8.5); handles are
  semantic selectors, never hashes (§13.2); `world_stamp` leaves the language (§13.3);
  two-database split with content-addressed program cache v0 (§5.2).
- Step 3 is **parallel scaffolding**: new crates `raql-plan` + `raql-ra` grow alongside
  the old host path. Nothing is deleted until cutover (step 8). A dev-only entry point
  (quarantined per AGENTS.md invariants) may drive the slice before the daemon switches.

## Decisions this session must make (with recommendations)

1. **Engine bridging**: implement the new `raql-plan` operator trait and give the engine a
   small adapter to drive it — do NOT implement the old `EngineHostView`/`ExternLookup*`
   vocabulary from `raql-ra` (that re-entrenches what step 8 deletes). Recommendation:
   new trait + adapter.
2. **`raql-ir` readiness**: unread as of this handoff. First task of Slice A: read it and
   check whether extern relations are represented as materialized relation IDs vs callable
   access paths. If the former, scope the refit before proceeding (it may be small).
3. **`query_group` vs raw `salsa::tracked`**: spike gate G1/G6 decides; record the outcome
   in the spike commit message and in SPEC §18.1.

## Working agreements

- The repo may be on a detached HEAD; create a branch off `main` before the first commit.
- One commit per spike gate cluster and per slice milestone; gate evidence (test names,
  timing numbers) in commit messages.
- Latency numbers: release build, daemon-backed, `RAQL_TRACE_TIMINGS=1` for breakdowns.
- On-touch rule: when `raql-plan`/`raql-ra` are created, give each a focused `AGENTS.md`
  (ownership boundary + bait warnings), and update the root `AGENTS.md` route map.

## Exit criteria

Step 2: G1–G6 pass with evidence; cold/warm timings recorded.
Step 3: SPEC §8.6 v0 predicates answer end-to-end on this repo and `vendor/rust-analyzer`;
truth fixtures per §16.1 for each; P2 (exact-name seed + projections) ≤100ms p95 warm,
release mode; no RAQL-side cache/revision state anywhere in the new crates (grep gate).
