## raql

We have the full rust-analyzer workspace in `vendor/rust-analyzer` for reference and perusal. TAKE ADVANTAGE OF THIS WHENEVER RUST ANALYZER FEATURES WOULD HELP OUR LOGIC.

IMPORTANT: we do not care about backwards compatibility here. This is an unpublished local crate. Prefer long-term maintainability, fewer lies, and the right ownership boundaries over compatibility shims.

## Worldview

- Semantic truth for supported surfaces comes from rust-analyzer state: HIR, IDE, VFS, and Cargo/workspace model.
- Thin deterministic wrappers are allowed only for stable IDs, normalization, and transport. They must not invent semantic truth.
- If a primitive is not RA-native and honest, disable it rather than keep an approximate custom extractor alive.
- Prefer query-shaped providers and cached RA identity over broad snapshot materialization, reverse rediscovery, raw filesystem scans, or text-search fallbacks.
- Use release-mode, daemon-backed runs for latency claims. Debug timings are not decision-grade.
- This repo does **not** use rustfmt: never run `cargo fmt` (it would reformat everything). Match the surrounding manual style (~100 cols). `cargo clippy` is kept clean on the new-architecture crates (`raql-plan`, `raql-ra`).

## Route work

- `crates/raql-plan/` owns the predicate catalog (single source of truth for extern predicates) and the binding-aware planner (SPEC §8–§10; both implemented). No RA types, no execution.
- `crates/raql-ra/` owns the new-architecture RA-native layer (SPEC §6): Salsa-tracked queries over `RootDatabase` and catalog operator bodies. Growing alongside the old host path until cutover; read its `AGENTS.md` before adding queries.
- `crates/raql-host-ra/` owns RA-native providers, workspace integration, lookup/index logic, and the boundary between RAQL and rust-analyzer. (Legacy path: scheduled for deletion at cutover, SPEC §17; mechanical maintenance only.)
- `crates/raql-daemon/` owns process lifecycle, warmup, socket/protocol, and request scheduling. It must not grow semantic extraction logic.
- `crates/raql-cli/` stays a thin client over the daemon-backed path.
- `crates/raql-engine/` owns RAQL execution semantics and host lookup contracts, not Rust semantic discovery.
- `vendor/rust-analyzer/` is the reference tree for API choice, invariants, and integration patterns. Prefer matching RA's own patterns over inventing local approximations.

## Invariants / keep out

- Any supported RAQL query execution path must go through the daemon-backed, incremental rust-analyzer runtime. Any direct runtime path is dev-only, quarantined, and must not be wired into public CLI behavior.
- Bound lookups must answer from RA-backed identity or cached RA-built indexes, not from reverse rediscovery or ad hoc text scans.
- Do not introduce raw filesystem scans, manual AST walks, or path-prefix heuristics as the source of truth when rust-analyzer already exposes the answer.
- Do not keep adding responsibilities to `crates/raql-host-ra/src/workspace_service.rs`. If a change adds a new semantic family or proof model, split it into a focused provider/module instead of extending the blob.
- Daemon/client code must not import or recreate host semantic extraction logic. Keep lifecycle and semantics separate.
- The current public latency probe is `views/stdlib_callers_load_and_plan.raql`. Use the real daemon-backed release binary when making cold/warm latency claims.

## AGENTS.md policy

- The hierarchy rule is highest stable truth, not highest possible truth.
- Child files should mostly contain delta, not duplication.
- On-touch rule: if you change a directory's boundary, canonical pattern, verification command, or known hazard, update the nearest relevant `AGENTS.md` in the same change.
- Add a child `AGENTS.md` when a subtree has its own ownership boundary, proof model, side effects, or nearby bait that a smart stranger would otherwise copy.
