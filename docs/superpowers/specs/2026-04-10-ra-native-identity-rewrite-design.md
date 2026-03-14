# RA-Native Identity Rewrite Design

**Date:** 2026-04-10

## Goal

Replace the current synthetic function recovery and named-target fallback logic in the daemon-backed rust-analyzer host with a single RA-native identity model for the hot lookup path.

This rewrite is intentionally scoped to the supported `P0` path:
- exact-name and bound-def lookup
- function-backed `def_*` lookups
- `call_edge` lookup and caller/callee expansion

Disabled families stay disabled. Broader structure/type/syntax rewrites remain follow-on work.

## Why this rewrite exists

The current host still mixes RA/HIR truth with custom recovery logic:
- exact-name lookup still falls back to tracked-file text scans and candidate-file AST walks
- function recovery still relies on rel-path/span/name reconstruction
- bound-callee caller lookup still has a named-target fallback that iterates tracked files and reparses text

That is the wrong architecture. The supported daemon path should operate on RA/HIR identity directly and treat IDs, spans, and names as derived wrappers.

## Architecture

The rewrite introduces three layers:

1. `RA identity layer`
- real semantic truth from rust-analyzer/HIR
- no synthetic semantic recovery

2. `query-shaped provider layer`
- exact lookup and relation emission for the extern families the query actually uses
- no whole-universe precomputation for hot-path lookups

3. `deterministic wrapper layer`
- RAQL-facing `DefId`, `SpanId`, `Call`, `Impl`, and string outputs
- thin wrappers only; no semantic rediscovery

## Identity model

The supported hot path uses a shared cache entry model:

```rust
enum RaEntity {
    Function(hir::Function),
    Adt(hir::Adt),
    Trait(hir::Trait),
    Variant(hir::Variant),
    Module(hir::Module),
    Const(hir::Const),
    Static(hir::Static),
    TypeAlias(hir::TypeAlias),
    Macro(hir::Macro),
    Impl(hir::Impl),
}

struct RaSpan {
    file_id: base_db::EditionedFileId,
    range: syntax::TextRange,
}

struct LookupEntry {
    entity: RaEntity,
    span: Option<RaSpan>,
}
```

Key rule:
- `DefId` is not semantic truth
- `DefId` is a deterministic wrapper key into `LookupEntry`

## Provider map

### `DefProvider` `P0`

Surface:
- `def`
- `def_name`
- `def_kind`
- `def_span`
- `def_path`
- `handle`
- `method_of`
- `is_public`
- `in_test`

Requirements:
- function-backed entries must carry `hir::Function`
- non-function entries must carry the correct `RaEntity` variant
- no function lookup may depend on rel-path/span/name reconstruction

### `CallProvider` `P0`

Surface:
- `call_edge`
- `dispatch_str`
- `call_id`

Requirements:
- caller/callee identity must be `hir::Function`
- site identity must come from RA syntax ranges
- outgoing/incoming call lookup must operate on function identity, not named-target recovery
- supported function call-edge paths must not call `collect_lookup_callers_for_named_target`

### Follow-on providers `P1`

These are explicitly out of scope for the first rewrite pass:
- `StructureProvider`
- `TypeProvider`
- `SyntaxProvider`
- `SearchProvider`
- build/reload cleanup around them

## Scope

### In scope

- shared RA-native identity/cache layer
- `DefProvider`
- `CallProvider`
- removal of synthetic function recovery from the supported hot path
- removal of named-target function caller fallback from the supported hot path
- rewrite of supported lookup caches around `LookupEntry`

### Out of scope

- full structure/type/syntax provider rewrite
- build-script invalidation redesign
- disabled families
- engine-owned helpers

## Execution model

Supported execution remains:
- daemon-backed only
- incremental rust-analyzer runtime only

Direct runtime remains dev-only and quarantined.

The rewrite does not change that boundary.

## Deletions this rewrite should cause

After the rewrite lands, the supported function path must no longer depend on:
- tracked-file text scans for function identity recovery
- `lookup_function_from_record` on supported function lookup paths
- `collect_lookup_callers_for_named_target` for function call edges

Code may remain temporarily for non-function fallback or non-rewritten families, but it must not sit on the supported `P0` path.

## Verification gates

### Correctness

- exact-name seeded function lookup passes
- bound-callee caller lookup passes
- alias-based caller lookup passes
- repo caller probe passes
- stdlib exact-name seed query passes on the daemon path
- stdlib caller query passes on the daemon path

### Performance

Release-only:
- real query: `/Users/darin/Projects/raql/views/stdlib_callers_load_and_plan.raql`
- do not regress warm path from the current best `~59ms`
- improve cold path from the current best `~4.1s` if the new identity model removes remaining synthetic work

### Architectural

Supported function lookup path must not:
- recover function identity from rel-path/span/name heuristics
- use the named-target caller fallback
- require tracked-file text scans to answer bound function call-edge lookups

## Acceptance

This rewrite is complete when:
- `LookupEntry` is the semantic source of truth for the supported hot path
- `DefProvider` and `CallProvider` are both operating on RA-native identity
- the supported daemon path no longer uses synthetic function recovery or named-target function fallback
- release verification on the real repo still passes with a non-regressed warm path
