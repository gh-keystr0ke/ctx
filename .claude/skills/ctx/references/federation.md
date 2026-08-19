# Federation: multiple repositories, one team

`ctx` can share a bounded slice of one repository's product knowledge with a sibling repository checked out locally on the same machine — the shape of a multi-service codebase where each service lives in its own Git repository. This is entirely local: there is no server, no remote fetch, and no network call. `ctx sync` reads a neighbor's exported manifest straight off disk by the filesystem path it was registered with.

Use this when the user is working across more than one service/repository owned by the same team and asks about cross-service impact, "what calls this endpoint", or wants to trace a request across services. Don't set this up for a single-repository project.

## What crosses the boundary

Only two things: `public`-visibility product-context documents (Features/Requirements/Invariants/Decisions — see `authoring-context.md` § visibility), and every indexed HTTP endpoint the neighbor's code exposes. Everything else — code, `private` documents, database schema, tests — never leaves the repository.

## Setup

Each repository that wants to participate needs its own `.ctx/` baseline already onboarded (see `onboarding.md`) plus a service identity in `.ctx/config.toml`:

```toml
[service]
name = "billing"
```

Mark the documents each service is willing to share:

```yaml
id: REQ-SUB-014
visibility: public
```

Then, from a sibling repository (say, `checkout-service`), register `billing` by its local path:

```bash
cd ../checkout-service
ctx registry add ../billing --name billing   # --name is optional if [service].name is already set there
ctx registry list
```

`ctx registry add` requires `billing` to already be a Git repository root with a resolvable `[service].name`; a name passed explicitly must match it if both are present. The registry lives in `.ctx/registry.toml`, which is local and never committed — every clone (and every machine) registers its own neighbors.

## Export and sync

`ctx export` writes this repository's own manifest by hand — useful to inspect what would be shared:

```bash
ctx export                 # writes .ctx/export.json
ctx export --out /tmp/billing-export.json
```

Export requires an index current with `HEAD` (run `ctx index` first) and a configured `[service].name`. You normally don't run `ctx export` yourself in the repository that's *consuming* federation data — `ctx sync` does it for you, once per registered neighbor:

```bash
ctx sync
```

For each neighbor, `sync` shells out to that neighbor's own `ctx` binary (`ctx --json export --out .ctx/export.json`, run with the neighbor's directory as the working directory), reads the resulting manifest, and replaces that neighbor's stored snapshot atomically. A neighbor that fails to export (uncommitted index, missing service name, incompatible schema version, or a mismatched service name) does not block the others — its error is reported and the run continues. `sync` then resolves every one of *your* repository's outbound HTTP calls against every synced neighbor's endpoints by HTTP method and normalized path template, recording a match as a `FEDERATED_MATCH` (never a `FACT` — its truth depends on both repositories' commits, not just yours) and reporting every call that matched no neighbor endpoint honestly as unresolved.

Run `ctx sync` again whenever a neighbor's checkout has moved — `ctx federation list`'s `stale` flag tells you when.

## Inspecting synced state

```bash
ctx federation list          # every neighbor's last sync time, source commit, and staleness
ctx federation show billing  # that neighbor's imported documents, endpoints, resolutions, and unresolved calls
```

## Tracing a request across services

```bash
ctx trace "POST /pay"          # an endpoint selector, method-first
ctx trace main.pay             # or anything ctx impact/ctx explain would resolve to a handler
```

`ctx trace` walks one endpoint's request sequence: its `Exposes` handler, that handler's `ReadsFrom`/`WritesTo` data entities, and its `CallsExternal` outbound contracts. When an outbound call's method and normalized path match a synchronized neighbor's endpoint, the trace crosses into that neighbor by invoking *that neighbor's own* `ctx` binary in its own checkout — so only that neighbor's own process ever decides what of its graph is traceable — and continues the same sequence from its handler. A call that matches no synchronized neighbor is reported honestly as unresolved rather than guessed; a matched neighbor whose checkout has moved past the last-synced commit is reported stale and the branch stops there.

A target always names an endpoint *this* repository exposes — a trace walks outward from a local `Exposes` edge, it never jumps into a neighbor's index directly. Pasting a neighbor's own path from `ctx federation show`'s output doesn't resolve here; point at the local handler that already reaches it instead.

Bounds (fixed, not configurable): at most 8 cross-repository transitions, 50 total structural nodes, 16 branches (outbound calls examined), with cycle detection keyed by `(service, source commit, method, path)`. Every cap, stale neighbor, unmatched call, or cycle ends only the branch it happened on and says so explicitly — the rest of the trace still completes.

`ctx trace` only ever reads each neighbor's already-synchronized snapshot — it never fetches, re-indexes, or re-syncs during a trace query. Run `ctx sync` first (in every service along the path) to trace against current structure.

### Attaching product context to a trace

```bash
ctx trace "POST /pay" --verbose      # or -v
```

Product-semantic assertions (Features/Requirements) are deliberately outside `ctx trace`'s own structural traversal — `--verbose`/`-v` attaches them anyway, as a display-only annotation: each hop's own Features/Requirements (from that hop's *own* repository) are printed right under it. This works across a federation crossing too — the flag travels with the trace into every neighbor it visits.

### Tracing every endpoint under a Feature or Requirement

```bash
ctx explain FEAT-PAYMENTS --trace
```

Shows the target's usual claims/evidence, then a separate `Traces:` section: every HTTP endpoint reachable from the target's own mapped implementation, each fully traced exactly like `ctx trace` — same bounds, same federation crossing, `--verbose` gates the per-hop annotation the same way.

## Current limits

- Local sibling checkouts only, on one machine — no remote/URL registry entry, no CI-friendly "pull the latest from a Git remote" mode.
- Federation manifest schema is versioned; `ctx sync` refuses a neighbor on a mismatched version rather than guessing compatibility.
- `ctx sync` only ever reads one synchronized snapshot per neighbor — it never fetches, indexes, or recurses into a neighbor's own neighbors (no transitive federation).
- `ctx trace` follows HTTP request/response structure only, not field-level data flow (which request field ends up in which database column).
