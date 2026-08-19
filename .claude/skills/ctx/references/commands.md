# Full CLI reference

Every command accepts two global flags: `--json` for stable machine-readable output, and `-v`/`-vv` (repeatable) for more verbose diagnostics. Both must appear **before** the subcommand (`ctx --json impact ...`).

## Setup and indexing

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx init` | | Create `.ctx/config.toml`, `.context/{features,requirements,invariants,decisions}/`, and local SQLite storage. Adds `.ctx/ctx.db*`, `.ctx/registry.toml`, and `.ctx/export.json` to the repository-local Git exclude file (not the shared `.gitignore`). Safe to re-run. |
| `ctx index` | | Incrementally index the current commit's configured source files and `.context` documents. Only committed content is read. Refuses to run over uncommitted changes to indexed inputs. Run again after every commit you want reflected in impact/explain/review/status. |
| `ctx status` | | Health report. See "Status fields" below. |

### Status fields

`ctx status --json` returns:

- `index_state`: `"not_indexed"` | `"behind"` | `"current"`.
- `health`: `"ready"` | `"needs_index"` (index_state isn't current) | `"needs_context"` (no product documents indexed yet) | `"needs_mappings"` (an active document has no implementation/test link) | `"needs_attention"` (stale claims or schema divergences exist).
- `source_scope`: effective configured languages/include/exclude.
- `uncommitted_index_inputs`: files that differ from HEAD among configured index inputs — a nonzero list means the stored graph still describes the last committed state, not your working tree.
- `knowledge`: node/edge counts — files, symbols, db_entities, features, requirements, invariants, decisions, public_documents, active_edges, structural_facts, active_assertions, active_inferences, stale_semantic_edges, rejected_semantic_edges.
- `schema_divergences`: best-effort ORM-vs-migration-history mismatches.
- `unmapped_intents`: active Feature/Requirement/Invariant/Decision IDs with no implementation/test mapping.
- `stale_claims`: every stale semantic relationship as a `"source -> target"` string, directly usable with `ctx explain`.
- `notices` / `suggested_actions`: human-readable diagnosis and the exact next command to run — prefer these over guessing what's wrong.

## Query

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx impact <target>` | | Bounded product + implementation + test impact for a file path, canonical symbol, stable ID (`REQ-...`), or `table.column`. |
| `ctx explain <target>` | `--trace` | Full stored claims and evidence for a node ID, or a quoted `"source -> target"` relationship. `--trace` additionally traces every HTTP endpoint reachable from the target's own mapped implementation, shown as a separate `Traces:` section — same bounds and federation-crossing as `ctx trace`, gated the same way by `--verbose`. |
| `ctx find <name>` | | Discover indexed symbols/nodes by short or exact name. Several matches across namespaces are returned independently, never merged or treated as an error. |

## Review

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx review` | `--base <REV>` (default `HEAD`), `-v` | Review the working diff (or a branch's diff against `--base`) for product-contract, schema, and API-contract impact. Three independent output streams: `findings`, `schema_findings`, `api_findings`. |

## Context compilation

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx context <task>` | `--file <PATH>` (repeatable), `--symbol <NAME>` (repeatable), `--token-budget <N>` (default 4000) | Compile a bounded, evidence-backed Context Pack for a coding task. An explicit `--file`/`--symbol` seed is a hard scope boundary; lexical auto-seeding only runs when nothing explicit resolves. |

## Mining existing knowledge

Full workflow and philosophy: `onboarding.md` § mining.

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx ingest <source>` | `--since <OID>` | Normalize external artifacts into their own store. `source` is `git` (commit messages/branch names), `code-comments` (comments/docstrings attributed to nearest symbol), or `gitlab` (issues/MRs/comments — needs `[gitlab]` in `.ctx/config.toml` and `CTX_GITLAB_TOKEN`). |
| `ctx enrich` | `--agent <claude\|codex\|antigravity>` (default `claude`), `--model <NAME>`, `--allow-ungrounded-symbols` | Ask an AI agent CLI already on `PATH` to propose typed knowledge candidates from ingested artifacts, one bounded neighborhood at a time. Always produces `pending` candidates, never asserted facts. |
| `ctx verify` | `--accept <FINGERPRINT>` \| `--reject <FINGERPRINT>`, `--author <NAME>` (default `local-user`) | List or decide heuristic implementation-link candidates (deterministic relation suggestions from indexing — not AI-derived). |
| `ctx verify --knowledge` | `--accept <FINGERPRINT> --id <STABLE-ID>` \| `--reject <FINGERPRINT>`, `--force`, `--author <NAME>` | List or decide pending AI-derived knowledge candidates from `ctx enrich`. Accepting writes an ordinary `.context/*.yaml` document. `--force` overrides the "looks like a restatement of an already-active document" refusal. |
| `ctx verify --knowledge --auto` | `--agent <NAME>`, `--model <NAME>`, `--id-prefix <PREFIX>` (required) | Have a review agent decide every pending knowledge candidate in bulk. Clusters related candidates first; can merge a cluster into one document. Every resulting decision is recorded as agent-made — `ctx explain` renders it "Auto-verified", never as human review. |
| `ctx verify --stale` | `--agent <NAME>`, `--model <NAME>`, `--author <NAME>` | Re-review every currently stale semantic claim through an independent agent. `accept` is binding (reactivates precisely that relationship); `reject` is never applied automatically — only ever printed as a suggestion for a human. |

## Federation (multiple repositories)

Full workflow: `federation.md`.

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx registry add <path>` | `--name <NAME>` | Register a neighboring local Git checkout by filesystem path. |
| `ctx registry list` | | List registered neighbors. |
| `ctx registry remove <name>` | | Unregister a neighbor by service name. |
| `ctx export` | `--out <PATH>` (default `.ctx/export.json`) | Write this repository's public documents and HTTP endpoints as a commit-labelled manifest. Requires `[service].name` and an index current with `HEAD`. |
| `ctx sync` | | Re-export and pull every registered neighbor's manifest, resolve local outbound calls against neighbor endpoints as `FEDERATED_MATCH`, report unresolved calls. Continues past a failing neighbor instead of aborting. |
| `ctx federation list` | | Every neighbor's last sync time, source commit, and whether it's gone stale since. |
| `ctx federation show <name>` | | One neighbor's imported documents, endpoints, call resolutions, and unresolved calls in full. |
| `ctx trace <target>` | `-v`/`--verbose` | Trace one HTTP endpoint's request sequence (handler, data reads/writes, outbound calls), crossing into a synchronized neighbor's own sequence wherever a call resolves via `FEDERATED_MATCH`. Bounded and deterministic; never fetches/indexes/syncs — run `ctx sync` first. `--verbose` attaches each hop's own mapped Features/Requirements. |

## Serving

| Command | Flags | Purpose |
| --- | --- | --- |
| `ctx serve --mcp` | | Serve the read-only MCP tools over stdio. |

## Output conventions

- `impact`/`context` JSON separates `data_contracts` (database entities and HTTP endpoints) from `implementation` and `tests`.
- `ctx review` reports three independent streams: `findings` (proven product-requirement impact), `schema_findings` (deterministic database schema changes), and `api_findings` (deterministic HTTP contract changes). None is a subset of another.
- Review deliberately favors precision over recall: formatting-only changes, renames, and likely refactors are suppressed by default. Pass `-v` to see suppressed-change counts and lower-confidence diagnostics.
- Environment variables: `CTX_GITLAB_TOKEN` (GitLab API access, `ctx ingest gitlab` only), `CTX_CLAUDE_CLI_BINARY`/`CTX_CODEX_CLI_BINARY`/`CTX_ANTIGRAVITY_CLI_BINARY` (override the agent CLI path for `enrich`/`verify --auto`/`verify --stale`), `CTX_FEDERATION_BINARY` (override which `ctx` executable `ctx sync` invokes against a neighbor). No token or API key for `claude`/`codex`/`antigravity` is read by `ctx` itself — each agent CLI handles its own authentication.
