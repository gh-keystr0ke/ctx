# Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo +1.88.0 check --workspace --all-targets --locked
```

The end-to-end tests build temporary real Git repositories. They cover the complete subscriptions product journey and a mixed Python/Rust/Go repository through initialization, indexing, language-scoped call resolution, status, and Rust/Go diff review.

Run the deterministic product-quality corpus separately:

```bash
cargo run --locked -p ctx-eval
```

It currently covers 26 Git-history cases and 113 typed checks across recall, precision/noise, classification, and Context Pack budgets, including changed DB writes and the full schema-aware review/reconciliation/impact scenario set. This is a reproducible regression baseline, not a statistically significant product study. See [docs/evaluation.md](evaluation.md) for the case matrix, current result, and the human/agent experiments that still require real participants or historical PR ground truth.

The workspace tests also enforce structural regression bounds: production functions may not exceed 100 lines, the CLI composition root may not grow beyond 1,800 lines, and the audited `too_many_arguments` exception count may not increase. New code should normally stay around 40–50 lines per function; the 100-line ceiling is a migration guard for the existing codebase, not a target. GitHub Actions additionally runs line coverage (65% floor), RustSec/cargo-deny checks, and a scheduled sharded mutation job. Credentialed GitLab/Jira and locally installed agent-CLI compatibility tests are opt-in with `cargo test -p ctx-adapters --test live_contracts -- --ignored`.

## Add another language module

Language support is isolated behind `AnalyzerModule` and the normalized `FileAnalysis` IR. To add TypeScript, Java, or Zig:

1. Add one parser adapter that implements `LanguageAnalyzer` and `AnalyzerModule`, including its language name and extensions.
2. Declare the language in `language.rs` and register its constructor in `AnalyzerRegistry::builtins`.
3. Normalize definitions, ranges, signatures, body/structure fingerprints, calls, and any supported static interactions into the existing IR; never expose parser nodes above the adapter crate. Bump the module's analysis version whenever those semantics change so existing repositories are safely reparsed.
4. Add parser-unit coverage plus a mixed-language executable test before enabling it in the default config.

The registry rejects duplicate language names and extension ownership. Indexing, review, CLI, MCP, persistence, and graph algorithms require no language-specific branch.

See [docs/architecture.md](architecture.md) for boundaries and persistence semantics, distilled from the original product and engineering specs.
