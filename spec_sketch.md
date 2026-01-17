# RAQL v0 Technical Specification

This spec synthesizes the design direction in the convo into one coherent system: RAQL as a context-assembly engine built on top of rust-analyzer (RA). It covers architecture, query/view language, CLI, output formatting contracts, and the minimal primitive schema.

The guiding principle throughout: **semantic truth is necessary but not sufficient**. RAQL must turn semantic truth into **readable, decision-ready artifacts** by default.

---

## Product definition

RAQL is a CLI that answers “code understanding” questions by producing **curated context artifacts** assembled from semantic relationships (types, traits, impls, calls, refs, error flow) rather than returning coordinates alone.

It is designed for:

* Agents operating via tool calls (navigation is not free).
* Humans who want “show me the territory” output without IDE hopping.

It is not:

* A grep replacement.
* A documentation generator that dumps everything by default.
* A full Rust trait solver (it surfaces RA’s best available resolution and labels uncertainty).

---

## Goals and non-goals

### Goals

1. **Comprehension-first defaults**
   Default output is readable, curated, grounded code, not lists of `file:line`.

2. **Queryable assembly**
   The query system can express not only *what* to include, but *how to render* it (intentful extraction, budgets, grouping).

3. **Stable, composable identity**
   Every printed fragment includes a handle usable as input to subsequent commands.

4. **Opinionated “theory of showing”**
   Every “lens” has a clear, canonical output contract that matches its intent.

5. **Fast enough to be conversational**
   Lazy evaluation, bounded outputs, caching, and careful “text access” so we do not accidentally enumerate source text.

### Non-goals

* Perfect stability across arbitrary refactors for identity. Instead: “stable-ish handles + best-effort resolution with confidence.”
* Printing entire subsystems with no limits by default.
* A general-purpose text processing language inside the query engine.

---

## Architecture

### High-level pipeline

1. **rust-analyzer substrate**

   * RA holds file contents in memory (VFS).
   * RA provides syntax trees, HIR, name resolution, type inference, and many semantic relationships.

2. **Fact Providers**

   * Small, well-defined relations derived from RA (some precomputed, many lazy).
   * These form RAQL’s semantic “atoms.”

3. **Query/Assembly Engine**

   * Evaluates “views” (recipes) over facts.
   * Views emit **Fragments** and **Metrics**, not raw row dumps by default.

4. **Renderer**

   * Converts fragments into terminal artifacts or JSONL.
   * Applies expansion policies (STMT/BLOCK/EXPR), line budgets, elision messaging, and grouping.

### Why this split is deliberate

* RA already has code and semantics. RAQL’s value is **assembly and presentation discipline**.
* We keep “text retrieval” out of relational joins by default, so the engine stays semantic and bounded.
* We make text retrieval *first-class for output*, but *guarded for evaluation*.

---

## Core concepts

### Selector

A user-provided reference to a semantic target.

Accepted forms across commands:

1. **Handle** (preferred for precision)

   * `@H:…`
2. **Qualified name**

   * `crate::mod::Type::method`
3. **Bare name** (fuzzy)

   * `process`
4. **Location fallback**

   * `path:line` (no `:col`)
   * Optional disambiguator: `path:line/needle`

### Handle

A stable-ish identifier for a definition.

**Contract**

* Encodes: crate identity, module path, kind, name, and a lightweight “anchor signature hash.”
* Must support best-effort re-resolution:

  * `handle_resolve(handle) -> (Def, confidence, note)`
* Handles are printable and copy/pasteable.

**Tradeoff**

* Perfect stability is costly and fragile. Best-effort resolution + confidence is the pragmatic sweet spot for agent workflows.

### Fragment

The unit of output. A fragment is a readable piece of code or text that is:

* Semantically targeted (Def or Span).
* Rendered with an intentful extraction policy.
* Grounded with repo-relative location.
* Optional highlight anchors.

Conceptually:

* `Fragment { section, group?, rank, kind, render, target(Def|Span), anchor?, title, handle?, meta }`

### Render modes

A small vocabulary describing *how to slice code*:

* `DOC_SIG`
  Doc summary + signature (no body). The default “API surface” slice.

* `ITEM`
  Full syntactic item (struct/enum/trait/impl/fn). Bodies included (subject to budgets).

* `HEADER`
  One-line declaration header (impl header, type header).

* `LINE`
  The containing line of a span.

* `STMT`
  The containing statement (semantic unit for refs, locals).

* `BLOCK`
  The smallest enclosing block / match arm / branch needed to see control flow around a span.

* `EXPR`
  The smallest meaningful expression (critical for compare audits).

**Opinionated constraint:** Keep this set small. If you need 20 modes, you are leaking formatting complexity into the language.

### Metrics

Structured numbers emitted by views:

* counts (callers, impls, refs by kind)
* breakdowns (operators used in comparisons)
* coverage (shown vs omitted)

Metrics are first-class outputs, not “raw tuples.”

---

## CLI design

You asked to avoid “minimal at all costs” while still requiring that each command earns its place. The design below is a **small set of operators** plus **explicit, task-shaped lenses** that are worth having as standalone entry points.

### Command families

1. **Selection**

   * `raql search`

2. **Comprehension (curated artifacts)**

   * Default dossier: `raql <selector>` (alias: `raql explain <selector>`)
   * Explicit “just these” lenses:

     * `raql callers <selector>`
     * `raql callees <selector>`
     * `raql refs <selector> [--kind …]`
     * `raql uses <selector> [--kind …]`
     * `raql impls <selector> [<selector> …]` (intersection use case)
     * `raql interface <selector>` (type surface area, intentional and distinct)

       * This earns its place because “I need to *use* this type” is a different intent than a general dossier.

3. **Narrative / cross-hop**

   * `raql trace <selector> [--to <selector>] [--depth N]`

4. **Verification**

   * `raql audit <check> <selector>`

5. **Subsystem packing**

   * `raql bundle <selector> [--radius N]`

6. **Power user**

   * `raql q` (custom view/query execution)

7. **Code generation**

   * `raql scaffold impl <Trait> for <Type>`

### Why this isn’t “too many commands”

Each of the lens commands above has a materially different output contract:

* `callers` is grouped callsite control flow context.
* `refs` is intent-classified EXPR/STMT slices.
* `uses` is type-site roles across APIs and locals.
* `interface` is API surface (docs + sigs) without usage noise.
* `dossier` is “what is this and what context do I need.”

If a command does not define a distinct assembly axis, it should not exist.

---

## Flags

### Output mode

* `--only {artifact|nav|counts}`

  * `artifact` default: curated code fragments + metrics.
  * `nav`: headers only (map mode).
  * `counts`: metrics only.

* `--jsonl`
  Stream fragments and metrics as JSONL events.

### Scope controls

* `--include-tests`
* `--include-macro`
* `--include-blanket`

### Concrete show knobs (explicit, not “more/full”)

* `--bodies=on|off`
* `--show callers=K|all`
* `--show callees=K|all`
* `--show impls=K|all`
* `--show examples=K|all`

### Budgets (rare but necessary)

* `--max-frags=N`
* `--max-lines=N`

### Explainability

* `--why`
  Annotate fragments with inclusion reasons and edge kinds (direct, through trait, etc).

---

## Output formatting contract

This is where RAQL earns its keep. The output is structured, curated, and predictable.

### Formatting goals

* First screen answers “what is this?”.
* Fragments are grounded but not noisy.
* Grouping makes patterns visible.
* Elision is honest and actionable (“what was omitted + how to expand”).

### Artifact preamble

Single line, log-friendly:

`raql <cmd>  <selector>  ->  <resolved-kind> <resolved-path>  [@H:…]`

**Tradeoff:** This is one line of overhead that pays for itself in debugging and copy/paste workflows.

### Summary section

Always key-value, compact, diff-friendly:

```
# Summary
kind: fn
handle: @H:…
path: crate::…
scope: prod, source, no-blanket
analysis: features=…, target=…
callers: 12 direct, 7 through-trait
```

### Sections and grouping

* Section headings start with `#`.
* Group headings start with `##`.

Example:

* `# Usage patterns`
* `## In fn handle_request(...) [@H:…]`

### Fragment header styles

Two styles depending on `--only`:

#### Artifact mode (semantic-first)

```
FN  fn process(&self, req: Request) -> Result<Response, MyError>
at src/processor.rs:41-78  [@H:…]
```

Rationale:

* Reads like documentation.
* Location is present but does not dominate.
* Prevents “organized grep” vibes.

#### Nav mode (location-first)

```
src/processor.rs:41-78  FN  fn process(&self, req: Request) -> Result<Response, MyError>  [@H:…]
```

Rationale:

* Optimizes jump workflows.
* Matches “map mode.”

### Code blocks

Use a guttered format for terminal readability and highlighting:

* Each line prefixed with `| `
* Anchor lines prefixed with `> | `

Rationale:

* Highlighting without column underlines.
* Works for LINE/STMT/BLOCK/EXPR.
* JSONL provides raw code without gutters for perfect copy/paste.

### Elision messages

Always:

* what was omitted
* how many
* the concrete knob to expand

Examples:

* `… omitted 11 more trait impls (showing 8/19). Add: --show impls=all`
* `… truncated BLOCK context to 80 lines. Add: --max-lines=200`

### Alternatives (ambiguity handling)

If resolution is ambiguous, RAQL prints a picker section:

```
# Alternatives (matched "process")

1. FN  fn crate::processor::process(req: Request) -> Result<Response, MyError>  [@H:…]
   doc: Processes incoming requests.

2. FN  fn crate::ingest::process(blob: Bytes) -> Result<()>  [@H:…]
   doc: Parses ingest payload.

Tip: rerun as `raql @H:…`
```

Rationale:

* Wrong symbol selection is expensive.
* Avoid interactive prompts; keep it scriptable.

---

## Canonical output contracts per command

These are “theories of showing” that make commands earn their place.

### `raql search <query>`

Purpose: selection without committing to a big artifact.

Output:

* A ranked list of candidates with `DOC_SIG` or `HEADER`, doc summary, handle.
* No callsites, no blocks.

Why it earns its place:

* It is the “selector tool” in the Select → Assemble loop.
* It is script-friendly and avoids accidental firehose.

### `raql <selector>` (Dossier, alias `explain`)

Purpose: “what is this and what context do I need to work safely?”

Sections (always in this order):

1. Summary
2. Definition (DOC_SIG by default, ITEM if type/trait)
3. Context you likely need (types, errors, trait parent)
4. Typical usage patterns (bounded, grouped)
5. Related edges (small, bounded)
6. Alternatives (only if needed)

Why it’s distinct:

* It is balanced and curated. It is not “just callers” and not “just refs.”

### `raql interface <selector>`

Purpose: “I need to use this type or trait. Show surface area.”

Default contents:

* Definition ITEM for type/trait
* Inherent methods DOC_SIG
* Trait implementations as HEADER only
* Associated types and key bounds (headers)

Why it earns its place:

* It eliminates usage noise, keeps API surface compact.
* This is a very common agent move.

### `raql callers <selector>`

Purpose: “how is this called, and what control flow surrounds usage?”

Output:

* Target DOC_SIG
* Groups by caller function:

  * caller DOC_SIG
  * callsites rendered as BLOCK with highlight
* Optional “through trait” section, labeled

### `raql callees <selector>`

Purpose: “what does this function touch?”

Output:

* Target DOC_SIG
* Direct callees DOC_SIG (bounded)
* Trait-target callees DOC_SIG (labeled)
* Optional: a small “hot edge” summary (most frequent)

### `raql refs <selector> [--kind K]`

Purpose: “how is this value used, by intent?”

Output:

* Summary with counts per kind, operator breakdown for COMPARE
* Groups by enclosing function (DOC_SIG)
* Each hit rendered by intent:

  * COMPARE: EXPR preferred, plus GUARD context when relevant
  * WRITE/MOVE/PASS: STMT
  * FIELD: LINE or STMT depending on clarity

### `raql uses <selector> [--kind K]`

Purpose: “where does this type appear in APIs and structure?”

Output grouped by role:

* FIELD_TYPE: field signature line
* PARAM_TYPE: signature line
* RETURN_TYPE: signature line
* LOCAL_TYPE: STMT
* WHERE_CLAUSE: LINE
* CONSTRUCT/PATTERN: STMT or BLOCK

### `raql impls <Trait1> [Trait2 …]`

Purpose: intersection queries (capability search).

Output:

* Summary counts
* Candidate types as HEADER (or DOC_SIG if type definition is small)
* Optional: show the matching impl headers under each type

This earns its place because it is a distinct capability search pattern, not just an “impl list.”

### `raql trace <selector> [--to …]`

Purpose: multi-hop narrative with witness paths.

Output:

* Summary with number of paths shown, edge kinds involved
* Path cards:

  * Each hop has a kind label (CALL, PROPAGATE_QMARK, CONVERT_FROM, HANDLE, etc)
  * Each hop includes a small code slice and grounding

Why it’s distinct:

* Trace outputs connected narratives, not exemplars.

### `raql audit <check> <selector>`

Purpose: verification, invariant enforcement.

Checks v0 (each must be sharply defined):

* `compare`: direct comparisons and ordering
* `write`: mutation sites
* `construct`: construction sites (type or variant)
* `handle`: match/handler sites (especially errors)

Output:

* Report-style summary
* Findings grouped by function/module, slices chosen to verify in one glance

Why it’s distinct:

* Its goal is correctness verification, not comprehension.

### `raql bundle <selector> [--radius N]`

Purpose: produce a “virtual source file” that reads like a subsystem.

Output must include:

* Contents (TOC) with numbered items and handles
* Ordered body:

  1. primary definitions
  2. related traits and impl headers
  3. key methods (DOC_SIG by default)
  4. key error types and flows (bounded)

Why it’s distinct:

* It optimizes for linear reading of a closure, not just “what’s relevant.”

---

## JSONL output contract

JSONL is a stream of events. Two main event types:

### Fragment event

```json
{
  "event": "fragment",
  "section": "Usage patterns",
  "group": "@H:caller…",
  "rank": 78,
  "kind": "CALLSITE",
  "render": "BLOCK",
  "handle": "@H:target…",
  "file": "src/http.rs",
  "line_start": 110,
  "line_end": 118,
  "title": "callsite",
  "code": "raw code without gutters",
  "anchor": {"line_start": 113, "line_end": 113},
  "meta": {"edge": "through_trait"}
}
```

### Metric event

```json
{
  "event": "metric",
  "section": "Summary",
  "name": "callers_direct",
  "value": 12
}
```

Rationale:

* Downstream tooling can reconstruct the artifact, filter sections, or feed handles into other commands.

---

## Query language and view system

You flagged that the QL started feeling less approachable. The fix is to **keep a relational core** but add a **view DSL** that is ergonomic and opinionated for assembly.

### Two layers

1. **RAQL Core (relational)**

   * Facts + derived relations
   * Joins, recursion (bounded), aggregates
   * Great for correctness and reuse

2. **RAQL View DSL**

   * Structured around sections, groups, and “show” operations
   * Emits fragments and metrics
   * Compiles to RAQL Core queries and a fragment emission plan

This keeps expressiveness while making authoring intuitive.

### View DSL primitives (minimal but powerful)

* `section "Name" { … }`
* `group by <key> { … }`
* `let <name> = <relation>(...)`
* `top K by <score>`
* `when <predicate>`
* `show <render>(<Def|Span>) kind=<K> title=<T> anchor=<Span?>`
* `metric <name> = <aggregate>`

The crucial ergonomic move: **blessed standard relations** so authors do not hand-join low-level call predicates.

Examples of standard library relations (conceptual):

* `callsites_of(target, mode=direct|through_trait) -> (caller_fn, call_span, resolution_kind)`
* `callees_of(fn) -> (callee_fn, resolution_kind)`
* `impls_of(type) -> (impl_def, trait_def?, impl_kind)`
* `methods_of(type) -> (method_def, impl_def)`
* `refs_of(def, kind?) -> (ref_span, enclosing_fn, kind, meta)`
* `type_sites_of(type, role?) -> (span, owner_def, role)`

### Output sinks in the language

Views can emit:

* fragments
* metrics
* small tables (optional)

This resolves the “metrics without fragments” concern cleanly: metrics are not a fallback to raw tuples.

### Overrides “slot into QL”

CLI flags compile into injected facts:

* `opt_include_tests()`, `opt_include_macro()`, `opt_include_blanket()`
* `opt_bodies_on()`
* `opt_show_callers(K|all)`, etc
* budget facts `opt_max_lines(N)`, `opt_max_frags(N)`
* `opt_only(mode)`

Views consult these to decide what to emit.

---

## Render pipeline and extraction

### Core idea

Views specify *intent* (render mode). Renderer uses RA syntax trees to expand spans.

### Required extraction relations

**Span splitting**

* `def_name_span(Def, Span)`
* `def_item_span(Def, Span)`
* `def_sig_span(Def, Span)`
* `def_header_span(Def, Span)`

**Span expansion**

* `expand_span(Span, Mode, OutSpan)` where Mode in {LINE, STMT, BLOCK, EXPR}

**Text retrieval (guarded)**

* `span_text(Span, Text)`
  Guard: Span must be bound. This prevents “enumerate every line of every file” queries.

**Grounding**

* `span_loc(Span, RelPath, LineStart, LineEnd)`

**Docs**

* `def_doc_summary(Def, OneLine)`
  Used in DOC_SIG and search results.

---

## Minimal primitive schema

This is the semantic waist. It stays small and precise.

### Identity and metadata

* `def_kind(Def, Kind)` (type, fn, trait, impl, field, variant, module…)
* `def_name(Def, Name)`
* `def_path(Def, PathStr)` (qualified)
* `def_crate(Def, Crate)`
* `def_visibility(Def, Vis)`
* `handle(Def, HandleStr)`
* `handle_resolve(HandleStr, Def, Confidence, Note)`

### Search

* `search(QueryStr, Def, Score)`

### Types and type refs

* `typeref_pretty(TypeRef, PrettyStr)`
* `typeref_mentions(TypeRef, Def)` (with written vs normalized split later if needed)
* `type_site(Role, OwnerDef, TypeRef, Span)`
  Role in {field_type, param_type, return_type, local_type, where_clause}

### Traits and impls

* `impl_kind(Impl, inherent|trait)`
* `impl_self_type(Impl, TypeDef)`
* `impl_trait(Impl, TraitDef)` (when trait impl)
* `impl_is_blanket(Impl)`
* `impl_item(Impl, AssocItemDef)`
* `trait_item(TraitDef, AssocItemDef)`
* `assoc_item_kind(AssocItemDef, Kind)`
* `fn_trait_parent(FnDef, TraitDef)` (if trait method)
* `fn_impl_parent(FnDef, ImplDef)` (if in impl)

### Functions

* `fn_sig(FnDef, SigPrettyStr)` (for titles, not necessarily rendering)
* `fn_param(FnDef, Index, ParamDef)`
* `param_type(ParamDef, TypeRef)`
* `fn_return_type(FnDef, TypeRef)`

### Calls

* `call(CallId)`
* `call_in_fn(CallId, CallerFn)`
* `call_span(CallId, Span)`
* `call_target(CallId, CalleeFn)` (may be multiple)
* `call_trait_target(CallId, TraitMethodFn)` (through bounds)
* `call_resolution(CallId, Kind)` (direct, inherent, trait_static, trait_dyn, closure, unknown)

### References (value intent)

* `ref_event(RefId, Def, RefKind, Span, EnclosingFn)`
* `ref_kv(RefId, Key, Val)` (operator, callee, by=move/borrow, etc)

### Error flow (to pass the real “context assembly” test)

* `error_event(ErrorDefOrVariant, Kind, Span, EnclosingFn, Detail)`

  * Kind: CONSTRUCT_VARIANT, RETURN_ERR, PROPAGATE_QMARK, MAP_ERR, MATCH_HANDLE, CONVERT_FROM

This is the “semantic event” primitive that turns error understanding from grep into narrative.

---

## Ranking, budgets, and determinism

### Why ranking must be specified

Without stable ordering, artifacts become noisy and hard to diff. Agents also need predictability to reason.

### Default ranking heuristics (opinionated)

* Prefer workspace defs over external.
* Prefer non-test over test unless included.
* Prefer non-macro over macro unless included.
* Prefer non-blanket impls unless included.
* For callers/usage:

  * rank callers by number of callsites, then by module proximity.
* For impl lists:

  * rank by “referenced in repo” if available, else stable alphabetical by path.

### Default budgets (example starting point)

* Dossier typical usage: show 3 caller groups, 2 callsites per group.
* Callers lens: show 8 caller groups, 2 callsites per group.
* Refs lens: show 5 groups per ref kind, 3 hits per group.
* Type interface: show 25 methods, 8 trait impl headers.
* Bundle: 30 items in TOC, radius 1 by default.

Budgets must produce elision messages with expansion knobs.

---

## The “just these” mechanism

You explicitly want “just callers”, “just refs”, “just uses”, etc. There are two complementary paths:

1. **Explicit lens commands** (recommended for usability)

   * `raql callers X`
   * `raql refs X --kind compare`
   * `raql uses X --kind param_type`

2. **Section selection on any artifact** (composition)

   * `raql X --just callers,refs`
   * `raql bundle X --just contents,definitions`

Implementation: `--just` compiles to facts that filter emitted sections or lens recipes.

Rationale:

* Lens commands are discoverable and ergonomic.
* `--just` supports composition without multiplying commands.

---

## Acceptance tests

These are the “does it feel like context assembly?” tests.

You already listed great ones. This spec treats them as v0 acceptance criteria:

1. **Trait method with multiple impls**
   `raql Processor::process` should assemble: trait method DOC_SIG, impl headers, and method bodies when `--bodies=on`, grouped per implementor.

2. **Callers through generics**
   `raql callers MyTrait::some_method --through-traits` should show DOC_SIG of caller plus BLOCK around `t.some_method()`, labeled through trait.

3. **Type with 15+ trait impls (budgeting)**
   `raql interface Processor` should show top impl headers and a precise elision message.

4. **Guardrail compare**
   `raql audit compare StoreId` should produce EXPR-first findings plus guard context when needed.

5. **Ambiguous symbol**
   `raql process` yields Alternatives picker with handles.

6. **Error flow narrative**
   `raql trace MyError` should show construction, propagation, conversion, and handling paths as readable hop cards.

Additional v0 tests worth adding:

* Macro-heavy callsites: primary source location vs expansion.
* cfg-gated symbols: analysis config stamped in Summary.
* dyn dispatch: labeled `trait_dyn`, show trait method target.

---

## What to implement first

If you want the shortest path to something that feels “real”:

1. Fragment IR + renderer (artifact/nav/counts + JSONL)
2. Span splitting + expand_span + guarded span_text
3. `search` + handle resolution + Alternatives picker
4. Lenses with the highest ROI:

   * dossier
   * callers
   * refs (compare)
   * interface
   * trace (call graph witness)
5. Error_event primitive and `trace MyError`

That sequence de-risks the product: you prove the assembly experience early, not after a year of schema work.

---

## Summary of the adjustments you just asked for

* Yes, **`raql search`** is part of the core UX and earns its place as the “Select” tool that does not force a dossier.
* Yes, “**just these**” is best served by **explicit lens commands** (callers/refs/uses) and optionally a compositional `--just` for section filtering.
* No, we do not optimize for “4 verbs”. We optimize for:

  * each command having a distinct intent and output philosophy
  * a shared assembly substrate so functionality does not create chaos

If you want, I can now do the next concrete step: write the canonical output contracts (section ord[<35;75;33M[<35;76;33Mer, default budgets, exact elision strings) for `search`, `dossier`, `callers`, `refs(compare)`, and `trace(MyError)` as “golden tests” you can snapshot in CI.
