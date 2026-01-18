# RAQL Design Sketch: Building on Rust-Analyzer

This document captures the architectural findings from exploring rust-analyzer (RA) and how RAQL can leverage it as a semantic substrate.

---

## Core Insight

**RA provides semantic truth. RAQL's value is assembly and presentation.**

RA already solves the hard problems: parsing, name resolution, type inference, trait solving, macro expansion. RAQL's job is to turn that semantic truth into readable, decision-ready artifacts.

---

## Integration Strategy

### No Vendoring Required

RA's `load-cargo` crate is designed for programmatic use. It provides exactly what RAQL needs:

```rust
load_cargo::load_workspace_at(
    &root,
    &CargoConfig::default(),
    &LoadCargoConfig {
        load_out_dirs_from_check: false,
        with_proc_macro_server: ProcMacroServerChoice::None,
        prefill_caches: false,
    },
    &|_| {},
) -> (RootDatabase, Vfs, Option<ProcMacroClient>)
```

This returns:
- `RootDatabase` - salsa-based incremental database with all semantic queries
- `Vfs` - virtual file system with all source text in memory
- `ProcMacroClient` - optional proc-macro support

### Minimal Dependency Set

```
load-cargo
├── project-model     # Cargo.toml / rust-project.json parsing
├── base-db           # CrateGraph, input definitions
├── vfs               # Virtual file system (no I/O, just storage)
└── ide-db            # RootDatabase + HirDatabase
    ├── hir           # High-level semantic API
    ├── hir-def       # Definition lowering
    ├── hir-ty        # Type inference + trait solving
    ├── hir-expand    # Macro expansion
    └── syntax        # Syntax trees (rowan-based)
```

---

## Mapping RAQL Primitives to RA APIs

### Identity & Metadata

| RAQL Primitive | RA API | Notes |
|----------------|--------|-------|
| `def_kind(Def, Kind)` | `ModuleDef` enum variants | Module, Function, Adt, Trait, etc. |
| `def_name(Def, Name)` | `.name(db)` on all items | Returns `Option<Name>` |
| `def_path(Def, PathStr)` | `.canonical_path(db, edition)` | Qualified path string |
| `def_crate(Def, Crate)` | `.krate(db)` via `HasCrate` trait | |
| `def_visibility(Def, Vis)` | `.visibility(db)` via `HasVisibility` | |
| `handle(Def, HandleStr)` | Encode: FileId + TextRange + name + sig hash | Best-effort stability |
| `handle_resolve(HandleStr, Def, Confidence)` | Decode + fuzzy match | RAQL-specific |

### Types

| RAQL Primitive | RA API | Notes |
|----------------|--------|-------|
| `typeref_pretty(TypeRef, Str)` | `Type::display(db, target)` | Pretty printing |
| `typeref_mentions(TypeRef, Def)` | `TypeRef::walk()` | Traverse nested types |
| `type_site(Role, Owner, TypeRef, Span)` | HIR traversal | field_type, param_type, etc. |

### Traits & Impls

| RAQL Primitive | RA API | Notes |
|----------------|--------|-------|
| `impl_kind(Impl, Kind)` | `impl_.trait_(db).is_some()` | inherent vs trait |
| `impl_self_type(Impl, Type)` | `impl_.self_ty(db)` | |
| `impl_trait(Impl, Trait)` | `impl_.trait_(db)` | `Option<Trait>` |
| `impl_is_blanket(Impl)` | `simplify_type(self_ty) == None` | Computed |
| `impl_item(Impl, AssocItem)` | `impl_.items(db)` | `Vec<AssocItem>` |
| `trait_item(Trait, AssocItem)` | `trait_.items(db)` | |
| `assoc_item_kind(AssocItem, Kind)` | `AssocItem` enum | Function, Const, TypeAlias |

### Functions

| RAQL Primitive | RA API | Notes |
|----------------|--------|-------|
| `fn_sig(Fn, SigStr)` | `FunctionSignature` query | |
| `fn_param(Fn, Index, Param)` | `fn_.assoc_fn_params(db)` | `Vec<Param>` |
| `fn_return_type(Fn, TypeRef)` | `fn_.ret_type(db)` | |
| `param_type(Param, TypeRef)` | `param.ty` | |

### Calls (via IDE crate)

| RAQL Primitive | RA API | Notes |
|----------------|--------|-------|
| `call_in_fn(Call, Fn)` | `incoming_calls()` | `CallItem.ranges` |
| `call_span(Call, Span)` | `CallItem.ranges` | Multiple per caller |
| `call_target(Call, Fn)` | `outgoing_calls()` | |
| `call_resolution(Call, Kind)` | `CandidateKind` | direct, inherent, trait_static, trait_dyn |

### References (via IDE crate)

| RAQL Primitive | RA API | Notes |
|----------------|--------|-------|
| `ref_event(Ref, Def, Kind, Span, Fn)` | `find_all_refs()` | With `ReferenceCategory` |
| `ref_kv(Ref, Key, Val)` | Category + HIR context | Computed metadata |

**ReferenceCategory values:** Read, Write, Import, Export, Execute, Entry, Macro, Definition

### Search

| RAQL Primitive | RA API | Notes |
|----------------|--------|-------|
| `search(Query, Def, Score)` | `Analysis::symbol_search()` | Fuzzy FST-based |

---

## Trait Solving & Uncertainty

RA uses rustc's next-gen trait solver. It surfaces uncertainty explicitly:

```rust
pub enum NextTraitSolveResult {
    Certain,      // Definitely implements
    Uncertain,    // May or may not (ambiguous or overflow)
    NoSolution,   // Does not implement
}
```

This aligns perfectly with RAQL's goal: "surfaces RA's best available resolution and labels uncertainty."

### Impl Classification

RA splits impls into:
- **Non-blanket**: Associated with a concrete simplified type
- **Blanket**: Generic impls with no concrete type anchor

```rust
pub struct TraitImpls {
    map: FxHashMap<TraitId, OneTraitImpls>,
}

pub struct OneTraitImpls {
    non_blanket_impls: FxHashMap<SimplifiedType, (Box<[ImplId]>, ...)>,
    blanket_impls: Box<[ImplId]>,
}
```

### Queries Available

```rust
Impl::all_for_type(db, ty)     // Impls for a specific type (excludes blanket)
Impl::all_for_trait(db, trait_) // All impls of a trait (includes blanket)
```

---

## File System & Spans

### VFS Abstraction

RA's VFS is purely in-memory, no I/O:

```rust
Vfs {
    interner: PathInterner,        // path -> FileId
    data: Vec<FileState>,          // indexed by FileId
    changes: IndexMap<FileId, ChangedFile>,
}
```

- `FileId` is just `u32` - fast, cheap
- All paths relative to VfsPath internally
- File contents accessed via: `db.file_text(file_id).text(db)` -> `Arc<str>`

### Span Representation

```
FileId (u32) + TextRange (start: u32, end: u32)
```

For output, convert back:
1. `db.file_source_root(file_id)` -> module context
2. Map to repo-relative path for printing
3. Convert TextRange to line:col via `LineIndex`

---

## Salsa & Incremental Computation

RA uses salsa for memoization. Key properties:

- **Inputs**: FileText, SourceRoot, CrateGraph
- **Queries**: All semantic computations (parse, type-check, resolve)
- **Durability hints**: Library = HIGH (rarely changes), Workspace = LOW

RAQL leverages salsa fully via the auto-daemon architecture:
- Daemon stays alive, keeps workspace loaded
- File changes applied incrementally via `apply_change()`
- Queries hit warm salsa cache (~1-50ms)
- No reload penalty between CLI invocations

---

## Runtime Architecture: Auto-Daemon

### Why Auto-Daemon?

A naive CLI that loads fresh every invocation wastes 1-5s per query. That's not "conversational." The auto-daemon pattern gives us:

- **First query**: spawns daemon, waits for load (~2-5s), then queries
- **Subsequent queries**: connects to existing daemon (<50ms)
- **File changes**: daemon watches and applies incrementally
- **Idle timeout**: daemon exits after N minutes of inactivity
- **Transparent**: user just runs `raql callers Foo`, daemon management is invisible

### Components

```
┌─────────────────────────────────────────────────────────────┐
│                         User                                │
│                           │                                 │
│                     raql callers Foo                        │
│                           │                                 │
│                           ▼                                 │
│  ┌─────────────────────────────────────────────────────┐   │
│  │                    raql CLI                          │   │
│  │  (thin client - parses args, connects to daemon)     │   │
│  └─────────────────────────────────────────────────────┘   │
│                           │                                 │
│            Unix socket: /tmp/raql-<project-hash>.sock       │
│                           │                                 │
│                           ▼                                 │
│  ┌─────────────────────────────────────────────────────┐   │
│  │                  raql daemon                         │   │
│  │                                                      │   │
│  │  ┌──────────────┐  ┌──────────────┐  ┌───────────┐  │   │
│  │  │ RootDatabase │  │     Vfs      │  │  Watcher  │  │   │
│  │  │   (salsa)    │  │  (in-memory) │  │ (notify)  │  │   │
│  │  └──────────────┘  └──────────────┘  └───────────┘  │   │
│  │         │                  ▲               │         │   │
│  │         │                  │               │         │   │
│  │         └────── queries ───┴── file Δ ─────┘         │   │
│  │                                                      │   │
│  └─────────────────────────────────────────────────────┘   │
│                           │                                 │
│                     (idle timeout)                          │
│                           │                                 │
│                           ▼                                 │
│                     daemon exits                            │
└─────────────────────────────────────────────────────────────┘
```

### Daemon Lifecycle

1. **CLI invoked**: `raql callers Foo`
2. **Check for daemon**: Try connect to `/tmp/raql-<hash>.sock`
3. **If no daemon**:
   - Spawn `raql --daemon` as background process
   - Daemon writes ready signal to socket
   - CLI waits for ready, then sends query
4. **If daemon exists**: Send query immediately
5. **Daemon processes**: Executes query against warm database
6. **Response**: Streams fragments back over socket
7. **Idle timeout**: Daemon tracks last query time, exits after 10min idle

### Socket Protocol

Simple request/response over Unix socket:

```
Request (JSON):
{
  "id": "uuid",
  "command": "callers",
  "selector": "Foo",
  "opts": { "show_callers": 8, "bodies": false }
}

Response (JSONL stream):
{"id": "uuid", "type": "fragment", "data": {...}}
{"id": "uuid", "type": "fragment", "data": {...}}
{"id": "uuid", "type": "metric", "data": {...}}
{"id": "uuid", "type": "done"}

// Or error:
{"id": "uuid", "type": "error", "message": "..."}
```

### File Watching

Daemon uses `vfs-notify` (RA's file watcher) or raw `notify` crate:

```rust
// In daemon main loop
loop {
    select! {
        // Handle client requests
        req = socket.accept() => {
            handle_request(req, &db).await;
            last_activity = Instant::now();
        }

        // Apply file changes
        changes = watcher.recv() => {
            for change in changes {
                vfs.set_file_contents(change.path, change.contents);
            }
            db.apply_change(vfs.take_changes());
        }

        // Idle timeout
        _ = sleep_until(last_activity + IDLE_TIMEOUT) => {
            break; // Exit daemon
        }
    }
}
```

### Project Identity

Daemon is per-project. Project identity = hash of:
- Absolute path to workspace root
- Cargo.toml mtime (or rust-project.json)

Socket path: `/tmp/raql-<project-hash>.sock`

This allows multiple projects to have separate daemons.

### Edge Cases

| Scenario | Behavior |
|----------|----------|
| Daemon crashed | CLI detects, spawns new one |
| Version mismatch | CLI checks daemon version, restarts if stale |
| Cargo.toml changed | Daemon detects, triggers full reload |
| New dependency added | `cargo metadata` rerun, crate graph rebuilt |
| Multiple terminals | All connect to same daemon |
| Workspace root moved | New project hash, new daemon |

### Daemon Control Commands

For debugging and explicit control:

```bash
raql daemon status          # Is daemon running? PID, uptime, memory
raql daemon stop            # Graceful shutdown
raql daemon restart         # Stop + start
raql daemon logs            # Tail daemon stderr (if logging enabled)
```

These are optional power-user commands. Normal usage never needs them.

---

## What RAQL Must Build

RA provides semantic facts. RAQL must build:

### 1. Handle System

Encode/decode stable-ish identifiers:
```
@H:<crate>:<path>:<kind>:<name>:<sig_hash>
```

With best-effort resolution + confidence scoring.

### 2. Selector Parser

Accept multiple forms:
- Handle: `@H:...`
- Qualified name: `crate::mod::Type::method`
- Bare name: `process` (fuzzy)
- Location: `path:line` or `path:line/needle`

### 3. Fragment Extraction

Given a span + render mode, extract readable code:
- `DOC_SIG` - doc summary + signature
- `ITEM` - full syntactic item
- `HEADER` - one-line declaration
- `LINE` / `STMT` / `BLOCK` / `EXPR` - context slices

Requires span expansion using RA syntax trees.

### 4. View DSL & Query Engine

The relational layer that composes semantic facts:
```
section "Callers" {
    let sites = callsites_of(target, mode=direct)
    group by caller_fn {
        show DOC_SIG(caller_fn)
        top 3 by proximity {
            show BLOCK(call_span) anchor=call_span
        }
    }
}
```

### 5. Renderer

Convert fragments to terminal/JSONL output:
- Grouping with section/group headers
- Guttered code blocks with anchors
- Elision messages with expansion knobs
- Alternatives picker for ambiguous selectors

### 6. Error Flow Tracing

RA has error types but not propagation traces. Build:
```rust
error_event(ErrorDef, Kind, Span, EnclosingFn, Detail)
// Kind: CONSTRUCT, RETURN_ERR, PROPAGATE_QMARK, MAP_ERR, MATCH_HANDLE, CONVERT_FROM
```

---

## Architecture Blueprint

```
raql/
├── Cargo.toml
├── src/
│   ├── main.rs              # Entry point (CLI or daemon mode)
│   │
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── args.rs          # Argument parsing (clap)
│   │   ├── client.rs        # Connects to daemon, sends requests
│   │   ├── spawn.rs         # Spawns daemon if needed
│   │   └── output.rs        # Renders daemon response to terminal
│   │
│   ├── daemon/
│   │   ├── mod.rs
│   │   ├── server.rs        # Socket listener, request dispatch
│   │   ├── state.rs         # RaqlState: RootDatabase + Vfs + Watcher
│   │   ├── loader.rs        # Uses load_cargo, initial workspace load
│   │   ├── watcher.rs       # File change detection, incremental updates
│   │   ├── protocol.rs      # Request/Response types, serialization
│   │   └── lifecycle.rs     # Idle timeout, graceful shutdown
│   │
│   ├── core/
│   │   ├── mod.rs
│   │   ├── selector.rs      # Selector parsing (handle, path, fuzzy)
│   │   ├── handle.rs        # Handle encoding/resolution
│   │   └── fragment.rs      # Fragment IR
│   │
│   ├── semantics/
│   │   ├── mod.rs
│   │   ├── facts.rs         # Fact providers wrapping HIR
│   │   ├── calls.rs         # Call graph facts
│   │   ├── refs.rs          # Reference facts with intent
│   │   ├── impls.rs         # Impl/trait facts
│   │   └── errors.rs        # Error flow facts
│   │
│   ├── query/
│   │   ├── mod.rs
│   │   ├── view.rs          # View DSL types
│   │   ├── executor.rs      # View evaluation
│   │   └── stdlib.rs        # Standard relations
│   │
│   ├── render/
│   │   ├── mod.rs
│   │   ├── extract.rs       # Span -> text extraction
│   │   ├── terminal.rs      # Terminal formatter (for CLI output)
│   │   └── jsonl.rs         # JSONL formatter (for protocol)
│   │
│   └── commands/
│       ├── mod.rs           # Command dispatch
│       ├── search.rs        # raql search
│       ├── dossier.rs       # raql <selector>
│       ├── callers.rs       # raql callers
│       ├── refs.rs          # raql refs
│       ├── uses.rs          # raql uses
│       ├── interface.rs     # raql interface
│       ├── impls.rs         # raql impls
│       ├── trace.rs         # raql trace
│       ├── audit.rs         # raql audit
│       └── bundle.rs        # raql bundle
```

### Key Separation

- **cli/**: Thin client. Parses args, connects to daemon, renders output.
- **daemon/**: Heavy lifting. Owns database, watches files, executes queries.
- **core/**: Shared types (selectors, handles, fragments).
- **semantics/**: Fact providers, shared between daemon and query layer.
- **commands/**: Query logic, runs inside daemon.

---

## Startup Sequences

### CLI (Thin Client)

```rust
fn main() -> Result<()> {
    let args = Args::parse();

    // Daemon mode?
    if args.daemon {
        return daemon::run(&args.root);
    }

    // Client mode (default)
    let project_hash = hash_project(&args.root);
    let socket_path = format!("/tmp/raql-{}.sock", project_hash);

    // 1. Connect to daemon (spawn if needed)
    let conn = match client::connect(&socket_path) {
        Ok(conn) => conn,
        Err(_) => {
            // Spawn daemon, wait for ready
            spawn::start_daemon(&args.root)?;
            client::connect_with_retry(&socket_path, Duration::from_secs(30))?
        }
    };

    // 2. Send request
    let request = Request {
        id: Uuid::new_v4(),
        command: args.command.clone(),
        selector: args.selector.clone(),
        opts: args.opts.clone(),
    };
    conn.send(&request)?;

    // 3. Stream response to terminal
    for event in conn.stream() {
        match event? {
            Event::Fragment(f) => output::render_fragment(&f, &args.output_opts)?,
            Event::Metric(m) => output::render_metric(&m, &args.output_opts)?,
            Event::Done => break,
            Event::Error(e) => return Err(e.into()),
        }
    }

    Ok(())
}
```

### Daemon

```rust
fn daemon::run(root: &Path) -> Result<()> {
    // 1. Load workspace
    let manifest = ProjectManifest::discover_single(root)?;
    let (db, vfs, _) = load_cargo::load_workspace_at(
        root,
        &cargo_config(),
        &load_config(),
        &|_| {},
    )?;

    // 2. Start file watcher
    let (watcher, rx) = watcher::start(root)?;

    // 3. Create state
    let state = RaqlState::new(db, vfs, watcher);

    // 4. Bind socket
    let socket_path = format!("/tmp/raql-{}.sock", hash_project(root));
    let listener = UnixListener::bind(&socket_path)?;

    // 5. Main loop
    let mut last_activity = Instant::now();
    loop {
        select! {
            // Handle client connection
            Ok((stream, _)) = listener.accept() => {
                let request: Request = stream.read_request()?;
                let response = execute(&state, &request);
                stream.write_response(response)?;
                last_activity = Instant::now();
            }

            // Apply file changes
            Ok(changes) = rx.recv() => {
                state.apply_changes(changes);
            }

            // Idle timeout (10 minutes)
            _ = sleep(IDLE_TIMEOUT.saturating_sub(last_activity.elapsed())) => {
                if last_activity.elapsed() >= IDLE_TIMEOUT {
                    break; // Exit daemon
                }
            }
        }
    }

    // Cleanup
    fs::remove_file(&socket_path)?;
    Ok(())
}
```

### Query Execution (Inside Daemon)

```rust
fn execute(state: &RaqlState, request: &Request) -> Response {
    // 1. Parse selector
    let selector = Selector::parse(&request.selector)?;

    // 2. Resolve to definition(s)
    let resolved = state.resolve(&selector)?;

    // 3. Execute command
    let fragments = match &request.command {
        Command::Callers => commands::callers(state, &resolved, &request.opts),
        Command::Refs => commands::refs(state, &resolved, &request.opts),
        Command::Search => commands::search(state, &request.selector, &request.opts),
        // ...
    }?;

    // 4. Return as JSONL stream
    Response::stream(fragments)
}
```

---

## Performance Expectations

### Cold Start (No Daemon)

| Phase | Time | Notes |
|-------|------|-------|
| Spawn daemon process | ~10ms | fork + exec |
| Project discovery | ~10ms | Finding Cargo.toml |
| Workspace metadata | ~50-200ms | cargo metadata |
| File loading | ~1-5s | I/O bound, parallelized |
| Crate graph build | ~100-500ms | |
| First semantic query | ~100-500ms | Cold salsa cache |
| **Total first query** | **~2-7s** | One-time cost |

### Warm (Daemon Running)

| Phase | Time | Notes |
|-------|------|-------|
| CLI startup | ~5ms | Parse args |
| Socket connect | ~1ms | Unix socket |
| Send request | ~1ms | JSON serialization |
| Query execution | ~10-100ms | Hot salsa cache |
| Response streaming | ~1-10ms | Depends on output size |
| **Total** | **~20-120ms** | Conversational |

### Incremental Update (File Changed)

| Phase | Time | Notes |
|-------|------|-------|
| Watcher notification | ~10ms | notify crate |
| Read file contents | ~1ms | Typically small |
| Apply to VFS | ~1ms | |
| Salsa invalidation | ~1ms | Marks queries dirty |
| **Next query** | ~50-200ms | Re-parses affected files |

The key insight: **users pay the load cost once**, then get sub-100ms queries until the daemon times out.

---

## Known Limitations

### From RA

1. **Call hierarchy through generics** - Doesn't find calls through generic trait bounds (RA issue #19358)
2. **Macro expansion** - Primary source location may not match expanded code
3. **cfg-gated symbols** - Analysis respects active features/target only
4. **dyn dispatch** - May not resolve to specific impl

### Design Constraints

1. **No perfect handle stability** - Refactors can break handles; use confidence scoring
2. **Blanket impl heuristics** - RA's `all_for_type()` excludes blankets intentionally
3. **Error flow is RAQL's job** - RA has types but not propagation narratives

---

## Next Steps

### Phase 1: Daemon Foundation
1. **Scaffold project** with load-cargo, tokio, serde dependencies
2. **Implement daemon skeleton**: socket listener, idle timeout, graceful shutdown
3. **Implement CLI skeleton**: connect, spawn, basic request/response
4. **Wire up workspace loading** in daemon startup
5. **Add file watcher** with incremental `apply_change()`

### Phase 2: First Query
6. **Build selector parser** (handle, path, fuzzy forms)
7. **Implement `raql search`** - simplest lens, proves the loop works
8. **Terminal output renderer** - guttered code, sections

### Phase 3: Core Lenses
9. **`raql callers`** - call hierarchy, grouped output
10. **`raql refs`** - reference finding with intent classification
11. **Dossier** (`raql <selector>`) - composite artifact

### Phase 4: Polish
12. **Handle system** - stable-ish identifiers with confidence
13. **Elision messaging** - budgets, "omitted N, use --show X=all"
14. **Error flow tracing** - `raql trace MyError`

The goal: prove the assembly experience early. Daemon infrastructure first, then lenses one at a time.
