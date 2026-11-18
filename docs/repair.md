# Repair product-context mappings

Run `ctx repair plan` to inspect worktree validation issues and indexed drift.
Add `--json` for machine-readable output. Add `--fail-on drift` for a nonzero status when drift exists.

## Apply proven changes

1. Index the code commit that changes the symbol.
2. Run `ctx repair plan` and inspect the replacement and its commit.
3. Run `ctx repair apply --policy safe`.
4. Review the file changes, then commit and index them.

The safe policy covers links with one successor supported by committed identity history.
It changes symbol links, preserves document wording, and records the rule and commit in a comment.
It validates the proposed mapping changes with a worktree check and a temporary preview index.
Safe edits support simple block YAML symbol entries. Other layouts require manual repair.

Add `--commit` to make a commit on a new `ctx/repair-*` branch in the context repository.
This option requires a clean context worktree before the repair.

## Review proposals

Run `ctx repair propose --agent codex` to propose changes from bounded code and context evidence.
Stale claim review uses the same verification flow as `ctx verify --stale`.
Run `ctx repair propose` to queue deterministic merge and retirement proposals without an agent.

Run `ctx repair review` to accept, edit, reject, or defer proposals by document.
A merge shows both document bodies. Provide the final wording when you accept it.
The merge combines code links and marks the second document superseded.

The default policy is:

```toml
[repair]
auto_apply = ["identity-rename", "test-rename"]
agent_auto = false
max_proposals = 20
```

If the committed configuration enables `agent_auto`, `ctx repair review --auto` can apply R3 and R8 link proposals.
These links keep the agent's provenance and enter the graph as inferences.

Run `ctx hooks install` to enable the background plan after commits. Check the latest result with `ctx status`.
