# ctx

`ctx` is a local-first product-context engine for Python, Rust, and Go repositories. It connects a small, explicit set of features, requirements, invariants, and decisions to code and tests, then uses those claims to answer impact questions, review diffs, and compile bounded context for coding agents.

The core — indexing, impact, explain, review, Context Packs — is deterministic and works without an LLM or network service. External knowledge ingestion and AI-assisted candidate extraction are optional, additive layers on top: `ctx ingest`/`ctx enrich` reach out to GitLab or a locally installed agent CLI only when explicitly run, and an agent's output is never more than a `pending` inference until a human accepts it through `ctx verify`. Every semantic finding — deterministic or AI-derived — carries its origin, evidence, confidence, validity, and staleness instead of being silently promoted to a fact.

## What works

- Git-aware incremental Python, Rust, and Go indexing with Tree-sitter
- file, class, struct, enum, interface, trait, module, function, method, test, containment, and call relationships
- evidence-backed database entities plus `READS_FROM`/`WRITES_TO` facts from static SQL in Python, Rust, and Go, including specific column names when the SQL form reliably names them (an `UPDATE ... SET` clause, an `INSERT` column list)
- table/column-level `DEFINES_SCHEMA` facts read from goose SQL migrations and SQLAlchemy declarative models, sharing the same `DbEntity` graph, with nullable/primary-key/foreign-key/unique/default constraint detail and table create/drop/rename, column add/drop/rename/alter, and index add/drop operations
- schema-aware `ctx review`: a new migration or an edited SQLAlchemy model that drops/renames a column, tightens nullability, changes a type, or changes a foreign key/unique constraint is a deterministic, clearly-labeled schema finding — kept structurally separate from proven requirement-impact findings, with a bounded advisory link to the requirements/invariants/tests the affected table's own readers/writers are mapped to
- best-effort `SQLAlchemy`/goose schema reconciliation surfaced in `ctx status` (a column the ORM expects with no migration declaring it, or vice versa)
- `ctx impact table.column` seeds resolve to the table and narrow to that column's specific readers/writers
- YAML or Markdown-front-matter product context under `.context/`
- evidence-backed `impact`, `explain`, and high-precision `review`
- token-budgeted Context Packs
- heuristic relation suggestions with durable accept/reject decisions
- external development-artifact ingestion — Git commit messages and branch names, code comments/docstrings, and GitLab issues/merge requests with their comments — normalized into their own store, idempotently re-synced, and deterministically linked to already-indexed code and to each other, never automatically promoted to product knowledge
- an interchangeable AI-agent boundary (`ctx enrich --agent claude|codex|antigravity`) that proposes typed Feature/Requirement/Invariant/Decision candidates from one bounded artifact neighborhood at a time, grounded only in evidence the agent actually cited; malformed output or evidence outside that neighborhood is rejected, never guessed at, and a candidate stays an inference until a human accepts it
- `ctx verify --knowledge` to accept or reject AI-derived candidates, with basic duplicate detection against already-active documents and the full artifact → agent-inference → human-verification chain surfaced by `ctx explain`
- a read-only stdio MCP server exposing the same application use cases
- local SQLite storage; source code is never sent elsewhere — an AI agent only ever sees the one bounded artifact neighborhood it is asked about, and only when `ctx enrich` is run explicitly

## Install

Rust 1.85 or newer and Git are required. The container build pins Rust 1.97.1.

```bash
cargo install --locked --path crates/ctx-cli
```

This installs `ctx`. The workspace also contains a standalone `ctx-mcp` binary, although `ctx serve --mcp` is sufficient for normal use:

```bash
cargo install --locked --path crates/ctx-mcp
```

## Quick start

Run these commands from a Git repository:

```bash
ctx init
# Add a few product-context documents under .context/.
git add .ctx/config.toml .context
git commit -m "docs: add product context"
ctx index

ctx impact billing.subscription.SubscriptionService.cancel
ctx explain "billing.subscription.SubscriptionService.cancel -> subscriptions"
ctx explain REQ-SUB-014
ctx context "preserve paid access during subscription cancellation" \
  --symbol billing.subscription.SubscriptionService.cancel \
  --token-budget 1200
```

`ctx index` only accepts committed configured sources and `.context` inputs so every indexed version has an honest Git validity boundary. After editing code, review the working diff before committing:

```bash
ctx review --base HEAD
```

For a committed feature branch, use its merge base or target branch instead:

```bash
ctx review --base main
```

After committing an accepted change, run `ctx index` again. Changed implementation bodies mark affected semantic claims stale until they are reviewed or re-established.

## Mine existing knowledge instead of writing it by hand

If a team already has commit history, code comments, or a GitLab project full of issues and merge requests, `ctx` can propose product-context documents from that instead of requiring everything to be authored from scratch:

```bash
ctx ingest git             # commit messages and branch names
ctx ingest code-comments   # comments and docstrings, attributed to their nearest symbol
ctx ingest gitlab          # issues, merge requests, and their comments — see Configuration

ctx enrich --agent claude  # or --agent codex / --agent antigravity

ctx verify --knowledge     # review each proposed candidate; accept allocates its stable ID
```

Ingested artifacts are never product knowledge on their own — they are source material an agent may derive typed candidates from, and a candidate is never asserted until `ctx verify --knowledge --accept --id <ID>` names it. Accepting writes an ordinary `.context/*.yaml` file; the next `ctx index` absorbs it exactly like a hand-authored one. Re-running `ctx enrich` skips an artifact whose content hasn't changed since its last analysis, and `ctx verify --knowledge --accept` refuses (unless `--force`) a statement that looks like a restatement of an already-active document, naming which one.

## Author product context

`ctx init` creates directories for four deliberately small document types:

```text
.context/
├── features/
├── requirements/
├── invariants/
└── decisions/
```

A requirement can link exact canonical symbols to intent and tests:

```yaml
id: REQ-SUB-014
type: requirement
feature: FEAT-SUBSCRIPTIONS
status: active
statement: When a paid user cancels, access must remain active until paid_until.
implementation:
  - symbol: billing.subscription.SubscriptionService.cancel
tests:
  - symbol: tests.test_subscription.test_cancel_keeps_access_until_paid_until
```

The other required fields are:

| Type | Required fields | Optional relationships |
| --- | --- | --- |
| `feature` | `id`, `name`; `description` is recommended | `implementation`, `tests` |
| `requirement` | `id`, `statement` | `feature`, `implementation`, `tests` |
| `invariant` | `id`, `statement` | `feature`, `implementation`, `tests` |
| `decision` | `id`, `title`, `decision` | `feature`, `implementation`, `tests` |

`status` defaults to `active`. IDs must be unique across `.context`. A link may be either `{ symbol: canonical.name }` or a plain canonical-name string. Markdown files are accepted when their metadata is YAML front matter delimited by `---`; prose after the closing delimiter is retained as source evidence but fields come from the front matter.

Python files below `src/` omit that prefix: `src/billing/subscription.py` plus `class SubscriptionService` and `def cancel` becomes `billing.subscription.SubscriptionService.cancel`.

Rust paths include a crate namespace. A root `src/lib.rs` uses `crate`, while a workspace file such as `crates/ctx-core/src/indexing.rs` uses the Cargo-directory name: `ctx_core.indexing.plan_incremental_index`. Inherent methods use their implemented type and trait declarations use their trait. Trait implementations include the implemented trait, including type arguments when needed to prevent collisions: `ctx_cli.CliError.From<std::io::Error>.from`.

Go paths use the source directory as the package path (matching Go's one-package-per-directory convention), not the file name and not `go.mod`'s module path: `billing/subscription.go` with `func (s *SubscriptionService) Cancel` becomes `billing.SubscriptionService.Cancel`. A root-level file with no directory uses `main`. Interfaces are indexed as traits.

Canonical names are normally enough. If two enabled languages produce the same canonical name, use the exact language-qualified stable key in the mapping, such as `symbol:rust:app.run:Function` or `symbol:python:app.run:Function`; `ctx status`, review JSON, and query output expose these keys.

## Configuration

`.ctx/config.toml` is intentionally small:

```toml
languages = ["python", "rust", "go"]

[paths]
include = ["src", "tests"]
exclude = ["generated", "vendor", "build", "dist", "target", ".venv"]
```

`languages` enables any subset of the built-in `python`, `rust`, `go`, and `goose` modules. `goose` reads goose SQL migration files (`.sql`) instead of a programming language; add it, and a directory such as `migrations`, to `paths.include`, to pick up schema declarations. The legacy singular `language = "python"` form remains accepted; do not set both forms. Unsupported or empty language sets fail during repository discovery instead of silently skipping code.

Include and exclude entries are repository-relative directory prefixes. Exclusions win. Generated, vendor, build, virtual-environment, cache, and non-configured source paths are also protected by built-in filtering. Commit the config when a team should share it. Changing languages or path boundaries is reconciled against the stored snapshot on the next index.

The database lives at `.ctx/ctx.db`. `ctx init` adds only the database, WAL, and shared-memory filenames to the repository-local Git exclude file; it does not edit the shared `.gitignore`.

`ctx ingest gitlab` needs a `[gitlab]` table naming the project (`base_url` defaults to `https://gitlab.com/api/v4`):

```toml
[gitlab]
project = "billing/subscriptions"
# base_url = "https://gitlab.example.com/api/v4"  # self-managed instances
```

The access token comes only from the `CTX_GITLAB_TOKEN` environment variable — never from a committed file, so it can never end up in `.ctx/config.toml` by accident. `ctx ingest gitlab` stores a per-project sync cursor and asks GitLab for only what changed since the previous run.

`ctx enrich --agent claude|codex|antigravity` shells out to that agent's own CLI (`claude`, `codex`, `agy`) already on `PATH`; each is independently overridable for testing or an alternate install location via `CTX_CLAUDE_CLI_BINARY`, `CTX_CODEX_CLI_BINARY`, or `CTX_ANTIGRAVITY_CLI_BINARY`. No token or API key is read from `ctx` itself — each CLI handles its own authentication.

## Command reference

| Command | Purpose |
| --- | --- |
| `ctx init` | Create config, context directories, and local SQLite storage |
| `ctx index` | Incrementally index committed code and synchronize product context |
| `ctx status` | Report indexed commit, files, symbols, active claims, and stale claims |
| `ctx impact <target>` | Traverse a bounded typed neighborhood from a file, symbol, or intent ID |
| `ctx explain <target>` | Show stored claims and evidence for an ID or quoted `source -> target` pair |
| `ctx review [--base REV]` | Review a branch or working diff against strong product contracts |
| `ctx context <task>` | Compile a bounded Context Pack; accepts repeated `--file` and `--symbol` seeds |
| `ctx find <name>` | Discover indexed symbols/nodes by short or exact name; several matches are never an error |
| `ctx ingest <source>` | Ingest external artifacts (`git`, `code-comments`, `gitlab`) as normalized, separately stored source material |
| `ctx enrich [--agent NAME]` | Analyze ingested artifacts with an AI agent (`claude`, `codex`, `antigravity`) for candidate product knowledge |
| `ctx verify [--knowledge]` | List or interactively decide heuristic semantic candidates, or (`--knowledge`) AI-derived knowledge candidates |
| `ctx serve --mcp` | Serve the read-only MCP tools over stdio |

Add global `--json` for stable machine-readable output. Add `-v` to review for lower-confidence diagnostics and suppressed-change counts. Script verification decisions with `ctx verify --accept <fingerprint> --author <name>` or `--reject`; script knowledge-candidate decisions with `ctx verify --knowledge --accept <fingerprint> --id <STABLE-ID> --author <name>` (add `--force` to accept a likely restatement of an existing document anyway) or `--knowledge --reject`.

`ctx status` is a health report, not just a graph-size counter. It compares the indexed commit with `HEAD`, shows the effective source scope, separates structural facts from assertions and inferences, counts each product-context type, reports dirty inputs/stale/rejected claims, best-effort `SQLAlchemy`/goose schema divergences, and suggests the next action. A current structural graph without product documents is reported as `needs product context`, not `ready`.

Impact JSON separates `data_contracts` from implementation and tests. Database entities use their normalized SQL identifier (for example `subscriptions` or `billing.subscriptions`) and can be queried or explained like any other node. Static data facts retain parser provenance, commit validity, and source-line evidence. A `table.column` query (for example `subscriptions.paid_until`) resolves to the table and narrows `implementation` to that column's specific readers/writers; an unrecognized column falls back to table-level impact plus an explicit uncertainty rather than silently looking like "no readers".

Review's schema findings appear separately from `findings` as `schema_findings`: each one is a deterministic, described schema change (`subscriptions.status dropped`, `subscriptions.amount type changed from INTEGER to NUMERIC(10, 2)`, ...) marked destructive or informational, plus a bounded advisory list of the requirements/invariants/tests the affected table's directly-connected code is mapped to — an empty list means no known product mapping was found, not that the change is unrelated to the product.

For Context Packs, a resolved `--file` or `--symbol` is a hard scope boundary: related context comes from that seed's bounded graph neighborhood, and independent lexical matches are not added as competing roots. Lexical auto-seeding is used only when no explicit seed resolves.

Review deliberately favors precision over recall. Formatting-only changes, renames, and likely refactors are suppressed. Findings require a strong implementation claim and always include the affected intent, stored evidence, linked tests, uncertainty, and a reviewer action.

## MCP integration

Initialize and index the repository before starting the server. Configure a local MCP client with an absolute executable and repository working directory:

```json
{
  "mcpServers": {
    "ctx": {
      "command": "/absolute/path/to/ctx",
      "args": ["serve", "--mcp"],
      "cwd": "/absolute/path/to/repository"
    }
  }
}
```

The server exposes exactly five tools: `get_context`, `get_impact`, `explain_relation`, `find_requirements`, and `review_change`. It supports current discovery and legacy initialization clients over newline-delimited stdio JSON-RPC. All graph and review decisions remain in `ctx-app`; MCP is a transport adapter.

## Docker

Build the image and run against the current repository as your host user:

```bash
docker build -t ctx:local .
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/workspace" ctx:local init
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/workspace" ctx:local index
```

Compose provides the same workflow:

```bash
export CTX_REPOSITORY="$PWD" CTX_UID="$(id -u)" CTX_GID="$(id -g)"
docker compose run --rm ctx status
```

The optional `mcp` Compose profile starts `ctx serve --mcp` with stdin attached.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

The end-to-end tests build temporary real Git repositories. They cover the complete subscriptions product journey and a mixed Python/Rust/Go repository through initialization, indexing, language-scoped call resolution, status, and Rust/Go diff review.

Run the deterministic product-quality corpus separately:

```bash
cargo run --locked -p ctx-eval
```

It currently covers 25 Git-history cases and 102 typed checks across recall, precision/noise, classification, and Context Pack budgets, including changed DB writes and the full schema-aware review/reconciliation/impact scenario set. This is a reproducible regression baseline, not a statistically significant product study. See [docs/evaluation.md](docs/evaluation.md) for the case matrix, current result, and the human/agent experiments that still require real participants or historical PR ground truth.

### Add another language module

Language support is isolated behind `AnalyzerModule` and the normalized `FileAnalysis` IR. To add TypeScript, Java, or Zig:

1. Add one parser adapter that implements `LanguageAnalyzer` and `AnalyzerModule`, including its language name and extensions.
2. Declare the language in `language.rs` and register its constructor in `AnalyzerRegistry::builtins`.
3. Normalize definitions, ranges, signatures, body/structure fingerprints, calls, and any supported static interactions into the existing IR; never expose parser nodes above the adapter crate. Bump the module's analysis version whenever those semantics change so existing repositories are safely reparsed.
4. Add parser-unit coverage plus a mixed-language executable test before enabling it in the default config.

The registry rejects duplicate language names and extension ownership. Indexing, review, CLI, MCP, persistence, and graph algorithms require no language-specific branch.

See [docs/architecture.md](docs/architecture.md) for boundaries and persistence semantics. The detailed product and engineering source specifications are in [product_conclu.md](product_conclu.md) and [eng_conclu.md](eng_conclu.md).

## Current limits

- Python, Rust, and Go are the built-in parsers; TypeScript, Java, and Zig modules are not implemented yet.
- Language modules are compiled into the binary; dynamic shared-library loading is not supported.
- Explicit symbol mappings are exact; unresolved mappings are reported instead of guessed.
- Static database extraction recognizes literal SQL inside known Python/Rust/Go execution calls and common `SELECT`/`INSERT`/`UPDATE`/`DELETE`/`MERGE` forms. Dynamic SQL, ORM expression trees, stored procedures, and dialect-complete parsing remain unknown rather than guessed. Column-level evidence is only extracted for writes (`UPDATE ... SET`, an `INSERT`/`MERGE` explicit column list); `DELETE`, a bare `INSERT ... VALUES` with no column list, and every `SELECT`/read form stay table-level, since attributing `SELECT` columns across joins without a real parser is guessing, not recognizing.
- goose migration parsing reads only `-- +goose Up` and recognizes `CREATE TABLE` (including table-level `PRIMARY KEY`/`UNIQUE`/`FOREIGN KEY`/`CHECK`), `ALTER TABLE ... ADD/DROP/RENAME COLUMN`, `ALTER TABLE ... RENAME TO`, `ALTER TABLE ... ALTER COLUMN ... TYPE/SET-DROP NOT NULL/SET-DROP DEFAULT`, `DROP TABLE`, and `CREATE/DROP INDEX`. `ALTER TABLE ... ADD/DROP CONSTRAINT` is deliberately unsupported (a bare constraint name cannot be resolved to columns without the table's already-declared column list). It is a deterministic recognizer, not a SQL dialect parser, and never merges multiple migrations into one computed "current" schema for storage — each migration file's declaration stays its own fact; a best-effort ordered replay exists only as a diagnostic for `SQLAlchemy` reconciliation, never as a stored fact.
- SQLAlchemy model recognition requires a static `__tablename__` string literal and reads `Column(...)`/`mapped_column(...)` attribute assignments, including `nullable=`/`primary_key=`/`unique=`/`default=`/`server_default=`/`ForeignKey(...)`; it does not resolve `Base`/inheritance, relationships, mixins, `Index(...)`/`__table_args__`, or Alembic migration history.
- Schema-aware review compares a schema-declaring file's diff or a migration's own declared operations; it does not diff an ORM model against its own migration history over time (that is `ctx status`'s reconciliation, which is presence-only and does not compare types/nullability between sources).
- Heuristic suggestions use lexical, structural, test, and shared-database-interaction signals, not embeddings or an LLM.
- Endpoint, event, and external-system node types are reserved in the domain model but are not yet extracted from source.
- There is no web UI, cloud backend, runtime tracing, or multi-repository graph.
- GitLab is the only ticket/review-system integration; GitHub and Jira are not implemented. GitLab sync is incremental for issues/merge requests via a stored per-project cursor, but each returned issue/MR's comments are always fetched in full, not incrementally.
- `ctx enrich` requires a real, already-authenticated `claude`, `codex`, or `agy` CLI on `PATH`; there is no direct API-key/HTTP integration with any model provider, and no local/offline model support.
- An AI-derived candidate is always `INFERENCE`, never asserted automatically: `ctx enrich` only ever produces a `pending` candidate, and only a human `ctx verify --knowledge --accept` turns one into a real `.context/*.yaml` document. Duplicate detection against existing documents is lexical term-overlap, not semantic similarity or an embedding model.
- Review is a conservative aid, not a proof that behavior is correct.

## License

Apache-2.0. See [LICENSE](LICENSE).
