# Current limits

`ctx` is deliberately conservative: absence of a fact is always preferred to a guessed one. This page collects every place that shows up as a real constraint.

## Languages

- Python, Rust, and Go are the built-in parsers; TypeScript, Java, and Zig modules are not implemented yet.
- Language modules are compiled into the binary; dynamic shared-library loading is not supported.
- Explicit symbol mappings are exact; unresolved mappings are reported instead of guessed.

## Database interactions

- Static database extraction recognizes literal SQL inside known Python/Rust/Go execution calls and common `SELECT`/`INSERT`/`UPDATE`/`DELETE`/`MERGE` forms. Dynamic SQL, ORM expression trees, stored procedures, and dialect-complete parsing remain unknown rather than guessed. Column-level evidence is only extracted for writes (`UPDATE ... SET`, an `INSERT`/`MERGE` explicit column list); `DELETE`, a bare `INSERT ... VALUES` with no column list, and every `SELECT`/read form stay table-level, since attributing `SELECT` columns across joins without a real parser is guessing, not recognizing.
- goose migration parsing reads only `-- +goose Up` and recognizes `CREATE TABLE` (including table-level `PRIMARY KEY`/`UNIQUE`/`FOREIGN KEY`/`CHECK`), `ALTER TABLE ... ADD/DROP/RENAME COLUMN`, `ALTER TABLE ... RENAME TO`, `ALTER TABLE ... ALTER COLUMN ... TYPE/SET-DROP NOT NULL/SET-DROP DEFAULT`, `DROP TABLE`, and `CREATE/DROP INDEX`. `ALTER TABLE ... ADD/DROP CONSTRAINT` is deliberately unsupported (a bare constraint name cannot be resolved to columns without the table's already-declared column list). It is a deterministic recognizer, not a SQL dialect parser, and never merges multiple migrations into one computed "current" schema for storage — each migration file's declaration stays its own fact; a best-effort ordered replay exists only as a diagnostic for `SQLAlchemy` reconciliation, never as a stored fact.
- SQLAlchemy model recognition requires a static `__tablename__` string literal and reads `Column(...)`/`mapped_column(...)` attribute assignments, including `nullable=`/`primary_key=`/`unique=`/`default=`/`server_default=`/`ForeignKey(...)`; it does not resolve `Base`/inheritance, relationships, mixins, `Index(...)`/`__table_args__`, or Alembic migration history.
- Schema-aware review compares a schema-declaring file's diff or a migration's own declared operations; it does not diff an ORM model against its own migration history over time (that is `ctx status`'s reconciliation, which is presence-only and does not compare types/nullability between sources).

## HTTP contracts

See [docs/api-contracts.md](api-contracts.md#current-limits) for the full list: Python-only, FastAPI/Flask/`requests`/`httpx`-only, five HTTP methods, heuristic parameter classification, and no fact for a dynamic route or call URL.

## Federation

See [docs/federation.md](federation.md#current-limits): local sibling checkouts on one machine only, no remote/URL registry, a versioned manifest schema with no compatibility guessing, bounded cross-repository HTTP tracing, and no field-level data-flow tracing.

## Heuristics and AI

- Heuristic implementation-link suggestions use lexical, structural, test, and shared-database-interaction signals, not embeddings or an LLM.
- `ctx enrich` requires a real, already-authenticated `claude`, `codex`, or `agy` CLI on `PATH`; there is no direct API-key/HTTP integration with any model provider, and no local/offline model support.
- An AI-derived candidate is always `INFERENCE`, never asserted automatically: `ctx enrich` only ever produces a `pending` candidate, and only `ctx verify --knowledge --accept` (a human) or `ctx verify --knowledge --auto` (an agent, honestly recorded as such) turns one into a real `.context/*.yaml` document. Duplicate detection against existing documents is lexical term-overlap, not semantic similarity or an embedding model.

## Integrations and scope

- Endpoint identity and outbound-call resolution are structural facts, not a runtime trace: there is no web UI, cloud backend, runtime tracing, or multi-repository graph beyond the local federation snapshot described above.
- GitLab ingests issues, merge requests, comments, and MR commits; its top-level and nested collections are fully paginated. Sync is incremental for issues/merge requests via a stored per-project cursor, while each returned issue/MR's comments and commits are fetched in full. `CTX_GITLAB_TOKEN` is optional for public projects and required when the configured GitLab instance/project requires authentication.
- Jira Cloud ingests only issue keys already referenced by known artifacts, plus one provider-reported relationship hop and all comments. Jira Server/Data Center, changelog, worklog, attachments, and classic-project custom-field epic links are not implemented.
- GitLab and Jira honor `Retry-After` on HTTP 429 and use bounded retries for rate limits and transient gateway/service failures. GitHub remains unimplemented; the reserved artifact provider/kinds do not constitute a connector.
- Review is a conservative aid, not a proof that behavior is correct.
