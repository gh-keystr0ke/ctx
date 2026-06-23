# Architecture

`ctx` uses a functional core with an imperative shell. Pure decisions depend only on normalized domain values; Git, Tree-sitter, SQLite, CLI, and MCP remain replaceable boundaries.

```text
Git + .context + configured languages
          │
          ▼
   adapters (I/O)
          │ normalized IR / port values
          ▼
      app use cases
          │
          ▼
 core planning, traversal, ranking, review
          │ explicit effects
          ▼
     SQLite adapter
          │
          ├── CLI
          └── MCP
```

## Workspace boundaries

| Crate | Responsibility |
| --- | --- |
| `ctx-core` | Domain types and pure incremental planning, impact, review, context compilation, and verification scoring |
| `ctx-app` | Narrow ports and use-case orchestration |
| `ctx-adapters` | Git inspection, analyzer registry, Python/Rust/Go Tree-sitter normalization, `.context` parsing, and SQLite transactions |
| `ctx-cli` | Human and JSON command surface |
| `ctx-mcp` | Thin stdio protocol adapter over the application services |

The core has no dependency on filesystem paths as handles, database row IDs, SQL, Git processes, parser nodes, CLI arguments, or protocol requests. SQLite row IDs never become domain identity.

## Index transition

1. Git resolves `HEAD`, verifies that configured source and `.context` inputs are committed, and reports changed paths.
2. The configured current source set is reconciled with the stored snapshot, covering config-only include/exclude changes.
3. The analyzer registry dispatches each added, modified, or renamed source by extension; its Tree-sitter module emits a complete language-neutral `FileAnalysis`, including supported static database interactions.
4. The pure incremental planner matches identities conservatively and emits node writes, retirements, structural/data-fact invalidation and rebuild, and semantic-staleness effects.
5. SQLite applies the plan and commit marker in one transaction; business documents and explicit claims are synchronized in a second bounded transaction.

Repeated indexing at the same commit performs no source parsing. Changed source bodies mark attached non-fact claims stale. Structural facts owned by the analyzer are closed and rebuilt; semantic assertions are never silently recreated as facts.

Each persisted file records its analyzer normalization version. If an upgraded module changes extraction or call-resolution semantics, version reconciliation schedules affected files for reparse even when Git bytes and `HEAD` are unchanged. A true same-commit no-op is returned only after source scope and analyzer versions are current.

## Identity and validity

Stable keys are derived from repository-relative file paths, language-qualified canonical symbols, or human-owned business IDs. Symbol matching never crosses a language boundary and tries, in order:

1. the same canonical path;
2. a unique name/signature match in the prior file;
3. a unique structural fingerprint.

Ambiguity creates a new identity instead of conflating two symbols. Versions are valid from one commit until a later transition closes them. An active relationship may still be marked stale when its input fingerprint changed; queries surface that uncertainty instead of hiding it.

Stable symbol keys have the form `symbol:<language>:<canonical-path>:<kind>`. Human-authored mappings may use the shorter canonical path when it is unique or the complete stable key to disambiguate equal paths across languages. Static call resolution is likewise language-scoped.

## Language modules

`AnalyzerRegistry` is the only analyzer passed to indexing and review. It owns independent self-describing `AnalyzerModule` implementations and routes by source extension. Git filtering uses the same supported-language declarations, so configured discovery and parser dispatch cannot disagree.

Python, Rust, and Go are built in today. A future TypeScript, Java, or Zig adapter supplies parser-specific extraction but must return the common IR: complete-file hash, symbol kind/name/canonical path, version range, signature, body hash, whitespace-insensitive structural fingerprint, simple call sites, and normalized interactions it can prove. Parser syntax types remain inside `ctx-adapters`; application and core crates stay unchanged. Duplicate module names or extension claims are rejected when the registry is built.

Rust canonical paths use `crate` for a root `src/` tree and the workspace crate directory for `crates/<name>/src/`. Inline modules and implemented types extend that namespace. Trait-implementation methods additionally include the complete trait name (including generic arguments), so legal pairs such as `impl From<u8>` and `impl From<u16>` cannot collide. Syntax-error trees are rejected rather than partially indexed.

Go canonical paths are directory-based rather than file-based, matching Go's one-package-per-directory convention: every `.go` file in a directory shares that directory's package namespace regardless of its declared `package` clause, and a root-level file with no directory falls back to `main`. Methods are namespaced under their receiver's innermost named type (pointer and generic receivers unwrap to that name). Interfaces map to the shared `Trait` symbol kind; defined/alias types that are not `struct`/`interface` map to `TypeAlias`. `const`/`var` blocks emit one symbol per bound identifier. Go raw (backtick) string literals reuse the same static-SQL-literal recognizer as Python/Rust since they cannot contain escapes or interpolation. Syntax-error trees are rejected rather than partially indexed, matching the other modules.

## Static database interactions

Language adapters inspect only recognized execution calls and macros. A shared deterministic SQL recognizer extracts normalized entity identifiers from common static `SELECT ... FROM/JOIN`, `INSERT INTO`, `UPDATE`, `DELETE FROM`, and `MERGE INTO/USING` forms. Interpolated/dynamic SQL and unsupported syntax produce no fact.

The normalized IR attaches typed reads/writes to their owning symbol. The core planner derives repository-scoped `DbEntity` nodes and `READS_FROM`/`WRITES_TO` `FACT` edges, deduplicates repeated accesses, retires entities after the last current access disappears, and gives every edge a static-analysis producer, commit boundary, source file, line locator, and statement fingerprint. Analyzer-version bumps force a same-commit reparse when extraction semantics change.

Impact and Context Pack treat a direct data interaction as one bounded structural hop and report database entities separately from implementation. Review compares before/after access sets and reports a concrete database-read/write change signal; it does not claim the related product contract is violated. Verification scoring can use a shared database interaction as one explained signal, never as an assertion by itself.

## Claims and provenance

Every edge is classified as one of:

- `FACT`: deterministic static structure such as containment or a uniquely resolved call;
- `ASSERTION`: explicit documentation or a separately recorded human acceptance;
- `INFERENCE`: a heuristic candidate with its derivation signals.

Evidence records source kind, URI, locator, commit, author/timestamp when relevant, and strength. Accepting an inference preserves the original inference and creates a distinct human assertion. Rejecting it preserves a rejected record so the same fingerprint is not repeatedly proposed.

## Query policies

Impact and Context Pack traversal are bounded and typed. Structural adjacency is limited to the seed neighborhood, semantic expansion is capped, rejected claims are excluded, and an inferred edge cannot recursively amplify another inference. Deterministic ordering is used throughout user-visible output.

Context compilation reserves budget for evidence and prioritizes invariants and requirements before implementation, tests, direct data contracts, adjacency, and low-confidence material. Its token estimate is deliberately conservative and never reports a pack above the requested limit.

Review compares normalized symbols before and after a Git diff, classifies the change, then joins only strong implementation claims to product intent and linked tests. Non-behavioral changes are suppressed by default. A finding is rendered from stored claims and evidence; the reviewer-facing rationale is not generated by an LLM.

## Storage and operations

SQLite runs locally with foreign keys, WAL mode, idempotent migrations, and transactional batch writes. The schema separates repositories, commits, stable nodes, node versions, claims/edges, sources, evidence, annotations, aliases, and derivations. The database and its WAL files are repository-local private state under `.ctx/`.

No source is sent to a network service. MCP tools are read-only and use the same query/review services as the CLI. Optional remote inference is intentionally absent from this release.

Status health is assembled in `ctx-app` from Git freshness, the effective source scope, current typed node counts, epistemic relationship counts, and staleness. This keeps health classification out of terminal rendering and prevents a large structural graph with no product assertions from being labelled ready.
