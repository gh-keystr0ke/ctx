# Evaluation baseline

`ctx-eval` is the repository's deterministic product-quality regression corpus. It creates a fresh temporary Git repository for every case, drives the same `ctx-app` use cases as the CLI, and scores typed results rather than parsing terminal text.

Run it with:

```bash
cargo run --locked -p ctx-eval
```

The command prints machine-readable JSON and exits non-zero when a case, harness step, or ground-truth check fails. `cargo test --locked -p ctx-eval` runs the same corpus as a release gate.

## Current corpus

| Case | Ground truth exercised |
| --- | --- |
| `cancellation-behavior-change` | mapped behavior regression surfaces the cancellation requirement and invariant |
| `formatting-only` | whitespace noise is classified and suppressed |
| `unrelated-refactor` | an unmapped helper creates no product findings |
| `rename-or-move` | in-file rename preserves identity and stays silent |
| `symbol-move-across-files` | cross-file move merges delete/add halves into one silent rename |
| `deleted-contract-implementation` | deleting a decision-mapped integration point surfaces the decision |
| `changed-database-write` | changing `subscriptions` to `subscription_archive` appears in review, impact, and Context Pack |
| `stale-semantic-mapping` | an indexed implementation change makes verified claims stale and visible as uncertainty |
| `shared-test-isolation` | a shared test cannot bridge unrelated requirements |
| `added-call-discovers-intent` | a new unmapped caller discovers intent through a deterministic call fact |
| `multi-commit-feature-evolution` | a three-commit span preserves identity, staleness, and aggregate classification |
| `goose-migration-declares-schema-without-code-access` | a table declared only by a goose migration still appears as a data contract, without review noise |
| `sqlalchemy-model-declares-schema-without-sql-access` | a table declared only by a SQLAlchemy model still appears as a data contract, without review noise |
| `migration-drops-mapped-column-is-destructive` | a destructive schema finding resolves to the exact requirement/invariant its table's writer implements, not an unrelated decision on a symbol that only calls the writer |
| `migration-renames-mapped-column-is-destructive` | a goose `RENAME COLUMN` is a destructive schema finding |
| `migration-adds-not-null-column-without-default-is-destructive` | a new `NOT NULL` column with no `DEFAULT` on an existing table is destructive |
| `migration-alters-existing-column-type-and-nullability-is-destructive` | `ALTER COLUMN ... TYPE`/`SET NOT NULL` (not `ADD COLUMN`) is detected too |
| `orm-model-edit-detects-type-fk-and-unique-changes` | one edited SQLAlchemy model surfaces type, foreign-key, and unique-constraint changes from one diff |
| `unrelated-schema-change-produces-no-business-warning` | a new table unrelated to any mapped code stays informational, with no related product intent |
| `noop-migration-produces-no-schema-finding` | a migration with no recognizable DDL produces no schema finding |
| `reconciliation-detects-both-direction-divergence` | `ctx status` finds a migration-only column and an ORM-only column on the same table in one run |
| `consistent-schema-across-sources-resolves-to-one-entity` | migration + matching ORM model + static SQL access share one `DbEntity` with zero reconciliation divergence (a false-positive control) |
| `dynamic-tablename-orm-model-stays-unrecognized` | a dynamic `__tablename__` is never guessed as schema — the project's only "ambiguous ORM mapping" case |
| `explicit-schema-seed-does-not-pull-unrelated-lexical-roots` | an explicit migration seed excludes a lexically similar unrelated table |
| `column-level-impact-seed-narrows-to-column-readers` | `ctx impact table.column` resolves to the table and narrows to that column's specific readers/writers |

## Recorded baseline

Version 0.5.0 passes all 26 cases and all 113 checks:

- recall-shaped checks: 35/35;
- precision/noise checks: 40/40;
- classification checks: 22/22;
- budget checks: 5/5;
- harness errors: 0.

These are exact regression counts for a small synthetic fixture family. They are not calibrated precision/recall estimates and are not statistically significant.

## What this does and does not prove

The corpus proves that the checked deterministic policies remain reproducible across real temporary Git histories, Tree-sitter analysis, SQLite persistence, application services, review, impact, and context compilation. It has repeatedly caught graph-identity, traversal, and cross-file review defects.

It does not prove the product hypothesis from `product_conclu.md` sections 49–52. The following experiments still require external ground truth or participants and therefore cannot be honestly completed by repository automation alone:

- review precision on a labeled corpus of real historical PRs;
- impact-understanding time with and without `ctx`;
- coding-agent task success and token efficiency with and without Context Packs;
- human maintenance cost for verified semantic mappings over real repository history;
- kill-criteria evaluation after several real workflow iterations.

Future result documents should record repository/corpus selection, labeling protocol, participant or agent setup, raw per-case outcomes, false-positive/false-negative definitions, and limitations. A green synthetic corpus must never be reported as confirmation of those experiments.

`docs/evaluation-historical.md` is a first, partial step in that direction: it replays 16 real commits from this repository's own history (not synthetic fixtures) through `ctx review` and grades the results against independently re-derived ground truth. It is real historical data but not the external, third-party-repository corpus the experiments above require — see that document for its methodology and honest limits, including a real recall gap it found in `ctx review`.

## SQLite index benchmark

Run `scripts/bench-index.sh <ctx-binary>` from an initialized ctx checkout.
The script uses an isolated clone and reports the median of five runs.
On macOS, the full index starts at the parent of `dc99f75`. The incremental run
advances to `dc99f75`, then review compares the working tree with `HEAD~20`.

| Operation | 0.8.7 | SQLite changes only | 0.8.8 |
| --- | ---: | ---: | ---: |
| Full index | 2.184 s | 0.563 s | 0.647 s |
| Incremental index | 0.324 s | 0.215 s | 0.242 s |
| Review against `HEAD~20` | 0.607 s | 0.594 s | 0.244 s |

## Release 0.8.8 checks

The ten lexical Context Pack cases use the subscriptions fixture and a
1,000-token budget. Mean estimated tokens per symbol description fell from
34.76 to 20.54. Expected symbol recall increased from 9/10 to 10/10.
The full corpus passed all 133 checks in 36 cases.

A one-line change in `build_review_findings` at `53188bc` reduced text output
from 62 to 21 lines. Its six signals produced five unique intent groups.
Repeated Markdown reports for the same diff were byte-identical.

The destructive exit policy replayed 100 first-parent commits through
`53188bc`. Each review compared a commit to its first parent. The replay
produced no blocking findings or execution errors. A fixture with a dropped
column separately verified the blocking exit code.

## Decision point corpus

The [labeling guide](../crates/ctx-eval/corpus/LABELING.md) defines the labels and edge cases.
The operator pins revisions, creates samples, and starts draft runs.
A reviewer checks the code and cited evidence. The second review hides the draft and first label.
For this run, Codex performed both reviews as authorized. The procedure did not use independent human reviewers.

The commands below use the development-tool binary.

```sh
corpus() { cargo run --locked -p ctx-eval --bin ctx-corpus -- "$@"; }
```

1. Pin each project and its evidence repositories in `corpus/projects/`. Run `corpus fetch PROJECT`.
2. Run `corpus sample PROJECT`. Commit the v0 sample and fill order before changing the ranking rules.
3. Run `corpus draft PROJECT --agent codex`. Drafting reads only the code and evidence package for each point.
4. Run `corpus review PROJECT --reviewer NAME`. Use `corpus sample PROJECT --extend --fill` for fixed batches of ten.
   Continue to the 15/15 quota or 60 points. Record a shortfall when the cap is reached.
5. Run `corpus review PROJECT --reviewer NAME --second`. Then run `corpus agreement PROJECT --min 0.6`.
6. If agreement is low, resolve ambiguous rules in the labeling guide. Update its version and review all affected points.
7. Run `corpus check` and `corpus score --tuning`. Freeze the ranking before running `corpus score --gate`.
   If the precision pool is incomplete, add its listed anchors with `corpus sample PROJECT --extend ANCHOR...`.

Precision@5 uses reviewed labels from all sources, including the pool. The quota and trivial false-positive rate exclude the pool.

### 0.9.0 release verification

Release status is blocked. The first-party regression corpus passed 145 checks
across 48 cases, including 12 deterministic coverage scenarios. Two coverage
reports for the same indexed ctx revision matched byte for byte.

All three tuning projects reached the 60-point limit. The second review used
the same Codex session with the first labels hidden in the review interface.
Agreement from this procedure does not measure agreement between independent
reviewers.

| Project | Significant | Trivial | Unclear | Second labels | Cohen's κ |
| --- | ---: | ---: | ---: | ---: | ---: |
| Zulip | 6 | 14 | 40 | 12 | 1.00 |
| Mattermost | 3 | 31 | 26 | 12 | 1.00 |
| Hyperswitch | 0 | 4 | 56 | 12 | 1.00 |

Each project fell short of the 15/15 quota. Measured tuning precision@5 was
20.0% on rated points, with an incomplete precision pool. The trivial
false-positive rate was 32.7% (16 of 49). The held-out release score remained
incomplete. Agent quota exhaustion interrupted its draft phase.

The historical repair replay indexed the parent and result of each code change.
Safe planning recovered 0 of 8 previously edited links across the ingestion
move, the inference command move, and two renamed test groups. The original
code commits are `3592730`, `ce9d872`, `14f2752`, and `53d7e67`. The case
associated with `3a61aaa` retained the old wrapper and did not produce a broken
link. Synthetic moves and pure test renames succeeded. The automatic replay
result did not meet the 80% target.

#### Repair replay classification

The 0/8 result measures all historical link edits. It is not the R1–R2 recall
denominator. The cases below were classified from the source diffs before
comparing them with the repair plan.

| Source change | Link edits | Classification | Evidence |
| --- | ---: | --- | --- |
| `3592730` | 2 | Contract change | The move also replaced the CLI parameters with scoped ingestion options and changed dispatch. |
| `14f2752` | 3 | Changed test behavior | The replacement test checks the new `--safe-mode` argument and its prompt position. |
| `53d7e67` | 2 | Changed test behavior | Rejection of invalid or incomplete results changed to retry and keep usable decisions. |
| `ce9d872` | 1 | Move with local refactoring | The signature and type inference flow survive. Reporting code moved into helpers. |

Seven link edits exercise review, where automatic edits must stay at zero.
The remaining edit is a confirmed move with refactoring. Safe repair missed
that move. Keep it as a recall case and add independently classified pure
moves and renames before interpreting the 80% target.

#### Ranking v2 on tuning projects

Of the 16 trivial points selected for questions by v1, 15 had downstream
call weight. V2 requires a product-related condition, direct effect, SDK call,
or entrypoint signal before call graph size can contribute to a question.
The number of trivial points selected fell from 16/49 (32.7%) to 2/49 (4.1%).
Only four of 44 top positions had decisive labels. Three were significant.
The precision pool remains incomplete. The 15/15 quotas are unchanged.
The reviewer found four significant points that the drafts did not propose.
No held-out results informed this ranking change.
