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
An independent agent performs the blind review for this release, as authorized.
When the same reviewer does both passes, wait at least one week before the second pass.

The commands below use the development-tool binary.

```sh
corpus() { cargo run --locked -p ctx-eval --bin ctx-corpus -- "$@"; }
```

1. Pin each project and its evidence repositories in `corpus/projects/`. Run `corpus fetch PROJECT`.
2. Run `corpus sample PROJECT`. Commit the v0 sample and fill order before changing the ranking rules.
3. Run `corpus draft PROJECT --agent codex`. Drafting reads only the code and evidence package for each point.
4. Run `corpus review PROJECT --reviewer NAME`. Use `corpus sample PROJECT --extend --fill` for fixed batches of ten.
   Continue to the 15/15 quota or the default cap of 60 points. Record any shortfall.
   Use the [extension procedure](#extension-to-the-frozen-sample) for an approved higher cap.
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
link. Synthetic moves and pure test renames succeeded. The original eight-link replay mixed mechanical and behavioral changes.
See the classified replay below for the expanded measure.

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
that move.

The expanded replay adds nine historical moves with unchanged function
bodies. The parent and resulting source revisions were indexed in separate
Git clones. For `f52a3e5`, the old context file was restored before applying
the repair to replay the broken link.

| Code change | Eligible links | Correct automatic repairs |
| --- | ---: | ---: |
| `f52a3e5` | 1 | 1 |
| `4137b33` | 8 | 8 |
| `ce9d872` | 1 | 0 |
| Total | 10 | 9 |

Recall for the expanded R1 replay was 90%. Each applied link matched its
expected successor. The original seven behavior-changing links had no
automatic replacements. The synthetic regression covers R2 and an ambiguous
same-body case. This historical sample has no pure test rename.

#### Ranking v2 on tuning projects

Of the 16 trivial points selected for questions by v1, 15 had downstream
call weight. V2 requires a product-related condition, direct effect, SDK call,
or entrypoint signal before call graph size can contribute to a question.
The number of trivial points selected fell from 16/49 (32.7%) to 2/49 (4.1%).
Only four of 44 top positions had decisive labels. Three were significant.
The precision pool remains incomplete. The 15/15 quotas are unchanged.
The reviewer found four significant points that the drafts did not propose.
No held-out results informed this ranking change.

All 23 previously unsampled tuning positions were added and reviewed.
After this pool extension, 10 of 44 positions had decisive labels, eight
significant and two trivial. The other 34 require stronger evidence.
Precision@5 on rated points was 80%, with only 22.7% of positions rated.
The 80% pool completeness gate therefore still blocks interpretation of that
precision figure. Pool labels did not change the quota or false-positive rate.

#### Next release verification pass

A second search found the invitation expiry rule in the pinned Mattermost
guide. Mattermost now has four significant points. The second label remains
unchanged, so agreement is κ = 0.846 on 12 pairs. The tuning result covers
11 of 44 top positions. Nine are significant and two are trivial.
The trivial false-positive rate remains 2/49.

| Tuning project | Additional significant labels needed | Additional trivial labels needed |
| --- | ---: | ---: |
| Zulip | 9 | 1 |
| Mattermost | 11 | 0 |
| Hyperswitch | 15 | 11 |

1. Search pinned docs by user-facing behavior, configuration names, and API
   terms. Follow linked issues from the commit that changed each decision.
2. Review the existing samples and record evidence for each changed label.
   Keep the original second labels for the agreement report.
3. Run `ctx-corpus score --tuning`. At the current ranking, 25 more decisive
   labels are required to reach 36 of 44 positions.
4. If the fixed sample still falls short, report the remaining counts for
   approval of a new sampling protocol. Preserve the current corpus as a
   baseline. Freeze any replacement sampling protocol before labeling.
5. Freeze the final ranking and run `ctx-corpus score --gate`. Resolve missing
   labels through the pool workflow. Check agreement, quotas, precision,
   false positives, and anchor resolution.
6. After the release checks pass, set version 0.9.0 and run release CI.
   Publish the tag and artifacts from the validated commit.

#### Pinned-source follow-up

A second pass found five more significant points in pinned project guides.
Four count toward the class quotas. One pool point counts toward precision.
The tuning scorer rated 12 of 44 positions and returned `incomplete`.
The required count is 36. The false-positive rate remains 2/49.

| Tuning project | Significant | Trivial | Rated top positions | Cohen's κ |
| --- | ---: | ---: | ---: | ---: |
| Hyperswitch | 2 | 4 | 0/10 | 0.636 |
| Mattermost | 5 | 31 | 4/15 | 0.846 |
| Zulip | 7 | 14 | 8/19 | 0.810 |

A new protocol would need approval if the fixed sample still falls short.
The least disruptive change is to extend the frozen v0 order beyond 60.
Set a new cap before collecting labels. Preserve the original 60-point
results so the change in sample size is visible. Continue to exclude pool
points from the quotas and false-positive rate.

A larger sample can address the quota shortfall. The precision pool still
requires evidence for the actual top positions. Keep unknown outcomes in
the completeness denominator, and keep the held-out projects sealed during
calibration.

#### Held-out v2 gate

All 180 anchors resolved on the pinned commits. The scorer returned
`complete`. Each project met the 80% threshold for rated top positions.

| Project | Rated top positions | Required | Significant for quota | Trivial for quota | Quota |
| --- | ---: | ---: | ---: | ---: | --- |
| Nova | 16/20 | 16 | 14 | 8 | Underfilled |
| Vector | 16/19 | 16 | 15 | 15 | Met |
| Gitea | 6/7 | 6 | 4 | 23 | Underfilled |

Each project reached the 60-point cap. The fill batches used the order
frozen at the first sample. Pool points count toward precision only.

Combined precision was 38/38 among the rated top positions. The trivial
false-positive rate was 0/46. Nova needs one more significant label and
seven more trivial labels. Gitea needs 11 more significant labels.
The held-out projects still need a second review to measure agreement.

#### Extension to the frozen sample

The original 60-point results are recorded in Git at commit `cfd1b65`.
For the quota extension, use a cap of 120 total points per project,
including any pool points. Keep the pinned commits, modules, seed, and v0 ranking.
Keep the existing labels and the original selection.

Run `ctx-corpus sample PROJECT --extend --fill --fill-cap 120` to append
the next batch. The command reproduces v0 and checks the initial sample
and the saved fill prefix before it extends the order. It skips anchors
already selected and adds up to ten points. Review the batch before
adding more. Stop at the quota, the cap, or the end of the order.
Report any remaining shortfall.

The extension met the quota for all three held-out projects. Nova
stopped at the end of its first batch that met the quota. Vector had
already met the quota at the original cap. Gitea reached the new cap.

| Project | Total points | Significant for quota | Trivial for quota | Quota |
| --- | ---: | ---: | ---: | --- |
| Gitea | 120 | 15 | 45 | Met |
| Nova | 100 | 17 | 16 | Met |
| Vector | 60 | 15 | 15 | Met |
| Hyperswitch | 120 | 2 | 7 | Underfilled |
| Mattermost | 120 | 7 | 47 | Underfilled |
| Zulip | 120 | 8 | 20 | Underfilled |

All 280 held-out anchors resolved. The held-out scorer returned
`complete`. Precision was 38/38 across 46 top positions. The trivial
false-positive rate was 1/76. The tuning scorer returned `incomplete`.
The expanded samples still require a second review for agreement.

#### Evidence review after the extension

A source review confirmed eight more tuning labels. Three count toward
the class quotas. The other five are pool points. The tuning scorer
still returned `incomplete`, with 24 upper positions unrated.

| Tuning project | Significant for quota | Trivial for quota | Unrated top positions |
| --- | ---: | ---: | ---: |
| Hyperswitch | 3 | 7 | 9 |
| Mattermost | 9 | 47 | 5 |
| Zulip | 8 | 20 | 10 |

The held-out labels and v2 ranking stayed fixed during this review.

#### Release build rehearsal

A detached worktree built `ctx` and `ctx-mcp` as version 0.9.0 with the
locked dependencies on macOS ARM64. The local release archive passed its
SHA-256 check. The CLI reported version 0.9.0 and exposed the coverage and
repair commands.

A follow-up source review confirmed deferred payment method creation in
Hyperswitch. Its quota counts are now four significant and seven trivial
points. The tuning scorer still returned `incomplete`, with 23 unrated
top positions. The held-out result remains complete. Agreement on the
expanded samples is pending.

#### Completed tuning precision pool

The v2 tuning scorer returned `complete`. The source review confirmed
16 more top positions. Eight of the 16 count toward the class quotas.
The ranking and held-out labels stayed fixed.

| Tuning project | Rated top positions | Required | Significant for quota | Trivial for quota |
| --- | ---: | ---: | ---: | ---: |
| Hyperswitch | 8/10 | 8 | 10 | 7 |
| Mattermost | 13/15 | 12 | 9 | 47 |
| Zulip | 16/19 | 16 | 10 | 20 |

Tuning precision was 35/37 among rated positions. The trivial
false-positive rate was 2/74. The per-project class quotas remain
underfilled at the 120-point cap.

#### Final class quotas

Review of the pinned sources resolved 24 unclear points within the frozen
tuning samples. Both v2 score runs returned `complete` without lost anchors.

| Project | Role | Significant | Trivial | Rated top positions | Second-pass sample |
| --- | --- | ---: | ---: | ---: | ---: |
| Hyperswitch | Tuning | 15 | 15 | 8/10 | 24 |
| Mattermost | Tuning | 15 | 47 | 13/15 | 24 |
| Zulip | Tuning | 15 | 20 | 16/19 | 24 |
| Gitea | Gate | 15 | 45 | 6/7 | 24 |
| Nova | Gate | 17 | 16 | 16/20 | 20 |
| Vector | Gate | 15 | 15 | 16/19 | 12 |

Tuning precision was 35/37. Two of 82 trivial tuning points triggered a
question. Gate precision was 38/38. One of 76 trivial gate points triggered
a question.

The final first pass ended on 2026-11-20. An independent second reviewer
can start immediately. The preliminary second labels remain in Git history.

#### Independent agreement, round 1

The independent reviewer used the blind CLI on the final 128-point
sample. The minimum Cohen's kappa is 0.60 for each project.

| Project | Paired points | Disagreements | Cohen's kappa |
| --- | ---: | ---: | ---: |
| Gitea | 24 | 10 | 0.394 |
| Hyperswitch | 24 | 6 | 0.478 |
| Mattermost | 24 | 6 | 0.642 |
| Nova | 20 | 5 | 0.590 |
| Vector | 12 | 1 | 0.867 |
| Zulip | 24 | 5 | 0.667 |

The release remains blocked. Most discrepancies concern how to classify
routine checks, error propagation, and conversions. A smaller set concerns
whether a source states the exact routing, response, or scheduling rule.
The next agreement round requires a clarified guide and a new sample.

#### Independent agreement, round 2

Guide version 2 clarifies error propagation, internal no-ops, equivalent
representations, and evidence for a specific behavior. Reviewers updated 33
primary labels in the frozen corpus. The ranking remains v2.

A fresh reviewer labeled 128 points from the guide version 2 draw. The
draw differs from round 1, so differences in kappa include sample effects.

| Project | Paired points | Disagreements | Cohen's kappa |
| --- | ---: | ---: | ---: |
| Gitea | 24 | 2 | 0.855 |
| Hyperswitch | 24 | 6 | 0.556 |
| Mattermost | 24 | 2 | 0.852 |
| Nova | 20 | 8 | 0.231 |
| Vector | 12 | 7 | 0.067 |
| Zulip | 24 | 5 | 0.665 |

Both score runs returned `complete`, and all six class quotas passed.
Gate precision was 38/38, with one question among 82 trivial points.
Tuning precision was 35/38, with 15 questions among 103 trivial points.
Hyperswitch accounted for 13 of those 15 tuning questions.

The release remains blocked on agreement. Review the link from each
disputed assertion to its exact condition and effect. The second reviewer
used some living documentation in addition to pinned sources. Verify
those citations against a fixed revision before relying on them for
adjudication.
