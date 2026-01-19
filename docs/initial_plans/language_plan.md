Below is a practical, engineering-first implementation plan for RAQL v0.1 as specified. I’m going to treat your spec as the “contract” and turn it into a build order, module boundaries, algorithms, and an acceptance-test matrix.

I’ll assume Rust for the engine + a rust-analyzer-backed host adapter, because your spec is already leaning that way.

---

## v0.1 scope checklist (what we are building)

### Language surface

* Lexer + parser for:

  * `.include`, `.type`, `.decl`, `.func`, `.mode`, `.pragma`
  * facts + rules
  * `not`, disjunction `( … ; … )`
  * constraints (`= != < <= > >=`, arithmetic)
  * aggregates (`count`, `count_distinct`, `sum`, `min`, `max`)
  * `choose_topk(...)` binder
  * option terms: `none`, `some(T)`
  * list literals: `[ ... ]`

### Static semantics

* Name resolution (predicates, types, enums)
* Type checking (light, but real)
* Mode checking (existence of a valid goal ordering per rule)
* Stratification:

  * negation
  * `choose_topk`
  * `witness_path` / `path_hop`
  * aggregates treated as non-monotonic for stratification (see below)
* Safety/range restriction (head vars must be bound by a positive goal)

### Runtime semantics

* Set semantics (no duplicates)
* Bottom-up evaluation to least fixpoint, per stratum
* Recursion guard (`max_iters`, fail-soft with `out_status(partial)` + `out_note(...)`)
* Built-ins:

  * `choose_topk`
  * `witness_path` + `path_hop` (engine-provided “extern” is fine)
  * `contains`, `starts_with`, `fmt`
  * `coalesce`

### Artifact contract

* Output relations (with `Seq`): `out_def_frag`, `out_span_frag`, `out_frag_kv`, metrics
* Engine-emitted: `out_status("ok"|"partial")`, `out_note(Section, Text)`

### Host contract (rust-analyzer adapter)

* Opaque value handles: `Def`, `Span`, `TypeRef`, `Node`, etc.
* Required externs: `handle/2`, `span_loc/2`, type structure (§16.1), node context (§16.2), and whatever base schema your views use.

---

## Architecture: four layers, one clean seam

Think of it like a kiln: the compiler shapes the clay, the engine fires it, the host provides the raw ore, the renderer plates the result.

### 1) Frontend crate: `raql_syntax`

**Responsibilities**

* Lexer (comments, strings, identifiers, numbers)
* Parser -> AST with source spans for good error messages
* Include loader (search path, cycle detection, “include stack” diagnostics)

**Key deliverable**

* `ProgramAst` with:

  * directives
  * type declarations
  * predicate declarations
  * mode declarations
  * rules/facts

### 2) Compiler crate: `raql_compiler`

**Responsibilities**

* Build symbol tables:

  * predicate schemas + attributes
  * enum types + variants
  * built-in predicates registry
* Type checking + term annotation
* Desugaring:

  * disjunction `(A ; B)` -> multiple rules
  * (optional) normalize constraints into internal form
* Static checks:

  * range restriction
  * mode validity (ordering exists)
  * stratification + SCC checks (negation/selection/aggregate)
  * choose_topk / witness_path not allowed in recursive SCC

**Key deliverable**

* `ProgramIr`:

  * fully resolved predicate ids
  * fully typed terms
  * per-rule evaluation plan (goal ordering selected by mode checker)
  * strata assignment + per-stratum SCC partitions
  * list of required extern predicates + their allowed modes

### 3) Engine crate: `raql_engine`

**Responsibilities**

* Relation storage (set semantics)
* Semi-naive fixpoint per stratum/SCC
* Execution of rule plans (joins, filters, negation checks)
* Built-ins:

  * string helpers
  * coalesce
  * aggregates
  * choose_topk binder
  * witness_path/path_hop binder + path storage
* Recursion guard and partial-run behavior
* Output collection

**Key deliverable**

* `Engine::run(program_ir, host, inputs) -> Outputs`

### 4) Host crate: `raql_host_ra` (rust-analyzer adapter)

**Responsibilities**

* Provide extern relations/functions with mode-respecting access
* Maintain stable handles and span loc strings
* Cache expensive lookups (handle/span_loc/type normalization)
* Provide `node_at` / parent chain primitives

**Key deliverable**

* `impl Host for RaHost { … }`

### 5) CLI + renderer crate: `raql_cli`

**Responsibilities**

* Load program (includes)
* Provide default input facts (`path_limit`, `path_max_depth`, `control_max_depth`, `max_iters`)
* Run engine
* Render outputs:

  * sort fragments by `(Section, Group, Rank desc, Seq asc, Kind)`
  * display spans using host/renderer expansion rules

---

## Milestones (ordered so you always have something runnable)

No time estimates, just build order and “definition of done.”

### Milestone 0: Skeleton + a runnable “toy”

**Build**

* Workspace with crates above
* Minimal lexer/parser that accepts:

  * `.decl`, facts, and a single non-recursive rule with plain predicate calls

**Done when**

* You can run a program like:

  ```raql
  .decl a(X: int) input.
  .decl b(X: int) output.
  b(X) :- a(X).
  ```
* CLI prints `b(…)` results.

---

### Milestone 1: Full parser (v0.1 grammar coverage)

**Add**

* Comments: `%`, `//`, `/* */` (nested if you want)
* Strings with escapes
* Directives: `.include`, `.type`, `.mode`, `.pragma`
* Terms: `none`, `some(...)`, list literals
* Rule bodies: `not`, disjunction groups, constraints, aggregates, `choose_topk(...)`

**Done when**

* Parser roundtrips examples from your spec without loss of structure.
* Error recovery gives usable messages (file/line/col, expected tokens).

**Important design note: identifier ambiguity**
Your spec allows **variables** like `S` and **enum atoms** like `IF`. Both are uppercase.
Implementation plan: **lex identifiers uniformly** and disambiguate during type checking:

* If an identifier matches an enum variant AND the expected type is that enum type → treat as enum constant.
* Otherwise → treat as variable.
* `_` alone → wildcard.

This keeps your surface syntax intact and avoids inventing new sigils.

---

### Milestone 2: Declarations + typing (light but strict)

**Add**

* Type system:

  * primitives: `int/string/bool`
  * `option<T>`, `list<T>`
  * enums
  * opaque host types by name (`Def`, `Span`, `TypeRef`, …)

**Implement**

* Predicate schema registry from `.decl` / `.func`
* Type checking of atoms:

  * arity match
  * each argument term type compatible with schema
* Type inference for variables:

  * variable types unify across occurrences
  * `some(T)` infers `option<type(T)>`
  * `none` is polymorphic but constrained by context

**Done when**

* Wrong-arity / wrong-type programs are rejected with precise diagnostics.
* Enum variant resolution works and doesn’t break `S`/`D` variables.

---

### Milestone 3: Mode system + rule plan construction

**Add**

* Parse `.mode pred(+Type, -Type, ?Type, …).`
* Store allowed modes per predicate.

**Implement mode checking as in your spec**

* For each rule:

  * Consider only **positive goals** (non-negated atoms, aggregates, choose_topk, func calls) to build an ordering.
  * Find an ordering where each goal’s `+` args are bound when executed.
* If no ordering exists → compile error.

**Practical algorithm**

* Treat each goal as a node with a set of required-bound vars.
* Greedy/topological-style:

  * Start with vars bound by constants in rule + facts already bound in earlier chosen goals
  * Repeatedly pick a goal whose required inputs are satisfied; add its outputs to bound-set.
  * If stuck → invalid.

**Done when**

* Mode-invalid rules are rejected.
* Compiler produces a concrete **execution order** for each rule body (even though semantics are order-free).

---

### Milestone 4: Stratification + SCC analysis (negation + non-monotonic bits)

**Add dependency graph edges**
For head predicate `P` and a referenced predicate `Q`:

* **Positive edge** `P -> Q` for normal atoms in body
* **Negative edge** `P -/-> Q` for `not Q(...)`
* **Selection edge** `P ~/> Q` for:

  * any predicate used inside `choose_topk(... : Goals)`
  * any predicate used inside `witness_path(...)` / `path_hop(...)` goals (or more precisely, treat these built-ins as stratum barriers)
  * any predicate used inside aggregate subgoals

**Compute SCCs** (Tarjan/Kosaraju)

**Validate**

* Stratification constraints:

  * positive: stratum(P) >= stratum(Q)
  * negative/selection/aggregate: stratum(P) > stratum(Q)
* Additionally:

  * `choose_topk` forbidden if rule head is in an SCC that includes any predicate referenced inside its `Goals`
  * `witness_path`/`path_hop` forbidden in recursive SCCs
  * aggregate recursion forbidden (cycle through aggregate edges)

**Done when**

* The compiler either:

  * produces strata (list of predicates per stratum), or
  * rejects with a clear “cannot stratify due to negative/selection cycle” error.

---

### Milestone 5: Core evaluation engine (facts + monotone rules)

**Implement**

* Relation storage:

  * `HashSet<Tuple>` for dedup
  * optional indexes: `HashMap<Key, Vec<TupleId>>` or `BTreeMap` for determinism in certain operations
* Rule execution plan:

  * join in the mode-chosen goal order
  * unify terms, extend bindings
  * emit head tuples

**Fixpoint**

* Evaluate per stratum:

  * within stratum, evaluate SCCs to fixpoint
* Start with a naive implementation, then upgrade to semi-naive (delta relations) once correct.

**Done when**

* Simple recursive transitive closure works:

  ```raql
  .decl edge(A:int,B:int) input.
  .decl reach(A:int,B:int).
  reach(A,B) :- edge(A,B).
  reach(A,C) :- reach(A,B), edge(B,C).
  ```

---

### Milestone 6: Constraints, unification, options, lists

**Implement**

* Unification:

  * variable binding
  * wildcard `_` never binds
  * structural match for `some(T)` and list terms
* Constraints:

  * `term = term` as unification (or equality check if both bound)
  * `var = expr` binds or validates
  * comparisons require both sides bound
* Arithmetic expressions for `int`

**Done when**

* Your optionality examples work without two-rule boilerplate:

  ```raql
  def_doc_summary(D, some(Doc)), contains(Doc, "unsafe").
  def_doc_summary(D, none).
  ```

---

### Milestone 7: Negation (stratified)

**Implement**

* During evaluation of a stratum:

  * negated goals may only reference lower strata (guaranteed by compiler)
  * so `not p(...)` is a membership check against a completed relation
* Enforce range restriction:

  * every head var must appear in some positive goal
  * for disjunction, enforce per-branch after desugaring

**Done when**

* You can express “all defs without docs” safely and deterministically.

---

### Milestone 8: Aggregates

**Implement binder evaluation**
For:

```raql
N = count(V : Goals).
```

* During rule execution, when reaching the aggregate goal:

  * evaluate `Goals` under current outer binding
  * accumulate aggregate value
  * bind `N`

**Implementation details**

* Give aggregates their own internal mini-evaluator:

  * it runs over already-materialized relations (lower stratum by construction)
  * local vars scoped

**Done when**

* Counting examples work and are stable.

---

### Milestone 9: `choose_topk` binder (deterministic)

**Implement**

* Evaluate candidate set for each outer binding + `Group`:

  * run `Goals` to generate `(Score, Item)` pairs
  * dedup pairs
  * sort by:

    1. Score descending
    2. `stable_order(Item)` ascending
    3. Score ascending (int order) for final tie-break if needed
  * emit top K as bindings

**You must implement `stable_order`**

* primitives: straightforward
* for `Def`: call host `handle(Def, string)` and compare strings
* for `Span`: call host `span_loc(Span, string)` and compare strings
* for `Path`: internal deterministic id

**Caching**

* Cache `handle` and `span_loc` results aggressively; top-k and witness paths will otherwise thrash.

**Done when**

* A view can select top 5 callers, deterministically, on repeated runs.

---

### Milestone 10: `graph_edge` + `witness_path` + `path_hop`

Even though the spec marks these as `extern`, you can implement them inside the engine as **built-ins** that read the already-computed `graph_edge/5` relation. That keeps the feature available for any host and avoids pushing complex graph search into the adapter.

**Implement `witness_path(Graph, From, To, P)`**

* Inputs:

  * `path_limit(N)` (default injected by runtime)
  * `path_max_depth(N)` (default injected by runtime)
* Build adjacency list per Graph:

  * edges are tuples `(From, To, EdgeKind, Evidence)`
  * sort outgoing edges by hop stable key:
    `(stable_order(From), stable_order(To), EdgeKind, stable_order(Evidence))`
* Enumerate paths in ranked order:

  1. shortest hop count first (BFS layers)
  2. within same length, lexicographic by hop keys
* Return up to `path_limit` distinct paths.

**Represent Path**

* `Path` is an engine-opaque value:

  * `struct PathId(u64)`
  * store `Vec<Hop>` in an arena/map keyed by `PathId`
* `path_hop(P, Seq, ...)` reads hops from the map.

**Done when**

* Your trace example works exactly:

  * Seq is 0-based, increasing, no gaps
  * repeated runs return identical paths and ordering

---

### Milestone 11: Recursion guard + partial honesty outputs

**Implement**

* `.pragma max_iters = N` (default 128)
* input override `opt_max_iters(N)`
* For each recursive SCC during fixpoint:

  * count iterations
  * if exceed:

    * stop evaluating that SCC
    * mark run partial
    * emit:

      * `out_status("partial")`
      * `out_note("Notes", "fixpoint iteration limit exceeded in SCC: <name>")`
* If no SCC exceeds limit:

  * emit `out_status("ok")`

**Done when**

* A deliberately divergent recursive program halts cleanly and reports partial results.

---

### Milestone 12: Standard library `std.raql` (v0.1)

**Provide (at minimum)**

* `enclosing_control/4` as derived predicate using:

  * `node_at`, `node_parent`, `node_kind`, `node_span`
  * bounded by `control_max_depth` default 32 (runtime inject)
* convenience helpers:

  * `span_allowed` / repo scoping hooks (optional)
  * `default_title` helpers (optional)

**Implementation choice**

* You can implement `enclosing_control`:

  * in RAQL itself (recursive parent walk with depth counter), or
  * as a host extern/built-in for speed
    Either is spec-compliant as long as semantics match.

**Done when**

* Example 17.3 produces correct enclosing control spans.

---

### Milestone 13: rust-analyzer adapter (the “load-bearing externs”)

Implement the required extern predicates/functions from §16.

**Host value representation**

* Define opaque wrapper types:

  * `DefId`, `SpanId`, `TypeRefId`, `NodeId`, etc.
* Maintain stable ordering keys:

  * `handle(Def) -> String`
  * `span_loc(Span) -> String` formatted consistently

**Type structure**

* Map rust-analyzer HIR types into a stable `TypeRef` handle layer
* Provide:

  * `ty_ctor` (recommended)
  * `ty_app`, `ty_arg`
  * wrappers: `ty_ref`, `ty_ptr`, `ty_tuple`, `ty_slice`
  * `ty_param`, `ty_prim`, `ty_unknown`
  * `ty_normalize` (strongly recommended, even if initially identity)

**Node context**

* `node_at(Span) -> option<Node>`:

  * locate smallest syntax node covering span (or whatever you define, but stable)
* `node_kind`, `node_span`, `node_parent`

**Done when**

* You can run the three canonical patterns from §17 against a real codebase snapshot:

  * optional docs
  * structural type match
  * enclosing control + witness trace

---

### Milestone 14: Renderer + “query mode”

**Renderer responsibilities**

* Consume output relations and present results deterministically:

  * sort by contract keys
  * fetch/render spans as needed
* Provide:

  * “query” output (raw relations)
  * “fragments” output (human-focused)

**Done when**

* A view produces stable, readable output for repeated runs, with Seq respected.

---

## Acceptance tests: a matrix tied to your spec

### 1) Parser golden tests

* All examples in §17 parse.
* Disjunction desugars into multiple rules.
* Option terms and list literals parse.

### 2) Typing tests

* Mis-typed predicate args are rejected.
* `some(...)` and `none` inference works.
* Enum atoms resolve only when the expected type is that enum.

### 3) Mode tests

* Guarded extern like `span_text(+Span,-string)` is rejected if Span not bound.
* Mode-valid program compiles even if goals are written “out of order”.

### 4) Stratification tests

* Classic invalid:

  ```raql
  p(X) :- not q(X).
  ```

  rejected for range restriction.
* Non-stratifiable negation cycles rejected with clear message.
* choose_topk inside recursion rejected.

### 5) Engine semantics tests

* Set semantics: duplicates eliminated.
* Fixpoint: transitive closure matches expected.
* Negation matches stratified semantics.

### 6) choose_topk determinism test

* Two items with equal score: stable_order(Item) tie-break applied consistently.

### 7) witness_path determinism test

* Multiple shortest paths: lexicographic ho[<35;136;41Mp key ranking stable.
* `Seq` from `path_hop` is 0..n-1 exactly.

### 8) Recursion guard test

* A runaway SCC hits max_iters:

  * outputs include `out_status("partial")`
  * note emitted
  * partial derived facts preserved

---

## Two “sharp corners” to decide early (so they don’t bite later)

### A) How to name/resolve enum atoms vs variables

Recommended (matches your syntax and examples):

* Parse identifiers as “UnresolvedIdent”
* During type checking:

  * if position expects enum `E` and token matches a variant of `E` → treat as enum atom
  * else treat as variable (unless it’s `_`)
    This preserves both `S` variables and `IF` constants.

### B) What is “extern” in practice

Treat `extern` as “provided by the runtime host,” where:

* some externs [<35;137;41Mare **engine built-ins** (strings, coalesce, witness_path)
* some externs are **adapter-provided** (rust-analyzer facts)
  This keeps the language spec intact and makes the system modular.

---

## Optional but high-leverage extras (still v0.1-friendly)

These aren’t required by the spec, but they make v0.1 feel sturdy:

1. **Index planning from modes**
   If a predicate mode is `(+Def,-Span)`, build an index keyed by `Def` for that relation.

2. **Key caching for stable_order**
   Memoize:

* `handle(Def)`
* `span_loc(Span)`
  This is the difference between “snappy” and “why is topk slow?”

3. **Explain mode errors**
   When mode-check fails, print:

* which predicate call needed which vars bound
* one suggested ordering or the point where it got stuck

---[<35;137;42M

## A concrete “first slice” plan (what to implement in week-one code, conceptually)

If you want the fastest path to something demonstrably working:

1. Parser (complete)
2. Decls + type checking (enough for `int/string/bool`, opaque types, option)
3. Monotone engine (facts + rules + recursion)
4. Mode checker (so extern calls don’t explode)
5. Add choose_topk
6. Add witness_path/path_hop
7. Add recursion guard + out_status/out_note
8. Only then: rust-analyzer adapter breadth

That order ensures you don’t get stuck building a huge adapter before the language runtime is solid.

---

If you want, I can also produce (in the same “no interpretation gaps” style as your spec):

* a **module-by-module Rust API sketch** (traits, structs, key methods), and
* the **[<35;138;42M[<35;139;42Mcanonical `std.raql`** plus **three golden example views** as executable acceptance tests, exactly as you suggested.

