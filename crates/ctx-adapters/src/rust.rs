use std::{fs, path::PathBuf};

use ctx_app::ports::{LanguageAnalyzer, PortError};
use ctx_core::ir::{CallSite, FileAnalysis, SourceRange, SymbolDefinition, SymbolKind};
use thiserror::Error;
use tree_sitter::{Node, Parser, TreeCursor};

use crate::analyzer::AnalyzerModule;

#[derive(Debug, Error)]
pub enum RustAnalysisError {
    #[error("could not read Rust source '{path}': {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("could not configure the Rust parser")]
    Language,
    #[error("could not parse Rust source '{0}'")]
    Parse(String),
    #[error("Rust source '{0}' contains syntax errors")]
    Syntax(String),
    #[error("Rust source '{0}' is not valid UTF-8")]
    InvalidUtf8(String),
}

pub struct RustAnalyzer {
    root: PathBuf,
}

impl RustAnalyzer {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Parses Rust into the same normalized IR consumed by the language-neutral
    /// indexing, review, and graph layers.
    ///
    /// # Errors
    ///
    /// Returns [`RustAnalysisError`] when Tree-sitter cannot be configured or
    /// the complete source does not form a valid syntax tree.
    pub fn analyze_source(
        relative_path: &str,
        source: &str,
    ) -> Result<FileAnalysis, RustAnalysisError> {
        let mut parser = Parser::new();
        let language = tree_sitter_rust::LANGUAGE.into();
        parser
            .set_language(&language)
            .map_err(|_| RustAnalysisError::Language)?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| RustAnalysisError::Parse(relative_path.to_owned()))?;
        if tree.root_node().has_error() {
            return Err(RustAnalysisError::Syntax(relative_path.to_owned()));
        }
        let module = module_path(relative_path);
        let mut symbols = Vec::new();
        collect_items(
            tree.root_node(),
            source.as_bytes(),
            &module,
            None,
            &mut symbols,
        );
        symbols.sort_by(|left, right| {
            left.canonical_path
                .cmp(&right.canonical_path)
                .then_with(|| left.range.start_byte.cmp(&right.range.start_byte))
        });
        Ok(FileAnalysis {
            path: relative_path.to_owned(),
            language: "rust".to_owned(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            symbols,
        })
    }
}

impl LanguageAnalyzer for RustAnalyzer {
    fn analyze(&self, relative_path: &str) -> Result<FileAnalysis, PortError> {
        let path = self.root.join(relative_path);
        let bytes = fs::read(&path).map_err(|source| {
            PortError::new(
                RustAnalysisError::Read {
                    path: path.display().to_string(),
                    source,
                }
                .to_string(),
            )
        })?;
        let source = std::str::from_utf8(&bytes).map_err(|_| {
            PortError::new(RustAnalysisError::InvalidUtf8(path.display().to_string()).to_string())
        })?;
        Self::analyze_source(relative_path, source)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn analyze_text(&self, relative_path: &str, source: &str) -> Result<FileAnalysis, PortError> {
        Self::analyze_source(relative_path, source)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

impl AnalyzerModule for RustAnalyzer {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }
}

fn collect_items(
    node: Node<'_>,
    source: &[u8],
    module: &str,
    method_parent: Option<&str>,
    symbols: &mut Vec<SymbolDefinition>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_item" => {
                if let Some(symbol) = parse_function(child, source, module, method_parent) {
                    symbols.push(symbol);
                }
            }
            "function_signature_item" => {
                if let Some(symbol) = parse_function_signature(child, source, module, method_parent)
                {
                    symbols.push(symbol);
                }
            }
            "struct_item" => push_named_item(child, source, module, SymbolKind::Struct, symbols),
            "enum_item" => {
                push_named_item(child, source, module, SymbolKind::Enum, symbols);
            }
            "trait_item" => {
                if let Some(symbol) = parse_named_item(child, source, module, SymbolKind::Trait) {
                    let canonical = symbol.canonical_path.clone();
                    symbols.push(symbol);
                    if let Some(body) = child.child_by_field_name("body") {
                        collect_items(body, source, module, Some(&canonical), symbols);
                    }
                }
            }
            "mod_item" => {
                if let Some(symbol) = parse_named_item(child, source, module, SymbolKind::Module) {
                    let canonical = symbol.canonical_path.clone();
                    symbols.push(symbol);
                    if let Some(body) = child.child_by_field_name("body") {
                        collect_items(body, source, &canonical, None, symbols);
                    }
                }
            }
            "type_item" => push_named_item(
                child,
                source,
                method_parent.unwrap_or(module),
                SymbolKind::TypeAlias,
                symbols,
            ),
            "const_item" | "static_item" => push_named_item(
                child,
                source,
                method_parent.unwrap_or(module),
                SymbolKind::Constant,
                symbols,
            ),
            "impl_item" => {
                if let Some(body) = child.child_by_field_name("body")
                    && let Some(type_node) = child.child_by_field_name("type")
                    && let Some(type_name) = implemented_type_name(type_node, source)
                {
                    let parent = join_path(module, &type_name);
                    collect_items(body, source, module, Some(&parent), symbols);
                }
            }
            _ => collect_items(child, source, module, method_parent, symbols),
        }
    }
}

fn push_named_item(
    node: Node<'_>,
    source: &[u8],
    module: &str,
    kind: SymbolKind,
    symbols: &mut Vec<SymbolDefinition>,
) {
    if let Some(symbol) = parse_named_item(node, source, module, kind) {
        symbols.push(symbol);
    }
}

fn parse_named_item(
    node: Node<'_>,
    source: &[u8],
    module: &str,
    kind: SymbolKind,
) -> Option<SymbolDefinition> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source)
        .ok()?
        .to_owned();
    let body = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("value"))
        .unwrap_or(node);
    Some(SymbolDefinition {
        canonical_path: join_path(module, &name),
        name,
        kind,
        range: source_range(node),
        signature: signature_before(node, body, source),
        body_hash: hash_bytes(&source[body.byte_range()]),
        structural_fingerprint: structural_fingerprint(&source[body.byte_range()]),
        calls: Vec::new(),
    })
}

fn parse_function(
    node: Node<'_>,
    source: &[u8],
    module: &str,
    method_parent: Option<&str>,
) -> Option<SymbolDefinition> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source)
        .ok()?
        .to_owned();
    let body = node.child_by_field_name("body")?;
    let parent = method_parent.unwrap_or(module);
    let kind = if has_test_attribute(node, source) {
        SymbolKind::Test
    } else if method_parent.is_some() {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    Some(SymbolDefinition {
        canonical_path: join_path(parent, &name),
        name,
        kind,
        range: source_range(node),
        signature: signature_before(node, body, source),
        body_hash: hash_bytes(&source[body.byte_range()]),
        structural_fingerprint: structural_fingerprint(&source[body.byte_range()]),
        calls: collect_calls(body, source),
    })
}

fn parse_function_signature(
    node: Node<'_>,
    source: &[u8],
    module: &str,
    method_parent: Option<&str>,
) -> Option<SymbolDefinition> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source)
        .ok()?
        .to_owned();
    let signature = node.utf8_text(source).ok()?.trim();
    let parent = method_parent.unwrap_or(module);
    Some(SymbolDefinition {
        canonical_path: join_path(parent, &name),
        name,
        kind: if method_parent.is_some() {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        },
        range: source_range(node),
        signature: Some(signature.to_owned()),
        body_hash: hash_bytes(signature.as_bytes()),
        structural_fingerprint: structural_fingerprint(signature.as_bytes()),
        calls: Vec::new(),
    })
}

fn has_test_attribute(node: Node<'_>, source: &[u8]) -> bool {
    let mut sibling = node.prev_named_sibling();
    while let Some(attribute) = sibling {
        if attribute.kind() != "attribute_item" {
            break;
        }
        if attribute.utf8_text(source).is_ok_and(is_test_attribute) {
            return true;
        }
        sibling = attribute.prev_named_sibling();
    }
    false
}

fn is_test_attribute(attribute: &str) -> bool {
    let normalized = attribute
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let path = normalized
        .strip_prefix("#[")
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(&normalized)
        .split('(')
        .next()
        .unwrap_or_default();
    path.rsplit("::").next() == Some("test")
}

fn implemented_type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if matches!(node.kind(), "type_identifier" | "primitive_type") {
        return node.utf8_text(source).ok().map(str::to_owned);
    }
    if let Some(name) = node.child_by_field_name("name") {
        return implemented_type_name(name, source);
    }
    if let Some(type_node) = node.child_by_field_name("type") {
        return implemented_type_name(type_node, source);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| implemented_type_name(child, source))
}

fn collect_calls(node: Node<'_>, source: &[u8]) -> Vec<CallSite> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    visit_calls(&mut cursor, source, &mut calls, true);
    calls.sort_by(|left, right| {
        left.range
            .start_byte
            .cmp(&right.range.start_byte)
            .then_with(|| left.callee.cmp(&right.callee))
    });
    calls
}

fn visit_calls(cursor: &mut TreeCursor<'_>, source: &[u8], calls: &mut Vec<CallSite>, root: bool) {
    let node = cursor.node();
    if !root && node.kind() == "function_item" {
        return;
    }
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && let Ok(expression) = function.utf8_text(source)
        && let Some(callee) = callee_name(expression)
    {
        calls.push(CallSite {
            callee,
            range: source_range(node),
        });
    }
    if cursor.goto_first_child() {
        loop {
            visit_calls(cursor, source, calls, false);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn callee_name(expression: &str) -> Option<String> {
    let without_generics = expression.split('<').next().unwrap_or(expression);
    let candidate = without_generics
        .rsplit(['.', ':'])
        .find(|part| !part.is_empty())?
        .trim();
    (!candidate.is_empty()).then(|| candidate.to_owned())
}

fn signature_before(node: Node<'_>, body: Node<'_>, source: &[u8]) -> Option<String> {
    let signature = std::str::from_utf8(&source[node.start_byte()..body.start_byte()])
        .ok()?
        .trim();
    (!signature.is_empty()).then(|| signature.to_owned())
}

fn source_range(node: Node<'_>) -> SourceRange {
    SourceRange {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn structural_fingerprint(bytes: &[u8]) -> String {
    let normalized = bytes
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    hash_bytes(&normalized)
}

fn module_path(path: &str) -> String {
    let mut components = path.split('/').collect::<Vec<_>>();
    if let Some(file) = components.last_mut() {
        *file = file.strip_suffix(".rs").unwrap_or(file);
    }
    if let Some(source_index) = components.iter().position(|component| *component == "src") {
        let crate_name = if source_index >= 2 && components[source_index - 2] == "crates" {
            components[source_index - 1].replace('-', "_")
        } else {
            "crate".to_owned()
        };
        let mut modules = components[(source_index + 1)..].to_vec();
        if matches!(modules.last(), Some(&"lib" | &"main" | &"mod")) {
            modules.pop();
        }
        return std::iter::once(crate_name)
            .chain(modules.into_iter().map(str::to_owned))
            .collect::<Vec<_>>()
            .join(".");
    }
    components.join(".")
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}.{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_items_methods_tests_and_calls() {
        let source = r"
pub struct SubscriptionService<T>(T);

pub trait Cancels {
    fn cancel(&self, subscription: &Subscription);
}

impl<T> SubscriptionService<T> {
    pub fn cancel(&self, subscription: &Subscription) {
        preserve(subscription);
        self.audit();
    }

    fn audit(&self) {}
}

fn preserve(subscription: &Subscription) -> Date {
    subscription.paid_until()
}

#[cfg(test)]
mod tests {
    #[test]
    fn cancel_keeps_access() {
        preserve(&fixture());
    }
}
";

        let analysis = RustAnalyzer::analyze_source("crates/subscriptions/src/service.rs", source)
            .expect("Rust analysis");

        assert_eq!(analysis.language, "rust");
        assert!(analysis.symbols.iter().any(|symbol| {
            symbol.canonical_path == "subscriptions.service.SubscriptionService"
                && symbol.kind == SymbolKind::Struct
        }));
        let cancel = analysis
            .symbols
            .iter()
            .find(|symbol| {
                symbol.canonical_path == "subscriptions.service.SubscriptionService.cancel"
            })
            .expect("inherent method");
        assert_eq!(cancel.kind, SymbolKind::Method);
        assert_eq!(
            cancel
                .calls
                .iter()
                .map(|call| call.callee.as_str())
                .collect::<Vec<_>>(),
            vec!["preserve", "audit"]
        );
        assert!(analysis.symbols.iter().any(|symbol| {
            symbol.canonical_path == "subscriptions.service.Cancels.cancel"
                && symbol.kind == SymbolKind::Method
        }));
        assert!(analysis.symbols.iter().any(|symbol| {
            symbol.canonical_path == "subscriptions.service.tests.cancel_keeps_access"
                && symbol.kind == SymbolKind::Test
        }));
    }

    #[test]
    fn rejects_incomplete_rust_instead_of_indexing_a_partial_tree() {
        let error = RustAnalyzer::analyze_source("src/lib.rs", "fn broken(")
            .expect_err("invalid Rust must fail");

        assert!(matches!(error, RustAnalysisError::Syntax(_)));
    }

    #[test]
    fn derives_collision_resistant_workspace_module_paths() {
        assert_eq!(module_path("src/lib.rs"), "crate");
        assert_eq!(module_path("src/billing/mod.rs"), "crate.billing");
        assert_eq!(
            module_path("crates/ctx-core/src/indexing.rs"),
            "ctx_core.indexing"
        );
    }
}
