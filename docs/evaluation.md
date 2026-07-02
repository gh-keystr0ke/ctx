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

Version 0.4.0 passes all 25 cases and all 102 checks:

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
