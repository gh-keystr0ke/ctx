# Static reports

`ctx report` turns the current local graph into files that can be browsed without a running ctx process:

```bash
ctx report html
ctx report markdown --out ../team-docs/context
```

Both commands require `.ctx/ctx.db` to describe the repository's current `HEAD`. A relative `--out` path is resolved from the repository root; an absolute path is used as given. Without `--out`, HTML is written to `.ctx/report/html/` and Markdown to `.ctx/report/markdown/`. The default report root is added to Git's repository-local exclude file, not the shared `.gitignore`.

## Contents

The dashboard reports the indexed commit, health, counts, notices, catalog links, and product graph. Catalog pages cover exactly:

- Feature
- Requirement
- Invariant
- Decision
- DomainConcept
- Event
- ExternalSystem

The separate source tree contains folders, files, and CodeSymbol nodes. A method is nested under another symbol only when that symbol exists in the same file and its canonical path is the unique longest segment-prefix of the method's path. Free functions and ambiguous owners remain flat; the report does not infer language-specific ownership from names.

Endpoint, ApiEndpoint, and DbEntity nodes do not receive catalogs or detail pages in this version. They remain typed leaves in relationship sections, matching the way impact output separates API/data contracts from implementation. Product entities and CodeSymbol nodes do receive detail pages with their stored incoming/outgoing claims, evidence, provenance, artifact history, and source links where available.

HTML includes a client-side substring search across every indexed node. It reads embedded JSON so the site works directly from `file://`; no local server or network fetch is needed. The dashboard canvas supports kind filters, pan, zoom, and navigation to entity pages. CSS and JavaScript are shipped as ordinary static assets so teams can reskin or extend the site independently of the Rust projection.

Markdown emits one dashboard, one file per catalog, the source tree, and one file per detailed entity. Detail pages include Mermaid relationship diagrams supported by common Git forges.

## Determinism and replacement safety

Every report list is derived from stable graph order or explicitly sorted identity keys. Generated content contains the source commit but no generation timestamp or absolute checkout path. Given the same commit and unchanged local ctx database, two Markdown runs produce the same paths and bytes.

Output is first written as a complete staged tree. ctx marks generated roots with `.ctx-report.json`; a later run may replace such a root, but refuses to overwrite an existing file or directory without that marker. Stale pages therefore disappear on regeneration without risking an unrelated user-owned directory.

## Visibility boundary

Reports are an internal team view in this release and intentionally include documents of every visibility. `visibility: public` still controls `ctx export` federation, but does not filter `ctx report`. A separate visibility policy is planned as an explicit report modifier; until then, do not publish generated reports outside the trust boundary of the indexed repository.
