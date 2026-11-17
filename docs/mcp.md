# MCP integration

Initialize the repository with `ctx init`.
Set the absolute path to `ctx` and the working directory in the MCP client configuration.

```json
{
  "mcpServers": {
    "ctx": {
      "command": "/absolute/path/to/ctx",
      "args": ["serve", "--mcp"],
      "cwd": "/absolute/path/to/repository"
    }
  }
}
```

The server exposes six tools over newline-delimited stdio JSON-RPC.

| Tool | Purpose | Same as |
| --- | --- | --- |
| `get_status` | Inspect index freshness and coverage. | `ctx status` |
| `get_context` | Compile bounded evidence-backed product and code context for a coding task. | `ctx context` |
| `get_impact` | Find bounded product intent, implementation, and tests related to a file, symbol, or stable ID. | `ctx impact` |
| `explain_relation` | Explain a node or a directed `source -> target` relationship from stored provenance and evidence. | `ctx explain` |
| `find_requirements` | Find indexed requirements by stable ID or lexical terms. | `ctx find --kind requirement` |
| `review_change` | Review a Git branch or working-tree diff using high-confidence product claims. | `ctx review` |

All graph and review decisions remain in `ctx-app`; MCP is a thin transport adapter, not a second implementation. There is no MCP tool for `ctx ingest`/`ctx enrich`/`ctx verify` or federation — those are explicit, side-effecting operations run from the CLI.

## Index freshness

MCP refreshes the local SQLite cache when HEAD advances and the index inputs have no uncommitted changes.
MCP never modifies Git or `.context/`.
If refresh fails or takes too long, the response includes a warning and the indexed commit.
The `get_status` tool only inspects the index.

Set `[index] auto = "never"` in `.ctx/config.toml` to disable automatic refresh.
The default `"mcp"` limits automatic refresh to MCP.
Use `"always"` to enable automatic refresh for CLI queries too.
Use `--refresh` to request a one-time refresh from the CLI.

Tool results include an `index` object with `commit`, `head`, and `state`.
Tools that return a list put the list in `matches`.

## Configure a coding agent

Install ctx and run `ctx init` in the checkout.
Run `ctx setup claude`, `ctx setup codex`, or `ctx setup antigravity`.
Setup adds the MCP server and the bundled ctx skill to the checkout.
Use `--dry-run` to see the proposed file changes.

Use `--hooks` to enable review of staged changes before commits.
With Claude Code, stage your changes before invoking `git commit`.
For Codex or Antigravity, setup installs a Git pre-commit hook.
The hook pauses once for each set of invariant or destructive findings.
Review the findings before you retry.

MCP review uses the merge base with the default branch when you omit `base`.
Use `ctx review --base auto` to select the same base on the CLI.
