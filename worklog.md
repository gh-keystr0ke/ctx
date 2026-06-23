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

## 2026-08-17 — Actionable status health

- Replaced the five opaque counters with an application-level `StatusService` and a structured JSON/text health report.
- Status now compares `HEAD` with the indexed commit, exposes the effective language/include/exclude scope, and lists relevant working-tree inputs without confusing the committed graph with the diff.
- Split knowledge into code files/symbols; Features, Requirements, Invariants, and Decisions; structural facts; active assertions; active inferences; stale semantics; and rejected inferences.
- Added explicit health states: `ready`, `needs_index`, `needs_context`, `needs_mappings`, and `needs_attention`, with deterministic explanations and suggested commands/actions.
- A structural-only graph is no longer called healthy. On this repository the repaired result is honestly `needs product context`, with 11 structural facts and zero product documents/assertions.
- Updated JSON end-to-end assertions so the complete subscriptions fixture must be `ready` with exactly 11 structural facts plus 7 active assertions.
- Added pure tests for health classification and updated the user/architecture documentation.
- Verified formatting, strict Clippy, and the full workspace suite (31 tests passed).

Next: final regression gate and handoff of the corrected status behavior.

## 2026-08-17 — Language-aware graph identity

- Removed the Python constant from generated symbol identities: stable keys now use the analyzer-reported language (`symbol:<language>:<canonical-path>:<kind>`).
- Persisted language on indexed symbols so rename/fingerprint matching cannot merge structurally identical symbols from different languages.
- Scoped static call resolution to one language and labelled structural-edge provenance with the responsible Tree-sitter analyzer.
- Added a regression test proving identical Python and Rust symbol definitions receive distinct stable identities.
- Ran formatting and the complete workspace suite (32 tests passed).

Next: replace the single Python analyzer/configuration path with a multi-language registry, then add the Rust module.

## 2026-08-17 — Pluggable Rust language module

- Replaced the single-language source scope with deterministic `languages = [...]` configuration while preserving legacy `language = "..."` files; invalid, empty, conflicting, and unsupported configurations fail with actionable errors.
- Added a self-describing `AnalyzerModule` contract and `AnalyzerRegistry`. Indexing, branch review, CLI, and MCP now dispatch by registered source extension without depending on Python- or Rust-specific types.
- Added a Tree-sitter Rust module that extracts functions, inherent and trait methods, structs, enums, traits, inline modules, type aliases, constants/statics, `#[test]`/`#[tokio::test]` functions, signatures, byte/line ranges, body hashes, structural fingerprints, and call sites into the existing language-neutral IR.
- Derived workspace-aware Rust canonical paths (for example `ctx_core.indexing.plan_incremental_index`) and rejected syntax-error trees instead of persisting incomplete analysis.
- Extended the IR with Rust-relevant symbol kinds and made review graph resolution language-aware, preventing equal canonical paths in different languages from being confused.
- Added mixed-language Git parsing/config tests, Rust parser tests (including generic `impl` and abstract trait methods), and an executable-level Python/Rust scenario proving one index has 2 files, 4 symbols, 6 structural facts, distinct call graphs, and a Rust working-tree change resolves back to its Rust stable key during review.
- Added the pinned `tree-sitter-rust` 0.24.2 grammar. Ran formatting, strict Clippy, the Rust-specific CLI scenario, and the complete workspace suite (38 tests passed).

Next: enable Rust dogfooding for this workspace, document the module extension point and configuration migration, then run the release gates.

## 2026-08-17 — Cross-language mapping disambiguation

- Kept concise canonical paths as the default `.context` mapping syntax.
- Added language-qualified stable keys as an exact mapping syntax for the case where two enabled languages produce the same canonical path, for example `symbol:rust:app.run:Function` versus `symbol:python:app.run:Function`.
- Added a focused regression test proving the canonical lookup remains deliberately ambiguous while each qualified stable key resolves to exactly one symbol.

Next: document both mapping forms and dogfood the Rust analyzer on the ctx workspace.

## 2026-08-17 — Rust configuration and extension guide

- Updated the quick start, capabilities, canonical-path rules, configuration reference, development checks, and current limitations for mixed Python/Rust repositories.
- Documented the `AnalyzerModule` extension procedure for planned TypeScript, Go, Java, and Zig adapters, including the normalized IR contract and required parser/e2e coverage.
- Documented language-qualified mapping keys for cross-language canonical-name collisions and stated explicitly that modules are compile-time components rather than dynamic shared libraries.
- Updated the architecture guide with registry dispatch, language-scoped identity/call resolution, Rust workspace namespaces, and syntax-error handling.
- Enabled both `python` and `rust` in this repository and expanded its source scope from the small Python fixture to `crates` plus `fixtures`, so the product will index its own Rust implementation.

Next: commit the shared scope, index the commit with the release binary, inspect the resulting graph, and run all release gates.

## 2026-08-17 — Trait implementation identity found by dogfooding

- The first full-workspace release index was rejected by SQLite's node-version uniqueness invariant and rolled back atomically; no partial graph state was committed.
- Traced the collision to legal repeated trait method names: the workspace contains multiple `impl From<...> for CliError` blocks, while the initial Rust canonicalizer represented all of them as `ctx_cli.CliError.from`.
- Namespaced trait-implementation methods by the complete implemented trait, including generic arguments, yielding distinct paths such as `CliError.From<std::io::Error>.from`.
- Added a regression with both `impl From<u8>` and `impl From<u16>` for one type and documented the canonical rule.
- Verified the focused parser test and strict adapter Clippy gate.

Next: commit the corrected identity rule and repeat the release index from a clean commit.

## 2026-08-17 — Pre-storage identity diagnostics

- A second full-workspace attempt still reached the node-version uniqueness guard, showing the first collision class was not the entire transition problem; the transaction again rolled back cleanly.
- Added a permanent dogfood test that parses every tracked Rust source below `crates/` and checks the exact `(canonical path, symbol kind)` inputs used by graph identities. It currently proves all Rust analyzer outputs are unique across the workspace.
- Added a core planner invariant that rejects duplicate stable-key writes before SQLite and reports the exact key, with a regression using conflicting file transitions.
- Verified both focused regressions and strict Clippy for the affected core/adapter crates.

Next: use the new planner diagnostic on a clean committed transition to isolate the remaining source-scope issue.

## 2026-08-17 — Historical matching isolated from the current transition

- The pre-storage invariant identified the exact duplicate: `symbol:rust:ctx_adapters.business_context.YamlBusinessContextReader.new:Method`.
- Root cause was language-neutral incremental logic, not another Rust parse collision. During a large initial scope expansion, symbols already planned earlier in the same transition were incorrectly admitted as historical rename/fingerprint candidates. A later, structurally similar `new` method inherited the earlier method's key.
- Split the immutable historical snapshot from the evolving current symbol set. Identity matching now consults only symbols that existed before the transition; call resolution still sees the fully assembled current set.
- Added a regression proving two structurally equal symbols added in separate files in one transition receive their own canonical stable keys.
- Verified the focused planner regression and strict core Clippy gate.

Next: commit the matcher fix and repeat the full release dogfood index.

## 2026-08-17 — Analyzer-version cache invalidation and callable targets

- The corrected matcher enabled a successful full release index at `8ac5f24`: 35 newly scoped Rust files were parsed, 637 nodes created, and 1,102 Rust structural facts added. The resulting current graph had 37 files, 608 symbols, and 1,113 active structural facts; a second index parsed zero files.
- Queried `ctx_core.indexing.plan_incremental_index` and `ctx_adapters.rust.RustAnalyzer.analyze_source` through the release CLI to confirm canonical selection and Rust adjacency/test traversal.
- That inspection exposed a false call edge from `Err(...)` to an associated `type Err`. Restricted call targets to callable symbol kinds and added a regression for the alias case.
- Added per-file analyzer normalization versions to the IR and persisted file snapshot. Indexing now checks these versions before returning a same-commit no-op and reparses unchanged files after analyzer semantics change.
- Added a regression proving one version mismatch schedules exactly one deterministic `Modified` transition. Documented version bumps as part of the language-module contract.
- Ran formatting, strict workspace Clippy, and the complete workspace suite (45 tests passed).

Next: commit the cache/call-quality changes, prove same-HEAD analyzer-version reindexing on the dogfood database, then run the final release gates.

## 2026-08-17 — Same-commit derived analysis replacement

- Deliberately changed the persisted analyzer version for exactly one current Rust file from v2 to v1, then ran the release index without changing `HEAD`.
- The version detector correctly scheduled the file, but persistence rejected a second commit row because `(repository, oid)` is unique. The index transaction rolled back; the deliberately stale marker remained available for the repaired binary to consume.
- Kept one Git validity point and made derived reanalysis idempotent there: commit metadata is upserted, a node version from an older commit is closed, and a node version already beginning at the same commit is atomically replaced and reopened.
- Added a SQLite regression that applies two analyses at one OID and proves there is one commit, one current node version, and the v2 attributes/content win.
- Verified the focused persistence test and strict adapter Clippy gate.

Next: commit the same-commit storage semantics, let the release binary repair the stale dogfood file, and complete the full gates.

## 2026-08-17 — Final Rust-module release verification

- Rebuilt the optimized workspace and indexed commit `58b5f75`. The pending deliberately stale analyzer marker plus the committed storage change reparsed 2 files; the next index was a zero-reparse no-op.
- Repeated the cache-upgrade experiment at the same `HEAD`: changed exactly `crates/ctx-core/src/context_pack.rs` from persisted Rust analysis v2 to v1, then ran the release binary. It reparsed exactly 1 file, versioned 34 nodes, rebuilt 72 structural facts at the existing Git OID, restored v2, and the immediate next index was a no-op.
- Final dogfood graph: 37 files, 619 symbols, 1,116 active structural facts, zero stale semantic edges, zero duplicate current edge fingerprints, and zero call edges targeting non-callable type/module/trait/constant symbols. Its honest health remains `needs product context` because this repository has no `.context` documents.
- Ran `cargo fmt --all -- --check`: passed.
- Ran strict locked workspace Clippy with all targets/features and `-D warnings`: passed.
- Ran the locked complete workspace suite: all 46 tests passed, including both real-Git CLI journeys, full-workspace Rust identity dogfooding, same-commit persistence, migration integrity, MCP, and documentation-independent core policies.
- Ran the locked optimized workspace build: passed; the release CLI reports `ctx 0.1.0`.
- Validated normal and MCP Docker Compose profiles and checked whitespace/worktree state.

Rust is now a first-class pluggable analyzer module beside Python, with a tested extension seam for TypeScript, Go, Java, and Zig.

## 2026-08-17 — First-party product context authored

- Promoted ctx from a structural-only dogfood graph to a self-described product by extracting the highest-value contracts from `product_conclu.md`, `eng_conclu.md`, the shipped architecture, and the regression suite.
- Added 19 deliberately compact Git-owned documents: 4 Features, 6 Requirements, 5 Invariants, and 4 Decisions. The taxonomy covers trusted indexing, evidence-backed impact/explain, conservative review, bounded agent context, language modules, actionable status, epistemic boundaries, provenance, committed inputs, determinism, budget safety, functional-core boundaries, local SQLite, and precision over recall.
- Kept Features free of broad direct implementation mappings so review findings originate from specific Requirements, Invariants, and Decisions instead of generic capability labels.
- Mapped only policy/entry-point symbols and direct regression/e2e tests. Preflight validation parsed all 19 YAML files, proved IDs are unique, and checked every declared implementation/test canonical path resolves to exactly one current graph symbol.

Next: commit the context corpus, import it through the release CLI, eliminate any importer-level unresolved links, and exercise status/impact/explain/Context Pack against ctx itself.

## 2026-08-17 — Context import and graph-noise calibration

- Imported the first-party corpus through the release CLI at `354d898`: 19 documents created, 92 explicit assertion edges created, and zero symbol mappings unresolved.
- Status moved from `needs_context` to `ready` with 4 Features, 6 Requirements, 5 Invariants, 4 Decisions, no stale semantic relationships, and no suggested remediation.
- `ctx explain REQ-REVIEW-001` returned seven stored claims covering its Feature, three implementation points, and three tests, each backed by context-file evidence.
- The first impact/Context Pack query for `build_review_findings` exposed excessive expansion through `ADR-CORE-001`: mapping one cross-cutting decision to indexing, review, and Context Pack entry points turned it into a semantic hub.
- Narrowed cross-cutting ADRs to representative ownership points and removed redundant implementation/test links. Requirements and Invariants retain the behavior-specific mappings; architectural documents remain discoverable without connecting unrelated product neighborhoods.

Next: re-import the calibrated mappings and require review impact/context to contain only the conservative-review product neighborhood.

## 2026-08-17 — Bounded typed impact traversal

- Dogfooding the calibrated corpus exposed a core traversal defect rather than another documentation problem: `expand_semantics` mutated the selected set while scanning edges, so one nominal hop could consume an arbitrarily long edge chain and pull most of a connected component into `ctx impact`.
- Replaced the scan with an explicit deterministic queue carrying graph distance and per-path inference state. Semantic expansion now stops after three actual hops, excludes rejected claims, reports but does not propagate through stale claims, and cannot chain one inference through another.
- Made product intent the only non-root semantic intermediary. This keeps a shared end-to-end test or Feature from becoming a bridge into unrelated requirements while still returning the selected implementation's requirement/decision, its Feature, and its direct verification tests.
- Added regressions for the exact three-hop boundary, shared-test isolation, rejected-claim exclusion, and inference non-amplification. All five focused impact tests and strict `ctx-core` Clippy pass.

Next: commit the traversal repair, rebuild/index the clean commit, and verify the first-party review neighborhood through the release CLI before the complete workspace gates.

## 2026-08-17 — Shared-node isolation in Context Pack

- The corrected `ctx impact` result contained only `FEAT-REVIEW`, `REQ-REVIEW-001`, `ADR-PRECISION-001`, and the three direct review tests, but the same shared end-to-end test could still bridge Context Pack traversal from review into indexing and context-compilation contracts.
- Added traversal state that distinguishes explicit/structural roots from nodes reached semantically. Direct seeds and their one-hop callers/callees may discover product intent; reached Requirements, Invariants, Decisions, and Domain Concepts may expose their direct implementation, Feature, and tests; reached tests, code, and Features do not fan out into unrelated intent.
- Stale candidates remain visible for uncertainty but no longer propagate traversal. Rejected and weak/double-inferred edges remain non-traversable, and the existing combined three-hop budget is preserved.
- Added a Context Pack regression proving a shared journey test cannot connect review intent to an unrelated indexing requirement. All focused Context Pack tests and strict `ctx-core` Clippy pass.

Next: commit, rebuild, and compare the release Context Pack against the pre-fix dogfood output before running all workspace and packaging gates.

## 2026-08-17 — Context traversal dogfood verification

- Rebuilt and indexed the Context Pack repair. The 5,000-token review query no longer includes `INV-COMMIT-001` or `REQ-CONTEXT-001`; its only product neighborhood is `FEAT-REVIEW`, `REQ-REVIEW-001`, and `ADR-PRECISION-001`, followed by directly related review code and tests.
- The same release query confirms `ctx impact ctx_core.review.build_review_findings` remains isolated to the conservative-review neighborhood after the Context Pack change.
- Indexing the changed `traversable` implementation correctly marked its explicit `INV-EPISTEMIC-001` enforcement claim stale. Refined that invariant to state the path-specific traversal rule explicitly so the documentation change re-establishes the claim instead of hiding or manually mutating graph state.

Next: import the revalidated invariant at a clean commit, require `ctx status` to return `ready`, then run the complete release gate matrix.

## 2026-08-17 — First-party context release verification

- Imported the refined epistemic invariant at `3164d02`: one document versioned, five explicit links recreated, and zero mappings unresolved. The stale enforcement edge was closed and replaced by a current evidence-backed assertion.
- Final graph health is `ready`: 37 files, 627 symbols, 4 Features, 6 Requirements, 5 Invariants, 4 Decisions, 1,139 active structural facts, 83 active assertions, and zero stale semantics.
- Validated all 19 YAML documents, unique IDs, and all 69 implementation/test mappings against exact current symbol resolution.
- Product acceptance checks prove review impact contains exactly `FEAT-REVIEW`, `REQ-REVIEW-001`, `ADR-PRECISION-001`, and three directly linked tests. A 600-token Context Pack stays within budget and excludes the previously leaked indexing/context contracts.
- Database integrity checks found zero duplicate current edge fingerprints, orphan current edges, active edges to retired nodes, and calls to non-callable symbol kinds.
- `cargo fmt --all -- --check` passed.
- Strict locked workspace Clippy with all targets/features and `-D warnings` passed.
- The locked complete workspace suite passed all 51 tests, including the new traversal-boundary and shared-node isolation regressions.
- The locked optimized workspace build passed; `ctx 0.1.0` runs from the release binary.
- Normal and `mcp` Docker Compose configurations both validate.

The repository now carries its own compact, tested product context and uses that corpus as a release-level dogfood fixture.

## 2026-08-17 — Next-agent handoff

- Audited the delivered implementation against the MVP, milestones, evaluation plan, future scope, and explicit non-goals in `product_conclu.md` and `eng_conclu.md`.
- Confirmed that the local technical MVP and its public command surface are complete, while product-value experiments, a ground-truth historical PR corpus, richer M6 signals, and data-interaction extraction remain future work.
- Added `prompt.md` as a self-contained handoff: current architecture and graph health, completed work, known gaps, protected invariants, non-goals, an evaluation-first next mission, release gates, and definition of done.

Next: the following agent should build a small deterministic evaluation corpus/harness before expanding semantic automation or parser breadth.

## 2026-08-17 — Evaluation harness, and three real defects it caught on first run

- Followed the handoff's priority mission: built `ctx-eval`, a deterministic evaluation harness, before touching semantic scoring, DB-interaction extraction, or new parsers.
- Added a machine-readable ground-truth schema (`crates/ctx-eval/src/report.rs`: `Check`, `CaseRun`, `CheckOutcome`, `CaseReport`, `Summary`) that scores recorded `ReviewReport`/`ImpactReport`/`ContextPack` results against typed checks, classified by kind (`Recall`, `Precision`, `Classification`, `Budget`) instead of a single pass/fail count.
- Added a seven-case Git-history corpus (`crates/ctx-eval/src/cases.rs`) covering the fixture list from `prompt.md`: a real cancellation-entitlement regression, formatting-only noise, an unrelated refactor, a rename with an unchanged body, a deleted decision-mapped implementation, a stale semantic mapping (index the regression, confirm `ctx review` goes quiet while `ctx impact` surfaces the staleness as uncertainty), and shared-test isolation between two independent requirements.
- The harness (`crates/ctx-eval/src/harness.rs`) drives `ctx-app`/`ctx-adapters` directly (the same `GitRepo`/`SqliteStore`/`AnalyzerRegistry`/`IndexRunner`/`ReviewRunner`/`QueryService` the CLI wires up) against a fresh temporary Git repository per case, so no product logic is duplicated and results are real typed structs, not re-parsed JSON. `cargo run -p ctx-eval` prints the full report and exits non-zero on any failure; `cargo test -p ctx-eval` is the regression gate.
- Running the corpus against the pre-fix code immediately failed the shared-test-isolation case: `ctx impact` and `ctx context` both leaked an unrelated requirement/feature into a seed's impact just because a shared workflow test structurally called the seed. Traced to two independent defects and fixed both, each with a focused `ctx-core` regression test in addition to the corpus case:
  - `expand_semantics` (impact.rs) and `expand_candidates` (context_pack.rs) both treat a seed's one-hop structural callers/callees as free roots with unconditional semantic-expansion rights (an intentional, documented policy). Neither excluded *tests* from that exemption, so a shared test reached only because it calls the seed could still bridge into every other requirement it covers. Fixed by threading an `is_seed`/`semantic_root` state that denies a test node root rights unless it is itself the explicit seed.
  - `detect_seeds` (context_pack.rs) could auto-select a test as a lexical seed purely from incidental term overlap with the identifiers it calls (e.g. a task mentioning "subscription" matching a test's `Calls: ... Subscription ...` content). An auto-seeded test gets the same free-root rights. Fixed by excluding test nodes from lexical auto-seeding; a relevant test remains reachable through its covering requirement or an explicit seed's own call graph.
- Re-running the corpus after both fixes passed all 7 cases / 32 checks, then hit a second, unrelated defect while dogfooding the real repository: `ctx index` failed with "index plan contains more than one write for stable key 'symbol:rust:ctx_core.context_pack.touches:Function'" once a transition modified both `impact.rs` and `context_pack.rs` together (both files define a trivial one-line `touches()` helper with a byte-identical whitespace-stripped body). Root-caused via direct SQLite inspection: `ctx_core.impact.touches` had never been stored under its own canonical path — an earlier transition's cross-file structural-fingerprint fallback had already silently merged it into `context_pack.touches`'s identity, undetected because the two files were never modified in the same transition until now.
  - Fixed by (a) requiring the symbol *name* to also match for the cross-file fingerprint fallback (a genuine move keeps its name; two unrelated same-shaped one-liners usually do not), and (b) scoping the "already claimed" key tracking to the whole transition instead of resetting it per file, so one file's exact-path self-match makes that identity unavailable to every other file's fallback matching. Added two `ctx-core` regressions, including one that reproduces the exact historical corruption (same name, same shape, one file with a valid prior identity and one without, both modified together).
- Revalidated the five `.context` documents whose mapped symbols this touched (`INV-DETERMINISM-001`, `INV-EPISTEMIC-001`, `REQ-IMPACT-001`, `REQ-INDEX-001`, `ADR-CORE-001`): refined each statement to name the guarantee actually strengthened and linked the new regression tests as evidence, then re-imported so the resulting stale assertions were replaced by current ones instead of left stale or hidden.
- Verified `cargo fmt --all -- --check`, strict locked workspace Clippy, the complete locked workspace test suite (61 tests: 18 adapters + 2 app + 2 CLI e2e + 32 core + 5 eval unit + 1 eval regression + 2 MCP), a locked release build, and both Docker Compose profiles.
- Rebuilt the release binary at the clean commit and re-indexed this repository at its own `HEAD`: `ctx status` is `ready`, 44 files, 697 symbols, 1,349 active edges, 89 active assertions, zero stale semantics, zero unresolved mappings, and an immediate second `ctx index` is a no-op.

**Baseline honesty note**: the corpus is 7 synthetic cases on one small fixture family, not the historical-PR ground truth `product_conclu.md` sections 49-52 ask for; it does not yet measure review precision, impact-understanding time, Context Pack agent task success, or maintenance cost on a real repository, and none of the five critical experiments or kill criteria have been evaluated. What it does establish: a regression benchmark that reproducibly catches exactly the class of defect ("shared node silently bridges unrelated intent," "identity silently conflated") this project has hit multiple times before, now enforced in both `cargo test -p ctx-eval` and focused `ctx-core` unit tests, with a machine-readable, precision/recall-shaped report instead of a graph-size vanity metric.

Next: extend the corpus with real or realistic multi-commit history (not just synthetic single-diff cases) and, per the mission's step 2, add the missing fixture points (added call, changed DB write once DB-interaction extraction exists); only after that baseline is broader should semantic scoring (M6 `ResolutionScore.semantic_similarity`/`explicit`/`alias` fields, currently unused), DB/interaction extraction, or additional language parsers be picked up.

## 2026-08-17 — Corpus extension: added-call fixture and a real multi-commit case

- Followed the prior handoff's two concrete gaps: the "added call" fixture point from `prompt.md`'s evaluation matrix, and a corpus case built from real sequential commits instead of one synthetic before/after diff. "Changed DB write" stays blocked on DB/interaction extraction, unchanged from before.
- Added `added-call-discovers-intent` (`crates/ctx-eval/src/cases.rs`): a brand-new, unmapped caller (`billing.scheduler.run_daily_cancellation_sweep`) added elsewhere in the repository and given a real call to the mapped `SubscriptionService.cancel`. Confirms the fresh structural (`FACT`) call edge alone is enough for `ctx impact` on the new caller to discover `REQ-SUB-014`/`INV-SUB-003` through one-hop semantic discovery, while `ctx review` correctly stays silent on those intents for the new, unmapped symbol itself (`ChangeKind::BehaviorPotentiallyChanged` with reason "symbol added", zero findings).
- Added `multi-commit-feature-evolution`: three real sequential commits (base, then an actual behavior change extending cancellation with a grace period, then an unrelated signature-only follow-up adding an unused `dry_run` flag), each indexed in turn, reviewed as one span the way a reviewer looks at a whole PR. First run's expected checks were wrong twice, and fixing them is the actual finding:
  - I initially expected `ctx impact` on `cancel` to exclude `ADR-SUB-001`. It doesn't, correctly — `StripeWebhookHandler.handle_subscription_update` is a real one-hop structural caller of `cancel` and gets the same free semantic-discovery rights any other one-hop caller does (the same policy `added-call-discovers-intent` exercises deliberately). My check was wrong, not the product; removed it.
  - I initially expected fresh `High`-severity findings on `INV-SUB-003`/`REQ-SUB-014` for the full span. Instead the assertions go stale after the *first* commit is indexed (the grace-period change is a real behavior change to the same guarded logic `stale_semantic_mapping` already covers) and, correctly, stay stale through the second commit's index since nothing re-verifies them — `ctx review` reports them via `stale_relationships`, not as new findings, exactly matching the documented "stale means needs re-verification, never silently reactivated" rule. Corrected the case's ground truth to `Check::StaleRelationshipContains` instead of `Check::FindingIntentPresent`/`FindingSeverity`.
  - The case does still prove something new: reviewing the full three-commit span classifies `cancel` as `ContractChanged` (the second commit's added `dry_run` parameter changes the public signature), not `BehaviorPotentiallyChanged` — confirming multi-commit spans can shift the aggregate classification away from what any single commit in the middle would show, and that `ctx review --base` handles an arbitrary multi-commit range correctly, not just single-step diffs.
- Two things noticed while designing cases for the fixture matrix's other named points, deliberately not built into cases because I could not establish correct ground truth by static reading alone and didn't want to assert against unverified behavior:
  - `ChangeKind::RefactorLikely` looks structurally unreachable as currently wired. `classify_behavior_change` (`crates/ctx-core/src/review.rs`) only reaches the `RefactorLikely` branch when `body_hash` is equal, but `pair_symbols`'s entity-creation gate only creates a `ChangedEntity` at all when `body_hash`, `signature`, or `canonical_path` differs — and a `signature` difference is caught by the earlier `ContractChanged` branch before `RefactorLikely` is ever checked. Whether this is dead code or an intentional seam for a future signal isn't obvious from the code alone.
  - The fixture matrix's "symbol move" (a mapped symbol relocated to a different file, body unchanged) is not obviously handled as a `Rename` by `ctx review`: `resolve_changed_entities` pairs symbols per `FileChange` entry (`pair_symbols` is called once per Git-reported change), so a cross-file move that Git reports as a delete-from-old-file plus add-to-new-file (likely, since `git diff -M` needs the whole file to be similarity-matched, not one moved symbol out of several) would show as two independent entities — a deletion in the old file and an addition in the new one — rather than a single `Rename`. Confirming this needs an actual run, not just reading the pairing logic.
- Verified `cargo fmt --all -- --check`, strict locked workspace Clippy (`-D warnings`), and the full locked workspace suite: still 61 non-eval tests plus the corpus now at 9 cases / 45 checks (`cargo run -p ctx-eval` and `cargo test -p ctx-eval` both green).

Next: either resolve the two observations above (confirm/fix the cross-file move classification gap, decide whether `RefactorLikely` is dead code to remove or a seam to wire up) or move on to the mission's next priority — DB/interaction extraction, which unblocks the one remaining fixture matrix point (changed DB write) and a chunk of the north-star Context Pack coverage.

## 2026-08-17 — Cross-file move merge, and a confirmed-intentional dead branch

- Picked up the prior handoff's two open observations before moving to DB/interaction extraction, since both bore on review correctness (the flagship guarantee) rather than coverage.
- Confirmed the graph was already `ready` at `HEAD` (`2799ad1`, the ctx-skill/MCP dogfood commit only touched `.claude/`/`.mcp.json`, neither indexed); ran `ctx index` to catch up before investigating.
- **Cross-file symbol move produced duplicate high-severity findings — confirmed by running it, not just reading the pairing logic.** Reproduced by hand in a scratch Git repo: moved the mapped `SubscriptionService` class (body byte-identical) from `subscription.py` to a new `cancellation.py`. Git reports this as one `Modified` (old file) plus one `Added` (new file), never a `Renamed`. `resolve_changed_entities` (`crates/ctx-core/src/review.rs`) calls `pair_symbols` once per `FileChange`, so the two halves were never compared to each other — even though `graph_symbol_key`'s cross-file fingerprint fallback already resolved both to the *same* stored stable key. The result was two independent `BehaviorPotentiallyChanged` entities sharing one stable key, each independently walking the same `ENFORCES`/`IMPLEMENTS` edges: `ctx review` surfaced 4 duplicate high-severity findings against `INV-SUB-003`/`REQ-SUB-014` for a change that should have been silent.
  - Fixed with `merge_cross_file_moves`: after per-file pairing, group entities by stable key; a key with exactly one after-less ("deleted") and one before-less ("added") entity is collapsed into the single `Rename` they already represent at the graph level, reusing the same identity mechanism `graph_symbol_key` already applies — not a new, less conservative matching rule.
  - Added a focused `ctx-core` regression (`cross_file_move_merges_into_one_silent_rename`) and promoted the fixture-matrix "symbol move" case from a scratch probe into the corpus (`symbol-move-across-files`, `crates/ctx-eval/src/cases.rs`): asserts `Rename` classification and zero findings on the moved, still-mapped method. Corpus is now 10 cases / 49 checks.
- **`ChangeKind::RefactorLikely` is unreachable, and wiring it up would be actively unsafe, not merely unfinished.** Traced precisely: `pair_symbols` only creates a `ChangedEntity` when `body_hash`, `signature`, or `canonical_path` differ; `classify_behavior_change` only reaches the `RefactorLikely` branch when `body_hash` is equal. Since `structural_fingerprint` is a hash of the same bytes with whitespace stripped, `body_hash` equality always implies `structural_fingerprint` equality — so any pair reaching that branch would already have taken the `Rename` branch (if `canonical_path` differs) or never been paired as changed at all (if it doesn't). Verified this holds across every boundary `pair_symbols` and `classify_behavior_change` distinguish, not just the equal-fields case, with a new regression driven through `pair_symbols` itself (`refactor_likely_is_unreachable_through_pair_symbols`).
  - Considered wiring it up with a weaker signal (e.g. "body changed but the call set didn't"), since that's the only kind of deterministic proxy available without semantic analysis. Rejected: that signal is exactly what the `cancellation-behavior-change` case's regression scenario looks like (a guard condition removed while the surrounding calls stay the same) — using it to auto-suppress `RefactorLikely` findings would silently reintroduce the flagship bug class this project exists to catch. `eng_conclu.md` §38 rules out proving semantic equivalence for exactly this reason.
  - Left the variant and its suppressed-from-findings treatment in place (removing a spec-named `ChangeKind` isn't this task's call to make), but documented the unreachability directly on `classify_behavior_change` and backed it with the regression above, so it reads as a deliberate, tested gap instead of accidentally dead code a future change could "fix" into an unsound suppression.
- Verified `cargo fmt --all -- --check`, strict locked workspace Clippy (`-D warnings`, one `clippy::match_same_arms` finding from the first merge draft, fixed by restructuring the match), the full locked workspace suite (64 non-eval tests, up from 61: 34 core + 18 adapters + 2 app + 2 CLI e2e + 2 MCP + 5 eval unit + 1 eval regression), and a locked release build.

Next: re-index this repository at the clean commit, confirm `ctx status` is `ready` with a no-op second index, then either move on to the mission's next priority (DB/interaction extraction) or continue closing remaining fixture-matrix/secondary gaps noted in `prompt.md`.

## 2026-08-17 — Evidence-backed database interactions

- Added one language-neutral database-access IR and deterministic static SQL recognition for common `SELECT`/`INSERT`/`UPDATE`/`DELETE`/`MERGE` forms. Python and Rust adapters only inspect literals inside known execution calls/macros; Python f-strings, dynamic SQL, arbitrary prose, ORM expressions, and unsupported syntax remain unknown instead of becoming guessed facts.
- Persisted repository-scoped `DbEntity` nodes plus temporal `ReadsFrom`/`WritesTo` facts with parser provenance, exact commit validity, statement hash, and source-line evidence. Entity lifetime follows the complete current symbol snapshot, including retirement after the last access disappears and deduplication of repeated access facts.
- Integrated data contracts into `status`, `impact`, `explain`, Context Packs, semantic-candidate evidence, and review. Review now emits a concrete signal such as `database writes changed: subscriptions -> subscription_archive` without calling that signal a proven requirement violation.
- Extended the corpus with `changed-database-write`; the baseline is now 11 cases / 59 checks (15 recall, 26 precision, 16 classification, 2 budget), all green with zero harness errors.
- Promoted the workspace to 0.2.0 and published the operator/developer documentation set: README, architecture, changelog, evaluation methodology, refreshed first-party context, and a current `prompt.md` handoff. The documentation explicitly distinguishes deterministic regression evidence from the external historical-PR and participant experiments that have not been run and cannot be fabricated locally.

## 2026-08-17 — Explicit Context Pack seed isolation

- Release dogfooding with an 800-token request and an explicit cancellation symbol found that `detect_seeds` still added five unrelated lexical roots. They consumed the budget before the seed's direct `subscriptions` contract could be selected.
- Made any successfully resolved explicit file/symbol seed a hard scope boundary. Lexical auto-seeding now runs only when no explicit seed resolves; semantic and structural traversal from the explicit seed still supplies bounded related context.
- Added `explicit_seed_prevents_unrelated_lexical_roots` and promoted the rule into `REQ-CONTEXT-001`. The same release query now uses exactly one seed, returns the direct `WritesTo subscriptions` data contract and evidence, consumes 338/800 estimated tokens, and contains no unrelated lexical roots.

## 2026-08-17 — 0.2.0 release verification

- First-party index at `2bf128c` is `current` and `ready`: 45 files, 752 symbols, 12 database entities, 4 Features, 7 Requirements, 5 Invariants, 4 Decisions, 1,524 active edges, 1,414 structural facts, 110 assertions, and zero inferred, stale, or rejected semantic edges.
- Static explanation for `fixtures.subscriptions.src.billing.subscription.SubscriptionService.cancel -> subscriptions` returns one active `WritesTo` FACT at confidence 1.0 with `python_tree_sitter` provenance and `fixtures/subscriptions/src/billing/subscription.py#lines:21` evidence.
- Full-span self-review reported 11 expected high-confidence contract re-verification items and 2 medium architectural items, with zero stale relationships. Every high item has modified direct tests and `possible_requirement_drift: false`; the complete gates below re-ran those tests.
- `cargo fmt --all -- --check` passed; strict locked workspace Clippy with all targets/features and `-D warnings` passed.
- The locked complete workspace suite passed 74 tests: 23 adapters, 2 app, 2 CLI e2e, 39 core, 5 eval unit, 1 eval corpus regression, and 2 MCP.
- The executable evaluation report passed all 11 cases / 59 checks with zero errors.
- The locked optimized workspace and rustdoc for all six crates built successfully; the release binary reports `ctx 0.2.0`.
- Normal and MCP Docker Compose profiles validate. A clean multi-stage Docker build produced `ctx:0.2.0` with non-root `ctx:ctx`; running the image reports `ctx 0.2.0`.
- SQLite `integrity_check` is `ok`, with zero duplicate current edge fingerprints, current edges without current endpoints, current edges to retired nodes, or calls to non-callable targets.

The remaining work is deliberately external product validation rather than hidden local implementation debt: review precision on labeled historical PRs, impact-understanding time, agent A/B task success and token efficiency, mapping-maintenance cost, and the published kill-criteria evaluation all require real repositories or participants. The reproducible local 0.2.0 release is complete without pretending those experiments have already happened.
