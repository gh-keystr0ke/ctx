# Changelog

All notable changes to `ctx` are documented here. The project follows semantic versioning.

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
