# RAQL

semantic queries for rust. returns readable context, not file:line coordinates.

```bash
raql callers process
```

```
FN  fn process(&self, req: Request) -> Result<Response, ProcessError>
at src/processor.rs:41-78  @H:fn:6b11d0

# Summary
callers:    4 direct, 2 through-trait
scope:      workspace, no-tests

# Callers

## fn handle_request  @H:fn:c3d4e5
at src/http.rs:45-89

> |     let result = self.processor.process(req)?;
  |     match result {
  |         Ok(resp) => Ok(resp.into()),

## fn batch_ingest  @H:fn:e5f6a7
at src/ingest.rs:112-156

  | for item in items {
> |     processor.process(item)?;
  | }

... 2 more callers. expand: --show callers=all
```

the `@H:...` handles are stable identifiers. paste them into the next query.

## install

```bash
cargo install raql
```

## how it works

raql spawns a background daemon per project. first query loads the workspace (~3s). subsequent queries hit the warm cache (<100ms). daemon exits after 10 minutes idle.

file changes are watched and applied incrementally. no manual reload.

## commands

```
raql <selector>           dossier: "what is this and what do I need?"
raql search <query>       find symbols by name
raql callers <fn>         who calls this, with surrounding code
raql callees <fn>         what does this call
raql refs <def>           value usage: reads, writes, comparisons, moves
raql uses <type>          type sites: params, returns, fields, locals
raql interface <type>     API surface: methods, trait impls
raql impls <Trait>        types implementing a trait (supports intersection)
raql trace <def>          multi-hop flow through the code
raql audit <check> <def>  verification: compare, write, construct, handle
raql bundle <def>         pack a subsystem for reading
```

## selectors

```bash
raql @H:fn:6b11d0                 # handle (precise)
raql crate::processor::process    # qualified path
raql process                      # bare name (shows alternatives if ambiguous)
raql src/lib.rs:42                # file:line
raql src/lib.rs:42/process        # file:line with disambiguator
```

## output

```bash
raql callers X               # readable artifact (default)
raql callers X --only nav    # locations only, for jumping
raql callers X --only counts # just the numbers
raql callers X --jsonl       # structured stream
```

## scope and filtering

```bash
--scope crate              # current crate only
--scope workspace          # all workspace members (default)
--include-tests            # include test code
--include-blanket          # include blanket impls
--show callers=all         # remove budget limits
--why                      # annotate why each fragment was included
```

## why this exists

grepping for callers gives you file:line hits. you then open each one, scan for context, figure out if it's relevant. repeat 30 times.

raql gives you the context directly: the surrounding code, grouped by caller, with the callsite highlighted. one query, one answer.

same for references, type usage, error flow. the output is bounded and readable by default. if you need more, expand with `--show X=all`.

designed for agents that pay per tool call, and humans who don't want to play editor ping-pong.

## license

MIT
