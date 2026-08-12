## raql

We have the full rust-analyzer workspace in `vendor/rust-analyzer` for reference and perusal. TAKE ADVANTAGE OF THIS WHENEVER RUST ANALYZER FEATURES WOULD HELP OUR LOGIC.

IMPORTANT: we do not care about backwards compatibility here. This is an unpublished local crate. Prefer long-term maintainability, fewer lies, and the right ownership boundaries over compatibility shims.

## Worldview

- Semantic truth for supported surfaces comes from rust-analyzer state: HIR, IDE, VFS, and Cargo/workspace model.
- Thin deterministic wrappers are allowed only for stable IDs, normalization, and transport. They must not invent semantic truth.
- If a primitive is not RA-native and honest, disable it rather than keep an approximate custom extractor alive.
- Prefer query-shaped providers and cached RA identity over broad snapshot materialization, reverse rediscovery, raw filesystem scans, or text-search fallbacks.
- Use release-mode, daemon-backed runs for latency claims. Debug timings are not decision-grade.
- This repo does **not** use rustfmt: never run `cargo fmt` (it would reformat everything). Match the surrounding manual style (~100 cols). `cargo clippy` is kept clean on the new-architecture crates (`raql-plan`, `raql-ra`, `raql-engine`, and the new `raql-compiler` modules).

## Route work

- `crates/raql-plan/` owns the predicate catalog (single source of truth for extern predicates), the binding-aware planner (SPEC §8–§10), and the engine-facing contracts (`OperatorSet`, `EngineValue`). No RA types, no execution.
- `crates/raql-compiler/` (with `raql-syntax`, `raql-ir`) is the lang layer: resolution with catalog-injected extern signatures, typechecking, stratification, and the lowering to `raql_plan::logic` (SPEC §17.1 lang row). No ordering, no access paths, no RA types.
- `crates/raql-ra/` owns Rust semantic truth (SPEC §6): Salsa-tracked queries over `RootDatabase`, the catalog operator bodies (`SnapshotOperators`), and §13.1 projection primitives. Read its `AGENTS.md` before adding queries.
- `crates/raql-engine/` owns demand-driven row execution over the operator boundary (SPEC §9.2, §11): joins, recursion, negation, binders, memoization. Never invokes RA directly.
- `crates/raql-host-ra/` is the workspace-lifecycle shell (loading, watch/sync, warmup, capabilities) plus the projection boundary of `run_planned`. Dissolves into `raql-server` at step 5 (SPEC §12); no semantic machinery may return here.
- `crates/raql-daemon/` owns process lifecycle, warmup, socket/protocol, and request scheduling. It must not grow semantic extraction logic.
- `crates/raql-cli/` stays a thin client over the daemon-backed path.
- `vendor/rust-analyzer/` is the reference tree for API choice, invariants, and integration patterns. Prefer matching RA's own patterns over inventing local approximations.

## Invariants / keep out

- Any supported RAQL query execution path must go through the daemon-backed, incremental rust-analyzer runtime. Any direct runtime path is dev-only, quarantined, and must not be wired into public CLI behavior.
- Extern predicates exist only in the catalog (SPEC §8.1): a new semantic family is a catalog entry + a `raql-ra` operator + its §16 proof matrix, never a program-text declaration or an engine special case.
- Do not introduce raw filesystem scans, manual AST walks, or path-prefix heuristics as the source of truth when rust-analyzer already exposes the answer.
- Do not grow semantic machinery in `crates/raql-host-ra/` — it is a lifecycle shell awaiting the §12 server. New semantic families go through the catalog into `raql-ra`.
- Daemon/client code must not import or recreate host semantic extraction logic. Keep lifecycle and semantics separate.
- The current public latency probe is `views/stdlib_callers_load_and_plan.raql`. Use the real daemon-backed release binary when making cold/warm latency claims.

## AGENTS.md policy

- The hierarchy rule is highest stable truth, not highest possible truth.
- Child files should mostly contain delta, not duplication.
- On-touch rule: if you change a directory's boundary, canonical pattern, verification command, or known hazard, update the nearest relevant `AGENTS.md` in the same change.
- Add a child `AGENTS.md` when a subtree has its own ownership boundary, proof model, side effects, or nearby bait that a smart stranger would otherwise copy.
