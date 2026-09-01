# Mine existing knowledge instead of writing it by hand

If a team already has commit history, code comments, or a GitLab project full of issues and merge requests, `ctx` can propose product-context documents from that instead of requiring everything to be authored from scratch.

```bash
ctx ingest git
ctx ingest gitlab --scope business-linked   # details only for MRs tied to this Git repository
ctx ingest jira --scope business-linked     # Jira keys only from Git/selected MRs; related depth defaults to zero
ctx ingest code-comments --reconcile        # complete HEAD snapshot; removes comments/docstrings gone from code

ctx artifacts prune --scope business-linked # dry-run the keep/prune plan
ctx artifacts prune --scope business-linked --apply

ctx enrich --scope business-linked --agent claude  # or codex / antigravity

ctx verify --knowledge     # review each proposed candidate; accept allocates its stable ID
```

Ingested artifacts are never product knowledge on their own — they are source material an agent may derive typed candidates from, stored separately (never as a graph node), and never automatically promoted. A candidate is never asserted until a human (or `--auto`, below) names it with `ctx verify --knowledge --accept --id <ID>`.

## Ingest

`ctx ingest <git|code-comments|gitlab|jira>` normalizes artifacts into their own store, idempotently re-synced on every run. `--since <OID>` narrows `git` to commits after that point (branch names are always re-synced). The default `gitlab` mode stores a per-project sync cursor and ingests issues plus MRs. `ctx ingest gitlab --scope business-linked` deliberately does something narrower: it relists MR summaries, selects only those connected to current Git by source branch, commit SHA, or an explicit `!IID`, and fetches comments/commits only for the selected MRs. It does not fetch GitLab issues or advance the default-mode cursor.

`jira` doesn't use a cursor at all: since a Jira project can span many repositories, it never fetches "the whole project, or everything changed since last time." In business-linked mode, every run derives ticket keys (`PSI-1122`) only from current Git, selected MRs, and their comments. Old Jira rows and unrelated GitLab issues cannot seed or perpetuate the set. `--related-depth 0` admits only directly referenced issues; increase it explicitly to follow a bounded number of Jira-reported `issuelinks`/`parent` hops.

`ctx ingest code-comments --reconcile` treats the successfully read/analyzed HEAD as a complete snapshot. Comments and docstrings absent from that snapshot are removed together with their local links and analysis rows; a read/analyzer failure happens before deletion and preserves the previous snapshot.

## Prune oversized artifact stores

`ctx artifacts prune --scope business-linked` applies the same deterministic planner to everything already stored. It is a dry run by default and reports how many artifacts would be kept/pruned. Add `-v` for counts grouped by reason, `-vv` for every identity and reason, or `--json` for the full decision list. Only `--apply` performs deletion, atomically removing the named artifacts with their links and analysis-ledger rows. Repeating apply is idempotent.

Prune never edits `.context/` or `.ctx-candidates/`. A pending candidate that cites evidence in the prune set is reported for follow-up, not silently deleted. This makes the safe cleanup sequence explicit: strict GitLab/Jira ingestion, reconciled code comments, dry-run review, apply, then strict enrichment.

## Enrich

`ctx enrich --agent claude|codex|antigravity` shells out to that agent's own CLI (`claude`, `codex`, `agy`) already on `PATH` — see [docs/configuration.md](configuration.md) for the override environment variables. No token or API key is read from `ctx` itself; each CLI handles its own authentication.

The default `--scope all` keeps the legacy one-artifact-neighborhood behavior. `ctx enrich --scope business-linked` instead makes a retained Jira issue the only valid agent subject and assembles one bounded Jira↔MR↔commit→symbol/test bundle around it. Linked artifact bodies are included, so the agent receives the business requirement together with its implementation evidence; a branch name, one commit message, or an MR without Jira context is never sent alone. Every evidence citation and implementation/test-candidate path is checked against that bundle; anything outside it, or malformed output altogether, is dropped or rejected, never trusted. `--allow-ungrounded-symbols` relaxes only the implementation/test-candidate check — evidence-artifact grounding stays strict.

The analysis ledger fingerprints the exact rendered prompt, not only the Jira body. A changed MR/commit, link, symbol/test set, prompt rule, or grounding mode triggers re-analysis; an identical prompt is skipped regardless of whether the previous outcome was relevant.

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
