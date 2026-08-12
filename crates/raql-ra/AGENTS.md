# raql-ra

RA-native semantic operators: Salsa-tracked queries and (from slice A) the
operator implementations behind the `raql-plan` catalog. This crate is the
only place that computes Rust semantic truth, and it computes it exclusively
through rust-analyzer's database. Contract: `docs/SPEC.md` §6–§7.

## Ownership boundary

- Owns: tracked queries over `RootDatabase`, RA-native operator bodies,
  selector resolution, output projection of RA values (as they land per the
  build sequence).
- Must not own: process lifecycle, sockets, VFS/watching, engine semantics,
  request scheduling. Loading a workspace is the server's job — `load-cargo`
  and `project-model` appear here as **dev-dependencies only** (gate tests,
  quarantined timing probe in `examples/`).

## The extension idiom (don't reinvent)

Upstream RA deleted its `query_group` macro. A derived query is a free
function wrapping a `#[salsa::interned]` key struct + a `#[salsa::tracked]`
function — copy the shape of `raql_callees` in `src/lib.rs` (model:
`line_index` in `vendor/rust-analyzer/crates/ide-db/src/lib.rs`). Track a
query only for meaningful reusable derived work (SPEC §6.2); cheap
projections stay plain functions.

Tracked-fn bodies that touch type resolution must self-attach the database
(`hir::attach_db`) — salsa can re-execute them from a verification stack
where the TLS slot is empty.

**Attachment is typed, never blanket.** RA APIs that parallelize
internally (`world_symbols`, `parallel_prime_caches` — re-check the set on
every RA bump; reference search is currently sequential) clone the
database per rayon worker, and work-stealing runs those closures on the
*calling* thread; if that thread holds an attach, the clone's differing
address panics with "Cannot change attached database" (see RA's own
`Analysis::symbol_search` workaround). `src/snapshot.rs` turns the rule
into types: attach scopes are entered through `Snapshot::attached(&mut
self)` and hand out an `Attached` witness, unattached-only operators take
`&mut Snapshot`, and the borrow checker rejects mixing them.
`hir::attach_db` may appear in `snapshot.rs` only (test-enforced). No
future request boundary may hoist attachment around whole-plan execution —
the panic is query-content dependent and will not show up until a plan
mixes the wrong operators.

## Bait / keep out

- No RAQL-side revision counters, fingerprints, mtime state, or caches of RA
  results. Salsa is the only memoization. (Spike gate G5; `tests/spike_gates.rs`
  enforces the crate-local part.)
- Never reverse-resolve a `NavigationTarget` (or any other identity-lossy
  IDE value) back to a `hir` handle; adapt the IDE implementation to keep
  handles instead (SPEC §6.3).
- Unresolvable callsites/values are *absent*, never approximated or given
  fallback IDs (SPEC §4.3). The old host's `fallback_*` paths are the
  canonical example of what not to port.
- The salsa dependency version must match the pinned RA rev's own
  (workspace `Cargo.toml` comment).

## Verify

- `cargo test -p raql-ra` — spike gates G1–G6 (load, memoize, precise
  invalidation, cancellation, no-shadow-state, key stability).
- Timing claims: release-only, via the quarantined
  `examples/spike_timings.rs` probe.
