# RAQL daemon-backed RA-native cutover design

Date: 2026-03-13
Status: Approved design draft

## Goal

Make RAQL a proper rust-analyzer query language by moving all supported query
execution onto a warm, daemon-backed, incremental rust-analyzer runtime.

This cutover is intentionally hard:

- There is one supported execution model for user-facing queries.
- There is no public fallback to the current direct runtime path.
- The stdlib source surface stays stable.
- Runtime support becomes explicit and capability-gated.

## Problem statement

The current CLI path boots a fresh RA host per invocation and eagerly performs
expensive semantic extraction before most queries can do useful work. That
throws away the core property rust-analyzer is built for: a long-lived,
incrementally updated database that answers queries on demand.

The result is the worst possible integration shape:

- cold startup cost is paid repeatedly
- global semantic work is done eagerly instead of lazily
- user-facing execution does not actually run on a proper warm RA session
- the public CLI and the intended architecture have drifted apart

## Hard rules

The cutover locks these rules:

1. Any supported query execution path must go through the daemon-backed,
   incremental RA runtime.
2. Any direct runtime path is dev-only, quarantined, and must not be wired into
   public CLI behavior.
3. `raql lang run` is part of the supported product path and therefore must be
   daemon-backed.
4. The stdlib remains source-stable during the migration.
5. Unsupported stdlib-backed externs fail explicitly at planning time. They do
   not silently return empty results and they do not trigger fallback execution.
6. Eager "build the whole semantic universe first" execution is not part of the
   supported architecture after this cutover.

## Product boundary

### Supported path

- `raql lang run ...` goes through the daemon
- repeated queries reuse warm workspace state
- incremental file changes update the same live workspace state

### Quarantined path

- direct `RaHostRuntime` execution moves behind an explicit dev-only surface
- this surface exists for bring-up, experiments, and low-level debugging only
- it is not part of the supported user model
- it must not share command names or code paths with public CLI behavior

## Architecture

The supported runtime is split into four layers:

1. `raql-cli` thin client
2. `raql-daemon` process and workspace supervisor
3. `raql-host-ra` workspace service and provider families
4. `raql-engine` execution over host-provided relations

The critical change is that semantic facts are no longer produced by building a
single eager workspace snapshot. Instead, the daemon keeps a live RA world and
materializes fact families only when a query requires them.

## Component model

### `raql-cli`

Responsibilities:

- parse command-line input
- resolve workspace identity
- compile the query enough to produce a request payload and required capability
  set
- connect to or spawn the daemon
- stream daemon output to the terminal

Non-responsibilities:

- loading rust-analyzer state
- building workspace snapshots
- owning query-time semantic caches

### `raql-daemon`

Responsibilities:

- one live daemon instance per workspace identity
- socket lifecycle and version handshake
- request routing and response streaming
- idle shutdown and crash recovery policy
- ownership of the supported execution entrypoint

### `WorkspaceService` inside `raql-host-ra`

Responsibilities:

- own live rust-analyzer state for one workspace
- initialize the workspace
- apply source-file changes incrementally
- perform controlled reloads for workspace-shape changes
- expose narrow operations such as `prepare_query`, `run_query`,
  `apply_changes`, and `status`

Constraint:

- no other component owns or mutates live RA state directly

### Capability planner

Responsibilities:

- map a compiled query to the extern capability set it requires
- reject unsupported capability requirements before execution starts
- make unsupported migration state explicit and debuggable

### Provider families

Each provider family owns one semantic area and is loaded lazily:

- definitions, names, paths
- module/file ownership facts
- references/usages
- call edges
- type facts
- error-flow facts

Provider families cache results inside the live workspace runtime. They are
keyed by workspace state and provider inputs rather than by process lifetime
alone.

### Query adapter

Responsibilities:

- bridge `raql-engine` requests to provider-family operations
- materialize only the relations a query actually touches
- keep stdlib-facing names decoupled from backend implementation details

## Crate boundaries

The crate graph should enforce the architecture rather than merely document it.

### Existing crates that remain core

- `raql-syntax`: parsing, AST, source mapping
- `raql-ir`: typed intermediate forms and query model
- `raql-compiler`: lowering, planning, capability requirement extraction
- `raql-engine`: query execution over host-provided relations
- `raql-host`: backend-agnostic host contracts only
- `raql-host-ra`: rust-analyzer-backed implementation

### New crates

- `raql-protocol`: shared CLI <-> daemon request/response types
- `raql-daemon`: daemon process, workspace registry, socket server, request
  dispatch

### Existing crate to repurpose

- `raql-cli`: thin client only

### Dev-only surface

- `raql-dev` or an equivalently explicit internal binary/command surface for
  direct runtime experiments

### Boundary constraints

- `raql-cli` must not depend directly on `raql-host-ra` once the protocol
  boundary exists
- `raql-daemon` owns supported execution
- `raql-host-ra` owns RA-backed workspace state and provider implementations
- `raql-host` contains no rust-analyzer dependency
- the workspace root should stop carrying production CLI glue if it obscures the
  architecture

## Capability-gated stdlib

The migration compromise is:

- the stdlib surface remains stable
- backend support becomes smaller and more honest
- unsupported areas fail explicitly until rebuilt properly

Each stdlib-backed extern is mapped to a stable capability identifier. The
planner computes the set of required capabilities for a query before runtime
execution begins.

If a query requires any unsupported capability:

- planning fails
- the diagnostic names the missing capability or capabilities
- no partial fallback path is attempted

This allows incremental restoration without query churn:

1. implement a provider family
2. register its capabilities
3. queries using the existing stdlib surface start working again

## Incremental model

The daemon treats the workspace as a long-lived RA session.

### Incremental update cases

- Rust source edits: apply as in-place file changes
- new or deleted Rust files under known roots: update VFS and invalidate
  affected provider caches

### Controlled reload cases

- `Cargo.toml` changes
- `Cargo.lock` changes
- feature or target configuration changes
- workspace membership changes
- toolchain changes that affect workspace loading

The runtime maintains two stamps:

- `workspace epoch`: bumped when workspace structure changes and a reload occurs
- `content revision`: bumped for normal source edits applied incrementally

Providers choose cache keys based on the coarseness they need. The design goal
is to preserve warm state aggressively without confusing ordinary edits with
workspace reload events.

## Day-one supported core

Day one should ship a smaller honest RA-native core rather than pretend to
support the entire historical predicate surface.

The initial supported capability set should favor cheap, reliable structural
facts:

- defs
- names
- paths
- module/file ownership facts
- other similarly cheap RA-native structural facts once verified

Expensive or semantically wide families such as usages, full call hierarchy, and
error-flow tracing return only when they have a real provider family behind
them.

## Data flow

1. User runs `raql lang run ...`
2. `raql-cli` parses the command and produces a request payload
3. `raql-cli` connects to or spawns `raql-daemon`
4. `raql-daemon` routes the request to `WorkspaceService`
5. The capability planner computes required capabilities
6. Missing capabilities fail at planning time with explicit diagnostics
7. `raql-engine` executes against the host adapter
8. `raql-host-ra` provider families materialize needed relations lazily
9. Results, diagnostics, metrics, and final status stream back through
   `raql-protocol`

## Non-goals

This cutover explicitly does not try to do the following:

- preserve current full predicate coverage on day one
- keep legacy direct-runtime fallback in the supported path
- maintain backwards compatibility for unpublished internal architecture
- keep the root package as accidental production glue if that conflicts with the
  target crate boundaries
- preserve eager workspace snapshot extraction as the supported execution model

## Success criteria

The cutover is successful when all of the following are true:

1. all supported query execution goes through the daemon
2. `raql lang run` is daemon-backed
3. repeated queries in one workspace reuse warm RA state
4. ordinary source edits update results without rebuilding the world
5. workspace-shape changes trigger controlled reloads instead of ad hoc failure
6. unsupported stdlib-backed externs fail explicitly at planning time
7. the direct runtime path is quarantined into a dev-only surface
8. public CLI behavior cannot bypass the daemon

## AGENTS policy to codify

The following rule should be added to `AGENTS.md` before implementation work
begins:

> Any supported RAQL query execution path must go through the daemon-backed,
> incremental rust-analyzer runtime. Any direct runtime path is dev-only,
> quarantined, and must not be wired into public CLI behavior.

## Immediate planning implications

The implementation plan should treat these as the first-order workstreams:

1. establish crate boundaries and move public CLI behavior into `raql-cli`
2. stand up `raql-protocol` and `raql-daemon`
3. move supported `lang run` onto the daemon path
4. introduce capability planning and unsupported diagnostics
5. replace eager snapshot-driven execution with day-one RA-native provider
   families
6. quarantine the direct runtime path into a dev-only surface
