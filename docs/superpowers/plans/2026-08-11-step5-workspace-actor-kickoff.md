# Kickoff: step 5 — the workspace actor (and the debts this session left)

Session handoff written 2026-08-11 (third session that day) by the session
that executed the step-4 integration: compiler lowering, engine refit, and
the daemon rewire onto the new runtime. Supersedes
`2026-08-11-step4-integration-kickoff.md` (its "Next work" items 1–2 are
done; item 3 is this document's subject). Carries what the SPEC and code
deliberately don't.

## Read first

1. `docs/SPEC.md` — §8–§11 are implemented end to end; §12 (workspace
   server) is this kickoff's subject; §13 output boundary is partially
   implemented (projection exists, fragments/JSONL do not).
2. Root `AGENTS.md` + per-crate `AGENTS.md` (raql-plan, raql-ra,
   raql-compiler, raql-engine — compiler/engine ones are new/rewritten
   this session).
3. `git log` from `6e36f64` — commit messages carry the design decisions
   and gate evidence per slice.
4. This file.

## State of the world

- The **new pipeline is the only pipeline**: parse → resolve (catalog
  extern injection) → typecheck → flatten → stratify → lower
  (`logic::Program` + provenance) → `raql_plan::plan` (demand roots,
  RAQL03xx) → `raql_engine::execute` (demand-driven, memoized,
  OperatorSet-generic) → `raql_ra::SnapshotOperators` → §13.1 projection
  at the host boundary.
- Deleted with no successor, per SPEC §17.1: the compiler's reorderer and
  zero-bound allowlist, helper-rule inlining, `ExternLookupPlan`/
  `GoalPlan`/`RulePlan`, `RuntimeValue`, `EngineHostView`,
  `extern_relation_rows` bulk materialization, `LazyRaRuntime`,
  `DeterministicRaHost` and the `rv_*`/`fallback_*` vocabulary.
- `std.raql` is catalog-native (SPEC §8.1/§9.3): no extern/`.mode`
  blocks, no caller/callee aliases (catalog externs), filters bound-first,
  v1/deleted families removed until their catalog entries land.
- Suites: raql-plan 19, raql-ra 28, raql-compiler 52, raql-engine 20,
  raql-host-ra 7, raql-daemon 14 — all green (the 7 pre-existing host-ra
  failures died with the machinery they tested), clippy clean on
  plan/ra/engine + all new compiler modules.
- Daemon path proven end to end (`497bd95`): release binary,
  `raql lang run views/stdlib_callers_load_and_plan.raql --rust-file .`
  → cold **3.17s** (fresh daemon, first RA load), warm repeat **124ms**,
  `out_status("ok")`, 4 correct `caller_report` rows; a second probe
  proved Def/Span/handle/enum projection across the socket. Old-path
  reference was cold ~4.1s / warm ~59–69ms — warm regressed ~2x (no
  join batching, per-env operator invokes); see next-work item 3.

## Decisions made (don't relitigate)

- **Demand roots** replace the single query rule: a view's outputs are
  planned as roots (all-free pattern); an ad-hoc query is an arity-0
  derived def. Root extents are the request's demand — not scans, not
  denied by `--no-scan`; unseeded derived *calls* inside bodies still
  are (SPEC §9.2). Declared `.mode` binds roots like call sites.
- **Extern surface is catalog-only**: injected at resolve; RAQL0101
  (catalog redecl), RAQL0105 (user extern decl / `.mode` on extern),
  RAQL0106 (`.mode` on input). `fmt`/`coalesce` stay use-site-declared
  (generic schemas), shape-checked by the reserved validators.
- **Binder lowering**: aggregates/choose_topk are synthesized derived
  defs (bound slots ++ correlated ++ locals ++ outputs) with a single
  declared mode; the engine evaluates the specialization and applies
  binder semantics. Correlation counts only siblings' *outer interface*
  vars — two aggregates reusing a local name do not correlate.
- **Source-AST execution**: the engine executes source goals in planned
  order; `source_index` ↔ source body index is 1:1 by construction, with
  `GoalPath` provenance for binder sub-bodies. Compound-with-variable
  args are supported for engine builtins (flattened lowering), rejected
  (RAQL0304) for positional atoms — bind through `=` instead.
- **No ordering of semantic handles anywhere**: `EngineValue::plain_cmp`
  refuses them; choose_topk ties keep insertion order (deterministic per
  snapshot, weaker than the old handle-keyed tie-break — revisit only
  with a projection-based ordering at the output boundary).
- **Recursion** is naive iteration over the demanded subset with
  memo-per-(predicate, pattern, seed); frames that read an ancestor's
  partial complete provisionally without memoizing. §11.1 sanctions
  naive-under-cap for v0; semi-naive is future quality work.
- Function (`.func`) cardinality enforcement for externs is gone with the
  old engine; the catalog has no functional marker yet. If cardinality
  contracts matter, add a catalog field, don't resurrect the old check.
- RAQL0301 keeps its verbatim §10.3 message; `GoalLocation` (predicate,
  rule, source index) rides along for span mapping only.

## Landmines (verified this session)

- Enum tags meet operator rows by *string equality*: raql-ra's
  `DefKind`/`DispatchKind` tag strings must match `std.raql`'s `.type`
  variant names. No compile-time cross-check exists yet.
- `dispatch_str` is now an engine builtin: lowercased variant name
  (`THROUGH_TRAIT` → `through_trait`), matching the old host rows.
- The baseline `witness_path_is_forbidden_inside_recursive_scc` stack
  overflow disappeared once its fixture stopped user-declaring
  `witness_path` — the strata cycle-path search may still be
  overflow-prone on adversarial user-extern-shaped graphs; nobody
  root-caused it.
- `graph_edge` demanded by `witness_path` is an implicit root added in
  lowering; if a program's `graph_edge` is facts-only it classifies as an
  input relation and the engine reads it from inputs instead.
- Operator invocation happens per env row (no join batching); Salsa
  memoization on the host side is what keeps repeated `def_kind`-style
  probes cheap. Join indexing is the §11.1 quality bar, not done.
- The old engine's aggregate-under-unbound-correlation permissiveness is
  gone: correlated vars must be bound before the binder runs (declared
  mode on the synthesized def). This is stricter and honest; a program
  relying on the old accidental semantics fails at plan time.

## Next work, in order

1. **Step 5 workspace actor** (SPEC §12): `raql-daemon` → `raql-server`;
   the §12.2 change pipeline as a recognizable `GlobalState::
   process_changes` adaptation; structural reloads (§12.3); snapshot-per-
   request with `catch_unwind` + the §11.2 retry ladder (the engine is
   already unwind-safe; nobody catches today); delete host-ra's
   `sync_workspace` sweep at that point. Warmup must respect the typed-
   attachment rule (`raql-ra/AGENTS.md`).
2. **Output boundary completion** (§13): fragments/metrics/JSONL per
   `spec_sketch.md`, response stamping (§13.3), selector resolution
   (§13.2 `resolve()` with Alternatives) — `target_def` request bindings
   are currently always empty; P1-style views self-seed by name.
3. **Latency evidence** (§15): P1 through the new path is cold 3.17s /
   warm 124ms (2026-08-11, this repo). Warm sits ~2x above the old-path
   reference (~60ms) — the engine invokes operators per env row with no
   join batching; §11.1's join-indexing quality bar is the likely fix.
   Re-run P2 and add `tests/latency_smoke.rs` (mandatory at cutover).
4. Remaining §17.1 hygiene: the `raql-host` crate is now mostly dead
   vocabulary (host-ra keeps `CapabilityId`; raql-cli no longer depends
   on it); `ProtocolValue::Host` is unreachable from the daemon path;
   `views/stdlib_error_conversions.raql` stays dark by design.

## Known soft spots (candidates for the next test plan)

- The recursion machinery's provisional-completion path (mutual recursion
  across specializations) has no dedicated test; transitive-closure and
  cap tests cover the self-recursive path only.
- Cancellation: still no per-operator-family cancellation test (§16.4),
  and no `catch_unwind`/retry at any request boundary.
- `span_allowed`'s library-root negative remains probe-only.
- Reorderer memo invariant (bound-set is a function of the placed-set)
  still rests on argument, not property test.
- The `#agg`/`#topk` synthesized-def names appear in explain output;
  fine for agents, but the format is now part of the versioned interface
  — change deliberately.

## Verify before trusting

- `cargo test -p raql-plan -p raql-ra -p raql-compiler -p raql-engine`
  (19 + 28 + 52 + 20).
- Daemon path: the e2e smoke from the daemon-rewire commit (P1 view
  against this repo via the release binary).
