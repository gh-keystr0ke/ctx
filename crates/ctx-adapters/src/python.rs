use std::{fs, path::PathBuf};

use ctx_app::ports::{LanguageAnalyzer, PortError};
use ctx_core::ir::{
    CallSite, DatabaseAccess, FileAnalysis, ForeignKeyRef, SchemaColumn, SchemaTableDefinition,
    SourceRange, SymbolDefinition, SymbolKind,
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
            analysis_version: "python-tree-sitter-v3".to_owned(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            symbols,
        })
    }
}

impl LanguageAnalyzer for PythonAnalyzer {
    fn analysis_version(&self, _relative_path: &str) -> Result<String, PortError> {
        Ok("python-tree-sitter-v3".to_owned())
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
    let schema_tables = if is_class {
        sqlalchemy_schema_table(body, source).into_iter().collect()
    } else {
        Vec::new()
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
        schema_tables,
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

/// Recognizes a `SQLAlchemy` declarative model directly in the class body:
/// `__tablename__ = "..."` (the standard declarative-mapping marker) plus
/// `name = Column(...)`/`name = mapped_column(...)` attribute assignments.
/// Only classes that assign a static string to `__tablename__` are treated
/// as models at all, so an unrelated class that happens to define an
/// attribute named `Column` is not misread as ORM schema. Declarative base
/// detection (resolving `class X(Base):`) is deliberately not attempted:
/// aliasing and inheritance make it unreliable without import resolution,
/// while `__tablename__` is unambiguous and framework-mandated.
fn sqlalchemy_schema_table(class_body: Node<'_>, source: &[u8]) -> Option<SchemaTableDefinition> {
    let mut entity = None;
    let mut columns = Vec::new();
    let mut range = None;
    let mut cursor = class_body.walk();
    for statement in class_body.named_children(&mut cursor) {
        let Some(assignment) = direct_assignment(statement) else {
            continue;
        };
        let Some(left_name) = assignment
            .child_by_field_name("left")
            .filter(|left| left.kind() == "identifier")
            .and_then(|left| left.utf8_text(source).ok())
        else {
            continue;
        };
        let Some(right) = assignment.child_by_field_name("right") else {
            continue;
        };
        if left_name == "__tablename__" {
            if right.kind() == "string"
                && let Ok(literal) = right.utf8_text(source)
                && let Some(table_name) = static_string_content(literal)
            {
                entity = Some(table_name);
                range = Some(source_range(statement));
            }
            continue;
        }
        if let Some(mut column) = sqlalchemy_column(assignment, right, source) {
            left_name.clone_into(&mut column.name);
            columns.push(column);
        }
    }
    let entity = entity?;
    Some(SchemaTableDefinition {
        entity,
        table_created: true,
        columns,
        range: range.unwrap_or_else(|| source_range(class_body)),
        ..SchemaTableDefinition::default()
    })
}

/// A class body statement is `expression_statement > assignment` for both
/// plain (`x = ...`) and annotated (`x: T = ...`) attribute assignments.
fn direct_assignment(statement: Node<'_>) -> Option<Node<'_>> {
    if statement.kind() != "expression_statement" {
        return None;
    }
    let assignment = statement.named_child(0)?;
    (assignment.kind() == "assignment").then_some(assignment)
}

/// Recognizes one `Column(...)`/`mapped_column(...)` attribute assignment.
/// The declared type comes from the constructor's first positional argument
/// (covers both classic `Column(String)`/`Column(String(50))` and
/// `SQLAlchemy` 2.0 `mapped_column(Numeric(10, 2))`); when there is no
/// positional type argument at all, falls back to the variable's type
/// annotation text, then to the literal string `"unknown"`. `nullable`,
/// `primary_key`, `unique`, `default`/`server_default`, and a `ForeignKey(...)`
/// argument (positional or nested in a keyword argument's value) are read
/// from the call's keyword and positional arguments; a `name` field is left
/// empty for the caller to fill in from the assignment's left-hand side.
fn sqlalchemy_column(assignment: Node<'_>, right: Node<'_>, source: &[u8]) -> Option<SchemaColumn> {
    if right.kind() != "call" {
        return None;
    }
    let function = right.child_by_field_name("function")?;
    let callee = function.utf8_text(source).ok().and_then(|text| {
        text.rsplit('.')
            .find(|part| !part.is_empty())
            .map(str::to_owned)
    })?;
    if !matches!(callee.as_str(), "Column" | "mapped_column") {
        return None;
    }
    let arguments = right.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let first_positional = arguments
        .named_children(&mut cursor)
        .find(|argument| argument.kind() != "keyword_argument");
    let data_type = first_positional
        .and_then(|argument| argument.utf8_text(source).ok())
        .map_or_else(
            || {
                assignment
                    .child_by_field_name("type")
                    .and_then(|annotation| annotation.utf8_text(source).ok())
                    .map_or_else(|| "unknown".to_owned(), str::to_owned)
            },
            str::to_owned,
        );

    let mut column = SchemaColumn {
        name: String::new(),
        data_type,
        ..SchemaColumn::default()
    };
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        if argument.kind() == "keyword_argument" {
            let (Some(name_node), Some(value)) = (
                argument.child_by_field_name("name"),
                argument.child_by_field_name("value"),
            ) else {
                continue;
            };
            match name_node.utf8_text(source).unwrap_or_default() {
                "nullable" => column.nullable = python_bool_literal(value, source),
                "primary_key" => {
                    column.primary_key = python_bool_literal(value, source).unwrap_or(false);
                }
                "unique" => column.unique = python_bool_literal(value, source).unwrap_or(false),
                "default" | "server_default" => {
                    column.default = value.utf8_text(source).ok().map(str::to_owned);
                }
                _ => {}
            }
            if let Some(foreign_key) = foreign_key_from_call(value, source) {
                column.foreign_key = Some(foreign_key);
            }
        } else if let Some(foreign_key) = foreign_key_from_call(argument, source) {
            column.foreign_key = Some(foreign_key);
        }
    }
    Some(column)
}

fn python_bool_literal(value: Node<'_>, source: &[u8]) -> Option<bool> {
    match value.utf8_text(source).ok()? {
        "True" => Some(true),
        "False" => Some(false),
        _ => None,
    }
}

/// Recognizes a `ForeignKey("table.column")` call and extracts its target.
/// Only a single static string argument in the standard `table.column` form
/// is recognized; anything else (a dynamic expression, a bare table name
/// with no column) yields `None` instead of a guess.
fn foreign_key_from_call(node: Node<'_>, source: &[u8]) -> Option<ForeignKeyRef> {
    if node.kind() != "call" {
        return None;
    }
    let function = node.child_by_field_name("function")?;
    let callee = function.utf8_text(source).ok()?;
    if callee.rsplit('.').next().unwrap_or(callee) != "ForeignKey" {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let first = arguments
        .named_children(&mut cursor)
        .find(|argument| argument.kind() != "keyword_argument")?;
    if first.kind() != "string" {
        return None;
    }
    let literal = first.utf8_text(source).ok()?;
    let target = static_string_content(literal)?;
    let (table, column) = target.rsplit_once('.')?;
    Some(ForeignKeyRef {
        table: table.to_owned(),
        column: Some(column.to_owned()),
    })
}

fn collect_sql_literals(node: Node<'_>, source: &[u8], accesses: &mut Vec<DatabaseAccess>) {
    if node.kind() == "string"
        && let Ok(literal) = node.utf8_text(source)
        && let Some(statement) = static_string_content(literal)
    {
        let statement_hash = blake3::hash(statement.as_bytes()).to_hex().to_string();
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

    #[test]
    fn extracts_sqlalchemy_declarative_model_columns() {
        let source = r#"
class Subscription(Base):
    __tablename__ = "subscriptions"

    id = Column(String, primary_key=True)
    status = Column(String(50), nullable=False)
    amount: Mapped[int] = mapped_column(Numeric(10, 2))
    note = mapped_column(Text)
"#;
        let analysis =
            PythonAnalyzer::analyze_source("models.py", source).expect("SQLAlchemy model");
        let class_symbol = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Subscription")
            .expect("model class");
        assert_eq!(class_symbol.schema_tables.len(), 1);
        let table = &class_symbol.schema_tables[0];
        assert_eq!(table.entity, "subscriptions");
        assert_eq!(
            table
                .columns
                .iter()
                .map(|column| (column.name.as_str(), column.data_type.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("id", "String"),
                ("status", "String(50)"),
                ("amount", "Numeric(10, 2)"),
                ("note", "Text"),
            ]
        );
    }

    #[test]
    fn extracts_sqlalchemy_column_constraints() {
        let source = r#"
class Subscription(Base):
    __tablename__ = "subscriptions"

    id = Column(String, primary_key=True)
    account_id = Column(String, ForeignKey("accounts.id"), nullable=False)
    status = Column(String(50), nullable=False, default="active")
    email = Column(String(255), unique=True)
"#;
        let analysis =
            PythonAnalyzer::analyze_source("models.py", source).expect("SQLAlchemy model");
        let table = &analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Subscription")
            .expect("model class")
            .schema_tables[0];

        let id = table.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(id.primary_key);

        let account_id = table
            .columns
            .iter()
            .find(|c| c.name == "account_id")
            .unwrap();
        assert_eq!(account_id.nullable, Some(false));
        assert_eq!(
            account_id.foreign_key,
            Some(ForeignKeyRef {
                table: "accounts".to_owned(),
                column: Some("id".to_owned()),
            })
        );

        let status = table.columns.iter().find(|c| c.name == "status").unwrap();
        assert_eq!(status.nullable, Some(false));
        assert_eq!(status.default.as_deref(), Some("\"active\""));

        let email = table.columns.iter().find(|c| c.name == "email").unwrap();
        assert!(email.unique);
    }

    #[test]
    fn ignores_classes_without_a_static_tablename() {
        let source = r"
class Column:
    pass

class PlainDataclass:
    id = Column()
    Column = 5
";
        let analysis =
            PythonAnalyzer::analyze_source("plain.py", source).expect("non-model classes");
        assert!(
            analysis
                .symbols
                .iter()
                .all(|symbol| symbol.schema_tables.is_empty())
        );
    }

    #[test]
    fn ignores_a_dynamic_tablename() {
        let source = r"
class Subscription(Base):
    __tablename__ = table_name_from(config)

    id = Column(String)
";
        let analysis =
            PythonAnalyzer::analyze_source("dynamic.py", source).expect("dynamic tablename");
        assert!(
            analysis
                .symbols
                .iter()
                .all(|symbol| symbol.schema_tables.is_empty())
        );
    }
}
