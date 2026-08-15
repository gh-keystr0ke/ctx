# Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

The end-to-end tests build temporary real Git repositories. They cover the complete subscriptions product journey and a mixed Python/Rust/Go repository through initialization, indexing, language-scoped call resolution, status, and Rust/Go diff review.

Run the deterministic product-quality corpus separately:

```bash
cargo run --locked -p ctx-eval
```

It currently covers 25 Git-history cases and 102 typed checks across recall, precision/noise, classification, and Context Pack budgets, including changed DB writes and the full schema-aware review/reconciliation/impact scenario set. This is a reproducible regression baseline, not a statistically significant product study. See [docs/evaluation.md](evaluation.md) for the case matrix, current result, and the human/agent experiments that still require real participants or historical PR ground truth.

## Add another language module

Language support is isolated behind `AnalyzerModule` and the normalized `FileAnalysis` IR. To add TypeScript, Java, or Zig:

1. Add one parser adapter that implements `LanguageAnalyzer` and `AnalyzerModule`, including its language name and extensions.
2. Declare the language in `language.rs` and register its constructor in `AnalyzerRegistry::builtins`.
3. Normalize definitions, ranges, signatures, body/structure fingerprints, calls, and any supported static interactions into the existing IR; never expose parser nodes above the adapter crate. Bump the module's analysis version whenever those semantics change so existing repositories are safely reparsed.
4. Add parser-unit coverage plus a mixed-language executable test before enabling it in the default config.

The registry rejects duplicate language names and extension ownership. Indexing, review, CLI, MCP, persistence, and graph algorithms require no language-specific branch.

See [docs/architecture.md](architecture.md) for boundaries and persistence semantics. The detailed product and engineering source specifications are in [product_conclu.md](../product_conclu.md) and [eng_conclu.md](../eng_conclu.md).
