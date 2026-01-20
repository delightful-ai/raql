Below is a **canonical, self-contained RAQL v0.1 language spec** (Datalog/Prolog-inspired) that incorporates exactly the v0 priorities you listed:

**Included in v0.1**

1. **Type structure** (`ty_app`, `ty_arg`, …)
2. **Node/expr domain** (minimal, enough for enclosing control structure)
3. **`witness_path` + `path_hop`**
4. **`choose_topk`** (single built-in binder)
5. **`Seq` field** in output predicates

**Deferred**

* multi-world cfg/features/targets (v1)
* full CFG/dataflow (v1 unless RA makes it trivial)

It also addresses the explicit issues you raised:

* **Optionality** (no more “two rules every time” for docs/parents)
* **Mode declarations** (including whether you can enumerate args)
* **Recursion guard** (enforced fixpoint iteration limit)

I’m being deliberately explicit so there are no “interpretation gaps.”

---

# RAQL v0.1 Language Specification

## Type inference usability upgrade (v0.1)

This spec includes an inference-focused usability upgrade intended to support one-off queries
written by humans and agents:

* Derived predicates may omit `.decl` and have their schemas inferred.
* `none` and `[]` participate in constraint-based inference and do not immediately error.
* When inference is still ambiguous, users can annotate `none` and list literals using Rust-style turbofish:
  `none::<T>`, `[]::<T>`, `[... ]::<T>`.

## Change log (v0.1 spec errata)

This spec is intended to be the contract for the RAQL v0.1 implementation. The following clarifications/fixes
resolve ambiguities discovered during plan review:

* Arithmetic binding is now written with `:=` (not `=`) to eliminate grammar ambiguity between unification and
  arithmetic evaluation.
* Integer division semantics are now explicitly defined (truncation toward zero).

## 0. Model and terminology

RAQL is a **Datalog** language with a **Prolog-like surface**:

* Programs consist of **facts** and **rules**:
  `head :- body.`
* Semantics are **set-based** (no duplicates) and **bottom-up** (least fixpoint).
* Output is produced by populating standard **output relations** (fragments + metrics).

RAQL runs against an **analysis snapshot** provided by a host (rust-analyzer adapter). All extern predicates read from that snapshot. In v0.1, there is exactly **one analysis world** (one feature/target configuration); the host is required to expose a stamp of that configuration (see §13.4).

## 0.1 Compile-time errors and diagnostics (required)

If a program fails to parse or fails any static check (name resolution, typing, safety/range restriction,
mode validity, stratification), the program is rejected and evaluation does not start.

Implementations must produce diagnostics that include:

* a primary source location (file/line/col span)
* a clear error message
* optional secondary labels and actionable help text (recommended)
* include-stack context for `.include` chains when relevant

---

# 1. Lexical syntax

## 1.1 Whitespace

Spaces, tabs, and newlines separate tokens and otherwise have no meaning.

## 1.2 Comments

* Line comments:

  * `% ...` (preferred)
  * `// ...` (allowed)
* Block comments:

  * `/* ... */` (nesting recommended; if not supported, behavior must be documented)

## 1.3 Identifiers

* Predicate names, type names: `[A-Za-z_][A-Za-z0-9_]*`
* Convention:

  * Predicates: `snake_case`
  * Types/enums: `PascalCase`

## 1.4 Variables

Variables begin with an uppercase letter or `_`:

* `Def`, `CallerFn`, `S`, `_Tmp`
* `_` alone is the **anonymous variable** (wildcard), never binds.

Note:

* Because uppercase identifiers are reserved for variables, enum values are never written as bare identifiers.
* Enum values are always written in qualified form: `EnumType::Variant` (see §3.3).

## 1.5 Literals

* `int`: `0`, `42`, `-7`
* `string`: `"..."` with escapes (`\"`, `\\`, `\n`, `\t`)
* `bool`: `true`, `false`

---

# 2. Terms (values)

A **term** is one of:

* Variable: `X`
* Wildcard: `_`
* Literal: `123`, `"hi"`, `true`
* Enum variant atom: `RenderMode::DOC_SIG`, `RefKind::COMPARE`
* Option value: `none`, `none::<Type>`, `some(Term)`
* List literal: `[Term, Term, ...]`, with optional `::<Type>` annotation

## 2.1 Option values (built-in)

RAQL has a built-in generic option type `option<T>` with two constructors:

* `none`
* `some(Value)`

Unification works structurally, so this is legal and idiomatic:

```prolog
def_doc_summary(D, some(Doc)), contains(Doc, "safety").
def_doc_summary(D, none).   % matches defs with no doc summary
```

## 2.2 Unification (precise)

RAQL uses first-order structural unification when matching terms (including option/list constructors) and in `=` relational constraints (§7.4).

**Rules:**

* Variables may be unbound or bound.
* The anonymous variable `_` matches any term and never binds.
* Unifying an unbound variable `V` with a term `T` binds `V := T`, subject to the occurs check below.
* Unifying two bound terms succeeds iff they are structurally equal (including enum type + variant).
* Opaque host types (`Def`, `Span`, `TypeRef`, `Node`, `Call`, `Ref`, `Impl`, `Path`) are atomic:
  * They unify only by equality (same value).
  * They never unify with structured terms such as `some(...)`, lists, or literals.

**Occurs check (required):**

* A variable `V` may not be bound to any term that (transitively) contains `V`.
* If a unification attempt would violate this, the unification goal fails.

## 2.3 Groundness (required)

A term is **ground** iff it contains **no unbound variables**.

* Literals (`int`, `string`, `bool`) are ground.
* Enum atoms `E::V` are ground.
* Opaque host values (`Def`, `Span`, `TypeRef`, `Node`, `Call`, `Ref`, `Impl`, `Path`) are ground.
* `some(T)` is ground iff `T` is ground.
* `none` is ground.
* A list literal is ground iff all of its elements are ground.

Groundness is used by:
* constraint semantics (`!=`, `<`, `<=`, `>`, `>=`) which require ground operands (§7.4)
* mode-checking: a `+Type` argument must be ground at evaluation time (§6)

---

# 3. Types

RAQL is **lightly typed**. Types exist to:

* prevent obvious mismatches (`Span` vs `Def`)
* enforce **guarded/demand-driven** predicates via modes (§6)
* provide reliable tooling (linting/autocomplete)

## 3.1 Primitive types

* `int`, `string`, `bool`

**`int` semantics (v0.1):**

* `int` is a signed 64-bit integer (two's complement), range `[-2^63, 2^63-1]`.
* Arithmetic in constraints and aggregates is checked.
* Integer overflow and division by zero are runtime errors (see §13.5).

## 3.2 Opaque host types (v0.1)

Opaque types are values produced only by extern predicates:

* `Def`
* `Span`
* `TypeRef`
* `Node`
* `Call`
* `Ref`
* `Impl`
* `Path` (for witness paths)

Opaque values are equality-comparable and orderable by the language’s stable ordering rules (§12.2), but cannot be constructed directly.

## 3.3 Enums

Enums may be declared via directive:

```prolog
.type RenderMode = { DOC_SIG, ITEM, HEADER, LINE, STMT, BLOCK, EXPR }.
.type RefKind    = { READ, WRITE, COMPARE, PASS, MOVE, FIELD }.
.type Mutability = { IMM, MUT }.
.type NodeKind   = { IF, MATCH, WHILE, FOR, LOOP, BLOCK, TRY, ARM, OTHER }.
```

Enum values are written as qualified identifiers, e.g. `RenderMode::BLOCK`.
This avoids ambiguity with variables, which are also uppercase-initial (§1.4).

## 3.4 Type checking and inference (v0.1)

RAQL programs are type-checked at compile-time against predicate schemas declared via `.decl` and `.func`.
Programs with type errors are rejected.

Additionally, for usability, schemas for derived predicates may be inferred when `.decl` is omitted (§5.3).
Inferred schemas are still statically typed and monomorphic.

**Variable typing (rule-local, monomorphic):**

* Within a single rule, each named variable has exactly one type.
* A variable's type is inferred from the positions it appears in (predicate schemas, constructors, constraints).
* If a variable is used in incompatible typed positions, it is a compile-time error.

**Wildcard typing:**

* Each occurrence of `_` is treated as a fresh anonymous variable.
* It is type-checked from context, but never binds and never shares identity with other `_` occurrences.

**Literal typing:**

* `int` literals have type `int`.
* `string` literals have type `string`.
* `true`/`false` have type `bool`.
* Enum atoms `E::V` have type `E`.

**Constructor typing:**

* `some(T)` has type `option<T>`.
* `none` has type `option<Tv>` where `Tv` is a fresh type variable resolved by inference (§3.5).
  * If `Tv` remains unresolved when a concrete schema is required, it is a compile-time error.
* List literals are homogeneous:
  * `[ ]` has type `list<Tv>` where `Tv` is a fresh type variable resolved by inference (§3.5).
  * `[T1, T2, ...]` has type `list<T>` where all elements unify to the same type `T`.

**Constraint typing:**

* Relational `=` unifies types structurally.
* Relational `!=` and order comparisons require both sides to have the same type.
* Arithmetic binding `X := Expr` requires `X:int` and `Expr:int`.

**Aggregate typing:**

* `count(...)` and `count_distinct(...)` produce `int`.
* `sum(V : ...)` produces `int` and requires `V:int`.
* `min(V : ...)` and `max(V : ...)` produce the type of `V` and require `V` to be orderable (see below).

**Orderable types (v0.1):**

An orderable type is any type for which `stable_order` is defined (§12.2), including:

* `int`, `string`, `bool`
* enums
* `Def`, `Span`, `Path`, `TypeRef`, `Node`, `Call`, `Ref`, `Impl`
* `option<T>` where `T` is orderable
* `list<T>` where `T` is orderable

**`choose_topk` typing constraint:**

* `Group` and `Item` must be orderable types.

## 3.5 Type inference model (v0.1, required)

RAQL uses constraint-based type inference to support ergonomic queries.

Key properties:

* Inference is monomorphic:
  * each variable within a rule has exactly one type
  * each predicate argument position has exactly one type
* Type variables (meta-variables) may be introduced internally during inference
  (e.g. for `none` and `[]`), but they must be resolved to concrete types before execution.

### 3.5.1 Sources of constraints

The type checker generates equality/compatibility constraints from:

* predicate schemas:
  * declared via `.decl` / `.func`
  * inferred for derived predicates when `.decl` is omitted (§5.3)
* constructors:
  * `some(T)` implies `some(T): option<type(T)>`
  * `none` implies `none: option<Tv>` for fresh `Tv`
  * `[]` implies `[]: list<Tv>` for fresh `Tv`
  * `[a,b,c]` implies `list<T>` and `type(a)=type(b)=type(c)=T`
* constraints:
  * `T1 = T2` implies `type(T1) = type(T2)`
  * `T1 != T2` implies `type(T1) = type(T2)` and both sides ground at evaluation time
  * order comparisons imply both sides share a comparable type
  * arithmetic binding `X := Expr` implies `type(X)=int` and `type(Expr)=int`
* aggregates:
  * `count`/`count_distinct` produce `int`
  * `sum` requires and produces `int`
  * `min`/`max` require orderable types and produce that type

### 3.5.2 Resolution requirement

After constraint solving:

* all predicate argument types (declared or inferred) must be concrete (no remaining type variables)
* all rule-local variable types must be concrete
* any remaining unconstrained type variable is a compile-time error

### 3.5.3 Infinite type protection (recommended)

Implementations SHOULD reject inferred infinite types (e.g. constraints like `Tv = option<Tv>`)
with a compile-time diagnostic.

Rationale: even if a rule would be unsatisfiable at runtime due to occurs check, infinite types degrade tooling and
produce confusing downstream errors.

## 3.6 Typed literal annotations (v0.1)

When inference is ambiguous, users may annotate `none` and list literals using Rust-style turbofish:

* `none::<T>` has type `option<T>`
* `[]::<T>` has type `list<T>`
* `[a,b,c]::<T>` asserts the list has type `list<T>` (elements must unify with `T`)

These annotations are purely type-level and do not affect runtime semantics.

## 3.7 Parametric built-ins (v0.1)

The spec describes some built-in extern predicates/functions using schematic type variables such as `T`
(for example `coalesce(option<T>, T, T)`).

Rules:

* User programs cannot declare their own type variables in v0.1.
* For each call site, the compiler must infer a single concrete monomorphic type for each schematic variable.
* If the type cannot be inferred from context, the program is rejected at compile-time.

---

# 4. Program structure

A RAQL program is a set of:

* directives
* declarations
* rules and facts

Order does not affect meaning (except for name resolution in includes).

## 4.1 Includes

```prolog
.include "std.raql".
.include "views/callers.raql".
```

Include paths are resolved relative to a configured search path.

---

# 5. Declarations

## 5.1 Relation declarations: `.decl`

Declares a predicate schema.

```prolog
.decl def_kind(D: Def, K: string) extern.
.decl target_def(D: Def) input.
.decl out_def_frag(...) output.
```

### Attributes

* `extern`: provided by the runtime environment (built-ins and/or host adapters)
* `input`: facts injected by runtime (CLI options, selected target, etc.)
* `output`: consumed by renderer / printed in query mode

If no attribute is given, the predicate is **derived** (defined by rules).

**Note:**

* The required rust-analyzer-backed extern predicates for v0.1 are specified in §16 and §20.
* Other extern predicates may be implemented by the RAQL engine runtime (e.g. string helpers, witness selection).

## 5.2 Functional predicates: `.func`

`.func` declares a deterministic mapping from bound inputs to exactly one output tuple.

Syntax:

```prolog
.func def_doc_summary(D: Def, Doc: option<string>) extern.
```

Meaning:

* For any **bound** `D` for which the call is well-typed, `def_doc_summary(D, Doc)` produces **exactly one** tuple.
* Absence is represented via `none`, not "no tuple".

**Host contract requirement:**

* If an extern `.func` produces zero or multiple results for a bound input, this is a runtime error (see §13.5).
  This is considered a host contract violation.

This is RAQL's canonical answer to optionality (§8).

> Rule: "attribute-like" properties must be `.func` returning `option<T>` when absence is common (docs, parent, display name, etc.). Multi-valued edges remain `.decl` relations.

## 5.3 Implicit schemas for derived predicates (v0.1 usability, required)

To support one-off queries, `.decl` is OPTIONAL for predicates that are:

* not marked `extern`
* not marked `input`
* not marked `output`

Such predicates are treated as **derived** and their schemas are inferred.

### 5.3.1 What is inferred

For each derived predicate `p/n` lacking a `.decl`, the compiler infers:

* arity `n`
* the type of each argument position

### 5.3.2 Constraint sources for schema inference

Schema inference must incorporate constraints from:

* all rule heads with predicate `p(...)`
* all call sites of `p(...)` in rule bodies

Rules:

* All occurrences must agree on arity, otherwise compile-time error.
* Types are inferred using the same constraint system as §3.5.
* The final inferred schema must be monomorphic and fully concrete (no type variables).

### 5.3.3 Interaction with explicit `.decl`

If a predicate has an explicit `.decl`, that schema is authoritative.
All call sites and rule heads must type-check against it; otherwise compile-time error.

---

# 6. Modes (binding discipline)

Modes prevent expensive or nonsensical enumeration (especially around text/syntax).

## 6.1 Mode declaration: `.mode`

A predicate may have **one or more** allowed modes. Each mode specifies, per argument:

* `+Type`: input (must be **ground** at call time; see §2.3)
* `-Type`: output (will be produced/bound)
* `?Type`: *either* bound or unbound is allowed (syntactic convenience)

Example:

```prolog
.mode ty_arg(+TypeRef, -int, -TypeRef).
.mode ty_arg(+TypeRef, +int, -TypeRef).
```

`?Type` is shorthand for defining both the `+` and `-` variants.

**Expansion rule (precise):**

* A `.mode` declaration containing one or more `?Type` positions expands to the cartesian product of replacing each `?` with `+` and `-`.
* Duplicate modes after expansion are permitted but redundant.

Example:

```prolog
.mode p(?int, +string, ?Def).
```

expands to:

```prolog
.mode p(+int, +string, +Def).
.mode p(+int, +string, -Def).
.mode p(-int, +string, +Def).
.mode p(-int, +string, -Def).
```

## 6.2 Mode checking rule (no ambiguity)

Because RAQL is Datalog (order-independent), mode checking is defined as:

A rule is **mode-valid** iff the compiler can find **some ordering** of the positive (non-negated) goals in its body such that, when each goal is evaluated, all of its `+` arguments are bound by:

* constants, or
* variables bound by earlier positive goals, or
* variables bound by aggregates already evaluated, or
* variables bound by `.func` calls whose required inputs are already bound, or
* variables newly bound by earlier constraints:
  * unification constraints `T1 = T2` may bind variables (§2.2, §7.4)
  * arithmetic binding `X := Expr` may bind `X` (§7.4)

If no such ordering exists, the rule is rejected at compile-time.

## 6.3 Guarded predicates

Any predicate with a mode requiring `+Span` (or similar) is effectively guarded. Example:

```prolog
.decl span_text(S: Span, Text: string) extern.
.mode span_text(+Span, -string).
```

You cannot call `span_text(S, Text)` unless `S` is bound by other goals. This is non-negotiable for safety.

## 6.4 Multi-mode predicates (call-site selection)

If a predicate has multiple allowed modes, a call site is valid if there exists at least one mode under which the call can be evaluated (given the chosen goal ordering) with all `+` arguments bound.

The compiler may select any such mode as part of finding a mode-valid ordering; this selection does not change the logical meaning of the program.

---

# 7. Facts and rules

## 7.1 Facts

A fact is a predicate application ending with `.`

```prolog
opt_include_tests().
max_depth(3).
```

**Groundness requirement (required):**

Facts must be ground. Facts may not contain named variables and may not contain the wildcard `_`.
If a fact contains a variable or `_`, the program is rejected at compile-time.

## 7.2 Rules

A rule has a head and body:

```prolog
Head :- Goal1, Goal2, ..., GoalN.
```

### Goals can be:

1. A predicate atom: `p(X, Y)`
2. A negated atom: `not p(X, Y)`
3. A disjunction group: `(A ; B ; C)` where each branch is a comma-separated goal list (semantics in §7.3)
4. A constraint: `X := Y + 1`, `X != Y`, `X < 3`
5. An aggregate binder: `N = count(V : Goals...)`
6. A selection binder: `choose_topk(...)` (defined in §11.1)

## 7.3 Disjunction groups (`;`) (precise semantics)

A disjunction group in a rule body is syntactic sugar for multiple rules.

**Rewrite rule:**

```
H :- G0, (B1 ; B2 ; ... ; Bn), G1.
```

is equivalent to the set of rules:

```
H :- G0, B1, G1.
H :- G0, B2, G1.
...
H :- G0, Bn, G1.
```

**Variable scope and safety rule for disjunction:**

* Variables introduced only inside a branch are local to that branch after rewriting.
* Any variable referenced outside the disjunction group must be range-restricted in every branch (otherwise the program is rejected).

## 7.4 Constraints and expressions (precise semantics)

Constraints are evaluated as goals. They may bind variables (via unification or arithmetic binding), or they may purely test and filter bindings.

There are two constraint forms:

**A) Relational constraints:** `T1 relop T2`

where `relop ∈ { "=", "!=", "<", "<=", ">", ">=" }`.

* `T1 = T2`:
  * Performs unification as defined in §2.2.
  * May bind previously unbound variables.
* `T1 != T2`:
  * Is a pure disequality test.
  * Requires that both `T1` and `T2` are ground (contain no unbound variables) at evaluation time.
  * Succeeds iff `T1` and `T2` are not equal.
* Order comparisons (`<`, `<=`, `>`, `>=`):
  * Are pure comparisons (never bind variables).
  * Require both sides to be ground at evaluation time.
  * Require both sides to have the same type, and that type must support ordering:
    * `int`: numeric order
    * `string`: lexicographic UTF-8 byte order
    * `bool`: `false < true`
    * enums: by variant ordinal in declaration order (§12.2)
  * If the types are not comparable, the program is rejected at compile-time (type error).

**B) Arithmetic binding:** `X := Expr`

* This is the `var := expr` constraint form in the grammar (§19).
* `Expr` must be an int expression using `+`, `-`, `*`, `/`, unary `-`, and parentheses.
* All variables referenced by `Expr` must be bound to int at evaluation time.
* Evaluate `Expr` to an int value `V`, then:
  * If `X` is unbound, bind `X := V`.
  * If `X` is bound, require `X == V`, otherwise fail the goal.
* This form never solves for variables inside `Expr` (no algebraic rearrangement).
* Division `/` is truncating integer division toward zero.
* Division by zero and integer overflow are runtime errors (see §13.5).

---

# 8. Optionality (fully specified)

This is the part that fixes the “two rules every time” pain.

## 8.1 Default Datalog semantics: missing tuple means false

For normal relations (`.decl`, derived relations):

* If `p(D, X)` has no matching tuple, the goal fails and the rule body fails.
* This is correct for edges/events.

## 8.2 Optional attributes: use `.func` returning `option<T>`

For properties that are often missing (docs, parent, display label, trait parent), RAQL requires they be modeled as `.func` returning option:

```prolog
.func def_doc_summary(D: Def, Doc: option<string>) extern.
.mode def_doc_summary(+Def, -option<string>).
```

Now the join never “drops” `D`. You get `Doc = none`.

## 8.3 Pattern matching on option values

You can filter on presence without extra helper predicates:

```prolog
def_doc_summary(D, some(Doc)), contains(Doc, "unsafe").
def_doc_summary(D, none).
```

## 8.4 Coalesce helper (built-in)

Built-in functional predicate (parametric in `T`, see §3.5):

```prolog
.func coalesce(Opt: option<T>, Default: T, Out: T) extern.
.mode coalesce(+option<T>, +T, -T).
.mode coalesce(+option<T>, +T, +T).  % validate
```

Semantics:

* if `Opt = some(V)`, then `Out = V`
* if `Opt = none`, then `Out = Default`

This makes view writing compact:

```prolog
def_doc_summary(D, OptDoc),
coalesce(OptDoc, "", DocLine).
```

---

# 9. Negation

RAQL supports `not` with **stratified negation**.

## 9.1 Stratification rule

Let predicate dependency edges be:

* positive dependency: `p -> q` if `p` uses `q(...)`
* negative dependency: `p -/-> q` if `p` uses `not q(...)`

A program is valid iff there exists an assignment of strata integers to predicates such that:

* positive edges do not decrease stratum
* negative edges strictly increase stratum

If impossible, program is rejected.

## 9.2 Range restriction (safety)

RAQL enforces Datalog safety (range restriction).

A rule is **safe** iff every variable that appears in the rule head, in any negated goal,
or in any non-binding constraint is **range-restricted** by positive evidence in the body.

### 9.2.1 What counts as range-restricting evidence (required)

The following are range-restricting:

1) **Positive predicate goals** (non-negated atoms):
   * variables appearing in a positive predicate goal are range-restricted (they range over that relation's set of ground facts).

2) **Binding constraints**:
   * Unification `T1 = T2` range-restricts any variable that becomes bound by unifying with a ground term,
     or by unifying with another range-restricted variable/term.
     Example: `EdgeKind = "direct"` range-restricts `EdgeKind`.
   * Arithmetic binding `X := Expr` range-restricts `X` if every variable used in `Expr` is already range-restricted.

3) **Binder outputs**:
   * Aggregate binders range-restrict their binder variable (e.g., `N = count(...)` range-restricts `N`).
   * `choose_topk` range-restricts `Score` and `Item`.
   * `witness_path` range-restricts `P`.
   * `path_hop` range-restricts its output variables (`Seq`, `From`, `To`, `EdgeKind`, `Evidence`).

4) **Disjunction groups**:
   After desugaring (§7.3), any variable referenced outside the disjunction must be range-restricted in every branch
   (including via binding constraints like `X = "..."`).

### 9.2.2 Non-binding constraints (required)

For `!=`, `<`, `<=`, `>`, `>=`:
* every variable appearing in the constraint must be range-restricted
* and the operands must be ground at evaluation time (§7.4)

Classic illegal example (rejected because `X` is not range-restricted by any positive goal):

```prolog
p(X) :- not q(X).
```

Another illegal example (rejected because `Y` is not range-restricted by any positive goal):

```prolog
p(X) :- q(X), not r(Y).
```

---

# 10. Aggregates

RAQL supports aggregates as **binders** in rule bodies.

## 10.1 Syntax

General form:

```prolog
N = count(V : Goals).
N = count(Goals).                 % count rows
N = count_distinct(V : Goals).
S = sum(V : Goals).
M = min(V : Goals).
M = max(V : Goals).
```

* `Goals` is a comma-separated list of goals, with its own local variables.
* Variables mentioned only inside `Goals` are scoped to the aggregate.
* Variables from the outer rule may be referenced inside `Goals` (capture).

## 10.2 Semantics

The aggregate is evaluated for each binding of the outer variables (implicit grouping by outer variables), producing a single value.

**Aggregate evaluation model (precise):**

Given an aggregate binder with inner `Goals`:

* Evaluate the inner `Goals` under the current binding of outer variables.
* This yields a set of satisfying bindings for the inner `Goals`' variables (set semantics).

**Projection rule (required, no ambiguity):**

For any aggregate written as `agg(V : Goals)`:
* Let `Rows` be the set of satisfying bindings of all variables that appear in `Goals`
  (with outer variables treated as fixed parameters).
* Let `Vals` be the **set** `{ V | row ∈ Rows }` (projection to `V`, then dedup).
* The aggregate is computed over `Vals`.

Therefore:
* `count(V : Goals)` = `|Vals|`
* `sum(V : Goals)` = sum over elements of `Vals`
* `min(V : Goals)` / `max(V : Goals)` are taken over `Vals`

For the row-form `count(Goals)`:
* Let `Rows` be as defined above.
* `count(Goals)` = `|Rows|` (distinct satisfying bindings, i.e. distinct rows)

**The optional `V : Goals` form:**

* When an aggregate is written as `agg(V : Goals)`, `V` must be a variable that appears in `Goals`.
* The aggregate is computed over the set of values that `V` takes across all satisfying bindings of `Goals`.

**Row-form:**

* `count(Goals)` counts the number of satisfying bindings ("rows") of `Goals`.

## 10.3 Restrictions

Aggregates are **non-monotonic** and must be **stratified**.

For stratification purposes, an aggregate introduces an *aggregate dependency*:

* If predicate `p` contains an aggregate binder whose inner `Goals` reference predicate `q`,
  then `p` has an aggregate dependency on `q`.

Rules (required):

* Aggregate dependencies must go to a strictly lower stratum (same rule as negation and `choose_topk`/`witness_path`).
* Aggregates are forbidden inside any recursive SCC (directly or indirectly).

Rationale: aggregates must observe a complete input set. Requiring lower-stratum inputs eliminates ambiguity and
ensures deterministic, set-based semantics.

## 10.4 Empty input semantics (required)

Let the inner `Goals` produce an empty set of satisfying bindings.
Then:

* `count(Goals)` = 0
* `count(V : Goals)` = 0
* `count_distinct(V : Goals)` = 0
* `sum(V : Goals)` = 0
* `min(V : Goals)` **fails the goal** (produces no binding)
* `max(V : Goals)` **fails the goal** (produces no binding)

## 10.5 Type restrictions (required)

* `sum(V : Goals)` requires `V:int`.
* `min(V : Goals)` and `max(V : Goals)` require `V` to be orderable (§3.4, §12.2).
* `count_distinct` is a synonym for `count` in v0.1:
  * `count_distinct(V : Goals)` == `count(V : Goals)` (both count `|Vals|` as defined in §10.2)
  * both are retained for readability and future evolution

---

# 11. Non-monotonic selection built-ins (v0.1)

Two built-ins are deliberately non-monotonic but are essential for RAQL as an assembly language:

* `choose_topk` (top-K selection)
* `witness_path` (path witness selection)

They are treated like negation for stratification purposes.

## 11.1 `choose_topk` (single built-in binder)

### Purpose

Select top-K items per group by integer score, deterministically, without awkward self-joins.

### Syntax

`choose_topk` is a **binder goal**:

```prolog
choose_topk(Tag, K, Group, Score, Item : Goals).
```

* `Tag: string` — must be a string literal constant used to namespace the selection site.
  * Tag is not required to be globally unique.
  * Each syntactic occurrence of `choose_topk(...)` is semantically independent.
  * Engines should key any internal caches by `(Tag, occurrence-id)` (not Tag alone) to prevent collisions.
* `K: int` — must be bound at call time (literal, input fact, or previously bound variable).
* `Group` — grouping key (any orderable term).
* `Score: int` — score variable bound inside `Goals`.
* `Item` — item variable bound inside `Goals`.
* `Goals` — goals that generate candidates `(Score, Item)`.

### Semantics (precise)

For each binding of the outer variables (including `Group`), define the candidate multiset:

`C = { (Score, Item) | Goals succeeds }`

Convert to set by removing duplicate `(Score, Item)` pairs.

Then keep at most `K` pairs with largest `Score`:

* Sort candidates by:

  1. Score descending
  2. `stable_order(Item)` ascending
  3. if still tied, `stable_order(Score)` (int order)

Return a solution per selected `(Score, Item)`, binding `Score` and `Item` in the outer rule.

### Stability (required)

`stable_order(Term)` is defined in §12.2 and must be total.

### Stratification rule for choose_topk

`choose_topk` introduces a **selection dependency**:

* any predicate that uses `choose_topk` must be in a strictly higher stratum than predicates used in `Goals`.

Additionally:

* `choose_topk` is forbidden inside any recursive SCC (direct or indirect recursion). If detected, reject program.

This makes evaluation unambiguous.

### Modes

Because it's a binder, mode is structural:

* `Tag` must be a string literal constant.
* `K` must be bound at call time.
* `Group` may be bound or bound by outer goals.
* `Score` and `Item` are outputs of the binder (bound by `Goals` + selection).

---

## 11.2 `witness_path` and `path_hop`

### Purpose

Produce actionable traces: a connected, ordered witness path, not a bag of reachable nodes.

### Standard graph relation

Paths are computed over a standard relation defined by the program:

```prolog
.decl graph_edge(Graph: string, From: Def, To: Def,
                 EdgeKind: string, Evidence: Span).
```

**Required well-formedness rule:**

* `graph_edge/5` is a reserved predicate name and arity for v0.1.
* Programs may declare `graph_edge` only with exactly this schema; otherwise compilation fails.

* `Graph` is a string namespace (e.g. `"call"`, `"error"`, `"convert"`).
* `EdgeKind` is a free-form label (e.g. `"direct"`, `"trait"`, `"qmark"`).
* `Evidence` is a span grounding the edge.

### witness_path declaration

```prolog
.decl witness_path(Graph: string, From: Def, To: Def, P: Path) extern.
.mode witness_path(+string, +Def, +Def, -Path).
```

### path_hop declaration

```prolog
.decl path_hop(P: Path, Seq: int,
               From: Def, To: Def, EdgeKind: string, Evidence: Span) extern.
.mode path_hop(+Path, -int, -Def, -Def, -string, -Span).
```

### Inputs controlling path selection

These are scalar inputs (injected by runtime); defaults are specified:

```prolog
% These are scalar inputs and must be single-valued.
.func path_limit(N: int) input.         % default 1 (runtime inject)
.mode path_limit(-int).

.func path_max_depth(N: int) input.     % default 8 (runtime inject)
.mode path_max_depth(-int).
```

If not provided, the runtime must inject the default fact.

### Semantics (precise)

Given `Graph`, `From`, `To`, define the directed multigraph `G` from all tuples in `graph_edge(Graph, A, B, Kind, S)`.

`witness_path(Graph, From, To, P)` returns up to `path_limit(N)` distinct paths subject to `path_max_depth`.

Paths are ranked deterministically by:

1. hop count ascending (shorter paths first)
2. lexicographic order over the hop sequence’s **edge stable keys** where each hop key is:
   `(stable_order(From), stable_order(To), EdgeKind, stable_order(Evidence))`

A returned `Path` is opaque but stable for the lifetime of the evaluation.

`path_hop(P, Seq, A, B, Kind, Evidence)` enumerates the hops:

* `Seq` is 0-based and strictly increasing with no gaps for a given `P`.
* The hops must correspond to edges from `graph_edge` for the same `Graph` used in `witness_path`.

### Stratification rule for witness_path

Like choose_topk:

* `witness_path` and `path_hop` may not appear in recursive SCCs.
* They must be in a stratum strictly above `graph_edge/5` (and anything `graph_edge` depends on).

---

# 12. Determinism and ordering

Datalog is unordered; RAQL artifacts require stable ordering. RAQL enforces determinism via explicit keys and stable orders.

## 12.1 Output ordering is not “query order”

There is no procedural evaluation order. If you need ordering, you encode it in fields (Rank/Seq) or you use `choose_topk`.

## 12.2 `stable_order(Term)` (required total order)

RAQL defines a total order for tie-breaking in choose_topk and witness_path.

**Definition (no ambiguity)**

`stable_order` is defined by comparing the tuple:
    `(TypeTag, ValueKey...)`
where:

* `TypeTag` is the fully elaborated static type name of the term (e.g. `"int"`, `"Def"`, `"option<string>"`, `"RenderMode"`).
* `ValueKey` is defined below per type.

Cross-type ordering is by `TypeTag` first, then `ValueKey`.

**Clarification (required):**

* `TypeTag` comparison uses lexicographic UTF-8 byte order over the canonical textual `TypeTag` string.

**ValueKey per type/form:**

* `int`: numeric order
* `string`: lexicographic UTF-8 byte order
* `bool`: `false < true`
* enum `E`: by variant ordinal in its `.type` declaration (0-based), i.e. declaration order is the stable order
* `option<T>`:
  * `none < some(_)`
  * `some(A) < some(B)` iff `stable_order(A) < stable_order(B)`
* `list<T>`: lexicographic by element `stable_order`; shorter list first when one list is a prefix of the other
* `Def`: order by `handle(D, HandleStr)` (host-provided)
* `Span`: order by `(RelPath, L0, C0, L1, C1)` using `span_key/6` (host-provided)
* `TypeRef`: order by `typeref_id(TR, IdStr)` (host-provided)
* `Node`: order by `node_id(N, IdStr)` (host-provided)
* `Call`: order by `call_id(C, IdStr)` (host-provided)
* `Ref`: order by `ref_id(R, IdStr)` (host-provided)
* `Impl`: order by `impl_id(I, IdStr)` (host-provided)
* `Path`: order by internal deterministic ID (opaque but total)

**Host requirements for stable_order**

The host must provide the following stable key functions. Their exact formats are not specified, but they must be stable within a repo snapshot.

```prolog
.func handle(D: Def, H: string) extern.
.mode handle(+Def, -string).

.func span_key(S: Span, RelPath: string, L0: int, C0: int, L1: int, C1: int) extern.
.mode span_key(+Span, -string, -int, -int, -int, -int).

.func typeref_id(TR: TypeRef, H: string) extern.
.mode typeref_id(+TypeRef, -string).

.func node_id(N: Node, H: string) extern.
.mode node_id(+Node, -string).

.func call_id(C: Call, H: string) extern.
.mode call_id(+Call, -string).

.func ref_id(R: Ref, H: string) extern.
.mode ref_id(+Ref, -string).

.func impl_id(I: Impl, H: string) extern.
.mode impl_id(+Impl, -string).
```

---

# 13. Recursion and the recursion guard (enforced)

## 13.1 Fixpoint evaluation

For each stratum, RAQL evaluates derived predicates to least fixpoint using semi-naive or equivalent.

## 13.2 Global iteration limit (required)

To prevent runaway in malformed views, RAQL includes an enforced fixpoint iteration bound.

### Directive form

```prolog
.pragma max_iters = 128.
```

### Runtime override

```prolog
% Scalar override modeled as option for uniqueness and simplicity.
.func opt_max_iters(N: option<int>) input.
.mode opt_max_iters(-option<int>).
```

**Default injection:**

* If not provided by the runtime, the runtime must inject `opt_max_iters(none)`.

**Effective max_iters (required, no ambiguity):**

* If `opt_max_iters(some(N))` then `max_iters = N`.
* Else if a `.pragma max_iters = P` is present then `max_iters = P`.
* Else `max_iters = 128`.

## 13.3 Behavior when exceeded (no ambiguity)

If any recursive SCC exceeds `max_iters` iterations:

* evaluation **halts**
* the run is marked **partial**
* the engine must emit:

```prolog
out_status("partial").
out_note("Notes", "fixpoint iteration limit exceeded in SCC: <name>").
```

…and still returns all facts derived up to the stopping point.
This is intentionally "fail-soft but honest," which is better for an agent tool than silent truncation or hanging.

`out_status/1` and `out_note/2` are built-in output predicates (declared in §14.4).
The engine must emit `out_status("ok")` if evaluation completes without hitting limits.

**Clarification (required):**

* When evaluation halts due to the iteration limit, no higher strata are evaluated.
* The engine still returns all facts derived in completed strata, plus the partial facts derived in the stratum where the limit was exceeded.

## 13.4 Analysis world stamp (required)

The runtime must expose a stable stamp identifying the analysis world used to produce the snapshot (e.g. target triple, enabled features, relevant cfg).

This is exposed via a required extern functional predicate:

```prolog
.func world_stamp(Stamp: string) extern.
.mode world_stamp(-string).
```

**Semantics:**

* `world_stamp/1` must produce exactly one tuple per run.
* The stamp format is not specified, but it must be stable within a repo snapshot.

## 13.5 Runtime errors (required)

Some operations may encounter runtime errors, including:

* division by zero in arithmetic binding (§7.4, `:=`)
* int overflow in arithmetic or sum (§3.1, §10.4)
* extern `.func` cardinality violations (0 or >1 results for bound input) (§5.2)
* implementation-defined failures in extern predicates (e.g. adapter I/O failure)

**On any runtime error:**

* evaluation halts immediately
* the run is marked partial
* the engine must emit:

```prolog
out_status("partial").
out_note("Errors", "runtime error: <description>").
```

* the engine returns all facts derived up to the stopping point

If the run is already partial, additional errors may emit additional `out_note/2` facts, but the status remains `"partial"`.

---

# 14. Standard output relations (v0.1, with Seq)

These relations define the artifact contract. Views populate them; renderer consumes them.

## 14.1 Fragments

### Def fragments

```prolog
.decl out_def_frag(
  Section: string,
  Group: string,
  Rank: int,
  Seq: int,
  Kind: string,
  Render: RenderMode,
  D: Def,
  Anchor: option<Span>,
  Title: option<string>
) output.
```

### Span fragments

```prolog
.decl out_span_frag(
  Section: string,
  Group: string,
  Rank: int,
  Seq: int,
  Kind: string,
  Render: RenderMode,
  S: Span,
  Anchor: option<Span>,
  Title: option<string>
) output.
```

Notes:

* `Rank` sorts higher first.
* `Seq` orders within `(Section, Group, Rank)` ascending.
* `Group` may be `""` for "no grouping."
* `Anchor` is optional (`none` means no highlight).
* `Title` optional: renderer derives a default if `none`.

**Deterministic rendering (required):**

When presenting fragments, renderers MUST sort deterministically.
At minimum, renderers MUST sort:
* `(Section asc, Group asc, Rank desc, Seq asc, Kind asc)`

If two fragments still tie on these fields, renderers MUST break ties by:
* `stable_order(D)` for `out_def_frag` ties, or
* `stable_order(S)` for `out_span_frag` ties.

This ensures stable output even when authors accidentally reuse `(Rank, Seq, Kind)`.

## 14.2 Fragment key/value metadata (optional but included)

To avoid requiring map types, and to disambiguate between def and span fragments:

```prolog
.type FragTarget = { DEF, SPAN }.

.decl out_frag_kv(
  Section: string,
  Group: string,
  Rank: int,
  Seq: int,
  Kind: string,
  Target: FragTarget,
  Key: string,
  Val: string
) output.
```

This attaches metadata to the fragment identified by `(Section,Group,Rank,Seq,Kind,Target)`.

* `Target = FragTarget::DEF` refers to `out_def_frag`
* `Target = FragTarget::SPAN` refers to `out_span_frag`

This discriminator ensures that metadata can unambiguously target a fragment even if both `out_def_frag` and `out_span_frag` emit the same `(Section,Group,Rank,Seq,Kind)` tuple.

**Recommended runtime warning (v0.1):**

If multiple fragments of the same `Target` share the same `(Section,Group,Rank,Seq,Kind)` key,
the engine SHOULD emit an `out_note("Notes", "...")` warning that metadata may be ambiguous.

## 14.3 Metrics

```prolog
.decl out_metric_int(Section: string, Name: string, Value: int) output.
.decl out_metric_str(Section: string, Name: string, Value: string) output.
```

## 14.4 Run status and notes

```prolog
.decl out_status(Status: string) output.  % "ok" | "partial"
.decl out_note(Section: string, Message: string) output.
```

**Reservation rule (required):**

`out_status/1` is reserved for the engine. User programs may not emit `out_status/1` facts or rule heads.
If user code attempts to populate `out_status/1`, compilation fails.

User programs MAY emit `out_note/2` facts/rules (for user-level notes), and the engine may also emit `out_note/2`.

---

# 15. Built-in helpers (required)

## 15.1 String predicates

All are pure and deterministic.

```prolog
.decl contains(Haystack: string, Needle: string) extern.
.decl starts_with(S: string, Prefix: string) extern.
.decl fmt(Format: string, Args: list<string>, Out: string) extern.
```

Modes:

```prolog
.mode contains(+string, +string).
.mode starts_with(+string, +string).
.mode fmt(+string, +list<string>, -string).
```

(You can add more later; v0.1 requires at least these.)

## 15.2 Option predicates

These are pure, deterministic helpers. `coalesce/3` is required for v0.1.

```prolog
.func coalesce(Opt: option<T>, Default: T, Out: T) extern.
.mode coalesce(+option<T>, +T, -T).
.mode coalesce(+option<T>, +T, +T).
```

---

# 16. v0.1 REQUIRED rust-analyzer-backed predicates (the "new" core)

This section is the “load-bearing” set you prioritized. These are not “nice-to-haves”; they are required for v0.1.

## 16.1 Type structure predicates

### `ty_ctor` (optional but recommended; reduces awkwardness)

Functional: always returns the top-level constructor kind.

```prolog
.type TyCtor = { APP, REF, PTR, TUPLE, SLICE, ARRAY, DYN, IMPL_TRAIT, PROJ, PARAM, PRIM, NEVER, UNIT, UNKNOWN }.
.func ty_ctor(TR: TypeRef, C: TyCtor) extern.
.mode ty_ctor(+TypeRef, -TyCtor).
```

### `ty_app` and `ty_arg` (required)

As-written application head and type args.

```prolog
.decl ty_app(TR: TypeRef, Head: Def) extern.
.mode ty_app(+TypeRef, -Def).

.decl ty_arg(TR: TypeRef, Index: int, Arg: TypeRef) extern.
.mode ty_arg(+TypeRef, -int, -TypeRef).
.mode ty_arg(+TypeRef, +int, -TypeRef).
```

**Indexing rule (no ambiguity):** `Index` is **0-based**.

**What counts as an arg:** `ty_arg/3` enumerates **type arguments only**. Lifetimes and const generics are not exposed in v0.1.

### Wrappers (required subset)

```prolog
.decl ty_ref(TR: TypeRef, Mut: Mutability, Inner: TypeRef) extern.
.mode ty_ref(+TypeRef, -Mutability, -TypeRef).

.decl ty_ptr(TR: TypeRef, Mut: Mutability, Inner: TypeRef) extern.
.mode ty_ptr(+TypeRef, -Mutability, -TypeRef).

.decl ty_tuple(TR: TypeRef, Index: int, Elem: TypeRef) extern.
.mode ty_tuple(+TypeRef, -int, -TypeRef).
.mode ty_tuple(+TypeRef, +int, -TypeRef).

.decl ty_slice(TR: TypeRef, Elem: TypeRef) extern.
.mode ty_slice(+TypeRef, -TypeRef).
```

### Parameters and primitives (required)

```prolog
.decl ty_param(TR: TypeRef, Param: Def) extern.
.mode ty_param(+TypeRef, -Def).

.decl ty_prim(TR: TypeRef, Name: string) extern.
.mode ty_prim(+TypeRef, -string).

.decl ty_unknown(TR: TypeRef) extern.
.mode ty_unknown(+TypeRef).
```

### Normalization (optional in v0.1 but strongly recommended)

To avoid painting yourself into “as-written only” corners:

```prolog
.func ty_normalize(TR: TypeRef, Norm: TypeRef) extern.
.mode ty_normalize(+TypeRef, -TypeRef).
```

**Semantics:** returns a normalized type ref (alias-expanded, projection-reduced where RA can), or returns `TR` unchanged if normalization is not available.

---

## 16.2 Node / minimal context predicates (enclosing control)

We want enough to answer: “what control structure is this span in?”

### Mapping from span to node (required, functional + optional)

```prolog
.func node_at(S: Span, N: option<Node>) extern.
.mode node_at(+Span, -option<Node]).
```

**Determinism rule for `node_at` (required):**

Let `Candidates` be the set of nodes `N` such that `node_span(N, NS)` and span `S` is fully contained in `NS`.
If `Candidates` is empty, return `none`.
Otherwise return `some(N*)` where `N*` is the node with the smallest `node_span` (most specific).

If multiple candidates have equal `node_span` (implementation-defined), break ties by `stable_order(N)`.

### Node attributes (required, functional)

```prolog
.func node_kind(N: Node, K: NodeKind) extern.
.mode node_kind(+Node, -NodeKind).

.func node_span(N: Node, S: Span) extern.
.mode node_span(+Node, -Span).

.func node_parent(N: Node, P: option<Node>) extern.
.mode node_parent(+Node, -option<Node]).
```

**Why node_parent is option:** root nodes exist; this avoids “two rules” when walking parents.

### “Enough for enclosing control structure” (required via std)

The standard library must provide a derived predicate:

```prolog
% enclosing_control(Span, ControlKind, ControlSpan, Distance)
.decl enclosing_control(S: Span, K: NodeKind, ControlS: Span, Dist: int).
```

**Precise semantics:**

* Let `node_at(S, some(N0))`. If `none`, then `enclosing_control` has no tuples.
* Walk parent chain `N0 -> N1 -> N2 -> ...` until:

  * first node `Ni` with `node_kind(Ni, K)` where `K ∈ { IF, MATCH, WHILE, FOR, LOOP }`.

Bearing in mind §3.3, the control kinds above are the NodeKind enum variants:

  * `K ∈ { NodeKind::IF, NodeKind::MATCH, NodeKind::WHILE, NodeKind::FOR, NodeKind::LOOP }`.

* Then:

  * `ControlS = node_span(Ni)`
  * `Dist = i` (0 means the anchor node itself is control kind)
* If no such ancestor exists, no tuples.

**Boundedness requirement (mandatory):**
The std implementation must respect a bounded climb depth to avoid accidental huge traversals:

```prolog
% Scalar input and must be single-valued.
.func control_max_depth(N: int) input.   % default 32 (runtime inject)
.mode control_max_depth(-int).
```

If no control node found within depth, return no tuple.

(Implementation can be recursive in Datalog or implemented by host; either is valid as long as the semantics match.)

---

# 17. Putting it together: canonical patterns

## 17.1 Optional doc summary, without defensive rules

```prolog
emit_def("Search", "", 0, "HIT", D) :-
  search(Q, D, _Score),
  def_doc_summary(D, OptDoc),
  coalesce(OptDoc, "", DocLine),
  contains(DocLine, "processor").
```

No second rule required.

## 17.2 Type matching without string hacks

“Return type is Result<_, MyError>”:

```prolog
returns_myerror(F) :-
  fn_return_type(F, TR),
  ty_app(TR, ResultDef),
  def_path(ResultDef, "std::result::Result"),
  ty_arg(TR, 1, ETR),
  ty_app(ETR, MyErrorDef),
  def_path(MyErrorDef, "crate::error::MyError").
```

## 17.3 Enclosing control structure

```prolog
out_span_frag("Findings", "", 0, 0, "CONTROL_CTX", RenderMode::BLOCK, ControlSpan, some(S), none) :-
  ref_event(_, Def, RefKind::COMPARE, S, _Fn),
  enclosing_control(S, _K, ControlSpan, _Dist).
```

## 17.4 Trace with witness path

```prolog
% define graph edges
graph_edge("call", Caller, Callee, EdgeKind, S) :-
  call(Call),
  call_in_fn(Call, Caller),
  call_span(Call, S),
  ( call_target(Call, Callee), EdgeKind = "direct"
  ; call_trait_target(Call, Callee), EdgeKind = "through_trait"
  ),
  span_allowed(S).

% pick a path and emit hops
out_metric_int("Summary", "paths", N) :-
  N = count(P : witness_path("call", A, B, P)),
  target_pair(A, B).

out_span_frag("Trace", "", 100, Seq, "HOP", RenderMode::BLOCK, Evidence, some(Evidence), none) :-
  target_pair(A, B),
  witness_path("call", A, B, P),
  path_hop(P, Seq, _From, _To, _Kind, Evidence).
```

(Enum render mode is qualified per §3.3.)

---

# 18. Deferred items (explicitly out of scope)

RAQL v0.1 does **not** provide:

* multiple analysis worlds, cfg exploration, feature toggling in-language
* first-class CFG blocks or general dataflow lattices

However, v0.1 **does** support:

* ref_event kinds including MOVE/WRITE/etc if the host provides them
* honest status + notes when evaluation is partial (§13.3)

---

# 19. Grammar (EBNF-ish, complete for v0.1)

This is sufficient to implement a parser.

```
program        := { stmt } ;

stmt           := directive
               | decl
               | rule
               | fact
               ;

directive      := ".include" string "."
               | ".type" ident "=" "{" ident { "," ident } "}" "."
               | ".mode" ident "(" mode_args ")" "."
               | ".pragma" ident "=" int "."
               ;

mode_args      := [ mode_arg { "," mode_arg } ] ;
mode_arg       := ("+"|"-"|"?") type ;

decl           := ".decl" ident "(" args ")" { attr } "."
               | ".func" ident "(" args ")" { attr } "."
               ;

attr           := "extern" | "input" | "output" ;

args           := [ arg { "," arg } ] ;
arg            := ident ":" type ;

type           := ident
               | "int" | "string" | "bool"
               | "option" "<" type ">"
               | "list" "<" type ">"
               ;

fact           := atom "." ;

rule           := atom ":-" goals "." ;

goals          := goal { "," goal } ;

goal           := atom
               | "not" atom
               | constraint
               | aggregate
               | choose_topk
               | "(" goals ";" goals { ";" goals } ")"
               ;

atom           := ident "(" [ terms ] ")" ;
terms          := term { "," term } ;

term           := var
               | "_"
               | int
               | string
               | "true" | "false"
               | enum_atom
               | "none" [ "::" "<" type ">" ]
               | "some" "(" term ")"
               | list
               ;

enum_atom      := ident "::" ident ;

list           := "[" [ term { "," term } ] "]" [ "::" "<" type ">" ] ;

constraint     := term relop term
               | var ":=" expr
               ;

relop          := "=" | "!=" | "<" | "<=" | ">" | ">=" ;

expr           := expr_add ;
expr_add       := expr_mul { ("+"|"-") expr_mul } ;
expr_mul       := expr_unary { ("*"|"/") expr_unary } ;
expr_unary     := term | "-" expr_unary | "(" expr ")" ;

aggregate      := var "=" agg_name "(" [ var ":" ] goals ")"
               ;

agg_name       := "count" | "count_distinct" | "sum" | "min" | "max" ;

choose_topk    := "choose_topk" "("
                   string "," term "," term "," var "," var ":" goals
                 ")" ;

% Lexical tokens (derived from §1; included here for completeness):
ident          := /[A-Za-z_][A-Za-z0-9_]*/ ;
var            := /[A-Z_][A-Za-z0-9_]*/ ;
int            := /-?[0-9]+/ ;
string         := a double-quoted string literal with escapes as in §1.5 ;
```

Notes:

* Aggregate syntax is `N = count(V : goals)` as specified.
* `choose_topk` syntax is a dedicated binder form, not a normal atom.

---

# 20. Minimal “host contract” summary (what rust-analyzer adapter must provide)

To build a working RAQL v0.1 engine, the host must provide, at minimum:

* Identity and grounding

  * `handle/2` as `.func`
  * `span_key/6` as `.func` (stable ordering key for spans; see §12.2)
  * `typeref_id/2`, `node_id/2`, `call_id/2`, `ref_id/2`, `impl_id/2` as `.func` (stable ordering keys for opaque host types; see §12.2)
  * `world_stamp/1` as `.func` (analysis world stamp; see §13.4)
* Type structure predicates in §16.1
* Node predicates in §16.2
* Text extraction predicates (if you want rendering in-language; renderer can also do it directly)

  * `span_text(+Span, -string)` (guarded)
  * `def_sig_span/2`, `def_item_span/2`, `def_header_span/2` (guarded/functional)
  * `expand_span(+Span, +RenderMode, -Span)` (guarded)
* Enough semantic base relations for your views (calls, refs, type sites, etc.) — not restated here since this is the language spec, not the full RA fact schema.

---

## Why this v0.1 is “actually RAQL”

With these five upgrades, RAQL stops being “a def/span graph query tool” and becomes a language that can express Rust-aware questions:

* structural type queries (no string hacks)
* contextual queries (“inside match arm / if guard”)
* true trace narratives (witness path, hop evidence)
* curated selection (top-K) without schema bloat
* ordered artifacts (Seq) without relying on accidental ordering

…and it does so while staying **Datalog/Prolog intuitive**.

