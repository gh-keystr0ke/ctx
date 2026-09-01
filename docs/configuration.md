# Configuration

`.ctx/config.toml` is intentionally small. `ctx init` writes a default; commit it when a team should share it.

```toml
languages = ["python", "rust", "go"]

[paths]
include = ["src", "tests"]
exclude = ["generated", "vendor", "build", "dist", "target", ".venv"]

# Required only for ctx export/sync/registry — see docs/federation.md
[service]
name = "billing"

# Required only for ctx ingest gitlab
[gitlab]
project = "billing/subscriptions"
# base_url = "https://gitlab.example.com/api/v4"  # self-managed instances
```

## `languages` and `[paths]`

`languages` enables any subset of the built-in `python`, `rust`, `go`, and `goose` modules. `goose` reads goose SQL migration files (`.sql`) instead of a programming language; add it, and a directory such as `migrations`, to `paths.include`, to pick up schema declarations. The legacy singular `language = "python"` form remains accepted; do not set both forms. Unsupported or empty language sets fail during repository discovery instead of silently skipping code.

Include and exclude entries are repository-relative directory prefixes. Exclusions win. Generated, vendor, build, virtual-environment, cache, and non-configured source paths are also protected by built-in filtering. Changing languages or path boundaries is reconciled against the stored snapshot on the next `ctx index`.

## `[service]`

Names this repository for federation (`ctx registry`/`ctx export`/`ctx sync`/`ctx federation`). Optional — everything else in `ctx` works without it. See [docs/federation.md](federation.md).

## `[gitlab]`

Required only to run `ctx ingest gitlab`. `project` is the GitLab project path; `base_url` defaults to `https://gitlab.com/api/v4` and only needs overriding for a self-managed instance. For private projects, the access token comes only from the `CTX_GITLAB_TOKEN` environment variable — never from a committed file, so it can never end up in `.ctx/config.toml` by accident. The variable may be omitted for a public project that allows anonymous API reads.

## `[jira]`

Required only to run `ctx ingest jira`. Jira Cloud only (Jira Server/Data Center isn't supported). Both `base_url` (e.g. `https://your-domain.atlassian.net`) and `project` (the project key, e.g. `PSI`) are required — unlike GitLab, Jira Cloud has no shared default host. Credentials come only from the `CTX_JIRA_EMAIL` (the account the API token was issued under) and `CTX_JIRA_TOKEN` environment variables — never from a committed file:

```toml
[jira]
base_url = "https://your-domain.atlassian.net"
project = "PSI"
```

An issue's stable identity is its human-readable key (`PSI-1122`), not Jira's internal numeric id — this is why a `PSI-1122` mention in a commit message or branch name already resolves to the ingested issue via `ctx-core`'s existing deterministic ticket-key linking, with no Jira-specific linking logic. Issues and comments only; changelog/worklog/attachments aren't ingested.

`ctx ingest jira` never fetches an entire project — a Jira project routinely spans several unrelated services/repositories, so pulling all of it into each one would be mostly noise. The legacy default scans already-known artifact text. For repository isolation, prefer `ctx ingest jira --scope business-linked`: candidate keys then come only from current Git plus GitLab MRs selected by `ctx ingest gitlab --scope business-linked` and their comments; old Jira artifacts and unrelated GitLab issues cannot seed the set. `--related-depth 0` is the strict default, and a larger explicit value admits only that many Jira-reported `issuelinks`/`parent` hops. Run `ctx ingest git`, then strict GitLab ingestion, before strict Jira ingestion.

## Redirecting the context store (`ctx context-store`)

`.context/` and `.ctx-candidates/` normally live inside this checkout, alongside `.ctx/config.toml` above. When the checkout isn't yours to commit into — documenting a third-party repository, for example — `ctx context-store set <path>` redirects both elsewhere, resolved through a machine-local registry (`~/.config/ctx/contexts.toml` by default, or `$XDG_CONFIG_HOME/ctx/contexts.toml`) rather than anything written into the checkout itself:

```
ctx context-store set ../notes/some-repo-context         # plain directory, no commit gate
ctx context-store set --git ../notes/some-repo-context   # also `git init`s it: same commit-before-index guarantee .context/ normally has
ctx context-store show                                   # where it currently resolves, and whether it's Git-backed
```

Without `--git` the redirected location is just files on disk — `ctx index` reads them as-is, with no commit-before-index guarantee. With `--git` (or when the target already is a Git repository), the same [INV-COMMIT-001](../.context/invariants/committed-inputs.yaml) guarantee this checkout's own `.context/` has applies there too, checked independently. `ctx init` scaffolds `.context/{features,requirements,invariants,decisions}/` and `.ctx-candidates/` under whichever location is currently resolved — run `context-store set` before `init` for a fresh redirect. See [ADR-CTX-050](../.context/decisions/external-context-store.yaml).

## Environment variables

| Variable | Used by | Purpose |
| --- | --- | --- |
| `CTX_GITLAB_TOKEN` | `ctx ingest gitlab` | Optional GitLab API access token (needed for private/authenticated projects); never read from a file. |
| `CTX_CLAUDE_CLI_BINARY` | `ctx enrich`/`ctx verify --auto` with `--agent claude` | Override the `claude` binary path (testing, alternate install location). |
| `CTX_CODEX_CLI_BINARY` | ... with `--agent codex` | Override the `codex` binary path. |
| `CTX_ANTIGRAVITY_CLI_BINARY` | ... with `--agent antigravity` | Override the `agy` binary path. |
| `CTX_FEDERATION_BINARY` | `ctx sync` | Override which `ctx` executable is invoked against each neighbor checkout (defaults to the currently running executable). |
| `CTX_CONTEXTS_FILE` | `ctx context-store set`/`show`, and context-store resolution on every command | Override the context-store registry file path (defaults to `$XDG_CONFIG_HOME/ctx/contexts.toml` or `~/.config/ctx/contexts.toml`). |

No token or API key for `claude`/`codex`/`antigravity` is read by `ctx` itself — each agent CLI handles its own authentication.

## What's stored where

| Path | Committed? | Contents |
| --- | --- | --- |
| `.ctx/config.toml` | Yes (if the team shares config) | Language/path/service/GitLab settings. |
| `.context/**/*.yaml`\|`.md` | Yes, unless redirected | Hand-authored or accepted product-context documents. Location is `<checkout>/.context/` unless `ctx context-store set` redirects it elsewhere — see above. |
| `.ctx-candidates/*.yaml` | Yes, unless redirected | Pending AI-derived knowledge candidates from `ctx enrich`, one file per candidate. This is the one exception to `.ctx/`'s local-only rule — `git add`/commit/push/pull it like `.context/` so a teammate sees the same pending queue without re-running `ctx enrich`. Moves with `.context/` under `ctx context-store set`. |
| `.ctx/ctx.db`, `.ctx/ctx.db-wal`, `.ctx/ctx.db-shm` | No | The local SQLite index. `ctx init` adds these to the repository-local Git exclude file (not the shared `.gitignore`). Always local to this checkout, never redirected. |
| `.ctx/registry.toml` | No | Local machine's list of neighboring repository checkouts, by absolute path. Not portable across machines — each clone runs its own `ctx registry add`. |
| `.ctx/export.json` | No | This repository's last exported public manifest. Regenerated by `ctx export` or automatically by a neighbor's `ctx sync`. |
| `~/.config/ctx/contexts.toml` | No — outside any checkout entirely | This machine's context-store redirects, keyed by checkout path. Not read from or written into any repository. |
