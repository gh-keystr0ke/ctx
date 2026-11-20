# Context coverage

Run `ctx coverage` to see how much indexed code has product-context mappings.
The structural percentage divides mapped symbols by analyzed symbols. Both counts exclude test symbols.
A mapping counts if an active assertion links the symbol to an active requirement, invariant, or decision.

Behavioral decisions have separate counts. Their ranking uses syntax, calls, visible effects, and existing mappings.
The report lists decision states, stale anchors, and source files the index did not analyze.

## Set the scope

- `ctx coverage --path src/payments` selects a file or directory.
- `ctx coverage --symbol payments.retry` selects an indexed symbol.
- `ctx coverage --requirement REQ-RETRY-001` selects mapped implementation.
- `ctx coverage --base main` limits the report to changes since that revision.
- `ctx coverage --json` produces a reproducible report for the same index, scope, and coverage map.

## How ctx finds and ranks decision points

### Extract candidates from syntax

The Python, Rust, and Go adapters walk the syntax tree for each supported symbol.
The coverage report excludes test symbols. Research can still use tests as evidence.

| Candidate | Examples |
| --- | --- |
| Branch or guard | An `if`, Python `elif`, or condition that returns early |
| Case | A Rust `match` arm or a Go `switch` case |
| Error handling | A Python `except` clause or a Rust `?` expression |
| Default | A Python parameter with a default value |
| Literal return | A return of a number, string, boolean, or `None` |
| Constant | A Rust or Go constant declaration |

Each candidate records its source range, condition or pattern, and the regions it controls.
For example, a guard can control both its early return and the code it skips.
The adapter records calls associated with these regions when it can identify them.

An anchor contains the symbol's stable key, a BLAKE3 hash of the candidate subtree,
and the occurrence number among identical fingerprints in that symbol.
The hash uses syntax node kinds and leaf text and skips comments.
Line numbers locate candidates in the report. They are outside the anchor.
A code change that alters the fingerprint can leave a stale anchor.
If the symbol still matches, a new candidate in that symbol can be marked reopened.

[Syntax extraction](../crates/ctx-adapters/src/decisions.rs) ·
[Anchor construction](../crates/ctx-core/src/coverage.rs)

### Score candidates with ranking v2

| Contribution in JSON | Score |
| --- | --- |
| `condition` | +4 if the condition contains any of `attempt`, `retry`, `limit`, `quota`, `status`, `state`, `permission`, `role`, `expired`, `timeout`, `amount`, `price`, or `paid`. The match ignores case and uses substrings. |
| `direct_effects` | +5 per indexed database or ORM write, +2 per read, and +5 per external HTTP call in controlled regions. Maximum 12. |
| `branch_asymmetry` | Difference between the largest and smallest direct effect totals across controlled regions. Requires two or more regions. Maximum 4. |
| `downstream_calls` | Traverses up to three levels from callees that resolve uniquely. Each visited symbol adds weight, with less weight for effects deeper in the graph. Maximum 8. |
| `entrypoint_reach` | +1 per indexed entry point reachable through up to three caller steps, including the symbol itself. Maximum 3. |
| `external_sdk_calls` | +3 per recognized external SDK call associated with the point. Maximum 6. |
| `mapped_intent` | +1 if an active mapping connects the symbol to a requirement, invariant, or decision. |
| `decision_shape` | +1 for a guard or a parameter default. |

The score is the sum of contributions. ctx sorts by descending score and uses the anchor to break ties.
A candidate qualifies for a question if it scores at least 4 and has a condition,
direct effect, external SDK call, or entry-point contribution.
Call graph weight alone cannot qualify a candidate.

ctx suppresses recognized routine patterns even if they meet the score threshold.
Examples include Python type checks, selected empty-value checks with a bare return or continue,
Rust `?`, and Go checks that propagate the same error with standard zero values.
These rules use syntax and indexed evidence. They can miss behavior expressed through unrecognized patterns.

[Ranking weights](../crates/ctx-core/src/coverage/ranking.rs) ·
[Question threshold](../crates/ctx-core/src/coverage.rs)

### Example

```python
def retry_delay(attempt):
    if attempt == 0:
        return None
    return 1
```

The guard receives 4 points for `attempt` and 1 for its early return, before any other contributions.
The extractor also sees the two literal returns. In isolation, they contribute no points.
A requirement to support retries can map to this function without explaining what `None` means.
A current invariant that explicitly explains the first attempt can resolve the gap.

### Determine the state of a point

The gap report includes candidates that qualify for a question, candidates with an exact
document anchor, and candidates with a saved record.
A single active document with the exact anchor marks the point as explained.
Multiple active documents with the same anchor mark it as ambiguous.
If a saved record's referenced document or relevant linked context changes, ctx can reopen the point.

The structural percentage measures symbol mappings. Behavioral results are counts by state.
The ranking score estimates which candidates merit research.
It gives no measure of correctness or behavioral coverage across the whole program.

[Gap states](../crates/ctx-core/src/coverage/state.rs) · [Corpus evaluation](evaluation.md#decision-point-corpus)

## Research and answer gaps

1. Index the current commit with `ctx index`.
2. Run `ctx coverage research --agent codex --max-questions 5`.
3. Run `ctx coverage ask` to review unresolved points.
4. Run `ctx verify --knowledge` to review any new candidate.
5. Commit the accepted documents, then run `ctx index`.

Research sends a bounded evidence package to the selected CLI agent.
The recorded result includes the cited excerpts, prompt fingerprint, and configured model.
A checked citation to a current linked assertion can resolve a gap.
Agent and model failures leave the deterministic report available.

### Evidence and trust

The research package includes code around the point, up to six linked context documents,
up to six neighboring code or test excerpts, up to three Git history subjects, and
up to four indexed artifacts. It can also include a source named during the interview.
The agent must cite an evidence ID and quote text from its excerpt.

| Agent judgment | Meaning |
| --- | --- |
| `Established` | The agent identifies a current assertion that explains the behavior. |
| `Inferred` | The explanation relies on indirect evidence, such as code or tests. |
| `Ambiguous` | The evidence supports multiple interpretations. |
| `Conflicting` | Sources disagree about the behavior. |
| `Unknown` | The evidence is insufficient. |

ctx checks that each quoted string appears in the cited excerpt.
Automatic resolution requires an established explanation and exactly one current linked assertion
among the citations. An established response without that evidence is downgraded.
A suspected code-to-context conflict requires both code and assertion citations. It stays open for review.
The citation check validates the evidence references. The agent still interprets their meaning.

[Evidence collection](../crates/ctx-cli/src/coverage_command/research.rs) ·
[Citation validation](../crates/ctx-core/src/coverage/research.rs)

### Interview responses

Use `answer` for free text, `choose` to select a hypothesis, and `correct` to replace a hypothesis.
Use `source` to give a context ID or source path for the next research run.
Use `unknown`, `defer`, `dismiss`, and `reject` to record a disposition.
Use `--agent` on `ask` to research before the interview.
The default limit is five questions. Set `--max-questions` to change it.
Set `--max-follow-ups` to 0, 1, or 2.

## Share coverage state

Run `ctx coverage sync` to save point states without an agent call.
The active context store holds these records in `.ctx-coverage/`. Commit this directory to share state.
Existing invariants and decisions can include an optional `anchors` list.
Use the `anchor` object from JSON output as a list entry.

An anchor has a stable symbol key, syntax fingerprint, and ordinal.
A change to the point or its associated context can reopen a question.
A conflict between Git versions requires review if both sides changed the same record.
Run `ctx coverage merge ANCESTOR OURS THEIRS` to merge one map file into `OURS`.

`ctx explain` shows decision anchors and states. Context Packs include current anchored explanations.
Verbose review reports new unexplained decisions. HTML and MCP also expose coverage.
Suspected divergence appears on coverage reports and the `get_coverage` MCP tool.
