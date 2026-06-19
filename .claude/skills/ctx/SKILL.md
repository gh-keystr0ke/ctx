---
name: ctx
description: Use ctx, a local-first tool that links product intent (Features, Requirements, Invariants, Decisions) to code with provenance and confidence, whenever the working repository has a .ctx/ or .context/ directory, or the user asks about product/business impact, requirements, invariants, or "what does this change affect". Compiles bounded task context before editing, checks blast radius before touching a symbol, and reviews a diff for product-contract impact before finishing a task.
---

# ctx: product-context-aware coding

`ctx` connects product intent (Feature / Requirement / Invariant / Decision, stored in Git under `.context/`) to code, with provenance, confidence, and staleness on every claim. It answers questions plain code search cannot: *why does this code exist, and what product contract might this change break?* It is local-first (no network calls) and every claim is either a deterministic `FACT`, a human/documentation `ASSERTION`, or a machine `INFERENCE` — never invent certainty ctx did not report.

## Does this repository use ctx?

Check for `.ctx/config.toml` and/or a `.context/` directory at the repository root before doing anything below. If neither exists, this skill does not apply — do not run `ctx init` or otherwise introduce ctx into a project unprompted. If the user explicitly asks you to set ctx up, follow the repository's own README instead of this skill.

If `.ctx/` exists but `ctx status --json` reports `needs_index` or `behind`, run `ctx index` — but only when the working tree is clean (`git status --short` empty for the configured source/context paths). `ctx index` deliberately refuses to run over uncommitted changes to indexed inputs; do not work around that, just commit first or skip indexing and rely on `ctx review`, which works fine over an uncommitted diff.

## Two ways to call ctx

Prefer the MCP server when it is connected (check your available tools for `get_context`, `get_impact`, `explain_relation`, `find_requirements`, `review_change`). Otherwise fall back to the `ctx` CLI with `--json` for parseable output. Both call the identical underlying logic — there is no behavioral difference, only transport.

| MCP tool           | CLI equivalent                          | Notes |
|---------------------|------------------------------------------|-------|
| `get_context`       | `ctx context "<task>" [--file P]... [--symbol S]... [--token-budget N] --json` | `task` is required; everything else is optional |
| `get_impact`        | `ctx impact <file\|symbol\|stable-id> --json` | |
| `explain_relation`  | `ctx explain "<id>"` or `ctx explain "source -> target" --json` | |
| `find_requirements` | *(no CLI equivalent — MCP only)* | falls back to reading `.context/requirements/*.yaml` directly if MCP is unavailable |
| `review_change`     | `ctx review [--base <revision>] --json` | `base` defaults to `HEAD` |

## Workflow

**Before starting a coding task**, compile context instead of guessing what matters:

```
ctx context "<one-line description of the task>" --symbol <known symbol> --json
```

Read the returned Requirements, Invariants, and Decisions before touching code. Treat them as constraints on the change, not background reading — an Invariant is something whose violation is a bug by definition.

**Before editing a symbol you did not write**, especially in an unfamiliar area, check what depends on it:

```
ctx impact <file-or-symbol> --json
```

This returns the bounded set of Features/Requirements/Invariants/Decisions, related implementation, and tests connected to that symbol — not the whole reachable graph.

**After making the change, before calling the task finished or opening a PR**, review it:

```
ctx review --base <the commit you branched from> --json
```

`ctx` is deliberately conservative here: formatting-only, rename, and likely-refactor changes are suppressed, so a surfaced finding is meant to be taken seriously. Treat `severity: high` findings on Invariants/Requirements as must-address before finishing; report every finding to the user with its `reason`, `evidence`, and `suggested_action` verbatim — do not paraphrase away the uncertainty language (e.g. "possible requirement drift") or silently decide a finding does not apply.

**When you need to justify why ctx claims a relationship**, or the user asks "why does ctx think X affects Y":

```
ctx explain "<stable-id>" --json
ctx explain "<source-symbol> -> <target-id>" --json
```

Quote the returned claim class, confidence, and evidence. Never fabricate a rationale ctx itself did not return — that is the one thing this tool is built to prevent.

## Epistemic discipline

Every claim ctx surfaces has a class:

- **FACT** — deterministically observed (e.g. a call edge from static analysis). Trust it.
- **ASSERTION** — a human or an explicit `.context/` document confirmed it. Trust it, but it is only as current as the code it points at.
- **INFERENCE** — a heuristic guess, never auto-promoted to fact or assertion. Present it as uncertain, explicitly, every time.

A `stale` relationship means the code changed enough that a previously-confirmed claim needs re-verification — it is not necessarily wrong, but do not treat it as still confirmed. Never edit `.ctx/ctx.db` directly, and never mark a relation "verified" on the user's behalf; that decision belongs to `ctx verify`, which is an interactive human-in-the-loop step.

## What not to do

- Do not run `ctx init`/`ctx index` on a repository that has no `.ctx/`/`.context/` unless asked — ctx is local-first and opt-in.
- Do not treat ctx as a gate: if it is unavailable, absent, or `ctx status` is `needs_context` (structural graph only, no product documents indexed yet), proceed with the task using normal code-reading judgment and say so.
- Do not upgrade an `INFERENCE` to a stated fact in your own summary to the user, even implicitly.
- Do not let a low-confidence, `--verbose`-only finding dominate your report the way a high-confidence one would; ctx's own design principle is "surface fewer findings with stronger evidence."
