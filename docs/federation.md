# Federation

`ctx` can share a bounded slice of one repository's product knowledge with a sibling repository checked out locally on the same machine — the shape of a multi-service codebase where each service lives in its own Git repository. This is entirely local: there is no server, no remote fetch, and no network call. `ctx sync` reads a neighbor's exported manifest straight off disk by the filesystem path it was registered with. The only two things `ctx` ever contacts over a network are `ctx ingest gitlab` and `ctx enrich`/`ctx verify --auto` — federation is not one of them (see [docs/architecture.md](architecture.md)).

## What crosses the boundary

Only two things: `public`-visibility product-context documents (Features/Requirements/Invariants/Decisions — see [docs/authoring-context.md](authoring-context.md#visibility)), and every indexed HTTP endpoint your code exposes (see [docs/api-contracts.md](api-contracts.md)). Everything else — code, `private` documents, database schema, tests — never leaves the repository.

## Setup

Give the repository a service identity in `.ctx/config.toml` (see [docs/configuration.md](configuration.md#service)):

```toml
[service]
name = "billing"
```

Mark the documents you're willing to share:

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

`ctx registry add` requires `billing` to already be a Git repository root with a resolvable `[service].name`; a name you pass explicitly must match it if both are present. The registry lives in `.ctx/registry.toml`, which is local and never committed — every clone (and every machine) registers its own neighbors.

## Export and sync

`ctx export` writes this repository's own manifest by hand — useful to inspect what would be shared:

```bash
ctx export                 # writes .ctx/export.json
ctx export --out /tmp/billing-export.json
```

Export requires an index current with `HEAD` (run `ctx index` first) and a configured `[service].name`.

You normally don't run `ctx export` yourself in the repository that's *consuming* federation data — `ctx sync` does it for you, once per registered neighbor:

```bash
ctx sync
```

For each neighbor, `sync` shells out to that neighbor's own `ctx` binary (`ctx --json export --out .ctx/export.json`, run with the neighbor's directory as the working directory), reads the resulting manifest, and replaces that neighbor's stored snapshot atomically. A neighbor that fails to export (uncommitted index, missing service name, incompatible schema version, or a mismatched service name) does not block the others — its error is reported and the run continues. `sync` then resolves every one of *your* repository's outbound HTTP calls against every synced neighbor's endpoints by HTTP method and normalized path template, recording a match as a `FEDERATED_MATCH` (never a `FACT` — its truth depends on both repositories' commits, not just yours) and reporting every call that matched no neighbor endpoint honestly as unresolved, rather than guessing.

## Inspecting synced state

```bash
ctx federation list          # every neighbor's last sync time, source commit, and staleness
ctx federation show billing  # that neighbor's imported documents, endpoints, resolutions, and unresolved calls
```

`stale` in `federation list` means the neighbor's own checkout has moved past the commit `ctx sync` last imported from it — run `ctx sync` again to catch up.

## Current limits

- Local sibling checkouts only, on one machine — there is no remote/URL registry entry and no CI-friendly "pull the latest from a Git remote" mode yet.
- Federation manifest schema is versioned (`FEDERATION_SCHEMA_VERSION`); `ctx sync` refuses a neighbor on a mismatched version rather than guessing compatibility.
- Cross-repository request tracing (following an endpoint's data reads/writes and outbound calls through a `FEDERATED_MATCH` into a neighbor's own handler) is a documented future direction (`ADR-FEDERATION-003` in `.context/decisions/`), not yet implemented as a command.
- `ctx sync` only ever reads one synchronized snapshot per neighbor; it never fetches, indexes, or recurses into a neighbor's own neighbors.
