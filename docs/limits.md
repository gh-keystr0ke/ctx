# Current limits

`ctx` is deliberately conservative: absence of a fact is always preferred to a guessed one. This page collects every place that shows up as a real constraint.

## Languages

- Python, Rust, and Go are the built-in parsers; TypeScript, Java, and Zig modules are not implemented yet.
- Language modules are compiled into the binary; dynamic shared-library loading is not supported.
- Explicit symbol mappings are exact; unresolved mappings are reported instead of guessed.

## Database interactions

- Static database extraction recognizes literal SQL inside known Python/Rust/Go execution calls and common `SELECT`/`INSERT`/`UPDATE`/`DELETE`/`MERGE` forms. Python additionally recognizes import-gated `SQLAlchemy` expressions in the exact forms `select(Model)`, `select(Model.a, Model.b)`, `insert(Model)`, `update(Model).values(...)`, and `delete(Model)`, where every model expression is a bare identifier or a direct `Model.column` attribute. A model reference resolves only by an exact imported/same-file canonical path or by one unique language-scoped bare-name model carrying a static schema definition. `select(Model.column)` contributes the named columns; `.values(name=value)` and one dictionary literal whose keys are all string literals contribute write columns, while any other `.values(...)` shape leaves columns wholly unknown. Ambiguous or unresolved model references, `Session.query`, `.execute(select(...))` wrapping, dataflow-aliased models, and re-exported models remain unknown permanently, not as temporary recognition gaps. Dynamic SQL, stored procedures, and dialect-complete parsing likewise remain unknown rather than guessed. For literal SQL, column-level evidence is only extracted for writes (`UPDATE ... SET`, an `INSERT`/`MERGE` explicit column list); `DELETE`, a bare `INSERT ... VALUES` with no column list, and every `SELECT`/read form stay table-level, since attributing `SELECT` columns across joins without a real parser is guessing, not recognizing.
- goose migration parsing reads only `-- +goose Up` and recognizes `CREATE TABLE` (including table-level `PRIMARY KEY`/`UNIQUE`/`FOREIGN KEY`/`CHECK`), `ALTER TABLE ... ADD/DROP/RENAME COLUMN`, `ALTER TABLE ... RENAME TO`, `ALTER TABLE ... ALTER COLUMN ... TYPE/SET-DROP NOT NULL/SET-DROP DEFAULT`, `DROP TABLE`, and `CREATE/DROP INDEX`. `ALTER TABLE ... ADD/DROP CONSTRAINT` is deliberately unsupported (a bare constraint name cannot be resolved to columns without the table's already-declared column list). It is a deterministic recognizer, not a SQL dialect parser, and never merges multiple migrations into one computed "current" schema for storage — each migration file's declaration stays its own fact; a best-effort ordered replay exists only as a diagnostic for `SQLAlchemy` reconciliation, never as a stored fact.
- SQLAlchemy model recognition requires a static `__tablename__` string literal and reads `Column(...)`/`mapped_column(...)` attribute assignments, including `nullable=`/`primary_key=`/`unique=`/`default=`/`server_default=`/`ForeignKey(...)`; it does not resolve `Base`/inheritance, relationships, mixins, `Index(...)`/`__table_args__`, or Alembic migration history.
- Schema-aware review compares a schema-declaring file's diff or a migration's own declared operations; it does not diff an ORM model against its own migration history over time (that is `ctx status`'s reconciliation, which is presence-only and does not compare types/nullability between sources).

### Pyright-backed Python write inferences

`ctx index` remains a deterministic, file-local fact pass and never starts or depends on Pyright. After indexing a clean committed checkout, `ctx infer-types` can run a separate, optional whole-program enrichment pass. It starts one warm `pyright-typeserver --stdio` process, queries the structured Type Server Protocol at each exact write-site expression, and fully recomputes the `pyright` inference layer for the indexed commit. Candidate inputs are reparsed on demand rather than persisted by `ctx index`, so this layer does not change incremental Fact analysis or the Python analyzer version.

The epistemic boundary is explicit:

- syntax that directly names a database target remains a `Fact` with confidence `1`;
- a write target recovered from Pyright's computed type becomes `WritesTo` with `ClaimClass::Inference`, `SourceKind::TypeInference`, producer `pyright`, and default confidence `0.90`;
- `0.90` distinguishes the claim class and is not a statistical probability. Ambiguous or weak results are dropped rather than recorded at a lower score, and `--confidence` accepts only values below `1`;
- the source is the indexed function/method containing the write site. An active Fact for the same source symbol and table suppresses the inference; independent source symbols remain independent edges.

Tier 1 supports `obj.column = value`, `obj.column += value`, `Session.add(obj)`, `Session.add_all([a, b])`, `Session.merge(obj)`, and `Session.delete(obj)`. `add_all` enumerates only a literal list and resolves each element independently. Attribute mutation is emitted only when the attribute is already a statically known `Column`/`mapped_column` on the exact indexed model; relationship assignments, properties, and arbitrary attributes are dropped rather than widened into a potentially misleading table-level write. Unit-of-work calls require both a ctx-known ORM-model argument and a method declaration that Pyright resolves to SQLAlchemy's `Session` or `AsyncSession` API. A collection, queue, or application method named `add`, even when passed an ORM model, is therefore not a database inference.

The resolved type must identify concrete class declarations that match exactly one already-indexed SQLAlchemy model and exactly one existing `DbEntity`; class-name text alone is never enough. `Any`, `Unknown`, containers, unresolved declarations, and a remaining optional type such as `Model | None` produce no edge. A narrowed value works naturally because Pyright returns `Model` at that site. A union of model classes is accepted only when every alternative resolves and all alternatives map to the same table; for an attribute write, the column must be known on every alternative. ctx does not remove `None`, unwrap containers, or perform Python points-to/dataflow itself. Typed flows such as `Session.get`, `scalar_one`, helper returns, and annotated parameters work only insofar as Pyright already computes the concrete model type at the mutation site.

The release installer builds a checksum-verified Pyright Type Server from the pinned upstream source when Node.js 18.12+ and `npm` are available, installing its launcher beside `ctx`; `CTX_INSTALL_PYRIGHT=0` skips this optional step. The Type Server remains optional because indexing and every Fact workflow are independent of Node. A source-only `cargo install` does not install the oracle.

The command accepts `--pyright <path>`, `--confidence <value>`, and `--timeout-ms <value>`. `-vv` prints a source location, candidate form, probe, structured type identity, resolved model/table, and reason for each dropped candidate. Startup and total inference-phase time plus candidate/query/result counts are always reported. A missing executable is an actionable successful no-op that opens no graph transaction; malformed responses, timeouts, and server crashes abort the phase. Persistence happens once, after all queries, in one transaction, so a catastrophic oracle or storage failure cannot leave a half-recomputed inference layer. There is no cross-run cache in Tier 1 because a file-only cache key would be unsound when imported modules change.

## HTTP contracts

See [docs/api-contracts.md](api-contracts.md#current-limits) for the full list: from code, Python-only with FastAPI/Flask/`requests`/`httpx`-only and five HTTP methods with heuristic parameter classification; from OpenAPI, 3.0/3.1 documents only with local `$ref`s only; and no fact for a dynamic route or call URL either way.

## Federation

See [docs/federation.md](federation.md#current-limits): local sibling checkouts on one machine only, no remote/URL registry, a versioned manifest schema with no compatibility guessing, bounded cross-repository HTTP tracing, and no field-level data-flow tracing.

## Heuristics and AI

- Heuristic implementation-link suggestions use lexical, structural, test, and shared-database-interaction signals, not embeddings or an LLM.
- `ctx enrich` requires a real, already-authenticated `claude`, `codex`, or `agy` CLI on `PATH`; there is no direct API-key/HTTP integration with any model provider, and no local/offline model support.
- An AI-derived candidate is always `INFERENCE`, never asserted automatically: `ctx enrich` only ever produces a `pending` candidate, and only `ctx verify --knowledge --accept` (a human) or `ctx verify --knowledge --auto` (an agent, honestly recorded as such) turns one into a real `.context/*.yaml` document. Duplicate detection against existing documents is lexical term-overlap, not semantic similarity or an embedding model.

## Integrations and scope

- Endpoint identity and outbound-call resolution are structural facts, not a runtime trace: there is no web UI, cloud backend, runtime tracing, or multi-repository graph beyond the local federation snapshot described above.
- GitLab's default scope ingests issues, merge requests, comments, and MR commits incrementally. `--scope business-linked` instead relists MR summaries, selects only MRs tied to local branch names/commit SHAs/explicit `!IID`s, then fetches details for those MRs; it neither fetches GitLab issues nor advances the all-scope cursor. All requested collections are fully paginated. `CTX_GITLAB_TOKEN` is optional for public projects and required when the configured project requires authentication.
- Jira Cloud's business-linked scope accepts issue keys only from current Git and selected repository MRs/comments, with zero related-issue hops by default and an explicit `--related-depth` bound. Jira Server/Data Center, changelog, worklog, attachments, and classic-project custom-field epic links are not implemented.
- Business-linked scope is deterministic, not semantic: if Git/MR text contains no resolvable Jira key, that evidence is excluded even if a human would consider it relevant. `ctx artifacts prune` is a dry run unless `--apply` is passed; it never edits `.context/` or `.ctx-candidates/`, and only reports pending candidates whose evidence would be pruned.
- GitLab and Jira honor `Retry-After` on HTTP 429 and use bounded retries for rate limits and transient gateway/service failures. GitHub remains unimplemented; the reserved artifact provider/kinds do not constitute a connector.
- Review is a conservative aid, not a proof that behavior is correct.
