# ctx

`ctx` is a local-first product-context engine for Python, Rust, and Go repositories. It connects a small, explicit set of features, requirements, invariants, and decisions to code and tests, then uses those claims to answer impact questions, review diffs, and compile bounded context for coding agents.

The core — indexing, impact, explain, review, Context Packs — is deterministic and works without an LLM or network service. External knowledge ingestion and AI-assisted candidate extraction are optional, additive layers on top: `ctx ingest gitlab`/`ctx ingest jira` and the locally installed agent CLIs only reach a network when explicitly run, and an agent's output is never more than a `pending` inference until a human (or an explicitly configured `--auto` review agent) accepts it. Every semantic finding — deterministic or AI-derived — carries its origin, evidence, confidence, validity, and staleness instead of being silently promoted to a fact.

## What ctx does

| | |
| --- | --- |
| **Index** | Git-aware incremental Python/Rust/Go indexing (Tree-sitter) of files, symbols, calls, tests, database reads/writes, schema declarations, and HTTP contracts — plus language-neutral HTTP contracts auto-discovered from OpenAPI 3.0/3.1 specs. |
| **Impact & explain** | Bounded, evidence-backed traversal from a file, symbol, stable ID, or `table.column` to the product intent, code, and tests around it. |
| **Review** | High-precision diff review across three independent streams — product-requirement impact, database schema changes, and HTTP contract changes — each with linked evidence and a reviewer action. See [docs/architecture.md](docs/architecture.md). |
| **Context Packs** | Token-budgeted, evidence-backed context for a coding task, from the CLI or over MCP. |
| **Mine existing knowledge** | Propose product-context documents from Git history, code comments, GitLab, or referenced Jira Cloud issues instead of writing everything by hand. See [docs/mining-knowledge.md](docs/mining-knowledge.md). |
| **Federation** | Share public product docs and HTTP contracts with sibling repositories checked out locally. See [docs/federation.md](docs/federation.md). |

Full extraction scope and honest boundaries: [docs/limits.md](docs/limits.md). Local SQLite storage; source code is never sent elsewhere — see [docs/architecture.md](docs/architecture.md) for the explicitly invoked commands that can use network-backed adapters.

## Install

macOS (Apple Silicon or Intel) or Linux x86_64: download the latest release, verify its checksum, and install to `~/.local/bin`:

```bash
curl -fsSL https://raw.githubusercontent.com/gh-keystr0ke/ctx/main/install.sh | sh
```

The Linux binary is a statically linked `x86_64-unknown-linux-musl` build, so it runs unmodified on Arch, Ubuntu, Fedora, Gentoo, or any other x86_64 distribution. See [install.sh](install.sh) for the `CTX_INSTALL_VERSION`/`CTX_INSTALL_DIR` overrides.

Building from source instead requires Rust 1.88 or newer and Git (the container build pins Rust 1.97.1):

```bash
cargo install --locked --path crates/ctx-cli
```

Either way this installs `ctx`. The workspace also contains a standalone `ctx-mcp` binary, although `ctx serve --mcp` is sufficient for normal use.

## Quick start

Run these commands from a Git repository:

```bash
ctx init
# Add a few product-context documents under .context/ — see "Author product context" below.
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

`ctx index` only accepts committed configured sources and `.context` inputs, so every indexed version has an honest Git validity boundary. After committing a further change, run `ctx index` again — changed implementation bodies mark affected semantic claims stale until they're reviewed or re-established.

## Common workflows

Short answer to "what do I run, and when." Each scenario links to the doc with the full detail.

**Bootstrap a new repository.** `ctx init`, author a handful of Requirements/Invariants under `.context/` for your highest-value flows (see [docs/authoring-context.md](docs/authoring-context.md)), commit, then `ctx index`. You don't need to document everything up front — `ctx status` tells you what's mapped and what isn't as you go.

**Review a change before it merges.** Working tree not yet committed:
```bash
ctx review --base HEAD
```
A committed feature branch, against its target:
```bash
ctx review --base main
```
Add `-v` for lower-confidence diagnostics and suppressed-change counts. See [docs/commands.md](docs/commands.md#review) for what the three finding streams mean.

**What should I actually run before merging this?** `ctx review --related-tests` (or `--related-tests=<N>` to cap it to `N` call-graph hops). Deliberately broader than each finding's own `related_tests`: no product-intent gating, no confidence threshold — every test structurally reachable from a changed symbol, for recall over precision.

**Check blast radius before touching a symbol.** `ctx impact <target>` where `<target>` is a file path, canonical symbol, stable ID, or `table.column`. Not sure of the exact symbol name? `ctx find <name>` first. `ctx explain <id-or-relation>` shows the full evidence behind any one claim.

**Hand a coding agent bounded context.** Either `ctx context "<task>" --symbol ... --file ... --token-budget <N>` from the CLI, or point an MCP-capable client at `ctx serve --mcp` so it can call the same thing itself — see [docs/mcp.md](docs/mcp.md).

**Don't want to hand-author `.context/` from scratch?** Mine it from what already exists:
```bash
ctx ingest git
ctx ingest gitlab --scope business-linked    # if GitLab is configured
ctx ingest jira --scope business-linked      # Jira keys found in Git/selected MRs only
ctx ingest code-comments --reconcile         # remove comments/docstrings gone from HEAD
ctx artifacts prune --scope business-linked  # dry run; add --apply after review
ctx enrich --scope business-linked --agent claude  # or codex / antigravity
ctx verify --knowledge                       # accept/reject each proposed candidate
```
Full workflow, including bulk-reviewing hundreds of candidates with `ctx verify --knowledge --auto`, in [docs/mining-knowledge.md](docs/mining-knowledge.md).

**Database or HTTP contract changed?** Nothing special to run — `ctx review` already reports destructive/routine schema and API-contract changes as their own finding streams whenever the diff touches a migration, an ORM model, or a FastAPI/Flask/`requests`/`httpx` call. See [docs/api-contracts.md](docs/api-contracts.md) and [docs/architecture.md](docs/architecture.md#schema-migrations).

**Multiple services, one team.** Give each repository a `[service].name`, mark the documents and endpoints you're willing to share as `visibility: public`, then:
```bash
ctx registry add ../other-service --name other-service
ctx sync
ctx federation show other-service
```
Full setup and limits in [docs/federation.md](docs/federation.md).

## Author product context

`ctx init` creates four document types under `.context/` (`features/`, `requirements/`, `invariants/`, `decisions/`). A requirement links exact canonical symbols to intent and tests:

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

Required/optional fields per type, Markdown front-matter support, document `visibility`, and exactly how a Python/Rust/Go file and symbol become a canonical path (with worked examples) are in **[docs/authoring-context.md](docs/authoring-context.md)**.

## Configuration

`.ctx/config.toml` is intentionally small:

```toml
languages = ["python", "rust", "go"]

[paths]
include = ["src", "tests"]
exclude = ["generated", "vendor", "build", "dist", "target", ".venv"]
```

`languages` enables any subset of the built-in `python`, `rust`, `go`, and `goose` (SQL migrations, not a language) modules. Commit this file when a team should share it; the SQLite index at `.ctx/ctx.db` stays local and is never committed. Full field reference, environment variables (`CTX_GITLAB_TOKEN` and friends), and exactly what's committed vs. local-only: **[docs/configuration.md](docs/configuration.md)**.

## Command reference

| Command | Purpose |
| --- | --- |
| `ctx init` | Create config, context directories, and local SQLite storage |
| `ctx index` | Incrementally index committed code and synchronize product context |
| `ctx status` | Report indexed commit, files, symbols, active claims, and stale claims |
| `ctx impact <target>` | Traverse a bounded typed neighborhood from a file, symbol, stable ID, or `table.column` |
| `ctx explain <target>` | Show stored claims and evidence for an ID or quoted `source -> target` pair |
| `ctx find <name>` | Discover indexed symbols/nodes by short or exact name |
| `ctx review [--base REV]` | Review a branch or working diff against product, schema, and API contracts |
| `ctx context <task>` | Compile a bounded Context Pack; accepts repeated `--file`/`--symbol` seeds |
| `ctx ingest <source>` | Ingest external artifacts (`git`, `code-comments`, `gitlab`, `jira`) as separately stored source material |
| `ctx artifacts prune [--apply]` | Dry-run or apply removal of artifacts without a deterministic repository→Jira business anchor |
| `ctx enrich [--agent NAME] [--scope business-linked]` | Propose typed knowledge candidates; strict scope sends one Jira-anchored MR/commit/code bundle per agent call |
| `ctx verify [--knowledge] [--auto]` | Decide heuristic or AI-derived candidates, by hand or via a review agent |
| `ctx registry` / `ctx export` / `ctx sync` / `ctx federation` | Share and inspect federated knowledge with sibling repositories |
| `ctx serve --mcp` | Serve the read-only MCP tools over stdio |

Every full flag list, plus what each output stream means, is in **[docs/commands.md](docs/commands.md)**.

## MCP integration

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

Initialize and index the repository first. The server exposes five read-only tools (`get_context`, `get_impact`, `explain_relation`, `find_requirements`, `review_change`) mirroring the CLI commands above. Details: **[docs/mcp.md](docs/mcp.md)**.

## Docker

```bash
docker build -t ctx:local .
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/workspace" ctx:local index
```

Compose workflow and the MCP container profile: **[docs/docker.md](docs/docker.md)**.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo run --locked -p ctx-eval   # deterministic product-quality corpus
```

Adding a new language module and the full test/eval breakdown: **[docs/development.md](docs/development.md)**. Architecture and persistence boundaries, distilled from the original product and engineering specs: **[docs/architecture.md](docs/architecture.md)**.

## Current limits

`ctx` prefers no fact over a guessed one — every extraction boundary described above is deliberate. The complete, organized list (languages, database/schema, HTTP contracts, federation, AI/heuristics, integrations) is in **[docs/limits.md](docs/limits.md)**.

## License

Apache-2.0. See [LICENSE](LICENSE).
