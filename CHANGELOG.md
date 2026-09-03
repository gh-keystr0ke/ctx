# Changelog

All notable changes to `ctx` are documented here. The project follows semantic versioning.

## Unreleased

## 0.7.5 — 2026-09-03

### Fixed

- `ctx index` no longer fails with an `edges.repository_id, edges.fingerprint, edges.valid_from` uniqueness violation when an analyzer-version change reparses unchanged files at the already-indexed Git commit. Same-commit structural edges are now updated idempotently together with their derivation and static evidence, while preserving the one-current-edge-per-fingerprint invariant.

## 0.7.4 — 2026-09-02

### Added

- The Python adapter recognizes import-gated `SQLAlchemy` expression calls — `select(Model)`, `select(Model.a, Model.b)`, `insert(Model)`, `update(Model).values(...)`, and `delete(Model)` — and resolves the referenced model only by an exact imported/same-file canonical path or one unique schema-bearing symbol sharing its bare name, never by naming convention or dataflow aliasing. Resolved accesses join the same `DbEntity` a table's declarative model or migrations already produce, so `ReadsFrom`/`WritesTo` edges, impact analysis, and `ctx explain` now cover ORM expression-language queries, not just literal SQL.

## 0.7.3 — 2026-09-01

### Fixed

- CLI-agent subprocess startup now retries transient executable-busy errors with a finite budget, preventing intermittent Linux failures when an executable has just been written or replaced while preserving immediate failures for every other spawn error.
- Strict rustdoc builds no longer fail when public documentation names private implementation constants or helper functions.

## 0.7.2 — 2026-09-01

### Added

- `--scope business-linked` for GitLab/Jira ingestion and enrichment. GitLab fetches details only for MRs tied to current Git; Jira keys come only from Git and selected MRs/comments, with `--related-depth 0` by default. Enrichment sends one Jira-anchored Jira↔MR↔commit→symbol/test bundle per agent call and never submits an unanchored branch, commit, or MR alone.
- `ctx artifacts prune --scope business-linked`, dry-run-first cleanup of already-stored artifacts with deterministic keep/prune reasons, `--apply` for atomic deletion, `-v` reason counts, `-vv` identity details, full JSON plans, and warnings for pending candidates whose evidence intersects the prune set.
- `ctx ingest code-comments --reconcile` for treating successfully analyzed HEAD comments/docstrings as a complete snapshot and removing entries no longer present.

### Changed

- Incremental enrichment now fingerprints the exact rendered prompt — including linked artifact bodies, changed symbols, nearby tests, prompt rules, and grounding mode — instead of only the subject content hash.
- Destructive artifact maintenance is isolated behind its own storage port. Reconciliation/pruning removes incoming and outgoing links plus analysis rows before artifacts in one transaction, while `.context/` and `.ctx-candidates/` remain untouched.

### Fixed

- Jira tasks already stored for a shared Jira project can no longer seed repository-scoped ingestion themselves, preventing unrelated repositories' business context from accumulating in the local artifact store.

## 0.7.1 — 2026-09-01

### Added

- `-v`/`-vv`/`-vvv` (repeatable, global) select progressively richer terminal diagnostics on stderr without corrupting `--json` stdout: `-v` reports command and ingest lifecycle, `-vv` adds internal stages and external-call metadata, `-vvv` adds timings and raw request/response data. `--debug` independently records the full `-vvv`-equivalent trace as timestamped JSONL under `.ctx/logs` (Git-ignored, `0600` permissions). Covers ingest, Git, HTTP retry, GitLab, Jira, SQLite, indexing, and AI-agent subprocess calls.
- `--siga-siga` (global) paces AI-agent calls made by `ctx enrich`, `ctx verify --knowledge --auto`, and `ctx verify --stale`: the first request runs immediately, then at least 30s between requests, with a 15m pause after 30m of continuous calling.
- OpenAPI 3.0/3.1 support: conventional `openapi.yaml`/`openapi.yml`/`openapi.json` files are discovered automatically during `ctx index` regardless of configured `languages` or source include paths (normal excludes still apply). Every path operation for `GET`/`POST`/`PUT`/`DELETE`/`PATCH`/`HEAD`/`OPTIONS`/`TRACE` becomes a language-neutral `ApiEndpoint`, retaining `operationId`, descriptions, tags, deprecation, effective security and servers, path/query/header/cookie parameters, request-body content, and response content/schema metadata; local `$ref`s are followed. OpenAPI operations flow through the ordinary index/`ctx review` API-findings/`ctx export` federation pipeline, and are exported as public contracts even without a matching product document. When a code handler and an OpenAPI operation describe the same `(method, path)`, the OpenAPI contract is authoritative and the two export as one merged endpoint, keeping the real code handler as the trace target when one exists and combining evidence from both. See [docs/api-contracts.md](docs/api-contracts.md#openapi-specifications).

### Fixed

- `ctx enrich` now checkpoints each agent result as soon as it succeeds, instead of only writing candidates after every artifact in the batch finished — a later failure no longer discards already-produced results.
- AI agent responses wrapped in Markdown/prose around a JSON payload are now recovered by a JSON-aware scanner instead of requiring the response to be bare JSON; malformed, truncated, ambiguous, and schema-invalid JSON now fail with an explicit, distinguishing error instead of a generic parse failure.

## 0.7.0 — 2026-09-01

### Fixed

- `ctx enrich --model <name>` (and `ctx verify --auto`/`--stale --model <name>`) accepted the flag and recorded it into the resulting candidate's provenance, but never actually put it on the agent CLI's argv — the `claude`/`codex`/`agy` subprocess silently ran whichever model the CLI defaults to while `ctx explain` kept claiming the requested model produced the output. `--model` now reaches the real subprocess call for all three agents.
- `ctx enrich --agent claude` (and the `claude` `--auto`/`--stale` review path) now runs `claude` with `--safe-mode`, skipping CLAUDE.md/skills/plugins/hooks/MCP-server/custom-agent loading that a single self-contained prompt-in/JSON-out call never needed — cutting context spent on unrelated project configuration on every call, without affecting OAuth/keychain-based auth.

## 0.6.2 — 2026-09-01

### Fixed

- `ctx ingest gitlab` always failed with `HTTP 400`: the GitLab issues/merge-requests list request sent `order_by=iid`, a value GitLab's REST API has never accepted. Dropped it (`sort=asc` alone is enough for stable-ish paging; pagination itself only relies on page size, not ordering).
- `ctx ingest jira` always failed with `HTTP 410 Gone`: Atlassian has fully sunset the legacy `GET /rest/api/3/search` endpoint it called. Migrated to the enhanced `POST /rest/api/3/search/jql` endpoint, including its `nextPageToken`-based pagination (replacing the removed `startAt`/`total` offset model). Added a bounded page cap as a backstop against a pagination bug Atlassian Community has reported against this endpoint (a response can keep reissuing a fresh `nextPageToken`, and its `isLast` flag has been reported stuck at `false` through the same failure) — exceeding the cap is now a reported error instead of `ctx ingest jira` hanging indefinitely.

## 0.6.1 — 2026-08-31

### Added

- `install.sh`, a POSIX `sh` installer (`curl -fsSL .../install.sh | sh`) that detects the current OS/architecture, downloads the matching release archive, verifies its published SHA-256 checksum, and installs `ctx` to `~/.local/bin` by default.

### Changed

- The Linux release target changed from `x86_64-unknown-linux-gnu` to `x86_64-unknown-linux-musl`: a statically linked binary that runs on any x86_64 Linux distribution regardless of the host's glibc version (Arch, Ubuntu, Fedora, Gentoo, ...), rather than only distributions with a glibc at least as new as the build runner's.
- Release archives are now published alongside a `.sha256` checksum file for each target.

## 0.6.0 — 2026-08-31

### Added

- `ctx ingest jira` for Jira Cloud issues and comments, mirroring the existing GitLab connector's architecture (synchronous `ureq` behind an injectable transport trait, token from an env var never committed). Deliberately scoped to referenced issues, not the whole project: `JiraIngestRunner` scans every artifact this repository already knows about (commits, branches, GitLab issues/MRs, prior Jira issues) for ticket-key-shaped references, fetches exactly those under the configured project, plus one hop of expansion through Jira's own `issuelinks`/`parent` fields — never a project-wide query, and never recursing past that one hop. An issue's stable identity is its human-readable key (e.g. `PSI-1122`), so an existing commit/branch mention resolves to it via the existing deterministic ticket-key linker with no `ctx-core` changes. Jira Cloud only; Server/Data Center, changelog/worklog/attachments, and retry-on-429 are out of scope for v1.
- `ctx review --related-tests[=<DEPTH>]`, an opt-in, recall-first companion to review's existing conservative, product-intent-gated `related_tests`: a breadth-first walk of purely structural graph edges (calls, containment, data/API/event interactions) from every changed symbol, with no semantic gating and no confidence threshold, reported as `tests_to_run`. Answers "what should I run to check this diff," not "what does documented intent say is covered." Off by default; MCP and the eval harness are unaffected.
- CI (`.github/workflows/ci.yml`, `mutation.yml`, `dependabot.yml`): `cargo fmt`/`clippy --all-features -D warnings`/`cargo test --workspace`/the `ctx-eval` corpus/`cargo doc -D warnings` on every push and PR, an MSRV check pinned separately from the CI toolchain, `cargo-llvm-cov` with a 65%-line floor, `rustsec/audit-check`, and `cargo-deny` license/source gating, plus a weekly sharded `cargo-mutants` run.

### Changed

- MSRV raised from 1.85 to 1.88.
- `ctx verify --stale`'s agent re-review (`ctx_adapters::agent_contract::review_stale_claims`) now batches claims into independently validated, byte-bounded prompts (`StaleClaimReviewBudget`, default 64 KiB / 20 claims per batch) instead of one unbounded prompt — an individual claim that can't fit the budget now fails explicitly instead of producing an oversized CLI argument or silently truncated evidence.
- Artifact-adapter code (`ctx-adapters::{git,gitlab,jira}`, `sqlite::artifacts`) refactored onto new `Project`/`Timestamp`/`Url` value objects shared across all three ingest sources, replacing ad hoc string/i64 fields.
- Broader architecture-audit hardening across the adapters layer (agent-response validation, business-context reading, candidate-queue handling) — see `git log 787c6e7` for the full file list.

### Fixed

- 6 stale test/implementation mappings in this repository's own `.context/` surfaced by `ctx verify --stale` after unrelated code changes and confirmed against an independent agent review: `INV-CTX-070` was retargeted from a whole test module to its actual guarded logic (`classify_behavior_change`) with a real covering test added; `ADR-CTX-004`'s two listed tests were removed as not actually exercising the decision's claim; `ADR-PRECISION-001`, `INV-CTX-049`, and `INV-CTX-066` each had one mismatched test entry removed.

## 0.5.0 — 2026-08-22

### Added

- external development-artifact ingestion: `ctx ingest git` (commit messages, branch names), `ctx ingest code-comments` (comments/docstrings attributed to their nearest symbol), and `ctx ingest gitlab` (issues, merge requests, and their comments — the chosen end-to-end MUST provider) normalize artifacts into their own store (`artifacts`, `artifact_links`), idempotently re-synced, never a `ctx-core::domain::Node` and never automatically promoted to product knowledge;
- deterministic reference extraction (`ctx-core::linking`) linking a ticket key/issue/MR mention in artifact text to an already-known artifact, and changed-symbol links from an artifact's changeset to the code it touched — never a guessed relationship, and never using AI;
- bounded artifact-neighborhood assembly (`ctx-core::neighborhood`) — one artifact's own linked artifacts, changed code, nearby tests, and already-mapped product knowledge, one hop only, never the whole repository or artifact backlog — as the unit of work handed to an AI agent;
- an interchangeable `SemanticAgent` port boundary and three concrete CLI-based agents: Claude Code CLI, OpenAI Codex CLI, and Google Antigravity CLI (`ctx enrich --agent claude|codex|antigravity`, each independently overridable via `CTX_CLAUDE_CLI_BINARY`/`CTX_CODEX_CLI_BINARY`/`CTX_ANTIGRAVITY_CLI_BINARY`), sharing one prompt/JSON-response validation contract (`ctx-adapters::agent_contract`) so every vendor is held to the same evidence-grounding rule regardless of which one produced the response;
- typed `Feature`/`Requirement`/`Invariant`/`Decision` knowledge candidates (`ctx-core::knowledge::KnowledgeCandidate`) proposed only from evidence an agent actually cited from its given neighborhood; a citation outside that bound, an implementation/test path the neighborhood never surfaced, or malformed output altogether is dropped or rejected, never trusted — absence of extracted knowledge is always preferred to a fabricated candidate;
- `ctx verify --knowledge` to list and accept/reject pending AI-derived candidates; `--accept --id <STABLE-ID>` writes an ordinary `.context/*.yaml` document through the same import path a hand-authored file uses (no second, parallel truth store), refusing (unless `--force`) a statement that looks like a lexical restatement of an already-active document of the same kind;
- incremental `ctx enrich`: an artifact whose content hasn't changed since its last analysis (regardless of outcome) is skipped rather than re-sent to an agent every run;
- incremental `ctx ingest gitlab`: a stored per-project sync cursor narrows each run to issues/MRs GitLab itself reports as updated since the previous run;
- an artifact-evidence heuristic-scoring signal: an implementation candidate scores higher when the same artifact that backed an accepted AI-derived requirement's evidence also touched that candidate symbol;
- a precise, per-entity `needs_mappings` status health check (every active Requirement/Invariant/Decision with no implementation mapping, by identifier) replacing a coarser repository-wide aggregate that could hide one freshly accepted, still-unmapped document behind many already-mapped ones;
- full provenance rendering in `ctx explain` for a document that reached the graph through `ctx verify --knowledge --accept`: which artifacts, which agent (producer/model), and who accepted it and when;
- `ctx find <name>` discovery command, and independent per-match `ctx impact`/`ctx explain`/`ctx context` results when a short or bare name resolves to several distinct namespaces, instead of an ambiguity error or a merged result.

### Fixed

- a symbol-identity collision: the same-shape identity-matching fallback could let a brand-new file's symbol steal an unrelated, completely unchanged file's stable identity merely by sharing its name, kind, and structural shape — found by running `ctx index` against this repository's own history mid-sprint. The fallback's candidate pool is now restricted to symbols whose own file is actually part of the current transition.
- a false positive in the new `needs_mappings` health check that would have flagged every `Feature` document as unmapped, including this repository's own — Feature documents are a pure descriptive umbrella by established convention, with the real mapping carried by the Requirements beneath them.

## 0.4.0 — 2026-08-18

### Added

- a unified, language/framework-neutral persistent-state model: `SchemaColumn` now carries `nullable`/`primary_key`/`unique`/`foreign_key`/`default` when statically determinable, and `SchemaTableDefinition` carries table create/drop/rename, column add/drop/rename/alter (`ALTER COLUMN ... TYPE`/`SET-DROP NOT NULL`/`SET-DROP DEFAULT`), raw `CHECK` text, and index add/drop — all read from goose `CREATE TABLE` (including table-level `PRIMARY KEY`/`UNIQUE`/`FOREIGN KEY`/`CHECK`) and `ALTER TABLE`, and from `SQLAlchemy` `Column`/`mapped_column` keyword arguments;
- `ctx-core::schema`, a pure module classifying schema changes as destructive/contract-relevant or routine, from either a migration's self-declared operations or a structural diff between two versions of an edited ORM model; wired into `ctx review` as a new `schema_findings` stream kept structurally separate from proven product-impact `findings`, each with a bounded advisory link to the requirements/invariants/tests its affected table's readers/writers are mapped to;
- best-effort `SQLAlchemy`/goose reconciliation (`reconcile_orm_and_migrations`) surfaced in `ctx status` as `schema_divergences`, folded into the existing `NeedsAttention` health state;
- column-level static write evidence (`DatabaseAccess.columns`) for `UPDATE ... SET` and `INSERT`/`MERGE` explicit column lists across all three language adapters, flowing into edge evidence and Context Pack rendering;
- `ctx impact table.column` seeds (for example `subscriptions.paid_until`), resolving to the table's `DbEntity` and narrowing `implementation` to that column's specific readers/writers, with an explicit uncertainty when the column has no known evidence;
- twelve new evaluation-corpus cases covering destructive schema changes, both-direction reconciliation, a false-positive control, the project's only "ambiguous ORM mapping" case, explicit schema-seed isolation, and column-level impact (25 cases / 102 checks total, up from 13/67).

### Fixed

- a real truncation bug in the goose/DDL column-type reader: a comma-containing type like `NUMERIC(10, 2)` was captured as `"NUMERIC(10,"` by the old whitespace-only split.

## 0.3.0 — 2026-08-18

### Added

- a Go language module (`ctx_adapters::go`) with the same normalized-IR contract as Python/Rust: directory-based canonical paths, receiver-namespaced methods, interface/type-alias/const-block extraction, call resolution, and static SQL-literal recognition;
- table/column-level schema extraction from goose SQL migrations (`ctx_adapters::goose`, reading only `-- +goose Up`) and from SQLAlchemy declarative models (`__tablename__` plus `Column`/`mapped_column` attributes in the Python analyzer);
- a new `DEFINES_SCHEMA` `FACT` edge kind and `SchemaMigration` symbol kind, sharing the same `DbEntity` graph, incremental versioning, and evidence machinery as static SQL reads/writes — a table declared only by a migration or ORM model, never touched by code, now appears in impact, review, and Context Pack like any other data contract;
- two new evaluation-corpus cases exercising the new fact type end to end (13 cases / 67 checks total).

### Fixed

- a UTF-8 char-boundary panic in the new DDL recognizer when non-ASCII migration/model content sits next to a recognized keyword;
- a latent, order-dependent cross-file symbol-identity collision in `plan_incremental_index`: whether two files defining a same-named, identically-shaped helper correctly received distinct stable identities depended on which file was processed first. Fixed by reserving every changed file's exact-canonical-path historical identity across the whole transition before any file's same-shape fallback runs, so the outcome is independent of processing order.

### Changed

- the first-party `.context/` corpus gained a schema-migration requirement and refined seven existing documents to explicitly cover schema-migration/ORM-model data contracts alongside SQL-literal ones;
- the Python analyzer version was bumped so existing repositories pick up SQLAlchemy schema facts on their next `ctx index`.

## 0.2.0 — 2026-08-17

### Added

- deterministic Python and Rust extraction of static SQL database reads and writes;
- repository-scoped `DbEntity` nodes with temporal `READS_FROM`/`WRITES_TO` facts and source-line evidence;
- database contracts in impact reports, Context Packs, status, explanation, and semantic-candidate scoring;
- explicit review signals when a symbol's database read/write set changes;
- an 11-case/59-check evaluation baseline including changed DB writes;
- an evaluation guide that separates automated regression evidence from unrun human product experiments.

### Changed

- Python and Rust analyzer versions now invalidate older cached analysis so existing repositories receive the new normalized interaction facts;
- the subscriptions fixture now demonstrates an evidence-backed `subscriptions` write;
- status reports the current database-entity count;
- resolved explicit Context Pack seeds now remain hard scope boundaries instead of competing with unrelated lexical roots.

## 0.1.0 — 2026-08-17

- initial local-first MVP with incremental Python/Rust indexing, Git-owned product context, provenance and staleness, impact/explain/review, bounded Context Packs, verification, CLI/JSON, MCP, SQLite, Docker, and the first deterministic evaluation corpus.
