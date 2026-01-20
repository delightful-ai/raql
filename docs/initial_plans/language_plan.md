Below is a practical, engineering-first implementation plan for RAQL v0.1 as specified. I'm going to treat your spec as the "contract" and turn it into a build order, module boundaries, algorithms, and an acceptance-test matrix.

I'll assume Rust for the engine + a rust-analyzer-backed host adapter, because your spec is already leaning that way.

---

## Type inference upgrade: usability-first, still static

Goal: agents and humans can write short one-off queries without `.decl` boilerplate and without fighting inference.

Key deliverables:

* Derived predicate schemas inferred when `.decl` omitted
* Constraint-based type inference with type variables
* Typed literal escape hatch:
  * `none::<T>`
  * `[]::<T>`
  * `[... ]::<T>`

Diagnostics are part of the feature:

* ambiguity errors MUST suggest a fix (turbofish or `.decl`)
* errors MUST explain which constraints were missing

---

## Nonnegotiable: Rust-grade diagnostics (first-class)

Treat diagnostics as a product feature, not an afterthought.

**Recommended stack:**

* `miette` for rich, multi-span diagnostics and reports
* `thiserror` for structured error enums
* snapshot tests for diagnostic output with colors disabled

**Diagnostic requirements:**

* every error has a primary span and a short message
* wherever possible, include:
  * secondary labeled spans
  * `help:` suggestions (how to fix)
  * stable error codes (e.g., `RAQL0001`)
  * include-stack context for `.include` chains

**Make diagnostics testable:**

* render with `miette` report handler configured without ANSI color
* snapshot the rendered output text

**Error code ranges (suggested):**

* `RAQL0001-0099`: parse errors
* `RAQL0100-0199`: name resolution errors
* `RAQL0200-0299`: type errors
* `RAQL0300-0399`: mode errors
* `RAQL0400-0499`: stratification errors
* `RAQL0500-0599`: safety/range-restriction errors
* `RAQL0900-0999`: runtime errors

---

## v0.1 scope checklist (what we are building)

### Language surface

* Lexer + parser for:

  * `.include`, `.type`, `.decl`, `.func`, `.mode`, `.pragma`
  * facts + rules
  * `not`, disjunction `( … ; … )`
  * constraints (`= != < <= > >=`, arithmetic binding via `:=`)
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
  * aggregates:
    * **strict stratum barrier**: any predicate that contains an aggregate binder must be in a strictly
      higher stratum than every predicate referenced inside the aggregate's inner `Goals` (matches spec §10.3)
    * **also forbidden in recursive SCCs** (direct or indirect), regardless of stratum assignment
* Safety/range restriction (head vars must be bound by a positive goal)

Add spec-required reserved-schema checks:
* `graph_edge/5`:
  * if declared, it MUST have exactly the spec schema
  * otherwise compile error
* standard output relations and engine-owned outputs:
  * user may not redefine `out_status/1` or emit it in rule heads/facts
  * user may not redefine built-in output relation schemas with mismatching types
* built-in extern key funcs (`handle`, `span_key`, `*_id`, `world_stamp`) are pre-registered
  and redeclarations must match exactly.

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
* Required externs: `handle/2`, `span_key/6`, `typeref_id/2`, `node_id/2`, `call_id/2`, `ref_id/2`, `impl_id/2`, `world_stamp/1`, type structure (§16.1), node context (§16.2), and whatever base schema your views use.

NOTE: this plan is now aligned to the spec's stable ordering contract:
use `span_key/6` (not `span_loc/2`) and implement all required stable key functions.

---

## Architecture: four layers, one clean seam

Think of it like a kiln: the compiler shapes the clay, the engine fires it, the host provides the raw ore, the renderer plates the result.

### 0) Shared crate: `raql_ir` (new)

Add a small shared crate used by syntax/compiler/engine/hosts to make invariants hard to violate:

* ID newtypes: `PredId`, `RuleId`, `VarId`, `TypeId`, `EnumId`, `VariantId`, `StratumId`, `SccId`, plus opaque host ids
* Interned strings/symbols for predicate names, section/group/kind strings, string literals
* Typed term/value representations (monomorphized after type checking)
* A compact `VarSet` bitset type used by mode planning and runtime assertions

**Explicit program phases (make illegal states unrepresentable):**

Split compilation into distinct, typed phases:

* `AstProgram`:
  * raw identifiers, raw terms, source spans
* `ResolvedProgram`:
  * predicate/type/enum names resolved to ids
* `TypedProgram`:
  * every term annotated with a concrete monomorphic type
  * variables have fixed types per rule
* `PlannedProgram`:
  * disjunction desugared
  * goal ordering selected (mode planning)
  * selected modes recorded per call site
  * strata + SCC partitions computed

Only `PlannedProgram` can be executed by the engine. This makes entire classes of bugs impossible.

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
* Deterministic scheduling (required for reproducible partial runs):
  * Sort rules by (include stack path, file, line, col) once at compile time.
  * Evaluate strata in increasing stratum id.
  * Within a stratum, evaluate SCCs in deterministic order.
  * Within an SCC iteration, apply rules in that stable rule order.
  * Avoid `HashMap` iteration order for any semantic effect:
    * use stable iteration sources (sorted vectors, BTreeMap, or explicit "stable tuple order" indices)
* Built-ins:

  * string helpers
  * coalesce
  * aggregates
  * choose_topk binder
  * witness_path/path_hop binder + path storage
* Recursion guard and partial-run behavior
* Output collection

**Runtime error plumbing (move earlier, make central)**

Introduce a central run state:

* `RunStatus = Ok | Partial`
* `notes: Vec<(Section, Message)>`

Any runtime error (checked arithmetic overflow, division by zero, host extern failure, `.func` cardinality violation)
must:

1. emit `out_status("partial")` (once)
2. emit `out_note("Errors", "...")`
3. halt evaluation immediately and return partial results

**Key deliverable**

* `Engine::run(program_ir, host, inputs) -> Outputs`

### 4) Host crate: `raql_host_ra` (rust-analyzer adapter)

**Responsibilities**

* Provide extern relations/functions with mode-respecting access
* Maintain stable handles and span keys (as `span_key/6` components)
* Cache expensive lookups (handle/span_key/type normalization)
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

### Milestone 0.5: Diagnostics scaffolding (do this early)

**Build**

* A `RaqlDiag` type that implements `miette::Diagnostic`
* A shared source manager:
  * maps file ids to `miette::NamedSource`
  * tracks `.include` stacks for any parsed file
* Parser and compiler errors use `RaqlDiag`:
  * parse errors: expected token sets, where it went wrong
  * resolution errors: unknown predicate/type, unknown enum variant
  * type errors: show expected vs found types, highlight the term
  * mode errors: show the first unschedulable goal + currently bound vars + suggested reorder
  * stratification errors: show the dependency cycle and edge kinds (positive/negative/selection/aggregate)

**Done when**

* A parse error points at the correct span with a useful message and help text
* Include-stack context is printed (like Rust's "in file included from ...")
* Snapshot tests exist for diagnostics output (colors disabled)

**Add diagnostic templates for inference**

Define at least these error codes/messages early:

* `RAQL0201` Ambiguous `none`:
  * "cannot infer type parameter T for option<T>"
  * help: "use `none::<string>` (for `option<string>`) or add type context via a typed predicate position or `.decl`"
* `RAQL0202` Ambiguous empty list `[]`:
  * help: "use `[]::<string>` (for list<string>) or add context"
* `RAQL0203` Inferred schema mismatch:
  * "predicate p/3 used with inconsistent types across occurrences"
  * show two example call sites with spans
* `RAQL0204` Mode/schema disagreement:
  * "predicate p/2 has mode declarations inconsistent with its inferred or declared schema"
  * show the mode site and one conflicting call site

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

**Enum atoms are unambiguous in v0.1**
Per the spec, enum atoms are only written as `EnumType::Variant`.
Bare uppercase identifiers are always variables (or `_` wildcard).
The parser should construct distinct AST nodes for variables vs enum atoms without context-dependent resolution.

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
* Enum variant resolution works and doesn't break `S`/`D` variables.

**Polymorphic constructors: `none` and `[]`**

Implement type inference via constraint solving:

* When encountering `none`:
  * assign it a fresh type variable `option<Tv>`
* When encountering `[]`:
  * assign it a fresh type variable `list<Tv>`
* Collect unification constraints from:
  * predicate schemas
  * constructors (`some(T)`)
  * comparisons and arithmetic binding typing rules
* Solve constraints to a concrete monomorphic type per rule.
* If any type variable remains unconstrained at end:
  * reject with a diagnostic:
    * primary label: the `none` / `[]` literal
    * help: "add type context by placing this in a typed predicate position" (and show the nearest predicate schema)

### Type inference: concrete implementation plan (do in Milestone 2)

Implement constraint-based inference in two layers:

#### A) Schema inference for undeclared derived predicates

1. Parse and collect all predicate occurrences (`pred_name`, `arity`, arg term AST, source span).
2. For any predicate that lacks `.decl` and is not `extern/input/output`:
   * create an unknown schema `p(T1, T2, ... Tn)` where each `Ti` is a fresh type variable.
3. Add constraints for every occurrence:
   * unify the type of each argument term with the corresponding schema type variable
4. ALSO incorporate `.mode` declarations as constraints:
   * `.mode p(+T1, -T2, ...)` implies the schema positions are exactly `(T1, T2, ...)`
   * arity must match
   * if `.mode` exists but schema inference would otherwise pick a different type, error (`RAQL0204`)
5. Solve constraints to produce a concrete schema.
5. If any schema type variable remains unresolved:
   * error `RAQL0203` with help:
     * "add `.decl p(... )`"
     * or annotate literals (`none::<T>`, `[]::<T>`)

#### B) Rule-local inference (variables and literals)

Within each rule:

* assign each variable a fresh type variable on first sight
* assign each `none` a fresh `option<Tv>`
* assign each `[]` a fresh `list<Tv>`
* generate constraints from:
  * predicate schemas (declared or inferred)
  * constructors (`some`, lists)
  * constraints (`=`, `!=`, comparisons, `:=`)
  * aggregates and built-ins

Solve constraints and then:

* all variable types must be concrete
* all remaining unresolved type vars -> compile error with turbofish suggestion

#### Solver suggestion (practical)

Use union-find unification for type variables plus structural types:

* primitive types
* enums
* opaque host types
* option<T>
* list<T>

Implement "occurs check" in the type solver to prevent infinite types (recommended by spec).

**choose_topk static checks (spec-required)**

During typing/validation:
* enforce that the first argument `Tag` is a string literal constant
* reject `choose_topk(TagVar, ...)` even if `TagVar` is later unified to a string

Rationale: Tag is part of the semantic identity of the selection site.

### Typed literal turbofish parsing + typing

Parser:

* accept `none::<type>` and attach optional `TypeAst`
* accept list literal optional `::<type>`

Type checking:

* `none::<T>` yields `option<T>`
* `[]::<T>` yields `list<T>`
* `[a,b]::<T>` yields `list<T>` and constrains `type(a)=type(b)=T`

### "Explain inferred types" mode (agent-friendly)

Add an optional CLI flag:

* `--explain-types`

It prints:

* all inferred predicate schemas (for predicates without `.decl`)
* for each rule:
  * inferred type of each named variable
  * (optional) inferred type of each ambiguous literal once resolved

This massively helps agents self-correct without trial-and-error.

---

### Milestone 3: Mode system + rule plan construction

**Add**

* Parse `.mode pred(+Type, -Type, ?Type, …).`
* Store allowed modes per predicate.
* Implement `?Type` expansion exactly per spec:

  * A mode declaration containing one or more `?Type` positions expands to the cartesian product of replacing each `?` with `+` and `-`.
  * After expansion, duplicate modes are permitted but redundant.

**Implement mode checking as in your spec**

* For each rule:

  * Consider **positive goals and binders** to build an ordering:
    * positive predicate atoms
    * `.func` calls
    * aggregates
    * `choose_topk`
    * unification constraints (`T1 = T2`) which may bind vars
    * arithmetic binding constraints (`X := Expr`) which bind `X`
  * Find an ordering where each goal's `+` args are bound when executed, per spec's "exists some ordering".

**Practical algorithm**

Do NOT use a purely greedy picker. A greedy algorithm can reject a mode-valid rule.

Implement mode planning as a bounded search:

* Represent body goals in an arena and track remaining goals via a bitmask.
* Track currently bound variables via a bitset (`FixedBitSet` or equivalent).
* At each step:
  * compute runnable goals under at least one allowed mode (for predicates with multiple modes)
  * branch if multiple runnable goals exist
  * use heuristics to keep the search small (for example pick the goal that binds the most new vars first)
* Memoize states `(remaining_goals_mask, bound_vars_mask)` to avoid exponential blowups.
* Cap depth at number of goals.

**Multi-mode call sites:**

* If a predicate has multiple expanded modes, the planner must select a mode per call site.
* A call is valid if there exists at least one mode such that all `+` inputs are bound at the time of evaluation.
* Record the selected mode in the planned IR for execution.

**Constraints as goals:**

* Unification `T1 = T2`:
  * may bind previously unbound vars (respecting occurs check at runtime)
  * in planning, treat it as runnable when it is type-correct; it can:
    * bind a variable to a ground term
    * bind a variable to a structured term containing other variables
    * merge variable equalities
  * HOWEVER, do not treat this as making a `+` input "ground" unless the resulting term is ground
* Arithmetic binding `X := Expr`:
  * requires all vars used in `Expr` bound as `int`
  * binds `X`
* `!=`, `<`, `<=`, `>`, `>=`:
  * require both sides ground before evaluation (planner must schedule them only after their vars are ground)
  * bind nothing

**Bound vs Ground (spec-critical)**

Track two bitsets during planning:
* `bound_vars`: variables that have some binding (may be non-ground, e.g. `X = some(Y)`)
* `ground_vars`: variables whose current binding is ground

Rules of thumb:
* Positive relation lookups bind variables to ground values (facts are ground).
* `X := Expr` produces ground `int`.
* `X = "lit"` produces ground.
* `X = some(Y)` does NOT make `X` ground until `Y` is ground.
* `!=` and `<` require operands ground at the point they run (enforce via `ground_vars`).

**Done when**

* Mode-invalid rules are rejected.
* Compiler produces a concrete **execution order** for each rule body (even though semantics are order-free).
* Mode errors are explainable:
  * which vars were needed
  * which goal(s) were runnable
  * where the planner got stuck

**Quality requirement: mode error diagnostics**

For mode failures, the diagnostic should include:

* the selected partial ordering that worked so far
* the first goal that could not be scheduled
* which vars it required and which vars were currently bound
* at least one actionable suggestion:
  * "bind Span S earlier (e.g. call node_span(...) before span_text(...))"

### Important interaction: mode planning needs inferred schemas

Mode checking relies on knowing argument types to select allowed modes (especially for externs).

Therefore:

* schema inference must happen before mode planning
* inferred schemas must be included in the predicate registry as if they were explicit `.decl`

---

### Milestone 4: Stratification + SCC analysis (negation + non-monotonic bits)

**Add dependency graph edges**
For head predicate `P` and a referenced predicate `Q`:

* **Positive edge** `P -> Q` for normal atoms in body
* **Negative edge** `P -/-> Q` for `not Q(...)`
* **Selection edge** `P ~/> Q` for:

  * any predicate used inside `choose_topk(... : Goals)`
  * additionally, any rule using `witness_path` or `path_hop` has an implied dependency on `graph_edge/5`
    (the graph relation that `witness_path` ranges over)

* **Aggregate edge** `P -agg-> Q` for any predicate used inside aggregate subgoals
  * Aggregate edges are **stratum constraints** with weight = 1 (same as negation/selection):
    * enforce `stratum(P) > stratum(Q)` for every aggregate edge
  * Additionally enforce spec's "no aggregates inside recursive SCC" as a hard ban:
    * if any SCC contains an aggregate binder anywhere in any rule body, reject program (even if the stratum constraints happen to be satisfiable)

**Disjunction safety:**

* Desugar `(A ; B ; ...)` into multiple rules per spec.
* Before or during desugaring, enforce the spec's disjunction variable rule:
  * any variable referenced outside the disjunction must be range-restricted in every branch
* To prevent accidental capture/collision during desugaring:
  * rename branch-local variables to unique names per branch in the desugared rules (alpha-renaming)

**Compute SCCs** (Tarjan/Kosaraju)

**Validate**

* Stratification constraints:

  * positive: stratum(P) >= stratum(Q)
  * negative/selection: stratum(P) > stratum(Q)
  * aggregate: stratum(P) > stratum(Q)

* Use a real and deterministic stratum computation:
  * Build the condensation DAG (SCC graph).
  * Assign weighted constraints:
    * positive edge weight = 0 (stratum(head) >= stratum(dep))
    * negative/selection edge weight = 1 (stratum(head) >= stratum(dep) + 1)
  * Compute minimal satisfying strata via longest-path DP in topological order.
  * If the constraint system is inconsistent, reject and print the cycle and which edge(s) required strictness.

* Additionally:

  * `choose_topk` forbidden if rule head is in an SCC that includes any predicate referenced inside its `Goals`
  * `witness_path`/`path_hop` forbidden in recursive SCCs
  * aggregates forbidden in recursive SCCs (direct or indirect), and must be strictly above their referenced predicates
  * `witness_path`/`path_hop` must be strictly above `graph_edge/5` and anything `graph_edge/5` depends on (enforced via the implied dependency above)

**Make witness_path's implied dependency explicit**

Even if `witness_path` is implemented as an engine built-in, compilation must enforce the spec rule that it
operates over a completed `graph_edge/5`.

In dependency analysis:

* If a rule body contains `witness_path(...)` or `path_hop(...)`, add a selection edge from the head predicate to:
  * `graph_edge/5` (required)
  * (Optional, diagnostic-only) record the transitive "why" chain `head -> … -> graph_edge` for error reporting,
    but do **not** add extra constraint edges beyond `head ~/> graph_edge`.

This guarantees `stratum(head) > stratum(graph_edge)` and prevents evaluating paths while edges are still growing.

Rationale: `stratum(head) > stratum(graph_edge)` already implies `stratum(head) > stratum(dep)` for anything
`graph_edge` depends on, because positive deps never decrease strata. Extra constraint edges tend to over-constrain
otherwise valid programs and create confusing "why is this forced into a higher stratum?" diagnostics.

**Also enforce**: `witness_path` and `path_hop` require `graph_edge/5` to exist with the reserved schema.
If a program calls `witness_path` but never declares/defines `graph_edge/5`, reject at compile-time with a clear diagnostic.

**Done when**

* The compiler either:

  * produces strata (list of predicates per stratum), or
  * rejects with a clear "cannot stratify due to negative/selection cycle" error.

---

### Milestone 5: Core evaluation engine (facts + monotone rules + unification + constraints)

**Implement**

* Relation storage:

  * `HashSet<Tuple>` for dedup
  * optional indexes: `HashMap<Key, Vec<TupleId>>` or `BTreeMap` for determinism in certain operations
* Rule execution plan:

  * join in the mode-chosen goal order
  * unify terms, extend bindings
  * emit head tuples

**Move unification + constraints into the core engine milestone**

You cannot implement correct joins without unification and constraint evaluation. Treat these as part of the
minimum viable engine, not a later add-on.

Implement in this milestone:

* Unification with occurs check (spec-required)
* Opaque host types are atomic and unify only by equality
* Constraint evaluation:
  * `=` is unification
  * `:=` is checked integer expression evaluation
  * `!=` and order comparisons require ground operands

**Important runtime representation detail (correctness)**

Do NOT model the environment as "VarId -> ground Value only".
The spec's unification allows binding a variable to a structured term containing other variables
as long as occurs-check passes. You need a real term unifier:

* Represent bindings as a substitution/term graph:
  * nodes: `Var(VarId)`, `Lit(...)`, `Enum(...)`, `Opaque(...)`, `Some(Term)`, `None`, `List(Vec<Term>)`
  * plus a union-find over vars (optional but helps)
* Occurs check is performed over this graph when binding `V := Term`.
* Groundness is computed as "term graph contains no unbound vars".

* Checked arithmetic everywhere:
  * `checked_add/sub/mul`
  * `checked_div` with explicit division-by-zero detection
  * on failure: runtime error -> partial halt

**Fixpoint**

* Evaluate per stratum:

  * within stratum, evaluate SCCs to fixpoint

**Recursion guard hook (do it now, not later)**

Even if you only add the full `out_status/out_note` plumbing in Milestone 10,
wire the iteration counter and "stop evaluating higher strata" behavior as soon as you implement recursive SCCs.
Otherwise you risk "debug sessions that never return" during early development.

**Semi-naive baseline**

Implement semi-naive from the start for recursive SCCs. A naive evaluator will make even toy recursive workloads
painful and will obscure correctness issues behind timeouts.

Use naive evaluation only for non-recursive, acyclic SCCs if you want a simpler first implementation.

**Make semi-naive precise (so it stays correct)**

For each recursive SCC and each iteration:
* Maintain `delta[pred]` = tuples newly derived for `pred` since last iteration.
* For each rule in the SCC, generate one or more "delta variants":
  * For each occurrence of a predicate from the same SCC in the rule body, create a variant where
    exactly one such occurrence reads from `delta` and the others read from `total`.
  * This is standard semi-naive and prevents re-deriving known tuples.
* Apply rule variants in a deterministic order:
  * order by `(original_rule_order, variant_index)`

**Determinism requirement**

Never iterate raw `HashMap/HashSet` directly in any semantics-bearing loop.
If you store relations in a hash-based set, wrap enumeration in a deterministic view:
* `IndexSet` insertion order (with deterministic insertion order)
* or stable sorted iteration keyed by per-predicate tuple ordering.

**Rust type system: make invariants hard to violate**

Prefer typed-index collections for IR and runtime tables:
* `IndexVec<PredId, PredInfo>`
* `IndexVec<RuleId, RulePlan>`
* `IndexVec<StratumId, StratumPlan>`
so "wrong index into wrong vec" is a compile error.

**Determinism under partial runs: relation iteration order**

To keep partial outputs reproducible:

* Relations should have a deterministic enumeration order.
* Prefer storage that preserves insertion order:
  * `indexmap::IndexSet<Tuple, BuildHasherDefault<FxHasher>>`
* Ensure insertion attempt order is deterministic:
  * deterministic rule order
  * deterministic join enumeration
* Join operators must enumerate inputs in a deterministic order
  (avoid iterating `HashSet`/`HashMap` directly unless wrapped in a stable view).

**Done when**

* Simple recursive transitive closure works:

  ```raql
  .decl edge(A:int,B:int) input.
  .decl reach(A:int,B:int).
  reach(A,B) :- edge(A,B).
  reach(A,C) :- reach(A,B), edge(B,C).
  ```

* Your optionality examples work without two-rule boilerplate:

  ```raql
  def_doc_summary(D, some(Doc)), contains(Doc, "unsafe").
  def_doc_summary(D, none).
  ```

---

### Milestone 6: Negation (stratified)

**Implement**

* During evaluation of a stratum:

  * negated goals may only reference lower strata (guaranteed by compiler)
  * so `not p(...)` is a membership check against a completed relation
* Enforce range restriction:

Implement the spec's safety model (after Patch 1):
* variables can be range-restricted by:
  * appearing in positive goals
  * binding constraints (`=` and `:=`) when they bind from already range-restricted inputs or ground literals
  * binder outputs (aggregate binders, choose_topk outputs, witness_path/path_hop outputs)
* for disjunction, enforce "outside vars must be range-restricted in every branch", including via `X = "..."`

**Done when**

* You can express “all defs without docs” safely and deterministically.

---

### Milestone 7: Aggregates

**Implement binder evaluation**
For:

```raql
N = count(V : Goals).
```

* During rule execution, when reaching the aggregate goal:

  * evaluate `Goals` under current outer binding
  * compute the *set* of satisfying bindings (set semantics, no duplicates)
  * apply the spec's **projection-and-dedup rule**:
    * for `agg(V : Goals)`, collect `Vals = { V | row ∈ Rows }` as a **set** (dedup by term equality), then aggregate over `Vals`
    * for `count(Goals)`, treat `Rows` itself as a set and return `|Rows|`
  * then bind `N`

**Implementation details**

* Give aggregates their own internal mini-evaluator:

  * it runs over already-materialized relations (lower stratum by construction)
  * local vars scoped

**Done when**

* `count(V : Goals)` counts distinct `V` values (not rows).
* `sum(V : Goals)` sums distinct `V` values (not per-row repeats).
* `min/max` fail the aggregate goal on empty input (no binding), matching spec §10.4.

---

### Milestone 8: `choose_topk` binder (deterministic)

**Implement**

* Evaluate candidate set for each outer binding + `Group`:

  * run `Goals` to generate `(Score, Item)` pairs
  * dedup pairs
  * sort by:

    1. Score descending
    2. `stable_order(Item)` ascending
    3. `stable_order(Score)` as a final (redundant) total-order tie-break, matching the spec
  * emit top K as bindings

**Spec-compatibility checks to enforce during typing/validation**

* `Score` must be `int`.
* `Group` and `Item` must be **orderable** types (per spec's stable-order domain).
* `Group` must be **ground at binder evaluation time** (treat it as a required input for mode planning),
  because selection is "per group" and the semantics are defined "for each binding including `Group`."
  (If you later want "enumerate all groups" semantics, make it a separate binder in v1.)
* `K` must be ground at binder evaluation time; if `K <= 0`, treat as producing no results (or reject at runtime),
  but pick one behavior and test it.

**You must implement `stable_order`**

* primitives: straightforward
* for `Def`: call host `handle(Def, string)` and compare strings
* for `Span`: call host `span_key(Span, RelPath, L0, C0, L1, C1)` and compare the tuple
* for `Path`: internal deterministic id

Expand to match the spec's "orderable types":
* enums: by variant ordinal in `.type` declaration order
* `option<T>`:
  * `none < some(_)`
  * compare inner by `stable_order(T)`
* `list<T>`: lexicographic by elements' `stable_order(T)`; shorter list first if prefix
* all opaque host types listed in the spec (`TypeRef`, `Node`, `Call`, `Ref`, `Impl`) via their required `*_id` funcs

Canonical TypeTag:
* do NOT use user-surface strings
* generate TypeTag from your internal `TypeId` graph in a canonical format like:
  * `int`, `string`, `bool`
  * `option<string>`, `list<option<Def>>`
  * `RenderMode` (enum name)
  * `Def`, `Span`, etc.

**Caching**

* Cache `handle` and `span_key` results aggressively; top-k and witness paths will otherwise thrash.

**Done when**

* A view can select top 5 callers, deterministically, on repeated runs.

---

### Milestone 9: `graph_edge` + `witness_path` + `path_hop`

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

**Cycle control (strongly recommended)**

Even with a small `path_max_depth`, cyclic graphs can explode combinatorially. To keep witness search predictable:

* Treat candidate paths as **simple** (no repeated `Def` nodes) during enumeration.
  * This does not change shortest-path results (a shortest path never needs a cycle),
    but dramatically reduces search blowups.
* If you want to allow repeated nodes later (v1+), make it an explicit `.func` input knob.

**Represent Path**

* `Path` is an engine-opaque value:

  * `struct PathId(u64)`
  * store `Vec<Hop>` in an arena/map keyed by `PathId`
* `path_hop(P, Seq, ...)` reads hops from the map.

**Indexing requirement (make it explicit)**

Do not run BFS by scanning the raw `graph_edge` tuple set.

Before path enumeration:

* materialize an adjacency index:
  * `Map<(Graph, From), Vec<Edge>>`
  * store edges in a stable, pre-sorted vector

**Done when**

* Your trace example works exactly:

  * Seq is 0-based, increasing, no gaps
  * repeated runs return identical paths and ordering
* cyclic graphs do not cause pathological blowups under default `path_max_depth` (8)

---

### Milestone 10: Recursion guard + partial honesty outputs (tighten semantics)

**Implement**

* `.pragma max_iters = N` (default 128)
* input override `opt_max_iters(N)`
* For each recursive SCC during fixpoint:

  * count iterations
  * if exceed:

    * mark run partial
    * emit:

      * `out_status("partial")`
      * `out_note("Notes", "fixpoint iteration limit exceeded in SCC: <name>")`
    * halt evaluation immediately

**Critical semantic requirement**

When the recursion guard triggers, do NOT evaluate any remaining SCCs or any higher strata.
Higher strata would otherwise compute results using incomplete lower-stratum relations (closed-world assumption),
which is incorrect. The spec already requires "no higher strata are evaluated" on partial runs.

* If no SCC exceeds limit:

  * emit `out_status("ok")`

**Runtime errors (spec-required)**

* Implement checked `int` arithmetic:
  * overflow and division by zero are runtime errors
* Treat "extern predicate failure" (adapter error, unexpected cardinality for `.func`, etc.) as a runtime error
* On any runtime error:
  * halt immediately
  * mark run partial
  * emit `out_status("partial")` and `out_note("Errors", "runtime error: ...")`

**Done when**

* A deliberately divergent recursive program halts cleanly and reports partial results.

---

### Milestone 11: Standard library `std.raql` (v0.1)

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

### Milestone 12: rust-analyzer adapter (the "load-bearing externs")

Implement the required extern predicates/functions from §16.

**Snapshot model (required)**

* The adapter must construct exactly one rust-analyzer `Analysis` snapshot per RAQL run, obtained from an `AnalysisHost`.
* All extern predicate answers must come from that snapshot (immutability).
* `world_stamp/1` must describe the snapshot configuration (target/features/cfg) sufficiently for reproducibility.

Rationale:
* `AnalysisHost` owns the database and provides `Analysis` snapshots; the system is Salsa-based incremental computation.
  Use the snapshot boundary to guarantee internal consistency across all extern answers.

**Span representation**

Define Span as a stable handle to:

* a file identity that can represent real files AND (optionally) macro-expanded files
* byte/text range

Implement `span_key/6` by computing:

* stable relative path (or a stable virtual path for expanded files)
* line/col range via a line index

If macro-expanded spans are not supported in v0.1, document that `Span` refers only to original source files
and macro internals map back to call-site spans.

If macro-expanded spans ARE supported:
* don't use plain `FileId` as the identity; use a representation that can model macro-expanded "files"
  (rust-analyzer's APIs commonly distinguish real file ids from macro expansion file ids).
* ensure `span_key/6` produces a stable virtual `RelPath` for expanded files so ordering stays deterministic.

**Node representation**

Do not store raw `SyntaxNode` in opaque values. Store:

* a macro-aware file identity + `SyntaxNodePtr` (or equivalent stable pointer)

This keeps nodes stable across cheap clones and avoids lifetime issues.

**Semantics layer**

Prefer implementing externs via:

* `hir::Semantics` for syntax<->hir mapping and type queries
* position-specific analysis tools (`SourceAnalyzer` or equivalent)

Note:
* `hir::Semantics` is the intended facade boundary: it maps syntax nodes to semantic definitions,
  supports type queries, and can descend into macro expansions.

**TypeRef representation**

* Intern TypeRefs in the adapter:
  * `TypeRefId` is a stable handle keyed by the underlying RA type (`Ty`) or a normalized representation.
* `ty_normalize(TR, Norm)` must never fail:
  * return `TR` unchanged if normalization is unavailable.

**Host value representation**

* Define opaque wrapper types:

  * `DefId`, `SpanId`, `TypeRefId`, `NodeId`, etc.
* Maintain stable ordering keys:

  * `handle(Def) -> String`
  * `span_key(Span) -> (RelPath, L0, C0, L1, C1)` formatted consistently
  * `typeref_id(TypeRef) -> String`
  * `node_id(Node) -> String`
  * `call_id(Call) -> String`
  * `ref_id(Ref) -> String`
  * `impl_id(Impl) -> String`
  * `world_stamp() -> String`

**Type structure**

* Map rust-analyzer HIR types into a stable `TypeRef` handle layer
* Provide:

  * `ty_ctor` (recommended)
  * `ty_app`, `ty_arg`: expose TYPE args only
  * wrappers: `ty_ref`, `ty_ptr`, `ty_tuple`, `ty_slice`
  * `ty_param`, `ty_prim`, `ty_unknown`
  * `ty_normalize` (strongly recommended, even if initially identity)

Implementation detail to bake in early:
* rust-analyzer's generics model includes type, lifetime, and const args.
* v0.1's `ty_arg/3` must filter ONLY the type arguments (ignore lifetimes and const generics).

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

### Milestone 13: Renderer + "query mode"

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
* Mode-valid program compiles even if goals are written "out of order".
* Mode planning accounts for binding constraints (`T1 = T2`, `X := Expr`) and for groundness requirements (`!=`, `<`, `<=`, `>`, `>=`).

### 4) Stratification tests

* Classic invalid:

  ```raql
  p(X) :- not q(X).
  ```

  rejected for range restriction.
* Non-stratifiable negation cycles rejected with clear message.
* choose_topk inside recursion rejected.
* witness_path/path_hop in a recursive SCC rejected.
* witness_path/path_hop not strictly above graph_edge/5 rejected.

### 5) Engine semantics tests

* Set semantics: duplicates eliminated.
* Fixpoint: transitive closure matches expected.
* Negation matches stratified semantics.
* Occurs check: `X = some(X)` **must** fail the unification goal at runtime (no tuple produced),
  matching the spec's "goal fails" behavior. (A compile-time *warning* is OK; a compile-time *rejection* is not spec-faithful.)

### 6) choose_topk determinism test

* Two items with equal score: stable_order(Item) tie-break applied consistently.

### 7) witness_path determinism test

* Multiple shortest paths: lexicographic hop-key ranking stable.
* `Seq` from `path_hop` is 0..n-1 exactly.

### 8) Recursion guard test

* A runaway SCC hits max_iters:

  * outputs include `out_status("partial")`
  * note emitted
  * partial derived facts preserved

### 9) Aggregate corner cases

* `min`/`max` with empty inner Goals fails the aggregate goal (no binding).
* `sum` over empty inner Goals yields 0.

### 10) Missing spec corner tests (add these)

* Occurs check:

  ```raql
  .decl p(X: option<int>) output.
  p(X) :- X = some(X).
  ```

  Must fail the unification goal (no tuple produced).

* `none` ambiguity rejection:

  ```raql
  .decl r(X: option<int>) output.
  r(none).  % ok, context fixes T=int
  ```

  but

  ```raql
  .decl bad(X: option<int>) output.
  .decl bad2(X: option<string>) output.
  w(none).  % reject: no context for T
  ```

* Empty list ambiguity rejection (same idea for `[]`).

* Groundness for `!=` and order comparisons:
  * if `X != Y` can run before X/Y are bound, mode planner must reject or reorder.

* Checked arithmetic overflow and division by zero:
  * triggers runtime error -> partial halt -> out_note("Errors", ...)

* min/max empty input fail the goal:
  * `min(...)` over empty inner set produces no binding for the aggregate goal

* witness_path stratum rule:
  * reject if `witness_path` is in same stratum as `graph_edge`

* Determinism under partial runs:
  * same program + same inputs + same snapshot -> identical partial outputs

---

## One "sharp corner" to decide early (so it doesn't bite later)

### What is "extern" in practice

Treat `extern` as "provided by the runtime host," where:

* some externs are **engine built-ins** (strings, coalesce, witness_path)
* some externs are **adapter-provided** (rust-analyzer facts)
  This keeps the language spec intact and makes the system modular.

**Note:** The previous version of this plan discussed enum atom ambiguity - this is now resolved in the spec.
Enum atoms are always written as `EnumType::Variant`, so there is no ambiguity with variables.

---

## Optional but high-leverage extras (still v0.1-friendly)

These aren't required by the spec, but they make v0.1 feel sturdy:

1. **Index planning from modes (make this core, not optional)**
   Mode checking already tells you which columns are bound at each call site.
   Use that to:
   * build per-relation indexes keyed by the most common bound prefixes
   * choose join strategies deterministically

2. **Separate extern `.decl` vs extern `.func` at the Host boundary**
   Enforce `.func` semantics ("exactly one tuple") structurally:
   * `.decl extern` returns 0..N tuples (iterator/stream)
   * `.func extern` returns exactly 1 tuple or signals runtime error

3. **MockHost first (unit tests before rust-analyzer)**
   Add a `MockHost` implementation that:
   * provides the required stable key functions (`handle`, `span_key`, `*_id`, `world_stamp`)
   * provides a few small extern relations for tests
   This lets you validate language semantics without debugging rust-analyzer integration at the same time.

4. **Key caching for stable_order**
   Memoize:
   * `handle(Def)`
   * `span_key(Span, ...)`
   * `typeref_id`, `node_id`, `call_id`, `ref_id`, `impl_id`
   This is the difference between "snappy" and "why is topk slow?"

5. **Explain mode errors**
   When mode-check fails, print:
   * which predicate call needed which vars bound
   * one suggested ordering or the point where it got stuck

6. **Output key collision warnings (debuggability)**
   If multiple fragments share `(Section, Group, Rank, Seq, Kind)`, emit:
   * `out_note("Notes", "duplicate fragment key: ...")`
   and keep rendering deterministically (for example by stable_order on Def/Span).

---

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
* the **canonical `std.raql`** plus **three golden example views** as executable acceptance tests, exactly as you suggested.

