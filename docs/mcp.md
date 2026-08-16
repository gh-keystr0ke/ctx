# MCP integration

Initialize and index the repository before starting the server — the server is read-only and answers from whatever is already indexed. Configure a local MCP client with an absolute executable path and the repository as the working directory:

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

The server exposes exactly five tools over newline-delimited stdio JSON-RPC, supporting both current and legacy MCP discovery/initialization clients:

| Tool | Purpose | Same as |
| --- | --- | --- |
| `get_context` | Compile bounded evidence-backed product and code context for a coding task. | `ctx context` |
| `get_impact` | Find bounded product intent, implementation, and tests related to a file, symbol, or stable ID. | `ctx impact` |
| `explain_relation` | Explain a node or a directed `source -> target` relationship from stored provenance and evidence. | `ctx explain` |
| `find_requirements` | Find indexed requirements by stable ID or lexical terms. | `ctx find` (scoped to requirements) |
| `review_change` | Review a Git branch or working-tree diff using high-confidence product claims. | `ctx review` |

All graph and review decisions remain in `ctx-app`; MCP is a thin transport adapter, not a second implementation. There is no MCP tool for `ctx ingest`/`ctx enrich`/`ctx verify` or federation — those are explicit, side-effecting operations run from the CLI.
