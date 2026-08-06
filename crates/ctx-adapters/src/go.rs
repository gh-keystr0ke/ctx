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
pub enum GoAnalysisError {
    #[error("could not read Go source '{path}': {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("could not configure the Go parser")]
    Language,
    #[error("could not parse Go source '{0}'")]
    Parse(String),
    #[error("Go source '{0}' contains syntax errors")]
    Syntax(String),
    #[error("Go source '{0}' is not valid UTF-8")]
    InvalidUtf8(String),
}

pub struct GoAnalyzer {
    root: PathBuf,
}

impl GoAnalyzer {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Parses Go into the same normalized IR consumed by the language-neutral
    /// indexing, review, and graph layers.
    ///
    /// # Errors
    ///
    /// Returns [`GoAnalysisError`] when Tree-sitter cannot be configured or the
    /// complete source does not form a valid syntax tree.
    pub fn analyze_source(
        relative_path: &str,
        source: &str,
    ) -> Result<FileAnalysis, GoAnalysisError> {
        let mut parser = Parser::new();
        let language = tree_sitter_go::LANGUAGE.into();
        parser
            .set_language(&language)
            .map_err(|_| GoAnalysisError::Language)?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| GoAnalysisError::Parse(relative_path.to_owned()))?;
        if tree.root_node().has_error() {
            return Err(GoAnalysisError::Syntax(relative_path.to_owned()));
        }
        let package = package_path(relative_path);
        let is_test_file = relative_path.ends_with("_test.go");
        let mut symbols = Vec::new();
        collect_declarations(
            tree.root_node(),
            source.as_bytes(),
            &package,
            is_test_file,
            &mut symbols,
        );
        symbols.sort_by(|left, right| {
            left.canonical_path
                .cmp(&right.canonical_path)
                .then_with(|| left.range.start_byte.cmp(&right.range.start_byte))
        });
        Ok(FileAnalysis {
            path: relative_path.to_owned(),
            language: "go".to_owned(),
            analysis_version: "go-tree-sitter-v1".to_owned(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            symbols,
        })
    }
}

impl LanguageAnalyzer for GoAnalyzer {
    fn analysis_version(&self, _relative_path: &str) -> Result<String, PortError> {
        Ok("go-tree-sitter-v1".to_owned())
    }

    fn analyze(&self, relative_path: &str) -> Result<FileAnalysis, PortError> {
        let path = self.root.join(relative_path);
        let bytes = fs::read(&path).map_err(|source| {
            PortError::new(
                GoAnalysisError::Read {
                    path: path.display().to_string(),
                    source,
                }
                .to_string(),
            )
        })?;
        let source = std::str::from_utf8(&bytes).map_err(|_| {
            PortError::new(GoAnalysisError::InvalidUtf8(path.display().to_string()).to_string())
        })?;
        Self::analyze_source(relative_path, source)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn analyze_text(&self, relative_path: &str, source: &str) -> Result<FileAnalysis, PortError> {
        Self::analyze_source(relative_path, source)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

impl AnalyzerModule for GoAnalyzer {
    fn language(&self) -> &'static str {
        "go"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }
}

fn collect_declarations(
    node: Node<'_>,
    source: &[u8],
    package: &str,
    is_test_file: bool,
    symbols: &mut Vec<SymbolDefinition>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                if let Some(symbol) = parse_function(child, source, package, is_test_file, None) {
                    symbols.push(symbol);
                }
            }
            "method_declaration" => {
                if let Some(receiver) = receiver_type_name(child, source)
                    && let Some(symbol) =
                        parse_function(child, source, package, is_test_file, Some(&receiver))
                {
                    symbols.push(symbol);
                }
            }
            "type_declaration" => collect_type_specs(child, source, package, symbols),
            "const_declaration" => {
                collect_value_specs(child, source, package, "const_spec", symbols);
            }
            "var_declaration" => {
                collect_value_specs(child, source, package, "var_spec", symbols);
            }
            _ => {}
        }
    }
}

fn collect_type_specs(
    node: Node<'_>,
    source: &[u8],
    package: &str,
    symbols: &mut Vec<SymbolDefinition>,
) {
    let mut cursor = node.walk();
    for spec in node.named_children(&mut cursor) {
        if !matches!(spec.kind(), "type_spec" | "type_alias") {
            continue;
        }
        let Some(name_node) = spec.child_by_field_name("name") else {
            continue;
        };
        let Ok(name) = name_node.utf8_text(source) else {
            continue;
        };
        let kind = spec
            .child_by_field_name("type")
            .map_or(SymbolKind::TypeAlias, |type_node| match type_node.kind() {
                "struct_type" => SymbolKind::Struct,
                "interface_type" => SymbolKind::Trait,
                _ => SymbolKind::TypeAlias,
            });
        symbols.push(SymbolDefinition {
            canonical_path: join_path(package, name),
            name: name.to_owned(),
            kind,
            range: source_range(spec),
            signature: node_text(spec, source),
            body_hash: hash_bytes(&source[spec.byte_range()]),
            structural_fingerprint: structural_fingerprint(&source[spec.byte_range()]),
            calls: Vec::new(),
            database_accesses: Vec::new(),
            schema_tables: Vec::new(),
            api_endpoints: Vec::new(),
            external_calls: Vec::new(),
        });
    }
}

fn collect_value_specs(
    node: Node<'_>,
    source: &[u8],
    package: &str,
    spec_kind: &str,
    symbols: &mut Vec<SymbolDefinition>,
) {
    let mut cursor = node.walk();
    for spec in node.named_children(&mut cursor) {
        if spec.kind() != spec_kind {
            continue;
        }
        let mut names = spec.walk();
        for name_node in spec.children_by_field_name("name", &mut names) {
            let Ok(name) = name_node.utf8_text(source) else {
                continue;
            };
            symbols.push(SymbolDefinition {
                canonical_path: join_path(package, name),
                name: name.to_owned(),
                kind: SymbolKind::Constant,
                range: source_range(spec),
                signature: node_text(spec, source),
                body_hash: hash_bytes(&source[spec.byte_range()]),
                structural_fingerprint: structural_fingerprint(&source[spec.byte_range()]),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            });
        }
    }
}

fn parse_function(
    node: Node<'_>,
    source: &[u8],
    package: &str,
    is_test_file: bool,
    receiver: Option<&str>,
) -> Option<SymbolDefinition> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source)
        .ok()?
        .to_owned();
    let parent = receiver.map_or_else(
        || package.to_owned(),
        |receiver| join_path(package, receiver),
    );
    let kind = if receiver.is_some() {
        SymbolKind::Method
    } else if is_test_file && is_test_entry_point(&name) {
        SymbolKind::Test
    } else {
        SymbolKind::Function
    };
    let (calls, database_accesses, body_hash, structural_fingerprint, signature) =
        if let Some(body) = node.child_by_field_name("body") {
            (
                collect_calls(body, source),
                collect_database_accesses(body, source),
                hash_bytes(&source[body.byte_range()]),
                structural_fingerprint(&source[body.byte_range()]),
                signature_before(node, body, source),
            )
        } else {
            let text = node.utf8_text(source).ok()?;
            (
                Vec::new(),
                Vec::new(),
                hash_bytes(text.as_bytes()),
                structural_fingerprint(text.as_bytes()),
                Some(text.trim().to_owned()),
            )
        };
    Some(SymbolDefinition {
        canonical_path: join_path(&parent, &name),
        name,
        kind,
        range: source_range(node),
        signature,
        body_hash,
        structural_fingerprint,
        calls,
        database_accesses,
        schema_tables: Vec::new(),
        api_endpoints: Vec::new(),
        external_calls: Vec::new(),
    })
}

fn is_test_entry_point(name: &str) -> bool {
    ["Test", "Benchmark", "Example", "Fuzz"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn receiver_type_name(method: Node<'_>, source: &[u8]) -> Option<String> {
    let receiver_list = method.child_by_field_name("receiver")?;
    let mut cursor = receiver_list.walk();
    let declaration = receiver_list
        .named_children(&mut cursor)
        .find(|child| child.kind() == "parameter_declaration")?;
    let type_node = declaration.child_by_field_name("type")?;
    innermost_type_name(type_node, source)
}

fn innermost_type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => node.utf8_text(source).ok().map(str::to_owned),
        "pointer_type" | "generic_type" => {
            let inner = node.named_child(0)?;
            innermost_type_name(inner, source)
        }
        _ => None,
    }
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
    if !root && matches!(node.kind(), "function_declaration" | "method_declaration") {
        return;
    }
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && let Some(callee) = callee_name(function, source)
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

fn callee_name(function: Node<'_>, source: &[u8]) -> Option<String> {
    match function.kind() {
        "identifier" => function.utf8_text(source).ok().map(str::to_owned),
        "selector_expression" => function
            .child_by_field_name("field")?
            .utf8_text(source)
            .ok()
            .map(str::to_owned),
        _ => None,
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
    if !root && matches!(node.kind(), "function_declaration" | "method_declaration") {
        return;
    }
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && callee_name(function, source).is_some_and(|callee| is_database_execution_call(&callee))
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

fn is_database_execution_call(callee: &str) -> bool {
    matches!(
        callee,
        "Exec"
            | "ExecContext"
            | "MustExec"
            | "Query"
            | "QueryContext"
            | "QueryRow"
            | "QueryRowContext"
            | "QueryRowxContext"
            | "Queryx"
            | "Get"
            | "GetContext"
            | "Select"
            | "SelectContext"
            | "NamedExec"
            | "NamedQuery"
    )
}

fn collect_sql_literals(node: Node<'_>, source: &[u8], accesses: &mut Vec<DatabaseAccess>) {
    if matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    ) && let Ok(literal) = node.utf8_text(source)
        && let Some(statement) = static_string_content(literal)
    {
        let statement_hash = hash_bytes(statement.as_bytes());
        for (kind, entity, columns) in sql_entities(&statement) {
            accesses.push(DatabaseAccess {
                entity,
                kind,
                range: source_range(node),
                statement_hash: statement_hash.clone(),
                columns,
            });
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_sql_literals(child, source, accesses);
    }
}

fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    node.utf8_text(source).ok().map(str::to_owned)
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

/// Derives a stable, directory-based package path. Go groups symbols by
/// directory rather than by file, so (unlike the per-file Python/Rust module
/// path) the file name itself is dropped; only the directory chain matters.
fn package_path(path: &str) -> String {
    let mut components = path.split('/').collect::<Vec<_>>();
    components.pop();
    if components.first() == Some(&"src") {
        components.remove(0);
    }
    if components.is_empty() {
        "main".to_owned()
    } else {
        components.join(".")
    }
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
    fn extracts_go_functions_methods_types_tests_and_calls() {
        let source = r"
package billing

type Subscription struct {
	ID string
}

type Canceller interface {
	Cancel(id string) error
}

func Preserve(id string) error {
	return nil
}

func (s *SubscriptionService) Cancel(id string) error {
	Preserve(id)
	s.audit(id)
	return nil
}

func (s *SubscriptionService) audit(id string) {}
";
        let analysis =
            GoAnalyzer::analyze_source("billing/subscription.go", source).expect("Go analysis");

        assert_eq!(analysis.language, "go");
        assert!(analysis.symbols.iter().any(|symbol| {
            symbol.canonical_path == "billing.Subscription" && symbol.kind == SymbolKind::Struct
        }));
        assert!(analysis.symbols.iter().any(|symbol| {
            symbol.canonical_path == "billing.Canceller" && symbol.kind == SymbolKind::Trait
        }));
        let cancel = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.canonical_path == "billing.SubscriptionService.Cancel")
            .expect("method");
        assert_eq!(cancel.kind, SymbolKind::Method);
        assert_eq!(
            cancel
                .calls
                .iter()
                .map(|call| call.callee.as_str())
                .collect::<Vec<_>>(),
            vec!["Preserve", "audit"]
        );
    }

    #[test]
    fn recognizes_standard_go_test_functions_only_in_test_files() {
        let source = r#"
package billing

import "testing"

func TestCancelKeepsAccess(t *testing.T) {
	Preserve("id")
}
"#;
        let analysis = GoAnalyzer::analyze_source("billing/subscription_test.go", source)
            .expect("Go analysis");
        let test_symbol = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "TestCancelKeepsAccess")
            .expect("test function");
        assert_eq!(test_symbol.kind, SymbolKind::Test);

        let non_test = GoAnalyzer::analyze_source("billing/subscription.go", source)
            .expect("Go analysis outside a _test.go file");
        assert_eq!(
            non_test
                .symbols
                .iter()
                .find(|symbol| symbol.name == "TestCancelKeepsAccess")
                .expect("still a function")
                .kind,
            SymbolKind::Function
        );
    }

    #[test]
    fn rejects_incomplete_go_instead_of_indexing_a_partial_tree() {
        let error = GoAnalyzer::analyze_source("main.go", "func broken(")
            .expect_err("invalid Go must fail");

        assert!(matches!(error, GoAnalysisError::Syntax(_)));
    }

    #[test]
    fn extracts_database_sql_static_accesses_from_interpreted_and_raw_strings() {
        let source = r#"
package billing

func Archive(db *sql.DB, id string) {
	ignored := "DELETE FROM should_not_be_a_fact"
	_ = ignored
	db.QueryRow("SELECT id FROM subscriptions JOIN accounts ON accounts.id = subscriptions.account_id", id)
	db.Exec(`INSERT INTO subscription_archive(id) VALUES (?)`, id)
}
"#;
        let analysis = GoAnalyzer::analyze_source("billing/storage.go", source).expect("Go SQL");
        let function = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Archive")
            .expect("archive function");
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

    #[test]
    fn derives_directory_based_package_paths() {
        assert_eq!(package_path("main.go"), "main");
        assert_eq!(package_path("billing/subscription.go"), "billing");
        assert_eq!(
            package_path("internal/billing/subscription.go"),
            "internal.billing"
        );
        assert_eq!(package_path("src/billing/subscription.go"), "billing");
    }

    #[test]
    fn const_and_var_blocks_produce_one_symbol_per_bound_name() {
        let source = r"
package billing

const (
	StatusActive   = 0
	StatusInactive = 1
)

var DefaultTimeout = 30
";
        let analysis = GoAnalyzer::analyze_source("billing/status.go", source).expect("Go consts");
        let names = analysis
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"StatusActive"));
        assert!(names.contains(&"StatusInactive"));
        assert!(names.contains(&"DefaultTimeout"));
        assert!(
            analysis
                .symbols
                .iter()
                .all(|symbol| symbol.kind == SymbolKind::Constant)
        );
    }
}
