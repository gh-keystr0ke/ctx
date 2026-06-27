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

## Recorded baseline

Version 0.3.0 passes all 13 cases and all 67 checks:

- recall-shaped checks: 19/19;
- precision/noise checks: 28/28;
- classification checks: 16/16;
- budget checks: 4/4;
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
