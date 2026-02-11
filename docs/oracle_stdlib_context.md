# RAQL Stdlib Oracle Context Pack (Snapshot: 2026-02-12)

This document is a paste-ready context pack for asking a stronger model to design RAQL v0.1 standard library files.

## Short answer to "Is `language_spec.md` + `language_plan.md` enough?"

Not quite.

Those two files define intent and required semantics, but they do not fully capture what is currently implemented in code. You should also include:

- Required-spec acceptance matrix (what is covered today)
- Runtime/host behavior (what predicates and defaults actually exist)
- Include loading behavior (how `.include "std.raql".` is resolved right now)

## What RAQL is (project purpose)

Source: `/Users/darin/Projects/raql/README.md`

- RAQL is for semantic Rust queries that return readable context, not only file:line hits.
- It aims to produce bounded, deterministic artifacts for humans/agents.
- Target UX includes queries like callers/callees/refs/trace with contextual snippets.

## Source-of-truth order for stdlib design

Use this precedence order when conflicts appear:

1. Required semantics in `language_spec.md`.
2. Acceptance expectations in `required_spec_acceptance_matrix.md`.
3. Current code behavior in `crates/raql-*`.
4. `language_plan.md` as implementation planning guidance.
5. `spec_sketch.md` as conceptual (non-normative) ideas.

## Normative stdlib requirements to preserve

### Include/module model

- Spec examples explicitly include `.include "std.raql".`
  - `/Users/darin/Projects/raql/docs/initial_plans/language_spec.md:371`

### Required-via-std predicate

- `enclosing_control/4` must exist and follow precise semantics:
  - `/Users/darin/Projects/raql/docs/initial_plans/language_spec.md:1455`
  - `/Users/darin/Projects/raql/docs/initial_plans/language_spec.md:1461`
  - `/Users/darin/Projects/raql/docs/initial_plans/language_spec.md:1466`
- Bounded traversal with `control_max_depth` (default 32, runtime injected):
  - `/Users/darin/Projects/raql/docs/initial_plans/language_spec.md:1481`
  - `/Users/darin/Projects/raql/docs/initial_plans/language_spec.md:1486`

### Canonical usage pattern

- Example 17.3 shows expected stdlib usage shape:
  - `/Users/darin/Projects/raql/docs/initial_plans/language_spec.md:1524`

### Plan milestone statement

- Milestone 11 asks for `std.raql`, minimally including `enclosing_control/4`, with optional `span_allowed` and `default_title` helpers:
  - `/Users/darin/Projects/raql/docs/initial_plans/language_plan.md:1954`
  - `/Users/darin/Projects/raql/docs/initial_plans/language_plan.md:1958`
  - `/Users/darin/Projects/raql/docs/initial_plans/language_plan.md:1964`

## What is implemented today (critical to avoid stale design)

### Parser/include loading

- Include loader exists and supports file loading with include dirs:
  - `/Users/darin/Projects/raql/crates/raql-syntax/src/include_loader.rs:18`
  - `/Users/darin/Projects/raql/crates/raql-syntax/src/include_loader.rs:31`
  - `/Users/darin/Projects/raql/crates/raql-syntax/src/include_loader.rs:44`

### Compiler pipeline is present

- Resolve/typecheck/plan entrypoints exist:
  - `/Users/darin/Projects/raql/crates/raql-compiler/src/lib.rs:690`
  - `/Users/darin/Projects/raql/crates/raql-compiler/src/lib.rs:823`
  - `/Users/darin/Projects/raql/crates/raql-compiler/src/lib.rs:1052`
- `out_status/1` is enforced as engine-reserved:
  - `/Users/darin/Projects/raql/crates/raql-compiler/src/lib.rs:841`
  - `/Users/darin/Projects/raql/crates/raql-compiler/src/lib.rs:851`

### Engine runtime behavior

- Default scalar inputs injected by runtime:
  - `/Users/darin/Projects/raql/crates/raql-engine/src/lib.rs:836`
  - Includes `path_limit=1`, `path_max_depth=8`, `control_max_depth=32`, `opt_max_iters=none`.
- `out_status`/`out_note` emission semantics are implemented:
  - `/Users/darin/Projects/raql/crates/raql-engine/src/lib.rs:854`
  - `/Users/darin/Projects/raql/crates/raql-engine/src/lib.rs:866`
- Builtin atom dispatch exists (`contains`, `starts_with`, `fmt`, `coalesce`, and witness helpers path):
  - `/Users/darin/Projects/raql/crates/raql-engine/src/lib.rs:1235`
  - `/Users/darin/Projects/raql/crates/raql-engine/src/lib.rs:1239`
  - `/Users/darin/Projects/raql/crates/raql-engine/src/lib.rs:1242`

### Host extern coverage (RA adapter)

- Extern predicate row dispatch includes required type/node/span predicates and `enclosing_control`:
  - `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs:589`
  - `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs:699`
  - `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs:720`
  - `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs:727`

### Required coverage matrix

- Required sections are tracked as covered in acceptance matrix:
  - `/Users/darin/Projects/raql/docs/required_spec_acceptance_matrix.md:1`
  - Includes explicit rows for "required via std" enclosing-control behavior.

## Known current gaps

- No committed canonical stdlib source file yet (`std.raql` absent in repo tree).
- Main CLI entrypoints are still stubs:
  - `/Users/darin/Projects/raql/crates/raql-cli/src/main.rs:1`
  - `/Users/darin/Projects/raql/src/main.rs:1`
- Include loader is implemented, but end-to-end product wiring for stdlib loading is not finalized.

## Paste package recommendation

If context budget is tight, paste these first:

1. `/Users/darin/Projects/raql/docs/initial_plans/language_spec.md` sections around 4.1 includes, 16.2 enclosing control, and 17.3 example.
2. `/Users/darin/Projects/raql/docs/required_spec_acceptance_matrix.md`.
3. `/Users/darin/Projects/raql/docs/initial_plans/language_plan.md` Milestone 11 section.
4. `/Users/darin/Projects/raql/crates/raql-syntax/src/include_loader.rs`.
5. `/Users/darin/Projects/raql/crates/raql-engine/src/lib.rs` sections for defaults and builtins.
6. `/Users/darin/Projects/raql/crates/raql-host-ra/src/lib.rs` extern relation dispatch section.

If budget allows, also add:

- `/Users/darin/Projects/raql/crates/raql-host-ra/tests/extern_runtime_dispatch.rs`
- `/Users/darin/Projects/raql/crates/raql-host-ra/tests/required_sections_16.rs`
- `/Users/darin/Projects/raql/README.md`

## Copy/paste prompt for Oracle model

```text
Treat all pasted files as authoritative as of 2026-02-12.
Do not rely on prior/default RAQL assumptions.

Task: design RAQL v0.1 standard library and provide exact files to add.

Hard constraints:
- Conform to required semantics in language_spec, especially enclosing_control/4 and control_max_depth bounded behavior.
- Align with current parser/compiler/runtime/host behavior in pasted code.
- Do not invent new syntax or runtime capabilities unless clearly marked optional.
- Prefer minimal, testable stdlib first; optional helpers second.

Deliverables:
1) Proposed stdlib file tree.
2) Full file contents for each proposed file.
3) Acceptance tests mapped to required_spec_acceptance_matrix rows.
4) Explicit list of required code changes vs optional nice-to-have changes.
5) A migration order (smallest safe PR sequence).
```

