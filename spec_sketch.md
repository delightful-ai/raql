Got it – I’m on it. I’ve updated the design instructions accordingly.


Alright. With the real-world rust-analyzer asks + the “amnesia agent” constraint, the v0 design should feel like a **semantic compressor** and a **spiderweb radar**, not a query playground.

Here’s a concrete 80/20 v0 that gives a ridiculous amount of leverage immediately, stays noninteractive, and stays intuitive because it’s basically:

* `find` (what is this noun?)
* `explain` (what is true *here*, with minimal sufficient context?)
* `show` (shape of a def)
* `uses/refs` (where/how used, categorized)
* `callers/callees` (flow)
* `impact` (peripheral vision)
* `scaffold` (bridge from “know” to “write”)

Under the hood it’s Datalog over HIR, but the product is the **views and verbs**.

---

# V0 principles

## Views are the product

Agents almost never type primitives. They run **high-level views** (commands), and only drop into `q` to build a new view.

## Output is semantic compression

Default output is **dense plaintext**: high entropy, low whitespace, no table borders. It should be readable to humans but tuned for agent token economy.

## Two output views, max

* Default: dense plaintext
* Optional: JSONL (for Python bridge / post-processing)

No “concise/machine” dialects. Just the two.

## “No results” is a result

If something returns nothing, the CLI must say whether that’s **Verified 0** (strong fact) or **Unknown/Partial** (analysis degraded).

## Explicit trust + broken-state tolerance

Every result line carries a status:

* `V` = Verified (resolved via HIR/types)
* `H` = Heuristic (fallback because resolution unavailable or partial)

The tool remains useful mid-refactor.

---

# One mental model for chaining

Every line that represents a “hit” starts with:

`path:line:col`

and ends with a copy-pastable handle:

`@K:<qualified-id>`

So agents can go:

1. `raql find X`
2. copy handle
3. `raql type @T:...` / `raql uses @T:...` / `raql explain path:line:col`

Headings start with `#` so they’re trivially greppable out:

* keep headings for human comprehension
* `grep -v '^#'` gives pure results for piping

---

# Global flags (keep it tight)

These apply to most commands:

* `--jsonl`
  Output JSONL records instead of text.

* `--code[=N]`
  Include code excerpt. Default N=1 if provided. (For multi-hit commands, this can be expensive, so it’s opt-in.)

* `--include-tests`
  Default behavior is “tests excluded” (because it’s the 99th percentile exploration pain). Add this flag to include them.

* `--include-macro`
  Default excludes macro-generated hits. Add this to include macro-expanded results.

* `--include-blanket`
  Default hides blanket impls (the “.into() takes me to a blanket impl” problem). Add to include.

That’s it for global. Everything else is command-specific and minimal.

---

# The v0 command set

## 1) `raql find <noun>`

Cold start. “What even is this string in this repo?”

### Output shape

* lists *definitions* and common semantic categories
* returns handles

Example:

```
# FIND CanonicalState
src/core/state.rs:42:1  V  TYPE struct core::state::CanonicalState  fields=5 impls=12  @T:core::state::CanonicalState
src/core/traits.rs:15:1 V  TRAIT core::traits::CanonicalState  items=3 impls=9  @Tr:core::traits::CanonicalState
```

This solves the “amnesia noun ambiguity” problem immediately.

---

## 2) `raql explain <file:line:col | handle>`

This is the anchor. It should feel like “dump everything useful about this location”, not a bag of facts.

### What `explain` must do in v0

It detects the syntactic situation and prints the most useful semantic context:

* On a **call**: resolved callee, signature, where defined, whether it’s a trait method, and the receiver/arg types.
* On a **method call**: show the impl path if concrete; if not, show the trait item and bounds.
* On a **field access**: resolve field to its definition, field type, and show “what you can do with this value” (key methods or traits).
* On `.into()` / operators: show the relevant trait method mapping and skip blanket impls unless `--include-blanket`.

Example (call site):

```
src/core/apply.rs:42:13  V  CALL core::apply::apply_event(state: &mut CanonicalState, event: &EventBody) -> Result<()>  def=src/core/apply.rs:10:1  @F:core::apply::apply_event
  args: state=&mut core::state::CanonicalState, event=&core::event::EventBody
  returns: Result<(), core::error::Error>
  enclosing: core::daemon::ingest_remote_batch(...)  @F:core::daemon::ingest_remote_batch
```

Example (field access):

```
src/core/state.rs:88:9  V  FIELD core::state::CanonicalState.beads: BeadMap<BeadId, Bead>  def=src/core/state.rs:45:5  @Field:core::state::CanonicalState::beads
  value-type: core::beads::BeadMap<...>  traits: Clone Debug Serialize Deserialize  @T:core::beads::BeadMap
  common methods: insert get remove iter len
```

If `--code=2`:

* include 2 lines of code around the location.

This single command eliminates most “open file, scroll, parse imports” token waste.

---

## 3) `raql type <type>`

Type exploration: fields, variants, impls, key methods, embedding, conversions.

### Default output for `type` should include

* definition location + kind (struct/enum/type alias)
* fields/variants (names + types)
* implemented traits (top N, with “+K more”)
* embed/containment: “types that contain this as a field”
* conversions: From/Into edges (skipping blanket by default)

Example:

```
# TYPE core::state::CanonicalState  (tests excluded, macro excluded)
src/core/state.rs:42:1  V  struct core::state::CanonicalState  @T:core::state::CanonicalState
  fields:
    beads: core::beads::BeadMap<BeadId, Bead>
    tombstones: core::tomb::TombstoneSet
    ...
  traits (12): Default Debug Clone Serialize Deserialize ... (+6)
  embedded-by (3):
    src/daemon/repo.rs:21:1  V  struct daemon::RepoState.state: CanonicalState  @T:daemon::RepoState
  conversions:
    From<...> (2): core::state::CanonicalState <- core::state::Snapshot ... (+1)
    Into<...> (1): core::state::CanonicalState -> core::state::Snapshot
```

This covers rust-analyzer issues:

* “traits this type implements”
* “what structs contain this type as a field”
* “what can this type convert to/from”

---

## 4) `raql fn <fn>`

Function exploration: signature, location, parameters, return, and a tiny summary of connectivity.

Example:

```
src/core/apply.rs:10:1  V  fn core::apply::apply_event(state: &mut CanonicalState, event: &EventBody) -> Result<()>  @F:core::apply::apply_event
  calls: 7 (direct)   called-by: 3 (direct)
```

If the agent needs more: `callers`/`callees`.

---

## 5) `raql trait <trait>`

Trait exploration: methods/items + implementors summary.

Example:

```
src/core/traits.rs:15:1  V  trait core::traits::CanonicalState  items=3 impls=9  @Tr:core::traits::CanonicalState
  items:
    fn merge(&mut self, other: Self)
    fn snapshot(&self) -> Snapshot
    ...
  implementors (9): core::state::CanonicalState, core::state::StoreState, ... (+7)
```

This maps cleanly to:

* “find types implementing Trait1 AND Trait2” (see `impls` below)
* “show all associated items available on this type” (via type+trait)

---

## 6) `raql impls <trait> [<trait2> ...]`

Intersection by default. This directly answers the “Trait1 AND Trait2” class.

Example:

```
# IMPLS (AND) core::fmt::Debug core::serde::Serialize
src/core/state.rs:42:1  V  TYPE core::state::CanonicalState  @T:core::state::CanonicalState
src/core/store.rs:11:1  V  TYPE core::store::StoreState  @T:core::store::StoreState
... (+14)
```

Default skips blanket impls. `--include-blanket` shows them.

---

## 7) `raql callers <fn-or-method>` and `raql callees <fn>`

Call flow, with explicit cost knobs.

### Defaults

* `callers X` is **direct** calls only (cheap, reliable).
* To expand:

  * `--transitive[=N]` (medium). Default depth if flag present maybe 5.
  * `--through-traits` (expensive). Includes trait method calls where the concrete impl may be unknown. Lines are tagged `V` or `H` appropriately.

Example:

```
# CALLERS core::apply::apply_event (direct)
src/daemon/core.rs:287:5  V  CALL daemon::core::ingest_remote_batch -> apply_event  @F:daemon::core::ingest_remote_batch
src/daemon/repo.rs:156:9  V  CALL daemon::repo::replay -> apply_event  @F:daemon::repo::replay
```

Trait method usage example:

```
# CALLERS core::fmt::Display::fmt  (--through-traits)
src/ui/log.rs:44:12  V  FORMAT uses Display for core::state::CanonicalState  @T:core::state::CanonicalState
src/ui/log.rs:51:9   H  FORMAT uses Display for <T> where T: Display  @Tr:core::fmt::Display
```

This explicitly addresses the rust-analyzer “call hierarchy through generics” pain without pretending it’s free or exact.

---

## 8) `raql uses <thing>`

This is “grep but semantic, categorized by role”.

### Default behavior

* categorizes automatically, no extra flags
* prints only non-empty categories
* headings include totals and hidden counts (tests/macro)

For a **type**, categories include:

* `FIELD_TYPE`
* `PARAM_TYPE`
* `RETURN_TYPE`
* `LOCAL_TYPE`
* `WHERE_BOUND`
* `CONSTRUCT` (construction in expressions)
* `PATTERN` (destructuring/match sites)

Example:

```
# USES core::state::CanonicalState  (tests excluded, macro excluded)
# FIELD_TYPE (3)
src/daemon/repo.rs:21:1  V  daemon::RepoState.state: CanonicalState  @T:daemon::RepoState
...
# PARAM_TYPE (5)
src/core/apply.rs:10:21  V  fn apply_event(state: &mut CanonicalState, ...)  @F:core::apply::apply_event
...
# PATTERN (2)
src/core/state.rs:140:9  V  match CanonicalState { ... }  @T:core::state::CanonicalState
```

This directly answers the “construction vs pattern sites” request class.

For a **trait**, `uses` includes:

* `BOUND` (where Trait appears in bounds)
* `METHOD_CALL` (calls to its methods, including through generic bounds)
* optionally `IMPL` (types implementing it), though you may keep that in `trait/impls` to reduce noise.

For a **function**, `uses` is effectively call sites (like callers), but categorized:

* `DIRECT_CALL`
* `MACRO_CALL` (if `--include-macro`)
* `DYN_DISPATCH` (if applicable)

---

## 9) `raql refs <thing>`

This is “Find references, but by intent”.

Ref kinds in v0 are intentionally small, and tuned for exploration and writing:

* `READ`
* `WRITE`
* `COMPARE` (includes `== != < <= > >=`, match scrutinee checks, assert_eq-ish macros when resolvable)
* `PASS` (passed as argument / receiver)
* `FIELD` (field access `.foo`)
* `MOVE` (consumed; helps writing code correctly)

Example:

```
# REFS core::store::Request.store_id  (tests excluded, macro excluded)
# COMPARE (3)
src/core/validate.rs:77:9  V  store_id == expected_id  @Field:core::store::Request::store_id
src/core/guard.rs:41:5     V  assert_eq!(store_id, expected_id)  @Field:core::store::Request::store_id
# PASS (5)
...
# WRITE (0)
Verified: 0 write references (scope=prod, macro=excluded)
```

That last line is the “negative space” superpower: it turns “no hits” into an actionable guarantee.

---

## 10) `raql impact <thing>`

The “peripheral vision” command. Depth-1 spiderweb summary for refactors.

For a **type**:

* embedded-by (field containment)
* fns taking it
* fns returning it
* impl traits
* conversions

For a **fn**:

* callers
* callees
* types in signature

For a **trait**:

* implementors
* bounds usage
* method call usage (optional)

Example:

```
# IMPACT core::state::CanonicalState  (depth=1, tests excluded, macro excluded)
embedded-by: 3  (RepoState.state, Snapshot.state, ...)
param-of:    12 (apply_event, build_snapshot, ...)
returns:     4  (load_state, snapshot, ...)
traits:      12 (Default, Serialize, ...)
conversions: 3  (From/Into)
```

Optionally, include top locations per bucket:

```
embedded-by:
  src/daemon/repo.rs:21:1  V  daemon::RepoState.state  @T:daemon::RepoState
...
```

This is the “spooky action at a distance” antidote.

---

## 11) `raql scaffold ...`

This is the “reasoning -> writing” bridge. High leverage for agents.

### v0 scaffolds that pay off immediately

* `raql scaffold impl <trait> for <type>`
* `raql scaffold match <enum-type>` (optional v0, but very useful)

Example:

```bash
raql scaffold impl core::fmt::Display for @T:core::state::CanonicalState
```

Output:

```rust
impl core::fmt::Display for core::state::CanonicalState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        todo!()
    }
}
```

This reduces hallucinated signatures dramatically.

---

## 12) `raql q` and `raql sample`

For the 20% cases, view authorship, and “I need exactly this weird slice”.

* `raql sample uses` shows a handful of tuples for grounding.
* `raql q` runs a heredoc Datalog query.

But the daily workflow is the verbs above.

---

# How this hits the real-world rust-analyzer pattern list

* Trait queries (“what traits does this type implement?”): `type`, `explain`
* Types implementing Trait1 AND Trait2: `impls Trait1 Trait2`
* Calls to trait methods: `callers Trait::method --through-traits` or `uses Trait`
* Callers through generics: `callers --through-traits` (explicitly expensive, tagged)
* Field containment: `type` + `uses` + `impact`
* From/Into: `type` + `impact`
* Construction vs pattern: `uses Type` categories
* Exclude tests: default, `--include-tests` if needed
* Macro-generated: default off, `--include-macro` if needed
* Skip blanket impls: default on, `--include-blanket` if needed
* Operator resolution for PartialOrd/PartialEq: `refs --kind COMPARE`

---

# The “80/20 loop” this enables

A typical amnesiac agent session becomes:

1. `raql find CanonicalState`
2. `raql type @T:...`
3. `raql uses @T:...`
4. spot a location → `raql explain path:line:col`
5. identify the relevant fn → `raql callers @F:...`
6. before editing: `raql impact @T:...`
7. need to implement something: `raql scaffold impl Trait for Type`

That’s a tight, repeatable loop that replaces a lot of “rg + read + hope”.

---

# One last judgment call: default scoping

For v0, I’d default to:

* tests excluded
* macro excluded
* blanket impls excluded

…and make that visible in headers so the agent doesn’t forget the lens:

`# USES ... (tests excluded, macro excluded)`

This biases toward **signal**, which is what exploration needs. When you need the noise, you opt in.

---

If you want, next I can draft the **exact v0 “view catalog”** (the high-level views that back each command) and the minimal HIR-backed primitive relations required to implement them, but still in a “views-first” order so the design stays anchored to what you actually type.

