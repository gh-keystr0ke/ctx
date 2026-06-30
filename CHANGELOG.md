# Changelog

All notable changes to `ctx` are documented here. The project follows semantic versioning.

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
