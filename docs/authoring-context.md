# Authoring product context

`ctx init` creates four deliberately small document types under `.context/`:

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

## Visibility

Every document has a `visibility` of `public` or `private`. Omitting it defaults to `private`; any other value fails indexing. Only `public` documents can ever leave this repository — `ctx export` and federation sync ([docs/federation.md](federation.md)) skip every `private` document silently, and there is no override. Set `visibility: public` only on the documents you're willing to hand to another team's repository.

## Canonical symbol paths

**Python.** Files below `src/` omit that prefix: `src/billing/subscription.py` plus `class SubscriptionService` and `def cancel` becomes `billing.subscription.SubscriptionService.cancel`.

**Rust.** Paths include a crate namespace. A root `src/lib.rs` uses `crate`, while a workspace file such as `crates/ctx-core/src/indexing.rs` uses the Cargo-directory name: `ctx_core.indexing.plan_incremental_index`. Inherent methods use their implemented type and trait declarations use their trait. Trait implementations include the implemented trait, including type arguments when needed to prevent collisions: `ctx_cli.CliError.From<std::io::Error>.from`.

**Go.** Paths use the source directory as the package path (matching Go's one-package-per-directory convention), not the file name and not `go.mod`'s module path: `billing/subscription.go` with `func (s *SubscriptionService) Cancel` becomes `billing.SubscriptionService.Cancel`. A root-level file with no directory uses `main`. Interfaces are indexed as traits.

**Disambiguation.** Canonical names are normally enough. If two enabled languages produce the same canonical name, use the exact language-qualified stable key in the mapping, such as `symbol:rust:app.run:Function` or `symbol:python:app.run:Function`; `ctx status`, review JSON, and query output expose these keys.

## Database and HTTP contracts

You never author these by hand — they are extracted deterministically from code and migrations, and referenced from `implementation`/`tests` links or queried directly:

- Database tables/columns from static SQL, goose migrations, and SQLAlchemy models resolve as their normalized identifier (`subscriptions`, `billing.subscriptions`) or `table.column`. See [docs/architecture.md](architecture.md#static-database-interactions) and the schema-migration sections below it.
- HTTP endpoints and outbound calls extracted from Python FastAPI/Flask/`requests`/`httpx` code. See [docs/api-contracts.md](api-contracts.md).

Both appear in `ctx impact`/`ctx explain`/`ctx context` like any other node, and both feed `ctx review`'s `schema_findings`/`api_findings` streams independently of proven requirement impact.
