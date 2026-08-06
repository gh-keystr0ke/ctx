//! Extracts code comments and docstrings as source material for external
//! knowledge discovery (prompt3.md PR-CODEDOC-001/002/003): a comment can
//! carry real product knowledge ("Keep access until `paid_until` because the
//! current period has already been paid for"), but it is never more
//! authoritative than the code itself — this module only ever locates
//! comment text and its nearest enclosing symbol; turning that into an
//! [`crate::artifact::Artifact`]/[`crate::artifact::ArtifactLink`] and
//! deciding what, if anything, it implies is a later pass's job.

use serde::{Deserialize, Serialize};

use crate::ir::SymbolDefinition;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeDocKind {
    Comment,
    Docstring,
}

/// One extracted comment/docstring block, with its 1-based line span and
/// the canonical path of the symbol judged to be its nearest enclosing
/// context, if any (PR-CODEDOC-002: attach to the nearest code entity, not
/// only the file).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CodeDocCandidate {
    pub kind: CodeDocKind,
    pub text: String,
    pub start_line: usize,
    pub end_line: usize,
    pub nearest_symbol: Option<String>,
}

/// Extracts every line-comment block and (for Python) triple-quoted
/// docstring in `source`, each attributed to its nearest enclosing symbol
/// from `symbols` (already produced by this file's own language analyzer,
/// so no new parsing pass is needed to locate them).
#[must_use]
pub fn extract_code_docs(
    source: &str,
    language: &str,
    symbols: &[SymbolDefinition],
) -> Vec<CodeDocCandidate> {
    let mut candidates = Vec::new();
    if let Some(prefix) = line_comment_prefix(language) {
        candidates.extend(extract_line_comment_blocks(source, prefix));
    }
    if language == "python" {
        candidates.extend(extract_python_docstrings(source));
    }
    for candidate in &mut candidates {
        candidate.nearest_symbol =
            nearest_symbol(candidate, symbols).map(|symbol| symbol.canonical_path.clone());
    }
    candidates
}

const fn line_comment_prefix(language: &str) -> Option<&'static str> {
    match language.as_bytes() {
        b"python" => Some("#"),
        b"rust" | b"go" => Some("//"),
        _ => None,
    }
}

/// Groups consecutive same-prefix comment lines into one block, so a
/// multi-line `///`-style doc comment becomes one artifact, not one per
/// line. A line is only recognized when the prefix starts the trimmed
/// line — a trailing `// note` after real code is not a standalone comment
/// block this pass extracts.
fn extract_line_comment_blocks(source: &str, prefix: &str) -> Vec<CodeDocCandidate> {
    let mut candidates = Vec::new();
    let mut block: Option<(usize, Vec<&str>)> = None;
    for (index, line) in source.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let text = rest.strip_prefix(' ').unwrap_or(rest);
            match &mut block {
                Some((_, lines)) => lines.push(text),
                None => block = Some((line_number, vec![text])),
            }
        } else if let Some((start_line, lines)) = block.take() {
            candidates.push(comment_block(start_line, line_number - 1, &lines));
        }
    }
    if let Some((start_line, lines)) = block {
        let end_line = start_line + lines.len() - 1;
        candidates.push(comment_block(start_line, end_line, &lines));
    }
    candidates
}

fn comment_block(start_line: usize, end_line: usize, lines: &[&str]) -> CodeDocCandidate {
    CodeDocCandidate {
        kind: CodeDocKind::Comment,
        text: lines.join("\n").trim().to_owned(),
        start_line,
        end_line,
        nearest_symbol: None,
    }
}

/// Recognizes every triple-quoted (`"""..."""` or `'''...'''`) string in
/// Python source as a docstring candidate. Deliberately a textual scan, not
/// a claim that every such string is actually documentation-in-position
/// (a triple-quoted string used as an ordinary value would also match) —
/// PR-CODEDOC-003 already keeps every candidate at evidence/inference tier
/// regardless, so an over-inclusive match here is imprecision, not an
/// integrity violation.
fn extract_python_docstrings(source: &str) -> Vec<CodeDocCandidate> {
    let mut candidates = Vec::new();
    for quote in ["\"\"\"", "'''"] {
        let mut search_from = 0;
        while let Some(relative_start) = source[search_from..].find(quote) {
            let start = search_from + relative_start;
            let content_start = start + quote.len();
            let Some(relative_end) = source[content_start..].find(quote) else {
                break;
            };
            let content_end = content_start + relative_end;
            let text = source[content_start..content_end].trim().to_owned();
            if !text.is_empty() {
                let start_line = source[..start].lines().count().max(1);
                let end_line = source[..content_end].lines().count().max(start_line);
                candidates.push(CodeDocCandidate {
                    kind: CodeDocKind::Docstring,
                    text,
                    start_line,
                    end_line,
                    nearest_symbol: None,
                });
            }
            search_from = content_end + quote.len();
        }
    }
    candidates.sort_by_key(|candidate| candidate.start_line);
    candidates
}

/// The symbol whose range most tightly encloses the comment (a docstring,
/// which lives inside its function/class body), or, failing that, the
/// nearest symbol starting on or immediately after the comment's last line
/// (a doc comment written directly above the item it documents — the
/// common Rust/Go convention). Ties prefer the smaller/later-starting
/// range, which is the more specific enclosing scope.
fn nearest_symbol<'a>(
    candidate: &CodeDocCandidate,
    symbols: &'a [SymbolDefinition],
) -> Option<&'a SymbolDefinition> {
    let enclosing = symbols
        .iter()
        .filter(|symbol| {
            symbol.range.start_line <= candidate.start_line
                && candidate.end_line <= symbol.range.end_line
        })
        .min_by_key(|symbol| symbol.range.end_line - symbol.range.start_line);
    if enclosing.is_some() {
        return enclosing;
    }
    symbols
        .iter()
        .filter(|symbol| symbol.range.start_line >= candidate.end_line)
        .min_by_key(|symbol| symbol.range.start_line)
}

#[cfg(test)]
mod tests {
    use crate::ir::{CallSite, SourceRange, SymbolKind};

    use super::*;

    fn symbol(
        name: &str,
        canonical_path: &str,
        start_line: usize,
        end_line: usize,
    ) -> SymbolDefinition {
        SymbolDefinition {
            name: name.to_owned(),
            canonical_path: canonical_path.to_owned(),
            kind: SymbolKind::Function,
            range: SourceRange {
                start_byte: 0,
                end_byte: 1,
                start_line,
                end_line,
            },
            signature: None,
            body_hash: "hash".to_owned(),
            structural_fingerprint: "shape".to_owned(),
            calls: Vec::<CallSite>::new(),
            database_accesses: Vec::new(),
            schema_tables: Vec::new(),
            api_endpoints: Vec::new(),
            external_calls: Vec::new(),
        }
    }

    #[test]
    fn groups_consecutive_doc_comment_lines_into_one_block_attached_to_the_following_symbol() {
        let source = "// Keep access until paid_until because the current period\n\
                       // has already been paid for.\n\
                       fn cancel() {\n\
                       }\n";
        let symbols = vec![symbol("cancel", "billing.cancel", 3, 4)];

        let candidates = extract_code_docs(source, "rust", &symbols);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].kind, CodeDocKind::Comment);
        assert_eq!(
            candidates[0].text,
            "Keep access until paid_until because the current period\nhas already been paid for."
        );
        assert_eq!(
            candidates[0].nearest_symbol.as_deref(),
            Some("billing.cancel")
        );
    }

    #[test]
    fn python_docstring_attaches_to_its_enclosing_function_not_only_the_file() {
        let source = "def cancel():\n    \"\"\"Keep access until paid_until.\"\"\"\n    pass\n";
        let symbols = vec![symbol("cancel", "billing.cancel", 1, 3)];

        let candidates = extract_code_docs(source, "python", &symbols);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].kind, CodeDocKind::Docstring);
        assert_eq!(candidates[0].text, "Keep access until paid_until.");
        assert_eq!(
            candidates[0].nearest_symbol.as_deref(),
            Some("billing.cancel")
        );
    }

    #[test]
    fn a_trailing_inline_comment_is_not_extracted_as_a_standalone_block() {
        let source = "let paid_until = subscription.paid_until; // not a doc block\n";

        let candidates = extract_code_docs(source, "rust", &[]);

        assert!(candidates.is_empty());
    }

    #[test]
    fn nearest_symbol_prefers_the_smaller_enclosing_scope_over_a_larger_one() {
        // Line 2's docstring is enclosed by both the class (1-10) and the
        // method (2-2, a one-line body) — the method is the smaller, more
        // specific enclosing scope and should win over its containing class.
        let symbols = vec![
            symbol("SubscriptionService", "billing.SubscriptionService", 1, 10),
            symbol("cancel", "billing.SubscriptionService.cancel", 2, 2),
        ];
        let docstring_only_line_two = CodeDocCandidate {
            kind: CodeDocKind::Comment,
            text: "placeholder".to_owned(),
            start_line: 2,
            end_line: 2,
            nearest_symbol: None,
        };

        let resolved = nearest_symbol(&docstring_only_line_two, &symbols)
            .expect("an enclosing symbol")
            .canonical_path
            .clone();

        assert_eq!(resolved, "billing.SubscriptionService.cancel");
    }
}
