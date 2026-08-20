---
name: ctx
description: Use ctx, a local-first tool that links product intent (Features, Requirements, Invariants, Decisions) to code with provenance and confidence, whenever the working repository has a .ctx/ or .context/ directory, the user asks about product/business impact, requirements, invariants, blast radius, or "what does this change affect", the user wants to onboard a repository onto ctx (including fully automated onboarding by mining git history/comments/GitLab), or the work spans multiple related repositories (federation, cross-service request tracing). Compiles bounded task context before editing, checks blast radius before touching a symbol, reviews every diff for product-contract impact before a commit or PR, and keeps `.context/` documentation and the local index in sync with the code.
---

# ctx: product-context-aware coding

`ctx` connects product intent (Feature / Requirement / Invariant / Decision, stored in Git under `.context/`) to code, with provenance, confidence, and staleness on every claim. It answers questions plain code search cannot: *why does this code exist, and what product contract might this change break?* It is local-first — the only two operations that ever touch a network or another CLI are `ctx ingest gitlab` and `ctx enrich`/`ctx verify --auto`/`ctx verify --stale`, and only when explicitly run. Every claim is either a deterministic `FACT`, a human/documentation `ASSERTION`, or a machine `INFERENCE` — never invent certainty ctx did not report.

This file is the entry point. Deeper material lives alongside it and is loaded on demand:

- `references/commands.md` — full CLI flag reference for every subcommand.
- `references/authoring-context.md` — `.context/` document schema and exact canonical-symbol-path rules per language.
- `references/onboarding.md` — bootstrapping a repository onto ctx, by hand or fully automated by mining existing history.
- `references/federation.md` — sharing product knowledge and tracing requests across sibling repositories.

## Does this repository use ctx?

Check for `.ctx/config.toml` and/or a `.context/` directory at the repository root before doing anything below.

- **Neither exists and the user hasn't asked to set ctx up:** this skill's day-to-day workflow doesn't apply. Don't run `ctx init` unprompted — ctx is opt-in.
- **Neither exists and the user asks to add ctx, bootstrap product context, or "set up ctx here":** go to `references/onboarding.md`.
- **`.ctx/` exists:** this skill applies for the rest of the session. Continue below.

## Two ways to call ctx

Prefer the MCP server when it is connected (check your available tools for `get_context`, `get_impact`, `explain_relation`, `find_requirements`, `review_change`). Otherwise fall back to the `ctx` CLI with `--json` for parseable output. Both call the identical underlying logic — there is no behavioral difference, only transport. MCP is deliberately read-only and covers five query tools only; everything mutating (`index`, `ingest`, `enrich`, `verify`, `init`, `registry`, `export`, `sync`) is CLI-only — see `references/commands.md`.

| MCP tool           | CLI equivalent                          | Notes |
|---------------------|------------------------------------------|-------|
| `get_context`       | `ctx context "<task>" [--file P]... [--symbol S]... [--token-budget N] --json` | `task` is required; everything else is optional |
| `get_impact`        | `ctx impact <file\|symbol\|stable-id\|table.column> --json` | |
| `explain_relation`  | `ctx explain "<id>"` or `ctx explain "source -> target" --json` (add `--trace` for reachable HTTP endpoints) | |
| `find_requirements` | *(no CLI equivalent — MCP only)* | falls back to reading `.context/requirements/*.yaml` directly if MCP is unavailable |
| `review_change`     | `ctx review [--base <revision>] --json` | `base` defaults to `HEAD` |

## The per-task pipeline

Run every non-trivial coding task through this shape. Steps 1 and 4 are not optional — see the next two sections.

```
1. ctx context "<task>" --symbol ... --file ... --json   # compile bounded context BEFORE editing
2. ctx impact <symbol>  --json                            # before touching code you didn't write, check blast radius
3.  ...edit the code...
4. ctx review --base <branch point> --json                 # ALWAYS, before calling the task done / opening a PR / committing
5. Update .context/ if this changed a product contract        # see "Keep .context/ in sync" below
6. git commit                                               # only after 4 and 5
7. ctx index                                                 # after committing, so future queries see the new commit
```

### 1–2. Before and during editing

**Before starting a coding task**, compile context instead of guessing what matters:

```bash
ctx context "<one-line description of the task>" --symbol <known symbol> --json
```

Read the returned Requirements, Invariants, and Decisions before touching code. Treat them as constraints on the change, not background reading — an Invariant is something whose violation is a bug by definition.

**Before editing a symbol you did not write**, especially in an unfamiliar area, check what depends on it:

```bash
ctx impact <file-or-symbol> --json
```

This returns the bounded set of Features/Requirements/Invariants/Decisions, related implementation, data contracts, and tests connected to that symbol — not the whole reachable graph. Not sure of the exact symbol name? `ctx find <name>` first.

## Non-negotiable: review before every commit

**Before calling any coding task finished, opening a PR, or running `git commit` on changes that touch configured source paths, run:**

```bash
ctx review --base <the commit you branched from> --json
```

Working tree not yet committed and you branched straight off `main`/`master`? `--base` defaults to `HEAD`, so plain `ctx review --json` covers "review what I'm about to commit." A multi-commit feature branch should review against its actual target (`ctx review --base main --json`), not just the last commit.

`ctx` is deliberately conservative here: formatting-only, rename, and likely-refactor changes are suppressed, so a surfaced finding is meant to be taken seriously. Rules:

- Treat `severity: high` findings on Invariants/Requirements as must-address before finishing.
- Report every finding to the user with its `reason`, `evidence`, and `suggested_action` verbatim — do not paraphrase away the uncertainty language (e.g. "possible requirement drift") or silently decide a finding does not apply.
- `schema_findings` (database) and `api_findings` (HTTP contracts) are independent streams, not a subset of `findings` — check all three every time. An empty `related_intents`/`related_tests` list on a schema/API finding means no mapping is known, not that the change is unrelated to the product.
- Pass `-v`/`--verbose` when you want suppressed-change counts and lower-confidence diagnostics, e.g. while investigating why a change you believe is meaningful didn't surface a finding.
- Never skip this step because a change "looks small" — that judgment is exactly what `ctx review` exists to check instead of trusting.
- If `ctx` is unavailable or the repository has no `.ctx/`, this step doesn't apply — say so rather than silently skipping a step the user would expect to see.

## Non-negotiable: keep `.context/` documentation in sync

`ctx`'s entire value is that `.context/` accurately describes what the code does. A change that alters product behavior — a new Feature, a changed Requirement/Invariant, a new architectural Decision — and leaves `.context/` untouched is a change `ctx` itself can no longer see or protect, on this repository or any other using it.

**After a change is verified (tests pass, review is clean or its findings are addressed), before considering the task done:**

1. Ask: did this change product-observable behavior, a contract, or an architectural choice — or is it a pure refactor/formatting/internal cleanup? Only the former needs a `.context/` update.
2. If it does, check whether an existing document already covers this guarantee (`ctx impact <symbol>`, `ctx explain <id>`, or `ctx find <term>`). Extend that document — add/update its `implementation`/`tests` links, or adjust its `statement` — when the new behavior is the same guarantee's flip side or a natural extension. Prefer this over adding a near-duplicate document.
3. Otherwise, author a new Feature/Requirement/Invariant/Decision under `.context/` (see `references/authoring-context.md` for the exact schema and canonical-symbol-path rules per language — a typo in a symbol path silently produces an unresolved mapping instead of a hard error, so verify paths against real indexed output, e.g. `ctx find <symbol>`, don't hand-derive them from guesswork).
4. Commit the `.context/` change in the same commit (or same PR) as the code change, not as a follow-up someone has to remember — unless `.context/` is redirected to a location outside this checkout (`ctx context-store show` tells you), in which case commit there instead; nothing to add in this checkout either way.
5. A worklog/changelog entry is a supplement to this, never a substitute — it isn't queryable by `ctx impact`/`ctx review`, so a change documented only there is still invisible to the tool.

If you're not sure whether a change is "product-observable" enough to warrant this, err toward asking the user rather than silently skipping it.

## Scenario cookbook

| Scenario | Run this |
| --- | --- |
| Starting a coding task | `ctx context "<task>" --symbol ... --json` |
| About to edit a symbol you didn't write | `ctx impact <target> --json` (use `ctx find <name>` first if unsure of the exact symbol) |
| Finishing a task / before a commit or PR | `ctx review --base <branch point> --json` (see above — always) |
| User asks "why does ctx think X affects Y" | `ctx explain "<source> -> <target>" --json`, or `ctx explain <id> --json` for one node's claims |
| User asks what endpoints a Feature/Requirement touches | `ctx explain <id> --trace` |
| Tracing one HTTP request's full path, possibly across services | `ctx trace "METHOD /path"` (`--verbose` to attach product context per hop) — see `references/federation.md` |
| Repo has no `.context/` yet, or `ctx status` says `needs_context` | `references/onboarding.md` |
| Repo has real Git history / code comments / a GitLab project worth mining | `references/onboarding.md` § mining |
| Periodic health check, or after a big merge | `ctx status --json` — read `health`, `notices`, `suggested_actions` and act on them |
| Working across multiple related repositories | `references/federation.md` |
| A stale claim needs re-checking after code moved on | `ctx verify --stale --agent <name>` (accept is binding; reject is only ever a suggestion for a human) |

## Keeping the index honest

`ctx index` only accepts **committed** configured sources — every indexed version has an honest Git validity boundary. `.context`/`.ctx-candidates` inputs get the same commit requirement only when their (possibly redirected, `ctx context-store show`) location is itself a Git repository; a plain-directory context store has no commit gate and is read as-is. If `ctx status --json` reports `index_state: "behind"` or `"not_indexed"` (surfaced as `health: "needs_index"`), run `ctx index` — but only when the working tree is clean for the configured source/context paths that do require a commit (`git status --short`). `ctx index` deliberately refuses to run over uncommitted changes to indexed inputs that require one; do not work around that — commit first, or just skip indexing and rely on `ctx review`, which works fine over an uncommitted diff regardless of index freshness.

`ctx status --json` also reports, and you should act on: `schema_divergences` (ORM vs. migration-history mismatches), `unmapped_intents` (active documents with no implementation/test link — `health: "needs_mappings"`), and `stale_claims` (semantic relationships whose code changed since last confirmed). Its own `suggested_actions` array names the exact next command — prefer that over guessing.

## Epistemic discipline

Every claim ctx surfaces has a class:

- **FACT** — deterministically observed (e.g. a call edge from static analysis). Trust it.
- **ASSERTION** — a human or an explicit `.context/` document confirmed it. Trust it, but it is only as current as the code it points at.
- **INFERENCE** — a heuristic or AI-derived guess, never auto-promoted to fact or assertion. Present it as uncertain, explicitly, every time — including candidates from `ctx enrich` and `--auto`/`--stale` agent decisions, which are recorded honestly as agent-made, not human-verified.

A `stale` relationship means the code changed enough that a previously-confirmed claim needs re-verification — it is not necessarily wrong, but do not treat it as still confirmed. Never edit `.ctx/ctx.db` directly, and never mark a relation "verified" on the user's behalf; that decision belongs to `ctx verify`, which is a human-in-the-loop step by default.

## What not to do

- Do not run `ctx init` on a repository that has no `.ctx/`/`.context/` unless asked — ctx is local-first and opt-in.
- Do not treat ctx as a gate: if it is unavailable, absent, or `ctx status` is `needs_context` (structural graph only, no product documents indexed yet), proceed with the task using normal code-reading judgment and say so.
- Do not upgrade an `INFERENCE` to a stated fact in your own summary to the user, even implicitly.
- Do not let a low-confidence, `--verbose`-only finding dominate your report the way a high-confidence one would; ctx's own design principle is "surface fewer findings with stronger evidence."
- Do not run `ctx ingest gitlab`, `ctx enrich`, `ctx verify --auto`, or `ctx verify --stale` unprompted — they shell out to a real agent CLI (`claude`/`codex`/`agy`) or GitLab's network API, which costs time and, for the agent CLIs, money. Run them when the user asks to bootstrap/mine context, per `references/onboarding.md`.
- Do not treat `ctx verify --knowledge --auto`'s "Auto-verified" documents as equivalent to human review when reporting to the user — say plainly that they were agent-decided.
- Do not set up `ctx registry`/federation for a single-repository project — it exists for multi-service teams with sibling local checkouts; see `references/federation.md` for when it actually applies.
- Do not skip the pre-commit `ctx review` or the `.context/` documentation update because a change "seems obviously fine" — those are exactly the two non-negotiable steps above.
