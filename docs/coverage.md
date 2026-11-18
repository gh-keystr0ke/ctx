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
