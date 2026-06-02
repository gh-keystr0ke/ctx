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
