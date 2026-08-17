# Command reference

Every command accepts two global flags: `--json` for stable machine-readable output, and `-v`/`-vv` (repeatable) for more verbose diagnostics. Both must appear before the subcommand (`ctx --json impact ...`).

## Setup and indexing

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx init` | | Create `.ctx/config.toml`, `.context/{features,requirements,invariants,decisions}/`, and local SQLite storage. Adds `.ctx/ctx.db*`, `.ctx/registry.toml`, and `.ctx/export.json` to the repository-local Git exclude file (not the shared `.gitignore`). Safe to re-run. |
| `ctx index` | | Incrementally index the current commit's configured source files and `.context` documents. Only committed content is read — see [configuration.md](configuration.md). Run again after every commit you want reflected in impact/explain/review/status. |
| `ctx status` | | Health report: indexed commit vs. `HEAD`, effective source scope, node/claim counts, stale claims, `needs_mappings` (active Requirement/Invariant/Decision with no implementation link), best-effort schema `reconciliation` divergences, and a suggested next action. |

## Query

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx impact <target>` | | Bounded product + implementation + test impact for a file path, canonical symbol, stable ID (`REQ-...`), or `table.column`. See [authoring-context.md](authoring-context.md) for how targets resolve to symbols. |
| `ctx explain <target>` | `--trace` | Full stored claims and evidence for a node ID, or a quoted `"source -> target"` relationship. `--trace` additionally traces every HTTP endpoint reachable from the target's own mapped implementation (every endpoint under a Feature, a Requirement's own, or the target itself if it's already a handler), shown as a separate `Traces:` section — same bounds and federation crossing as `ctx trace`, gated the same way by `--verbose`. |
| `ctx find <name>` | | Discover indexed symbols/nodes by short or exact name. Several matches across namespaces are returned independently, never merged or treated as an error. |

## Review

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx review` | `--base <REV>` (default `HEAD`), `-v` | Review the working diff (or a branch's diff against `--base`) for product-contract, schema, and API-contract impact. See the README's "Review a change" scenario and [docs/architecture.md](architecture.md#schema-aware-review). |

## Context compilation

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx context <task>` | `--file <PATH>` (repeatable), `--symbol <NAME>` (repeatable), `--token-budget <N>` (default 4000) | Compile a bounded, evidence-backed Context Pack for a coding task. An explicit `--file`/`--symbol` seed is a hard scope boundary; lexical auto-seeding only runs when nothing explicit resolves. |

## Mining existing knowledge

See [docs/mining-knowledge.md](mining-knowledge.md) for the full workflow.

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx ingest <source>` | `--since <OID>` | Normalize external artifacts into their own store. `source` is `git`, `code-comments`, or `gitlab`. |
| `ctx enrich` | `--agent <claude\|codex\|antigravity>` (default `claude`), `--model <NAME>`, `--allow-ungrounded-symbols` | Ask an AI agent to propose typed knowledge candidates from ingested artifacts, one bounded neighborhood at a time. Always produces `pending` candidates, never asserted facts. |
| `ctx verify` | `--accept <FINGERPRINT>` \| `--reject <FINGERPRINT>`, `--author <NAME>` (default `local-user`) | List or decide heuristic implementation-link candidates (the deterministic relation suggestions from indexing, not AI-derived). |
| `ctx verify --knowledge` | `--accept <FINGERPRINT> --id <STABLE-ID>` \| `--reject <FINGERPRINT>`, `--force`, `--author <NAME>` | List or decide pending AI-derived knowledge candidates from `ctx enrich`. Accepting writes an ordinary `.context/*.yaml` document. |
| `ctx verify --knowledge --auto` | `--agent <NAME>`, `--model <NAME>`, `--id-prefix <PREFIX>` (required) | Have a review agent decide every pending knowledge candidate in bulk instead of a human pressing accept/reject by hand. Every resulting document is recorded as agent-decided (`ctx explain` renders it as "Auto-verified", never as a human review). |

## Federation

See [docs/federation.md](federation.md) for the full workflow, including what `[service]` config it requires.

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx registry add <path>` | `--name <NAME>` | Register a neighboring local Git checkout by filesystem path. |
| `ctx registry list` | | List registered neighbors. |
| `ctx registry remove <name>` | | Unregister a neighbor by service name. |
| `ctx export` | `--out <PATH>` (default `.ctx/export.json`) | Write this repository's public documents and HTTP endpoints as a commit-labelled manifest. Requires `[service].name` and an index current with `HEAD`. |
| `ctx sync` | | Re-export and pull every registered neighbor's manifest, resolve local outbound calls against neighbor endpoints as `FEDERATED_MATCH` records, and report unresolved calls. Continues past a failing neighbor instead of aborting. |
| `ctx federation list` | | Show every neighbor's last sync time, source commit, and whether it's gone stale since. |
| `ctx federation show <name>` | | Show one neighbor's imported documents, endpoints, call resolutions, and unresolved calls in full. |
| `ctx trace <target>` | `-v`/`--verbose` | Trace one HTTP endpoint's request sequence (handler, data reads/writes, outbound calls), crossing into a synchronized neighbor's own sequence wherever a call resolves via `FEDERATED_MATCH`. Bounded and deterministic; never fetches, indexes, or syncs — run `ctx sync` first. `--verbose` attaches each hop's own mapped Features/Requirements. |

## Serving

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx serve --mcp` | | Serve the read-only MCP tools over stdio. See [docs/mcp.md](mcp.md). |

## Output conventions

- `impact`/`context` JSON separates `data_contracts` (database entities and HTTP endpoints) from `implementation` and `tests`.
- `ctx review` reports three independent streams: `findings` (proven product-requirement impact), `schema_findings` (deterministic database schema changes), and `api_findings` (deterministic HTTP contract changes). None is a subset of another, and an empty advisory `related_intents`/`related_tests` list on a schema or API finding means no mapping is known — not that the change is unrelated to the product.
- Review deliberately favors precision over recall: formatting-only changes, renames, and likely refactors are suppressed by default. Pass `-v` to see suppressed-change counts and lower-confidence diagnostics.
