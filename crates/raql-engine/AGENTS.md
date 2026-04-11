## Boundary

This crate owns RAQL execution semantics, extern lookup/relation contracts, and runtime behavior over host-provided facts.

It does not own Rust semantic discovery.

## Route work

- Add or change lookup semantics here when the issue is about RAQL execution behavior, lookup cardinality, goal evaluation, or host contract shape.
- Keep host-facing lookup/relation APIs generic; rust-analyzer-specific discovery belongs in `crates/raql-host-ra/`.
- Prefer lookup-first execution when the host can answer query-shaped requests directly.

## Keep out

- Do not add rust-analyzer or workspace-specific discovery logic here.
- Do not make engine behavior depend on filesystem, Cargo, or VFS heuristics.
- Do not use engine changes to paper over unsupported or approximate host semantics.

## Verification

- `cargo test -p raql-engine -- --nocapture`
  proves execution semantics and lookup contract behavior.
- Focused pushdown/lookup regressions in `src/tests.rs` are the first proof loop when changing lookup semantics.

