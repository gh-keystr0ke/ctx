# ctx worklog

This file is the durable implementation history and hand-off context for the project.

## 2026-08-17 — Project intake

- Read `eng_conclu.md` and `product_conclu.md` as the authoritative engineering and product specifications.
- Confirmed the repository starts empty apart from those two untracked documents and has no prior commits.
- Chose the specification's vertical-slice order: deterministic Rust core, SQLite storage, Git-aware Python indexing, business context, impact/explain, review, bounded context packs, verification, then MCP.
- Preserved the main constraints: local-first, no required LLM or network access, provenance on semantic claims, conservative review findings, and bounded typed traversal.
- Planned to run `cargo fmt --check`, strict Clippy, and the full workspace test suite after every milestone.

### Current state

M0 is complete:

- Created a Rust 2024 workspace with strict shared lint settings.
- Added stable repository/node/commit/key identifiers, bounded confidence, closed domain enums, commit validity, claims, edges, and evidence to `ctx-core`.
- Added a concrete `SQLite` store using WAL, foreign keys, transactional idempotent migrations, and the full initial temporal/provenance schema.
- Kept database row IDs out of the public domain model.
- Verified with `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --workspace` (5 tests passed).

Next milestone: M1/M2 code-indexing vertical slice with normalized Python IR and pure incremental planning.

## 2026-08-17 — M1/M2 indexing core

- Added a language-neutral normalized IR for complete file analyses, symbols, source ranges, signatures, body fingerprints, and call sites.
- Added a pure incremental planner that emits explicit node/version/retirement, structural-edge invalidation/rebuild, and semantic-staleness effects.
- Identity matching is conservative and deterministic: exact canonical path, unique name/signature in the prior file, then unique structural fingerprint. Ambiguity creates a new identity.
- Added focused tests for body changes, file/symbol renames, and deterministic call facts.
- Confirmed changed symbol bodies mark semantic links stale, while a pure file rename preserves a uniquely matched symbol identity.
- Verified with formatting, strict Clippy, and the workspace suite (8 tests passed, including the existing SQLite test).

Next: execute this plan through Git/Tree-sitter/SQLite and expose `ctx init`, `ctx index`, and `ctx status`.

## 2026-08-17 — M1/M2 indexing shell

- Added narrow application ports and an indexing use case that orchestrates repository inspection, changed-file parsing, pure planning, and one atomic store transaction.
- Added a NUL-safe Git adapter for initial file discovery and add/modify/delete/rename diffs. Generated, vendor, build, virtual-environment, and cache paths are excluded.
- Added a Tree-sitter Python adapter for modules, classes, functions, methods, tests, signatures, body hashes, structural fingerprints, and call sites.
- Extended SQLite persistence to keep commit-bounded node versions, close analyzer-owned structural edges, retire deleted entities, preserve matched identities, and stale affected semantic claims.
- Added `ctx init`, `ctx index`, and `ctx status`; all commands support `--json`, and indexing exposes verbose invalidation statistics.
- Added the initial subscriptions fixture.
- Ran a real CLI smoke test: the first index parsed 2 files into 6 symbols and 11 relationships; a second index at the same commit parsed 0 files and made no changes.
- Verified formatting, strict Clippy, and the workspace suite (11 tests passed).

Next: ingest Git-versioned business context with explicit, evidence-backed semantic relationships.

## 2026-08-17 — M3 business context and provenance

- Added normalized Feature, Requirement, Invariant, and Decision documents with stable human IDs.
- Added YAML plus Markdown-front-matter ingestion under `.context/`; duplicate IDs, malformed documents, and missing required fields fail explicitly.
- Added exact-only resolution for `implementation` and `tests` mappings. Invariants produce `ENFORCES`, requirements/features produce `IMPLEMENTS`, decisions produce `SATISFIES`, tests produce `COVERED_BY`, and feature membership produces `DEPENDS_ON` assertions.
- Persisted every explicit semantic claim as an `ASSERTION` backed by a Documentation source, locator-level evidence, commit validity, confidence, and producer fingerprint.
- Context versions and removed documents are synchronized transactionally; changed implementation bodies can now stale these non-fact claims through the existing incremental plan.
- Added realistic feature, requirement, invariant, decision, implementation, and test mappings to the subscriptions fixture.
- Verified a fresh fixture index produced 4 intent documents and 7 explicit links with zero unresolved symbols; the database contained 18 active structural/semantic relationships.
- Verified formatting, strict Clippy, and the workspace suite (14 tests passed), including an integration test that joins edge → evidence → source.

Next: build bounded impact traversal and evidence-first explanation output over the stored claims.

## 2026-08-17 — M3 impact and explain

- Added a current graph read model that preserves typed nodes, claim class, status, confidence, commit validity, producer, staleness reason, and source-level evidence.
- Added a pure impact policy: exact seed resolution, one containment/call neighborhood, then at most three typed semantic expansions. Inferred edges below the conservative threshold are excluded and inference cannot recursively amplify inference.
- Added uncertainty output for stale and inferred relationships, while keeping output grouped into features, requirements, invariants, decisions, implementation, and tests.
- Added `ctx impact <file|symbol|ID>` and `ctx explain <ID|"source -> target">` with deterministic text and JSON rendering.
- `ctx explain` only renders stored claims and stored evidence; it never invents a rationale.
- Fixture validation for `SubscriptionService.cancel` returned the subscription feature, requirement, invariant, caller, file, test, and related Stripe decision. Relation explanation identified an active Documentation Assertion with confidence 1.0 and the exact YAML locator/commit.
- Verified formatting, strict Clippy, and the workspace suite (15 tests passed).

Next: implement the conservative `ctx review` product wedge over Git changes.

## 2026-08-17 — M4 conservative review

- Added Git review input for branch/working-tree diffs, including staged, unstaged, renamed, deleted, and untracked Python/context files. Base revisions are resolved to commit OIDs before use.
- Added old-source loading through Git and current-source parsing through the same normalized Python analyzer.
- Added pure symbol pairing and behavioral classification for formatting-only, rename/move, likely refactor, potential behavior, contract, and unknown changes.
- Added a conservative review engine that surfaces only implementation claims with strong composed confidence. Formatting, rename, and likely-refactor changes are suppressed from product warnings.
- Findings include severity, confidence, changed entity, affected intent, stored evidence, linked tests, whether those tests changed, possible requirement drift, and a concrete reviewer action.
- Added `ctx review [--base <revision>]`, text output, JSON output, verbose suppression diagnostics, and explicit stale-relationship reporting.
- Fixture validation replaced the paid-until guard with an unconditional inactive status. Review produced exactly two high-confidence findings: `INV-SUB-003` and `REQ-SUB-014`, both citing their exact YAML evidence and unchanged related test.
- Verified formatting, strict Clippy, and the workspace suite (17 tests passed), including formatting/contract classification and precision-focused review tests.

Next: compile a bounded, token-budgeted Context Pack from the same graph knowledge.

## 2026-08-17 — M5 Context Compiler

- Added deterministic task-term, file, and symbol seed detection with ambiguity errors for explicit seeds.
- Added bounded typed traversal: semantic relationships up to three hops, containment/calls only at the seed boundary, rejected claims excluded, confidence filtering, and maximum inferred-edge depth one.
- Added semantic priority tiers that protect invariants and requirements before implementation, tests, adjacency, and low-confidence material.
- Added a conservative character-based token estimator, content truncation, an evidence reserve, and accounting for selected evidence/uncertainty so reported pack size never exceeds the requested budget.
- Added evidence prioritization so `ENFORCES`/`IMPLEMENTS` provenance is retained before secondary feature membership when a pack is tight.
- Added `ctx context <task>` with optional repeated `--file`/`--symbol`, `--token-budget`, text output, and JSON output.
- Fixture validation compiled the Stripe/cancellation task into a 280/300-token pack containing the invariant, requirement, feature, decision, direct symbol/file context, and the highest-priority evidence instead of entire source files.
- Verified formatting, strict Clippy, and the workspace suite (19 tests passed), including hard-budget and no-inference-chaining tests.

Next: add heuristic semantic candidates, durable accept/reject verification, and the thin MCP adapter.

## 2026-08-17 — M6 semantic verification

- Added deterministic heuristic candidate generation using separately reported lexical, structural-neighborhood, and linked-test signals. Scores are ranking strengths, not probabilities.
- Added conservative candidate filtering at 0.65 and impact-first ordering so invariants/requirements are reviewed before lower-value mappings.
- Existing active or rejected semantic pairs are never proposed again without a different relationship/evidence fingerprint.
- Added `ctx verify`: JSON/non-interactive candidate listing, interactive accept/reject/skip/explain, and scriptable `--accept`/`--reject` decisions with author attribution.
- Acceptance persists the original heuristic `INFERENCE` and its derivation/evidence in history, attaches a human confirmation annotation, and creates a separate current Human `ASSERTION` with provenance back to the inference.
- Rejection persists a current rejected inference plus human rejection annotation, preventing repeated annotation work.
- Verified formatting, strict Clippy, and the workspace suite (20 tests passed), including signal breakdown, inference preservation, separate assertion creation, and durable rejection tests.

Next: expose the same application use cases through a thin stdio MCP server without duplicating business logic.

## 2026-08-17 — M7 MCP adapter

- Checked the current official MCP protocol before implementation. The adapter supports modern `2026-07-28` `server/discover` negotiation and legacy `2025-*` `initialize` clients over newline-delimited stdio.
- Added the dedicated `ctx-mcp` crate plus both `ctx-mcp` and `ctx serve --mcp` entry points.
- Added deterministic tool discovery for exactly the specified tools: `get_context`, `get_impact`, `explain_relation`, `find_requirements`, and `review_change`.
- Every tool calls the existing `ctx-app` query/review services; no graph traversal, ranking, review, or evidence logic is duplicated in the protocol adapter.
- Tool results include both MCP text content and structured JSON, while recoverable use-case errors use `isError` and protocol errors remain JSON-RPC errors.
- The stdio server writes only one-line JSON-RPC messages to stdout and supports parse errors, ping, notifications, current server metadata, and read-only/local-world tool annotations.
- End-to-end stdio validation completed modern discovery, `tools/list`, and `get_impact`; the returned structured content contained the full fixture product chain.
- Verified formatting, strict Clippy, and the workspace suite (22 tests passed), including stable tool schemas and modern/legacy negotiation tests.

Next: finish configuration correctness, end-to-end automation, Docker packaging, and user/architecture documentation.

## 2026-08-17 — Configuration boundaries

- Wired `.ctx/config.toml` into Git source discovery, incremental diffs, and review input instead of merely generating an unused file.
- The first release rejects unsupported languages explicitly and applies deterministic include/exclude prefixes on top of the built-in Python generated/vendor safeguards.
- Renames across a configured boundary become an addition or deletion, so excluded paths cannot remain active by masquerading as a move.
- Added pure source-scope reconciliation between the stored snapshot and current configured paths. A commit that changes only configuration now retires newly excluded files and indexes newly included files without requiring a full rebuild.
- Added unit coverage for configured boundaries, exclusions, cross-boundary renames, scope retirement, and duplicate-free inclusion.
- Verified formatting, strict Clippy, and the workspace suite (26 tests passed).

Next: automate the complete fixture journey through the compiled CLI, then add packaging and documentation.

## 2026-08-17 — CLI end-to-end product scenario

- Added an executable-level integration test that copies the subscriptions fixture into an isolated temporary Git repository and invokes the compiled `ctx` binary.
- The scenario proves initialization, a 2-file/6-symbol/18-edge first index, a zero-reparse second index, explicit product impact, and a Context Pack that remains within a 300-token budget.
- The same scenario removes the paid-entitlement guard and verifies that review emits exactly the two documented high-severity findings (`REQ-SUB-014` and `INV-SUB-003`), with evidence and an unchanged linked test.
- The test uses real Git, Tree-sitter, SQLite, YAML ingestion, application services, CLI argument parsing, and JSON rendering; only its filesystem is temporary.
- Verified formatting, strict Clippy, and the complete workspace suite (27 tests passed).

Next: ship reproducible container packaging and concise user/architecture documentation.

## 2026-08-17 — Commit-faithful indexing and safe local state

- Added a Git port check that refuses `ctx index` when configured Python sources or `.context` documents differ from `HEAD`; commit-bounded validity can no longer be attached to uncommitted input by a first index.
- Kept working-tree inspection in `ctx review`, where an uncommitted diff is the intended input.
- `ctx init` now records only `.ctx/ctx.db`, its WAL, and its shared-memory file in Git's repository-local exclude file. It does not mutate the project's shared `.gitignore`, and `.ctx/config.toml` remains trackable.
- Extended the CLI end-to-end scenario to prove the database is absent from Git status and a harmful dirty source is rejected by indexing while remaining reviewable.
- Verified formatting, strict Clippy, and the complete workspace suite (27 tests passed).

Next: package both binaries in Docker and finish the user and architecture guides.

## 2026-08-17 — Ignored context fidelity

- Closed the remaining commit-validity edge case for `.context`: ignored context files are visible to the filesystem reader but absent from normal untracked Git output, so indexing now detects and refuses them explicitly.
- Extended the full CLI scenario with an ignored requirement and proved the file is named in the refusal before any index state is written.
- Re-ran formatting, strict Clippy, and the complete workspace suite (27 tests passed).

Next: complete packaging validation and documentation.

## 2026-08-17 — Packaging and documentation

- Added a multi-stage Alpine `Dockerfile` that pins Rust 1.97.1, builds both `ctx` and `ctx-mcp`, runs as a non-root user, and includes only Git/CA certificates at runtime.
- Added BuildKit registry and target caches so source edits do not force dependency recompilation, plus a `.dockerignore` that excludes Git metadata, local databases, and host build output.
- Added `compose.yaml` with a normal CLI service and an opt-in stdin-attached MCP profile; host repository and UID/GID are configurable.
- Wrote the user guide with installation, quick start, context schemas, canonical symbol rules, configuration, complete command reference, MCP client setup, Docker/Compose usage, development gates, privacy, and current limitations.
- Wrote the architecture guide covering crate boundaries, index transitions, identity, validity/staleness, epistemic classes, provenance, traversal, review precision, and local storage. Added the Apache-2.0 license and removed the placeholder repository URL from package metadata.
- Verified both Compose profiles, Cargo metadata, generated Rust API documentation, a locked optimized workspace build, and `ctx 0.1.0` from the release binary.
- Docker validated both remote image manifests and completed the Alpine runtime package/non-root-user layer. The final multi-stage image could not finish because the Docker registry transfer stalled repeatedly while fetching the Rust builder layer; no Dockerfile compilation error was reached.

Next: run the final clean-tree release gates and summarize the delivered product.

## 2026-08-17 — Final release gate

- Ran `cargo fmt --all -- --check`: passed.
- Ran `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed with no warnings.
- Ran `cargo test --locked --workspace`: all 27 tests passed, including the complete temporary-Git CLI scenario and adapter persistence/provenance checks.
- Ran `cargo build --locked --workspace --release`: passed; the optimized CLI reports `ctx 0.1.0`.
- Revalidated the normal and MCP Docker Compose profiles, Cargo metadata, diff whitespace, and the clean Git worktree.
- All requested MVP milestones are implemented: local incremental indexing, explicit business context, provenance/validity/staleness, impact/explain, conservative review, bounded Context Packs, semantic verification, CLI/JSON, and MCP.
- The only incomplete environmental check is a full Docker image build: registry connectivity stalled during the pinned Rust builder-image download. The Docker/Compose definitions parsed successfully and the runtime layer completed, but the final image was not falsely reported as built.

The product implementation and release documentation are complete at version 0.1.0.

## 2026-08-17 — Post-release graph-integrity audit

- A real `ctx status` run exposed 22 active relationships where a fresh equivalent graph had 11 structural facts. Direct SQLite inspection proved these were two simultaneously active versions of every `contains`/`calls` fingerprint, not additional knowledge.
- Root cause: an early development database had indexed then-untracked fixture files; Git later reported those same paths as `Added`, while the planner assumed an addition could never replace a snapshot path and therefore did not close analyzer-owned edges.
- Changed the pure planner so `Added` plus an existing snapshot path is treated as a replacement for structural invalidation.
- Added migration 002. It deterministically closes every older current edge version, then creates a partial unique index on `(repository_id, fingerprint)` for current edges so this invariant is enforced by SQLite as well as planner logic.
- Added regression coverage for the planner edge case and migration of an intentionally duplicated legacy database.
- Applied the migration to the development database through the normal CLI open path: active structural relationships were repaired from 22 to 11 with one current version per fingerprint.

Next: replace the vanity-counter status screen with actionable graph-health diagnostics.
