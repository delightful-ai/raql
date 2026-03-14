# RA-Native Audit and Correctness-First Hardening Design

**Date:** 2026-04-09

## Goal

Audit every place RAQL currently substitutes custom logic for rust-analyzer truth, then replace or remove those substitutions so the supported daemon path is built on RA rather than merely informed by RA.

This effort is correctness-first. Latency work matters, but it is subordinate to the central rule: semantic truth must come from RA-owned state, with only small deterministic wrappers layered on top.

## Direction

The hard rule for this epic is:
- if semantic truth should come from rust-analyzer, RAQL must not invent it itself
- small deterministic wrappers are allowed for stable IDs, normalization, and transport shape
- approximate primitives are disabled until they are RA-native

This audit covers both:
- the daemon/runtime semantic layer
- daemon-path frontend work such as parse/resolve/typecheck/plan lifecycle and caching

## RA-Native Acceptance Rule

A primitive, runtime path, or semantic family is considered **RA-native** only if:
- semantic truth comes from RA-owned state:
  - HIR
  - IDE layer
  - VFS
  - Cargo/workspace model
  - RA parsing/analysis data
- the daemon does not rescan the filesystem to rediscover semantics RA already knows
- the runtime does not reparse source text itself when RA already has the parsed form needed
- the system does not infer semantics from heuristics when RA exposes a real answer or sufficient substrate

Allowed wrappers:
- deterministic stable IDs derived from RA-backed entities
- normalized enum/string labels for the stdlib surface
- transport/result shaping in the daemon protocol
- daemon-local caching for query parse/resolve/typecheck/plan work

Not allowed:
- custom semantic approximations presented as truth
- filesystem-scanned workspace membership as a substitute for Cargo/RA state
- homemade semantic analyses when RA exposes the needed substrate
- leaving approximate families live for convenience

## Audit Matrix

Every relevant surface gets a row with:
- `surface`
- `current source of truth`
- `should RA own this?`
- `current gap`
- `action`
- `ship state`

The live audit inventory for this design is tracked in:
- `docs/superpowers/specs/2026-04-09-ra-native-audit-matrix.md`

Initial audit rows:

1. `workspace scope / membership`
- Current: mixed custom filesystem scan plus RA state
- Should RA own it: yes
- Action: replace with Cargo/RA-loaded workspace and VFS truth
- Ship state: required

2. `reload / invalidation`
- Current: custom scan and rebuild heuristics
- Should RA own most of it: yes
- Action: drive from RA/VFS/workspace state as far as available; keep only narrow wrappers
- Ship state: required

3. `syntax nodes / node_at / enclosing_control`
- Current: custom raw-text parse pass
- Should RA own it: yes
- Action: replace with RA parse tree access and sema-backed attribution
- Ship state: required

4. `call graph`
- Current: mostly RA-backed but still custom traversal/attribution with known approximation
- Should RA own it: yes
- Action: tighten to RA-native callable ownership and boundary handling
- Ship state: required

5. `reference events / ref ids`
- Current: custom compare/write event summary
- Should RA own it: yes
- Action: disable approximate layer unless rebuilt from RA reference truth
- Ship state: disable until real

6. `error flow`
- Current: custom Result/From heuristic
- Should RA own substrate: partly yes, partly thin wrapper
- Action: narrow claim or disable until honestly derivable from RA-backed facts
- Ship state: partial disable allowed

7. `type / trait / impl / structure facts`
- Current: mostly RA-backed, but must be audited for custom interpolation
- Should RA own it: yes
- Action: keep only where backed by RA/HIR truth; rewrite any heuristic edges
- Ship state: required

8. `query compile pipeline`
- Current: reparsed/replanned per request in daemon path
- Should RA own semantics: no, but daemon should own lifecycle/caching
- Action: add daemon-local compiled/planned query cache keyed by query content, include graph, and execution options
- Ship state: required

9. `stable IDs`
- Current: deterministic wrappers over host entities
- Should RA own semantics: no, wrappers are acceptable
- Action: keep only over audited RA-native entities
- Ship state: allowed

## Cutover Rule

For each row in the audit matrix, the supported daemon path does exactly one of:
- keep it as RA-native
- rewrite it as a thin deterministic wrapper over RA-native truth
- disable it

There is no fourth option.

If a stdlib primitive family fails the audit and is not yet rewritten, capability gating disables it. Coverage may temporarily shrink, but what remains is honest.

## Testing Rule

Each audited family must have:
- a capability-gating test
- a workspace-service test
- a daemon-path CLI test

Cross-cutting suites required for this epic:
- `truth tests`: targeted cases designed to catch semantic overclaim or approximation
- `latency tests`: bounded measurements for warm query, cold query, edit-refresh, and workspace reload
- `corpus tests`: representative stdlib queries run against `raql` and `tmp/rust-analyzer`

## Latency Budget

Latency is a tracked outcome, not the primary design driver. The target budget is:
- `warm query`: p50 < 100ms, p95 < 300ms
- `cold first query`: p50 < 2s, p95 < 5s
- `single-file edit to fresh answer`: p95 < 500ms
- `workspace-shape reload`: p95 < 8s

Anything above 15s in normal use is a failure state.

## Known Gaps Entering the Epic

Current known problems that must be audited and either fixed or disabled:
- daemon path still reparses/resolves/typechecks/plans queries on every request
- workspace sync still performs broad filesystem scans and aggressive full host rebuilds
- call graph currently over-attributes some nested bodies and drops closure bodies
- reference-event layer is a narrow type-level event summary rather than a true reference index
- error-flow layer is largely Result/From-shaped and incomplete for broader carrier/conversion patterns
- build-script invalidation still relies on incomplete heuristics

## Acceptance Criteria

The epic is complete when:
- every supported primitive/family has an explicit audit verdict recorded
- no known-approximate family remains enabled
- workspace truth comes from RA/Cargo/VFS, not raw filesystem heuristics
- query frontend work is cached in the daemon where appropriate
- measured latency is tracked against the stated budget
- representative stdlib corpus runs on `raql` and `tmp/rust-analyzer` are part of completion evidence
- the supported daemon path is honest about what it knows and how it knows it
