# Historical-PR evaluation: this repository's own commit history

This is a first, honest step toward the "labeled historical PR corpus" experiment named in
`product_conclu.md` (sections 49-52) and repeated as an open gap in every prior `worklog.md`
handoff. It is real historical data (this repository's own real commits, real defects, real
fixes) and not a synthetic fixture, but it is **not** the external, third-party-repository
corpus that experiment ultimately requires. Read this as a self-corpus pilot, not as a
substitute for reviewing a real product codebase ctx did not itself produce.

## Why this repository, and its honest limits

`ctx`'s own commit history since `.context/` was first authored (`354d898`, 2026-08-17) is
unusually well suited to a first pass: every commit has a real diff, and `worklog.md` records
an independent, contemporaneous account of what each milestone actually did and why —
written before this evaluation existed, not for it. That gives an external-ish check on
labeling: ground truth here comes from re-reading the actual diff and the actual `.context/`
state at the time, cross-checked against prose nobody wrote with this experiment in mind.

The honest limits: this is an engineering tool reviewing its own engineering-tool source code,
not a product codebase. Its `.context/` corpus was authored by the same project (and largely
by prior agent sessions) rather than by an independent team documenting unrelated product
requirements. Results here say something real about `ctx review`'s mechanics on real diffs,
but nothing about review precision/recall on a real product team's pull requests, impact-
understanding time, agent task success, or maintenance cost — those experiments are still
`not evaluated` and require an external repository or participants, as `docs/evaluation.md`
already states.

## Methodology

`scripts/historical-corpus.tsv` lists 16 real commit pairs (`parent`, `commit`) drawn from
this repository's actual linear history, each `commit`'s direct Git parent (verified with
`git rev-parse <commit>^`, not eyeballed from `git log`). `scripts/run_historical_eval.sh`
replays each pair against a throwaway clone of this repository using the compiled release
binary, `target/release/ctx`:

1. checks the clone out to `parent` (a clean working tree, `HEAD == parent`);
2. runs `ctx init` (a no-op if `.ctx/config.toml` is already committed) and `ctx index`,
   building the graph from `.context/` **exactly as it was committed at that point in
   history** — not backfilled with today's mappings;
3. records `ctx status` at that point;
4. checks the clone out to `commit` (working tree now reflects the real commit's changes);
5. runs `ctx --json review --base <parent>`, which diffs `parent` against the working tree.

This mirrors how a contemporaneous reviewer would actually have used `ctx`: index at the
base, make the real change, review before the next index. No metric here uses hindsight
`.context/` state from a later commit.

Raw JSON output for every case lives in `docs/historical-eval-results/` (gitignored-equivalent
scratch output is not committed; regenerate with `bash scripts/run_historical_eval.sh`).

### Corpus composition (16 cases)

- **9 real bug-fix commits** (`fix:`) to already-shipped logic, each with a `worklog.md`
  entry written at the time describing the actual defect and fix.
- **4 real documentation/context-only commits** (`docs:`), touching only `.context/*.yaml`,
  `worklog.md`, or `prompt.md` — zero source files, a structural true-negative control.
- **3 real feature-addition commits** (`feat:`) that extended already-shipped, already-mapped
  entry points (a new language module registered into the existing analyzer registry, DB
  interactions wired through existing indexing/impact/review/context-pack entry points,
  schema-aware review wired into the existing review entry point) — cases where new capability
  and already-mapped code change in the same real commit.

## Results

All 16 replays completed without harness errors: `ctx index` succeeded at every parent, and
`ctx status` reported `ready` (0 stale semantics) at every parent before review ran — so every
case reviewed a real diff against a genuinely healthy, contemporaneous graph, not a degraded
one.

| Case | Category | Real change | Findings | Verified correct? |
| --- | --- | --- | --- | --- |
| `tp-impact-traversal` | fix | rewrote `expand_semantics` (private helper) | 0 | yes — `analyze_impact` (the mapped entry point) itself was untouched |
| `tp-context-shared-nodes` | fix | changed `traversable`'s own signature | 1 (`INV-EPISTEMIC-001`, contract_changed) | yes — verified `traversable`'s signature line changed |
| `tp-shared-test-bridge` | fix | changed `analyze_impact`'s own body | 1 (`REQ-IMPACT-001`) | yes — verified `analyze_impact`'s body line changed |
| `tp-fingerprint-name-match` | fix | changed `match_symbols`'s own body | 1 (`INV-DETERMINISM-001`) | yes — verified directly |
| `tp-transition-scope` | fix | changed `match_symbols` and `plan_incremental_index` | 3 | yes — both are directly changed, both mapped |
| `tp-cross-file-move` | fix | changed `resolve_changed_entities`/`changed_entity` (private helpers) | 0 | yes — `build_review_findings`/`classify_behavior_change` (the mapped entry points) untouched |
| `tp-explicit-seed-bounded` | fix | changed `detect_seeds` (private helper) | 0 | yes — `compile_context_pack`/`truncate_to_tokens`/`traversable` untouched |
| `tp-utf8-boundary-panic` | fix | changed `strip_word_ci`/`is_word_byte` (private helpers) | 0 | yes — `sql_entities` (mapped) untouched |
| `tp-order-independent-identity` | fix | changed `match_symbols` and `plan_incremental_index` | 3 | yes |
| `tn-docs-traversal-invariant` | docs | `.context` + `worklog.md` only | 0 | yes — no source file in the diff |
| `tn-docs-handoff` | docs | `prompt.md` + `worklog.md` only | 0 | yes — no source file in the diff |
| `tn-docs-revalidate-after-identity` | docs | `.context` only | 0 | yes — no source file in the diff |
| `tn-docs-revalidate-go-goose-sqla` | docs | `.context` only | 0 | yes — no source file in the diff |
| `new-go-module` | feat | Go module registered into `AnalyzerRegistry::builtins` (mapped) | 1 (`REQ-LANGUAGE-001`) | yes — `builtins`'s own body changed |
| `new-schema-aware-review` | feat | new schema logic wired into `build_review_findings` (mapped) | 3 | yes — `build_review_findings`'s own body changed |
| `new-db-interactions` | feat | DB interactions wired through 8 already-mapped entry points across indexing/impact/review/storage | 10 | yes — all 8 distinct changed symbols (`analyze_source`×2, `apply_index`, `persist_edge`, `traversable`, `analyze_impact`, `plan_incremental_index`, `classify_behavior_change`) verified directly changed via diff-hunk/function-span cross-check |

Precision on every one of the 23 findings emitted across all 16 cases: **0 false positives
found** — every finding cited a symbol whose own body or signature genuinely changed in that
exact commit, mapped to the exact requirement/invariant/decision that symbol already
implemented before the commit.

Recall relative to `ctx review`'s own documented contract (does a change to the mapped
symbol's own body/signature get flagged) was **9/9 (100%)** across the fix commits: every
case where the mapped entry point itself changed produced a finding, and every case where it
did not stayed silent.

## The one real finding worth acting on

Four of the nine real bug-fix commits (`tp-impact-traversal`, `tp-cross-file-move`,
`tp-explicit-seed-bounded`, `tp-utf8-boundary-panic`) are real, worklog-documented behavior
fixes to logic that a mapped requirement/invariant governs — and `ctx review` was silent on
all four, correctly by its own contract, because in every one of the four the actual changed
lines were in a *private helper* (`expand_semantics`, `resolve_changed_entities`/
`changed_entity`, `detect_seeds`, `strip_word_ci`/`is_word_byte`) called by the mapped public
entry point (`analyze_impact`, `build_review_findings`, `compile_context_pack`, `sql_entities`)
whose own body bytes and signature never changed.

This is not a bug in the sense of wrong output — `ctx review`'s classification is per-changed-
symbol by design, and it correctly reported "the symbol I was asked about did not change."
But it is a real, previously-undocumented **recall gap**: `ctx review` cannot currently tell a
reviewer "the function you mapped to this invariant now behaves differently because a helper
it calls changed," even when that is exactly what happened in real, worklog-verified defects
this project fixed. Four out of four real candidates in this small corpus hit it — that is a
consistent pattern, not one anecdote.

Whether to close this gap is a real architectural decision, not a quick fix: `ctx-core` has
already fixed the opposite failure mode three separate times (`worklog.md`, "shared-test
isolation," "bounded typed impact traversal," "shared-node isolation in Context Pack") — an
uncontrolled call-graph walk turning one changed symbol into unrelated findings on everything
that calls it. Naively flagging every mapped caller of a changed private helper risks
reintroducing exactly that hub-explosion/false-positive class for any widely-called utility.
A bounded, one-hop-only extension (mirroring the one-hop caller/callee exemption `impact.rs`
and `context_pack.rs` already use, with the same non-amplifying, non-bridging guards) is the
shape a fix would need to take, and it deserves its own evaluation-first scope with dedicated
regression cases — not a change made inside a measurement session while flying solo. Recorded
here as the corpus's flagship, actionable finding and the honest next priority for review
depth, distinct from the still-open external-validation priority below.

## What this does and does not prove

It proves `ctx review`'s per-symbol change detection is accurate and precise on 16 real
commits pulled from this project's own history, spanning bug fixes, pure documentation
changes, and cross-cutting feature work — with zero false positives found and a newly
identified, real, four-for-four recall gap at the private-helper boundary.

It does not prove review precision/recall on an external product codebase, impact-
understanding time, coding-agent task success/token efficiency, or human maintenance cost.
Those remain `not evaluated`, exactly as `docs/evaluation.md` already states, and still
require a real external repository or participants this automated session does not have.

## Reproducing this report

```bash
cargo build --locked --workspace --release
bash scripts/run_historical_eval.sh
```

Raw per-case JSON (`*.init.json`, `*.index-parent.json`, `*.status-parent.json`,
`*.review.json`, plus `*.stderr` on failure) is written to `docs/historical-eval-results/`.
