use std::{fs, path::PathBuf};

use ctx_app::ports::{LanguageAnalyzer, PortError};
use ctx_core::ir::{
    CallSite, DatabaseAccess, FileAnalysis, SourceRange, SymbolDefinition, SymbolKind,
};
use thiserror::Error;
use tree_sitter::{Node, Parser, TreeCursor};

use crate::{
    analyzer::AnalyzerModule,
    database::{sql_entities, static_string_content},
};

#[derive(Debug, Error)]
pub enum PythonAnalysisError {
    #[error("could not read Python source '{path}': {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("could not configure the Python parser")]
    Language,
    #[error("could not parse Python source '{0}'")]
    Parse(String),
    #[error("Python source '{0}' is not valid UTF-8")]
    InvalidUtf8(String),
}

pub struct PythonAnalyzer {
    root: PathBuf,
}

impl PythonAnalyzer {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Parses a supplied source string into the normalized code IR.
    ///
    /// # Errors
    ///
    /// Returns [`PythonAnalysisError`] when Tree-sitter cannot be configured or
    /// does not produce a syntax tree.
    pub fn analyze_source(
        relative_path: &str,
        source: &str,
    ) -> Result<FileAnalysis, PythonAnalysisError> {
        let mut parser = Parser::new();
        let language = tree_sitter_python::LANGUAGE.into();
        parser
            .set_language(&language)
            .map_err(|_| PythonAnalysisError::Language)?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| PythonAnalysisError::Parse(relative_path.to_owned()))?;
        let module = module_path(relative_path);
        let mut symbols = Vec::new();
        collect_definitions(
            tree.root_node(),
            source.as_bytes(),
            &module,
            None,
            &mut symbols,
        );
        symbols.sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
        Ok(FileAnalysis {
            path: relative_path.to_owned(),
            language: "python".to_owned(),
            analysis_version: "python-tree-sitter-v2".to_owned(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            symbols,
        })
    }
}

impl LanguageAnalyzer for PythonAnalyzer {
    fn analysis_version(&self, _relative_path: &str) -> Result<String, PortError> {
        Ok("python-tree-sitter-v2".to_owned())
    }

    fn analyze(&self, relative_path: &str) -> Result<FileAnalysis, PortError> {
        let path = self.root.join(relative_path);
        let bytes = fs::read(&path).map_err(|source| {
            PortError::new(
                PythonAnalysisError::Read {
                    path: path.display().to_string(),
                    source,
                }
                .to_string(),
            )
        })?;
        let source = std::str::from_utf8(&bytes).map_err(|_| {
            PortError::new(PythonAnalysisError::InvalidUtf8(path.display().to_string()).to_string())
        })?;
        Self::analyze_source(relative_path, source)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn analyze_text(&self, relative_path: &str, source: &str) -> Result<FileAnalysis, PortError> {
        Self::analyze_source(relative_path, source)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

impl AnalyzerModule for PythonAnalyzer {
    fn language(&self) -> &'static str {
        "python"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py"]
    }
}

fn collect_definitions(
    node: Node<'_>,
    source: &[u8],
    module: &str,
    parent: Option<&str>,
    symbols: &mut Vec<SymbolDefinition>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let definition = unwrap_decorated(child);
        if matches!(
            definition.kind(),
            "function_definition" | "class_definition"
        ) {
            if let Some(symbol) = parse_definition(definition, source, module, parent) {
                let canonical = symbol.canonical_path.clone();
                symbols.push(symbol);
                if let Some(body) = definition.child_by_field_name("body") {
                    collect_definitions(body, source, module, Some(&canonical), symbols);
                }
            }
        } else {
            collect_definitions(child, source, module, parent, symbols);
        }
    }
}

fn unwrap_decorated(node: Node<'_>) -> Node<'_> {
    if node.kind() == "decorated_definition" {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "function_definition" | "class_definition"))
            .unwrap_or(node)
    } else {
        node
    }
}

fn parse_definition(
    node: Node<'_>,
    source: &[u8],
    module: &str,
    parent: Option<&str>,
) -> Option<SymbolDefinition> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source).ok()?.to_owned();
    let canonical_path = parent.map_or_else(
        || format!("{module}.{name}"),
        |parent_path| format!("{parent_path}.{name}"),
    );
    let is_class = node.kind() == "class_definition";
    let kind = if is_class {
        SymbolKind::Class
    } else if name.starts_with("test_") {
        SymbolKind::Test
    } else if parent.is_some() {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    let signature = node
        .child_by_field_name("parameters")
        .and_then(|parameters| parameters.utf8_text(source).ok())
        .map(str::to_owned);
    let body = node.child_by_field_name("body").unwrap_or(node);
    let body_bytes = &source[body.byte_range()];
    let calls = if is_class {
        Vec::new()
    } else {
        collect_calls(body, source)
    };
    let database_accesses = if is_class {
        Vec::new()
    } else {
        collect_database_accesses(body, source)
    };
    Some(SymbolDefinition {
        name,
        canonical_path,
        kind,
        range: source_range(node),
        signature,
        body_hash: blake3::hash(body_bytes).to_hex().to_string(),
        structural_fingerprint: structural_fingerprint(body_bytes),
        calls,
        database_accesses,
        schema_tables: Vec::new(),
    })
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
    if !root && matches!(node.kind(), "function_definition" | "class_definition") {
        return;
    }
    if node.kind() == "call"
        && let Some(function) = node.child_by_field_name("function")
        && let Ok(expression) = function.utf8_text(source)
    {
        let callee = expression
            .rsplit('.')
            .next()
            .unwrap_or(expression)
            .to_owned();
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

fn collect_database_accesses(node: Node<'_>, source: &[u8]) -> Vec<DatabaseAccess> {
    let mut accesses = Vec::new();
    let mut cursor = node.walk();
    visit_database_calls(&mut cursor, source, &mut accesses, true);
    accesses.sort_by(|left, right| {
        left.entity
            .cmp(&right.entity)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.range.start_byte.cmp(&right.range.start_byte))
    });
    accesses.dedup_by(|left, right| {
        left.entity == right.entity
            && left.kind == right.kind
            && left.statement_hash == right.statement_hash
    });
    accesses
}

fn visit_database_calls(
    cursor: &mut TreeCursor<'_>,
    source: &[u8],
    accesses: &mut Vec<DatabaseAccess>,
    root: bool,
) {
    let node = cursor.node();
    if !root && matches!(node.kind(), "function_definition" | "class_definition") {
        return;
    }
    if node.kind() == "call"
        && let Some(function) = node.child_by_field_name("function")
        && function
            .utf8_text(source)
            .is_ok_and(is_database_execution_call)
        && let Some(arguments) = node.child_by_field_name("arguments")
    {
        collect_sql_literals(arguments, source, accesses);
    }
    if cursor.goto_first_child() {
        loop {
            visit_database_calls(cursor, source, accesses, false);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn is_database_execution_call(expression: &str) -> bool {
    matches!(
        expression.rsplit('.').next().unwrap_or(expression),
        "execute"
            | "executemany"
            | "execute_sql"
            | "query"
            | "query_one"
            | "query_all"
            | "fetch"
            | "fetchone"
            | "fetchall"
            | "fetch_one"
            | "fetch_all"
    )
}

fn collect_sql_literals(node: Node<'_>, source: &[u8], accesses: &mut Vec<DatabaseAccess>) {
    if node.kind() == "string"
        && let Ok(literal) = node.utf8_text(source)
        && let Some(statement) = static_string_content(literal)
    {
        let statement_hash = blake3::hash(statement.as_bytes()).to_hex().to_string();
        for (kind, entity) in sql_entities(&statement) {
            accesses.push(DatabaseAccess {
                entity,
                kind,
                range: source_range(node),
                statement_hash: statement_hash.clone(),
            });
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_sql_literals(child, source, accesses);
    }
}

fn source_range(node: Node<'_>) -> SourceRange {
    SourceRange {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    }
}

fn structural_fingerprint(bytes: &[u8]) -> String {
    let normalized = bytes
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    blake3::hash(&normalized).to_hex().to_string()
}

fn module_path(path: &str) -> String {
    let without_extension = path.strip_suffix(".py").unwrap_or(path);
    let without_source_root = without_extension
        .strip_prefix("src/")
        .unwrap_or(without_extension);
    without_source_root.replace('/', ".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_classes_methods_tests_and_calls() {
        let source = r"
class SubscriptionService:
    def cancel(self, subscription):
        preserve(subscription)

def preserve(subscription):
    return subscription.paid_until

def test_cancel_keeps_access():
    SubscriptionService().cancel(None)
";
        let analysis = PythonAnalyzer::analyze_source("src/billing/subscription.py", source)
            .expect("valid Python");
        let paths = analysis
            .symbols
            .iter()
            .map(|symbol| symbol.canonical_path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&"billing.subscription.SubscriptionService.cancel"));
        assert!(
            analysis
                .symbols
                .iter()
                .any(|symbol| symbol.kind == SymbolKind::Test)
        );
        let cancel = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "cancel")
            .expect("cancel method");
        assert_eq!(cancel.calls[0].callee, "preserve");
    }

    #[test]
    fn extracts_static_sql_reads_and_writes_only_from_execution_calls() {
        let source = r#"
def load_and_archive(connection):
    ignored = "DELETE FROM should_not_be_a_fact"
    rows = connection.execute(
        "SELECT id FROM subscriptions JOIN accounts ON accounts.id = subscriptions.account_id"
    )
    connection.executemany("INSERT INTO subscription_archive(id) VALUES (?)", rows)
"#;
        let analysis =
            PythonAnalyzer::analyze_source("src/billing/storage.py", source).expect("valid Python");
        let function = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "load_and_archive")
            .expect("storage function");
        let accesses = function
            .database_accesses
            .iter()
            .map(|access| (access.kind, access.entity.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            accesses,
            vec![
                (ctx_core::ir::DatabaseAccessKind::Read, "accounts"),
                (
                    ctx_core::ir::DatabaseAccessKind::Write,
                    "subscription_archive"
                ),
                (ctx_core::ir::DatabaseAccessKind::Read, "subscriptions"),
            ]
        );
    }
}
