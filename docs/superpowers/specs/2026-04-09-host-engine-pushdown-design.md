# Host/Engine Pushdown Design

**Date:** 2026-04-09

## Problem

The supported daemon path is warm-fast enough once `WorkspaceService` already has a built host, but cold exact-name repo queries still pay for full extern universe materialization. On `/Users/darin/Projects/raql`, the dominant wall is `workspace_service.populate_defs_from_symbols.world_symbols`, which takes about `57.4s` on the cold exact-name defs probe.

That is an architectural mismatch:
- the engine currently asks the host for full extern relations via `extern_relation_rows(...)`
- the host responds by materializing universe-shaped relations even when the query only binds a tiny lookup key
- rust-analyzer already has indexed symbol search that can answer the narrow question directly

## Design goal

Introduce a host-owned lookup boundary so the engine can issue lookup-shaped requests for pushdown-safe extern goals instead of forcing full relation scans.

## Scope for the first cut

Supported now:
- exact bound-argument lookups for function externs
- exact bound-argument lookups for tightly bounded relation externs only when the semantics are fully RA-native

Not supported in this cut:
- fuzzy or prefix search pushdown
- partial scans disguised as lookup
- approximate families that are currently disabled
- any public execution path outside the daemon-backed incremental runtime

## Contract

The host boundary owns these new concepts:
- `ExternLookupValue`
- `ExternLookupHostValue`
- `ExternLookupHostValueKind`
- `ExternLookupShape`
- `ExternLookupRequest`
- `HostRuntime::extern_lookup(...)`

### `ExternLookupShape`

Initial variants:
- `FunctionExactBindings`
- `RelationExactBindings`

These shapes mean:
- the engine already knows which argument positions are bound, even if they are nominally output positions in the declaration or mode
- the host receives the declaration arity, bound positions, and bound values
- the host either returns fully materialized matching rows for that exact lookup or reports `None` to decline pushdown

`None` means “no pushdown implementation for this request,” not “the lookup matched zero rows.”
An implemented lookup that matches zero rows returns `Some(vec![])`.

## Ownership boundary

`raql-host`
- owns the lookup request and value types
- owns the runtime trait contract
- does not depend on engine runtime value types

`raql-compiler`
- extracts pushdown-safe shapes from planned extern goals
- records only shapes that are semantically guaranteed by the plan

`raql-engine`
- converts bound goal terms into `ExternLookupRequest`
- calls `extern_lookup(...)` before full relation evaluation for pushdown-marked goals
- falls back to the existing supported path when a goal is not pushdown-safe or the host declines pushdown

`raql-host-ra`
- answers pushdown requests with RA-native indexed lookups
- must not synthesize approximate semantics just to satisfy pushdown

## First target families

1. `def_name`
- exact-name cold lookups are the clearest win
- rust-analyzer symbol search can answer these directly
- matching `def` rows can then be derived from the looked-up symbol set
- this requires exact bound-argument pushdown because the practical cold-path target is `def_name(_, "load_and_plan")`, not just `def_name(+Def, -string)`

2. `field`
- only if the lookup can be backed by true HIR field ownership and type truth
- otherwise it stays on the existing honest path

3. `search`
- only if the lookup shape maps cleanly onto existing RA-backed search semantics
- otherwise it remains materialized or gated

## Audit rule for pushdown

A family is eligible for pushdown only if:
- the lookup key is fully determined by bound query inputs
- RA exposes a real indexed or bounded semantic lookup for that key
- the host can return exact matching rows without widening to a full scan

If any of those fail, the family is not pushdown-safe yet.

## Verification gates for this design

1. Engine-level red tests prove missing lookup-first behavior.
2. Host-ra repo probes still show `world_symbols("")` on the old path.
3. After implementation, the exact-name cold repo probe must stop being dominated by full universe symbol enumeration.
4. Warm reruns must stay in the current `~69ms` ballpark.
5. Approximate disabled families stay disabled unless they become fully RA-native.
