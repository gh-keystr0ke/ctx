# Changelog

All notable changes to `ctx` are documented here. The project follows semantic versioning.

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
