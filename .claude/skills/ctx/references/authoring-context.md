# Authoring `.context/` documents

`ctx init` creates four deliberately small document types under `.context/`:

```text
.context/
├── features/
├── requirements/
├── invariants/
└── decisions/
```

A requirement links exact canonical symbols to intent and tests:

```yaml
id: REQ-SUB-014
type: requirement
feature: FEAT-SUBSCRIPTIONS
status: active
visibility: private   # default; see "Visibility" below
statement: When a paid user cancels, access must remain active until paid_until.
implementation:
  - symbol: billing.subscription.SubscriptionService.cancel
tests:
  - symbol: tests.test_subscription.test_cancel_keeps_access_until_paid_until
```

## Fields by type

| Type | Required fields | Optional relationships |
| --- | --- | --- |
| `feature` | `id`, `name`; `description` is recommended | `implementation`, `tests` |
| `requirement` | `id`, `statement` | `feature`, `implementation`, `tests` |
| `invariant` | `id`, `statement` | `feature`, `implementation`, `tests` |
| `decision` | `id`, `title`, `decision` | `feature`, `implementation`, `tests` |

`status` defaults to `active`. IDs must be unique across `.context`. A link may be either `{ symbol: canonical.name }` or a plain canonical-name string.

Markdown files are accepted when their metadata is YAML front matter delimited by `---`; prose after the closing delimiter is retained as source evidence but fields come from the front matter.

## Which document type to pick

- **Feature** — a user-facing capability or area, mostly an organizing parent for Requirements/Invariants/Decisions rather than a testable claim itself.
- **Requirement** — a specific behavior the product must exhibit ("when X happens, Y must be true"). This is the default choice for "we added/changed a behavior."
- **Invariant** — a constraint that must never be violated, independent of any one feature flow ("a cancelled subscription's `paid_until` never moves backward"). Use this when violating it is a bug by definition, not just a missed requirement.
- **Decision** — an architectural or design choice with a rationale worth preserving ("we use optimistic locking here because ..."), not itself a behavior to test.

When extending existing coverage rather than adding new coverage (see the "keep `.context/` in sync" rule in `SKILL.md`), prefer adding an `implementation`/`tests` link or refining a `statement` on the document that already owns this guarantee over creating a fourth near-duplicate document for the same flow.

## Visibility

Every document has a `visibility` of `public` or `private`. Omitting it defaults to `private`; any other value fails indexing. Only `public` documents can ever leave this repository — `ctx export` and federation sync (`federation.md`) skip every `private` document silently, and there is no override. Set `visibility: public` only on documents you're willing to hand to another team's repository.

## Canonical symbol paths

Verify a path against real indexed output before trusting it in a mapping — `ctx find <name>` or `ctx index`'s unresolved-mapping count will tell you if a hand-derived path is wrong; a typo silently produces an unresolved mapping rather than a hard error.

**Python.** Files below `src/` omit that prefix: `src/billing/subscription.py` plus `class SubscriptionService` and `def cancel` becomes `billing.subscription.SubscriptionService.cancel`.

**Rust.** Paths include a crate namespace. A root `src/lib.rs` uses `crate`, while a workspace file such as `crates/ctx-core/src/indexing.rs` uses the Cargo-directory name: `ctx_core.indexing.plan_incremental_index`. Inherent methods use their implemented type and trait declarations use their trait. Trait implementations include the implemented trait, including type arguments when needed to prevent collisions: `ctx_cli.CliError.From<std::io::Error>.from`.

**Go.** Paths use the source directory as the package path (matching Go's one-package-per-directory convention), not the file name and not `go.mod`'s module path: `billing/subscription.go` with `func (s *SubscriptionService) Cancel` becomes `billing.SubscriptionService.Cancel`. A root-level file with no directory uses `main`. Interfaces are indexed as traits.

**Disambiguation.** Canonical names are normally enough. If two enabled languages produce the same canonical name, use the exact language-qualified stable key in the mapping, such as `symbol:rust:app.run:Function` or `symbol:python:app.run:Function`; `ctx status`, review JSON, and query output expose these keys.

## Database and HTTP contracts

You never author these by hand — they are extracted deterministically from code and migrations, and referenced from `implementation`/`tests` links or queried directly:

- Database tables/columns from static SQL, goose migrations, and SQLAlchemy models resolve as their normalized identifier (`subscriptions`, `billing.subscriptions`) or `table.column`.
- HTTP endpoints and outbound calls extracted from Python FastAPI/Flask/`requests`/`httpx` code.

Both appear in `ctx impact`/`ctx explain`/`ctx context` like any other node, and both feed `ctx review`'s `schema_findings`/`api_findings` streams independently of proven requirement impact.

## Configuration (`.ctx/config.toml`)

```toml
languages = ["python", "rust", "go"]

[paths]
include = ["src", "tests"]
exclude = ["generated", "vendor", "build", "dist", "target", ".venv"]

# Required only for ctx export/sync/registry — see federation.md
[service]
name = "billing"

# Required only for ctx ingest gitlab
[gitlab]
project = "billing/subscriptions"
# base_url = "https://gitlab.example.com/api/v4"  # self-managed instances
```

`languages` enables any subset of the built-in `python`, `rust`, `go`, and `goose` (SQL migrations, not a language — pair it with a `migrations`-style directory in `paths.include`) modules. Include/exclude entries are repository-relative directory prefixes; exclusions win. Changing languages or path boundaries is reconciled against the stored snapshot on the next `ctx index`.

## What's committed vs. local-only

| Path | Committed? | Contents |
| --- | --- | --- |
| `.ctx/config.toml` | Yes (if the team shares config) | Language/path/service/GitLab settings. |
| `.context/**/*.yaml`\|`.md` | Yes | Hand-authored or accepted product-context documents. |
| `.ctx-candidates/*.yaml` | Yes | Pending AI-derived knowledge candidates from `ctx enrich`, one file per candidate — the one exception to `.ctx/`'s local-only rule, so a teammate sees the same pending queue without re-running `ctx enrich`. |
| `.ctx/ctx.db`, `.ctx/ctx.db-wal`, `.ctx/ctx.db-shm` | No | The local SQLite index. |
| `.ctx/registry.toml` | No | Local machine's list of neighboring repository checkouts, by absolute path. |
| `.ctx/export.json` | No | This repository's last exported public manifest. |
