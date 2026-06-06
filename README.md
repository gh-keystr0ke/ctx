# ctx

`ctx` is a local-first product-context engine for Python repositories. It connects a small, explicit set of features, requirements, invariants, and decisions to code and tests, then uses those claims to answer impact questions, review diffs, and compile bounded context for coding agents.

The current release is deterministic and works without an LLM or network service. Semantic findings carry their origin, evidence, confidence, validity, and staleness instead of being silently promoted to facts.

## What works

- Git-aware incremental Python indexing with Tree-sitter
- file, class, function, method, test, containment, and call relationships
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
ctx explain REQ-SUB-014
ctx context "preserve paid access during subscription cancellation" \
  --symbol billing.subscription.SubscriptionService.cancel \
  --token-budget 1200
```

`ctx index` only accepts committed Python and `.context` inputs so every indexed version has an honest Git validity boundary. After editing code, review the working diff before committing:

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

A requirement can link exact canonical Python symbols to intent and tests:

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

For files below `src/`, canonical symbols omit that prefix. For example, `src/billing/subscription.py` plus `class SubscriptionService` and `def cancel` becomes `billing.subscription.SubscriptionService.cancel`.

## Configuration

`.ctx/config.toml` is intentionally small:

```toml
language = "python"

[paths]
include = ["src", "tests"]
exclude = ["generated", "vendor", "build", "dist", "target", ".venv"]
```

Include and exclude entries are repository-relative directory prefixes. Exclusions win. Generated, vendor, build, virtual-environment, cache, and non-Python paths are also protected by built-in filtering. Commit the config when a team should share it.

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

The end-to-end test builds a temporary real Git repository from `fixtures/subscriptions` and covers initialization, indexing, impact, Context Pack compilation, dirty-input rejection, and precise review findings.

See [docs/architecture.md](docs/architecture.md) for boundaries and persistence semantics. The detailed product and engineering source specifications are in [product_conclu.md](product_conclu.md) and [eng_conclu.md](eng_conclu.md).

## Current limits

- Python is the only parser and source model in this release.
- Explicit symbol mappings are exact; unresolved mappings are reported instead of guessed.
- Heuristic suggestions use lexical/structural/test signals, not embeddings or an LLM.
- There is no web UI, cloud backend, runtime tracing, multi-repository graph, or external ticket/document integration.
- Review is a conservative aid, not a proof that behavior is correct.

## License

Apache-2.0. See [LICENSE](LICENSE).
