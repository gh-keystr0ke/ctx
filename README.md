# ctx

`ctx` is a local-first product-context engine for Python and Rust repositories. It connects a small, explicit set of features, requirements, invariants, and decisions to code and tests, then uses those claims to answer impact questions, review diffs, and compile bounded context for coding agents.

The current release is deterministic and works without an LLM or network service. Semantic findings carry their origin, evidence, confidence, validity, and staleness instead of being silently promoted to facts.

## What works

- Git-aware incremental Python and Rust indexing with Tree-sitter
- file, class, struct, enum, trait, module, function, method, test, containment, and call relationships
- evidence-backed database entities plus `READS_FROM`/`WRITES_TO` facts from static SQL in Python and Rust
- YAML or Markdown-front-matter product context under `.context/`
- evidence-backed `impact`, `explain`, and high-precision `review`
- token-budgeted Context Packs
- heuristic relation suggestions with durable accept/reject decisions
- a read-only stdio MCP server exposing the same application use cases
- local SQLite storage; source code is never sent elsewhere

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

Canonical names are normally enough. If two enabled languages produce the same canonical name, use the exact language-qualified stable key in the mapping, such as `symbol:rust:app.run:Function` or `symbol:python:app.run:Function`; `ctx status`, review JSON, and query output expose these keys.

## Configuration

`.ctx/config.toml` is intentionally small:

```toml
languages = ["python", "rust"]

[paths]
include = ["src", "tests"]
exclude = ["generated", "vendor", "build", "dist", "target", ".venv"]
```

`languages` enables any subset of the built-in `python` and `rust` modules. The legacy singular `language = "python"` form remains accepted; do not set both forms. Unsupported or empty language sets fail during repository discovery instead of silently skipping code.

Include and exclude entries are repository-relative directory prefixes. Exclusions win. Generated, vendor, build, virtual-environment, cache, and non-configured source paths are also protected by built-in filtering. Commit the config when a team should share it. Changing languages or path boundaries is reconciled against the stored snapshot on the next index.

The database lives at `.ctx/ctx.db`. `ctx init` adds only the database, WAL, and shared-memory filenames to the repository-local Git exclude file; it does not edit the shared `.gitignore`.

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
| `ctx verify` | List or interactively decide heuristic semantic candidates |
| `ctx serve --mcp` | Serve the read-only MCP tools over stdio |

Add global `--json` for stable machine-readable output. Add `-v` to review for lower-confidence diagnostics and suppressed-change counts. Script verification decisions with `ctx verify --accept <fingerprint> --author <name>` or `--reject`.

`ctx status` is a health report, not just a graph-size counter. It compares the indexed commit with `HEAD`, shows the effective source scope, separates structural facts from assertions and inferences, counts each product-context type, reports dirty inputs/stale/rejected claims, and suggests the next action. A current structural graph without product documents is reported as `needs product context`, not `ready`.

Impact JSON separates `data_contracts` from implementation and tests. Database entities use their normalized SQL identifier (for example `subscriptions` or `billing.subscriptions`) and can be queried or explained like any other node. Static data facts retain parser provenance, commit validity, and source-line evidence.

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

The end-to-end tests build temporary real Git repositories. They cover the complete subscriptions product journey and a mixed Python/Rust repository through initialization, indexing, language-scoped call resolution, status, and Rust diff review.

Run the deterministic product-quality corpus separately:

```bash
cargo run --locked -p ctx-eval
```

It currently covers 11 Git-history cases and 59 typed checks across recall, precision/noise, classification, and Context Pack budgets, including changed DB writes. This is a reproducible regression baseline, not a statistically significant product study. See [docs/evaluation.md](docs/evaluation.md) for the case matrix, current result, and the human/agent experiments that still require real participants or historical PR ground truth.

### Add another language module

Language support is isolated behind `AnalyzerModule` and the normalized `FileAnalysis` IR. To add TypeScript, Go, Java, or Zig:

1. Add one parser adapter that implements `LanguageAnalyzer` and `AnalyzerModule`, including its language name and extensions.
2. Declare the language in `language.rs` and register its constructor in `AnalyzerRegistry::builtins`.
3. Normalize definitions, ranges, signatures, body/structure fingerprints, calls, and any supported static interactions into the existing IR; never expose parser nodes above the adapter crate. Bump the module's analysis version whenever those semantics change so existing repositories are safely reparsed.
4. Add parser-unit coverage plus a mixed-language executable test before enabling it in the default config.

The registry rejects duplicate language names and extension ownership. Indexing, review, CLI, MCP, persistence, and graph algorithms require no language-specific branch.

See [docs/architecture.md](docs/architecture.md) for boundaries and persistence semantics. The detailed product and engineering source specifications are in [product_conclu.md](product_conclu.md) and [eng_conclu.md](eng_conclu.md).

## Current limits

- Python and Rust are the built-in parsers; TypeScript, Go, Java, and Zig modules are not implemented yet.
- Language modules are compiled into the binary; dynamic shared-library loading is not supported.
- Explicit symbol mappings are exact; unresolved mappings are reported instead of guessed.
- Static database extraction recognizes literal SQL inside known Python/Rust execution calls and common `SELECT`/`INSERT`/`UPDATE`/`DELETE`/`MERGE` forms. Dynamic SQL, ORM expression trees, stored procedures, and dialect-complete parsing remain unknown rather than guessed.
- Heuristic suggestions use lexical, structural, test, and shared-database-interaction signals, not embeddings or an LLM.
- Endpoint, event, and external-system node types are reserved in the domain model but are not yet extracted from source.
- There is no web UI, cloud backend, runtime tracing, multi-repository graph, or external ticket/document integration.
- Review is a conservative aid, not a proof that behavior is correct.

## License

Apache-2.0. See [LICENSE](LICENSE).
