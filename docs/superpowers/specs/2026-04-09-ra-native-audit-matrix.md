# RA-Native Audit Matrix

**Date:** 2026-04-09

This matrix records the current audit state for every major supported surface in the daemon-backed runtime.

## Legend

- `keep`: already acceptable as RA-native
- `rewrite`: keep the surface, but replace current implementation with RA-backed truth
- `disable`: turn off until the family is honestly RA-native

## Matrix

| Surface | Current source of truth | Should RA own this? | Current gap | Action | Ship state |
| --- | --- | --- | --- | --- | --- |
| `workspace scope / membership` | Mixed RA state plus raw filesystem scans | Yes | Stray files under scan roots can perturb reload behavior and observed scope | `rewrite` | `required` |
| `reload / invalidation` | Custom scan/rebuild heuristics | Yes, mostly | Broad rescans, incomplete build-script invalidation, aggressive full reloads | `rewrite` | `required` |
| `query parse/resolve/typecheck/plan lifecycle` | Recomputed per request in daemon | No semantic truth, but daemon should own lifecycle | No plan cache; warm queries still repay frontend compilation | `rewrite` | `required` |
| `host extern lookup boundary / extern_lookup(...)` | Full extern relation materialization through engine-owned scans | Yes, as the host contract | No lookup-shaped boundary for exact bound-input extern calls | `rewrite` | `required` |
| `base def surface / def / def_name` | RA symbol index, but currently materialized as a whole-universe relation | Yes | Cold exact-name lookups still pay `world_symbols("")` | `rewrite` via lookup pushdown | `required` |
| `call graph / call_edge` | Mostly RA-backed callable resolution plus custom body traversal | Yes | Nested item attribution and closure handling are approximate | `rewrite` | `required` |
| `dispatch labels / dispatch_str` | Thin deterministic wrapper over call-edge classification | Wrapper acceptable | Depends on audited call graph quality | `keep` after call graph audit | `required` |
| `syntax nodes / node_at / node_kind / node_span / node_parent / enclosing_control` | Custom raw-text syntax parse over files RA already loaded | Yes | Duplicate parse path instead of RA-backed syntax truth | `rewrite` | `required` |
| `type surface / fn_return_type / ty_* / typeref_id` | RA/HIR-backed lowering plus deterministic IDs | Mostly | Needs audit only for any heuristic edge cases | `keep` unless audit finds heuristics | `required` |
| `structure / traits / impls / methods / fields / variants / field` | Mostly HIR-backed extraction | Yes | Semantics are mostly HIR-native, but lookup-shaped evaluation for bound field probes does not exist yet | `rewrite` for pushdown candidates, otherwise `keep` | `required` |
| `stable IDs / handle / span_key / call_id / ref_id / impl_id / node_id / typeref_id` | Deterministic wrappers over extracted entities | Wrapper acceptable | Only honest if the underlying entity set is honest | `keep` for RA-native entities only | `allowed` |
| `search/3 / search` | Host-side semantic search rows | No direct RA primitive, but acceptable wrapper | Must stay aligned with audited def/path truth; pushdown is only allowed if the lookup shape stays exact and RA-backed | `keep` with audit, `rewrite` if pushdown is added | `required` |
| `engine-managed string helpers / contains / starts_with` | Engine-owned evaluation | No | Not a host concern and not a pushdown candidate | `keep` in engine | `required` |
| `reference events / compares / writes / ref_id` | Custom compare/write event summaries collapsed to type-level subjects | Yes | Not a real reference index; conflates distinct references | `disable` until rebuilt from RA reference truth | `disable pending rewrite` |
| `error flow / constructs / propagates / converts / handles` | Custom `Result`/`From`-shaped traversal | Partly | Overclaims broader correctness than the implementation supports | `disable` or narrow sharply until honest | `likely partial disable` |
| `build-script invalidation` | Heuristic parsing of `build.rs` and watched paths | Yes, as far as Cargo/RA expose it | Misses env-driven and conditional cases | `rewrite` | `required` |
| `workspace world stamp / session metadata` | Thin wrapper over daemon workspace state | Wrapper acceptable | Must continue reflecting executed state after sync/reload | `keep` | `required` |

## Immediate Known Gaps

1. Warm queries still rebuild too much of the daemon/runtime path.
2. Workspace truth still relies on raw filesystem scanning.
3. `call_edge` is not yet strict enough about callable ownership boundaries.
4. `compares` / `writes` are approximate and should not stay enabled in that form.
5. Error-flow facts are incomplete outside the current `Result`/`From`-shaped subset.

## Flagged Remaining Non-RA/HIR Seams

1. `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
   `lookup_def_name_rows`
   Exact-name lookup still does tracked-file text scans, raw file reads, path-prefix crate guessing, and RA AST walks over candidate files.
2. `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
   `lookup_function_from_record`
   Function recovery still relies on rel-path/span matching, local file text/span comparison, and broad name lookups.
3. `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
   `collect_lookup_callers_for_named_target`
   Named-target caller fallback still iterates tracked files, reads raw source text, and parses candidate files outside a pure RA identity/reference path.
4. `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
   `sync_workspace_state` and related tracked-file rebuild paths
   Workspace truth and invalidation still depend on custom tracked-file state, full host drops, and raw watch-set reconciliation.
5. `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
   `extract_call_edges`
   Supported call-graph extraction still uses custom body traversal and ownership attribution rather than a stricter RA-native callable identity model.
6. `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
   `extract_syntax_nodes`
   Syntax trees come from RA, but node graph materialization is still a host-owned traversal/wrapper layer.
7. `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
   deterministic-host rebuild/invalidation helpers
   Ordinary edits still force too much host-owned reconstruction instead of finer RA-backed reuse.
8. `/Users/darin/Projects/raql/crates/raql-host-ra/src/workspace_service.rs`
   build-script invalidation helpers
   Build-input discovery still uses heuristic scanning rather than Cargo/RA-backed truth.

## Exit Condition

This matrix is complete when every row has one of:
- `enabled RA-native`
- `enabled thin wrapper`
- `disabled pending rewrite`
