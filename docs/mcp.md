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
| `find_requirements` | Find indexed requirements by stable ID or lexical terms. | `ctx find` (scoped to requirements) |
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
