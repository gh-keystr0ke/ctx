# Mine existing knowledge instead of writing it by hand

If a team already has commit history, code comments, or a GitLab project full of issues and merge requests, `ctx` can propose product-context documents from that instead of requiring everything to be authored from scratch.

```bash
ctx ingest git             # commit messages and branch names
ctx ingest code-comments   # comments and docstrings, attributed to their nearest symbol
ctx ingest gitlab          # issues, merge requests, and their comments — needs [gitlab] in .ctx/config.toml

ctx enrich --agent claude  # or --agent codex / --agent antigravity

ctx verify --knowledge     # review each proposed candidate; accept allocates its stable ID
```

Ingested artifacts are never product knowledge on their own — they are source material an agent may derive typed candidates from, stored separately (never as a graph node), and never automatically promoted. A candidate is never asserted until a human (or `--auto`, below) names it with `ctx verify --knowledge --accept --id <ID>`.

## Ingest

`ctx ingest <git|code-comments|gitlab>` normalizes artifacts into their own store, idempotently re-synced on every run. `--since <OID>` narrows `git` to commits after that point (branch names are always re-synced). `gitlab` stores a per-project sync cursor so a later run asks GitLab for only what changed since the previous one — a missing or reset cursor only costs a fuller re-fetch, never a wrong result.

## Enrich

`ctx enrich --agent claude|codex|antigravity` shells out to that agent's own CLI (`claude`, `codex`, `agy`) already on `PATH` — see [docs/configuration.md](configuration.md) for the override environment variables. No token or API key is read from `ctx` itself; each CLI handles its own authentication.

Each run analyzes one bounded artifact neighborhood at a time — the artifact's own linked artifacts, the code it touched, nearby tests, and already-mapped product knowledge — never the whole repository or backlog. Every evidence citation and implementation/test-candidate path is checked against that neighborhood; anything outside it, or malformed output altogether, is dropped or rejected, never trusted. `--allow-ungrounded-symbols` relaxes only the implementation/test-candidate check to allow the agent's own heuristic knowledge of the repository — evidence-artifact grounding stays strict either way.

An artifact whose content hasn't changed since its last analysis is skipped rather than re-sent to an agent every run, regardless of the previous outcome.

## Verify

`ctx verify --knowledge` lists pending candidates. Decide one at a time:

```bash
ctx verify --knowledge --accept <FINGERPRINT> --id REQ-SUB-014 --author jane
ctx verify --knowledge --reject <FINGERPRINT> --author jane
```

The pending candidate is already a Git-tracked file under `.ctx-candidates/` before any decision, so it isn't lost or duplicated across a team. Accepting writes an ordinary `.context/*.yaml` file through the same import path a hand-authored document uses — there is no second, parallel truth store — and the next `ctx index` absorbs it exactly like one. `ctx verify --knowledge --accept` refuses (unless `--force`) a statement that looks like a lexical restatement of an already-active document of the same kind, naming which one. `ctx explain` on the resulting document renders the full artifact → agent-inference → human-verification provenance chain.

The plain `ctx verify` (no `--knowledge`) reviews a different, older queue: deterministic heuristic implementation-link suggestions produced during indexing, decided the same way (`--accept`/`--reject <FINGERPRINT> --author <NAME>`).

## Bulk-review with `--auto`

Hand-reviewing hundreds of candidates from a large `ctx enrich` run doesn't scale. `ctx verify --knowledge --auto` has a review agent decide every pending candidate instead:

```bash
ctx verify --knowledge --auto --agent claude --id-prefix SUB
```

This is not a mechanical bulk-accept of everything extraction already called relevant — the review agent is a genuine second opinion that re-examines each candidate on its own merits and can still reject it. Candidates are first clustered so ones describing the same underlying flow are reviewed together; the agent returns a verdict for every candidate in a cluster plus, only when it judges two or more accepted candidates to genuinely restate the same knowledge, one merged document instead of one per candidate. A cluster the agent doesn't merge still becomes one document per accepted candidate, exactly like the human accept path, with a stable ID allocated under the required `--id-prefix` (`REQ-SUB-001`, `INV-SUB-001`, ...). A likely restatement of an already-active document is left pending unless `--force`, same as a human accept.

Every resulting decision is recorded as agent-made, never human — `ctx explain` renders it honestly as "Auto-verified". `--auto` prints progress to stderr per cluster (`[12/715] reviewing cluster (requirement, 3 candidates) via claude...`) followed by one line per resulting decision, and mirrors the same counter into the terminal tab title, since a real review call can take tens of seconds and silence across many clusters is otherwise indistinguishable from a hang.
