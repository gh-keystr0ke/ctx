# Docker

Build the image and run against the current repository as your host user, so files it writes aren't root-owned:

```bash
docker build -t ctx:local .
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/workspace" ctx:local init
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/workspace" ctx:local index
```

Any command works the same way — mount the repository at `/workspace` and append the `ctx` subcommand.

## Compose

`compose.yaml` provides the same workflow without repeating the mount/user flags:

```bash
export CTX_REPOSITORY="$PWD" CTX_UID="$(id -u)" CTX_GID="$(id -g)"
docker compose run --rm ctx status
docker compose run --rm ctx review --base main
```

The `mcp` service, gated behind the optional `mcp` Compose profile, starts `ctx serve --mcp` with stdin attached, for an MCP client that expects a long-running container instead of a local binary:

```bash
docker compose --profile mcp run --rm mcp
```
