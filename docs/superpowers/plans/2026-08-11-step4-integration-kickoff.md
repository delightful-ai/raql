# Kickoff: wire the planner in — compiler lowering + engine bridging

Session handoff written 2026-08-11 (second session that day) by the session
that executed the step-3 remainder and the step-4 planner. Supersedes
`2026-08-11-step3-remainder-step4-planner-kickoff.md` (its "Next work"
items 1–2 are done; item 3 is this document's subject). Carries what the
SPEC and code deliberately don't.

## Read first

1. `docs/SPEC.md` — §9–§10 are implemented; §5.1/§17.1 describe the
   integration this kickoff is about.
2. Root `AGENTS.md` + `crates/raql-plan/AGENTS.md` +
   `crates/raql-ra/AGENTS.md` — ownership, planner shape, domain facts.
3. `git log` from `e1143af` — commit messages carry gate evidence and
   timings (P2 numbers are in `58380fe`).
4. This file.

## State of the world

- Branch `native-native/step2-spike-slice-a`, 9 commits ahead of old HEAD
  `6c4f650`. Tree clean. `cargo test -p raql-plan -p raql-ra`: 46/46;
  `cargo clippy -p raql-plan -p raql-ra --all-targets`: clean.
- **v0 catalog is complete** (SPEC §8.6): def family, call family, scans
  (`def`/`fn_def`/`def_name(-,-)`/`call_edge(-,-,-,-)`), filters
  (`is_public`/`in_test`/`span_allowed`), `handle` (impl ordinals work),
  `span_key`, plus mode-less `disabled` entries for the roadmap families.
- **Planner is complete** (SPEC §9–§10) in `raql-plan`: `logic.rs` input
  IR, §9.1 GFP mode inference, §9.2 demand specialization, §10.2
  backtracking reorderer, §10.3 error contracts (0301 verbatim-tested),
  §10.4 explain. Zero deps; nothing executes it yet.
- P2 probe (release, this repo, seed `raql_callees`): load 575ms · cold
  4656ms (first symbol-index build) · warm p50 141µs / p95 219µs. P2
  target (≤100ms p95) met; cold sits at the ~5s SPEC boundary, expected to
  improve when demand-driven evaluation replaces enumeration warm-up.
- Old host path untouched; still the only public execution path.

## Decisions made (don't relitigate)

- **Scan/seed domain = RA's local partition** (`CrateOrigin::is_local`):
  members and path deps, never registry/git/sysroot. SPEC §8.6 updated;
  `dep_lib` fixture pins it.
- **Per-mode caveats** (`ModeDef::caveats`) exist because the symbol index
  cannot surface fields (`fields_not_in_symbol_index` on `def_name(-,+)`).
- **Planner input is `raql_plan::logic::Program`**, not compiler IR. The
  compiler *lowers* to it; selector bindings arrive as input relations.
  Do not leak `raql-ir`/`raql-syntax` types into raql-plan.
- **Binding patterns are `raql_plan::Pattern`** (a newtype, `mode.rs`) —
  the shared vocabulary for demand keys, support sets, and specialization
  tables. Integration code speaks `Pattern`, never bare `Vec<bool>`.
- Error code assignments: 0301/0310 SPEC-normative; 0302 disabled, 0303
  declared-mode-not-inferable, 0304 malformed input, 0311 scan-under-
  negation (`raql-plan/src/error.rs`). The 0301 message is asserted
  verbatim in `tests/planner.rs` — change it deliberately or not at all.
- Ordering heuristic: derived goals cost C1 (C4 unseeded) for greedy
  placement; reported plan costs are exact (post-planning fixpoint).
- Handle grammar: `#ordinal` only when several defs share kind+path
  (SPEC §13.2 as written); ordinal order = (workspace-relative path,
  range start). Sole impls get no ordinal.
- `span_allowed` v0 = file in a non-library source root; request scope
  options compile onto it later.
- Declared `.mode` on a derived predicate is the contract everywhere,
  including recursive self-calls — a rule orderable only through an
  undeclared recursive pattern fails at planning time, honestly.

## Landmines (verified this session)

- `Module::declarations` covers types/values scopes only — `macro_rules!`
  live in the legacy-macro scope, which *also* contains macros textually
  inherited from earlier modules; filter by defining module
  (`crate_defs.rs` does).
- `Impl::all_for_type` is RA's "human-perceived impls" approximation
  (excludes blanket impls, shallow constructor check) — fine for handle
  ordinals because we filter peers to exact self-ADT identity anyway.
- Nested fixture workspaces need `[workspace] exclude = [...]` in the
  outer manifest or cargo metadata fails with "multiple workspace roots".
- The repo does **not** use rustfmt (root AGENTS.md) — `cargo fmt` would
  produce a monster diff. Match style manually.
- Salsa/attachment landmines from the previous kickoff all still apply
  (typed attachment, `world_symbols` fan-out, macro-definition-body
  callsite hole, `hir::Variant` name swap).

## Next work, in order

1. **Compiler lowering** (SPEC §17.1 raql-compiler row): feed extern
   signatures/modes from the catalog (delete the `.decl … extern`/`.mode`
   blocks from `std.raql`, §8.1); lower typechecked rules to
   `logic::Program`; route planning through `raql_plan::plan` and delete
   the compiler's own reorderer (`raql-compiler/src/lib.rs:1368-1470`,
   verify drift) and the zero-bound allowlist (`:3277-3283`,
   `supports_zero_bound_relation_lookup`). Rewrite `std.raql` derived
   filters bound-first (§9.3). RAQL0301 formatting now comes from
   raql-plan.
2. **Engine bridging** (per the first kickoff's decision #1): new trait +
   small adapter. The whole coupling is `eval_goal`'s Atom arm
   (`raql-engine/src/lib.rs:1682-1761`) + the `MissingRelation`
   precondition (`:1682-1691`). Execute `PhysicalPlan` specializations
   with demand memoization per §9.2/§11.1. Do **not** implement
   `EngineHostView` from raql-ra; the operator boundary is
   `raql_plan::OperatorSet` driven with `raql_ra::Value`.
3. **Step 5 workspace actor** (SPEC §12) after that; warmup must respect
   the typed-attachment rule (shape selector resolution like RA's
   `Analysis::symbol_search`).

## Known soft spots (candidates for the integration step's test plan)

- The planner has never driven real execution — integration seams are
  where the residual risk is.
- No cancellation test for the scan operator family yet (§16.4 wants one
  per family; the spike's G4 covers the tracked-query mechanism only).
- `span_allowed`'s library-root negative is untestable in the
  sysroot-free fixtures; it is exercised only by probe runs on real
  workspaces.
- Reorderer memo correctness rests on an argued invariant (bound-set is a
  function of the placed-set); a property test would firm it up.

## Verify before trusting

- `cargo test -p raql-plan -p raql-ra` (46 tests, seconds after build).
- `cargo test -p raql-host-ra` only when touching the legacy path
  (expect the 7 known pre-existing failures + occasional watcher flake).
