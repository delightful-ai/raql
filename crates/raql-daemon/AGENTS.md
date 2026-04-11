## Boundary

This crate owns daemon lifecycle, socket/protocol handling, warmup orchestration, request scheduling, and daemon-facing session metadata.

It does not own Rust semantic extraction.

## Route work

- Put socket/process lifecycle, warmup coordination, and request-state transitions in `src/lib.rs`.
- Keep daemon session metadata truthful about cold/warming/warm state.
- If the daemon needs new semantic behavior, add a host-facing contract and implement it in `crates/raql-host-ra/` instead of importing semantic logic here.

## Keep out

- Do not reimplement or import RA semantic discovery here.
- Do not let daemon warmup policy become a hidden semantic workaround for bad host boundaries.
- Do not move query execution semantics out of `crates/raql-engine/`.

## Verification

- `cargo test -p raql-daemon warmup -- --nocapture`
  proves the warmup lifecycle contract.
- `cargo test -p raql-daemon serve_marks_followup_requests_warm -- --nocapture`
  proves session state moves from cold to warm correctly.
- Release daemon-backed measurements must use the real binary from `/Users/darin/Projects/raql/target/release/raql`, not debug runs.

