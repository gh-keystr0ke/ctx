use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use ctx_app::ports::{LanguageAnalyzer, PortError};
use ctx_core::ir::{
    ApiEndpoint, ApiParam, CallSite, DatabaseAccess, ExternalCall, FileAnalysis, ForeignKeyRef,
    HttpMethod, ParamSource, SchemaColumn, SchemaTableDefinition, SourceRange, SymbolDefinition,
    SymbolKind,
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
        let routing = collect_python_bindings(tree.root_node(), source.as_bytes());
        let mut symbols = Vec::new();
        collect_definitions(
            tree.root_node(),
            source.as_bytes(),
            &module,
            None,
            &routing,
            &mut symbols,
        );
        symbols.sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
        Ok(FileAnalysis {
            path: relative_path.to_owned(),
            language: "python".to_owned(),
            analysis_version: "python-tree-sitter-v4".to_owned(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            symbols,
        })
    }
}

impl LanguageAnalyzer for PythonAnalyzer {
    fn analysis_version(&self, _relative_path: &str) -> Result<String, PortError> {
        Ok("python-tree-sitter-v4".to_owned())
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

#[derive(Default)]
struct PythonBindings {
    router_prefixes: BTreeMap<String, String>,
    httpx_clients: BTreeSet<String>,
}

fn collect_python_bindings(root: Node<'_>, source: &[u8]) -> PythonBindings {
    let mut bindings = PythonBindings::default();
    visit_python_bindings(root, source, &mut bindings);
    bindings
}

fn visit_python_bindings(node: Node<'_>, source: &[u8], bindings: &mut PythonBindings) {
    if node.kind() == "assignment"
        && let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        )
        && left.kind() == "identifier"
        && right.kind() == "call"
        && let (Ok(name), Some(function)) = (
            left.utf8_text(source),
            right.child_by_field_name("function"),
        )
        && let Ok(callee) = function.utf8_text(source)
    {
        let short = callee.rsplit('.').next().unwrap_or(callee);
        if short == "APIRouter"
            && let Some(arguments) = right.child_by_field_name("arguments")
            && let Some(prefix) = keyword_argument(arguments, "prefix", source)
            && prefix.kind() == "string"
            && let Ok(literal) = prefix.utf8_text(source)
            && let Some(prefix) = static_string_content(literal)
        {
            bindings.router_prefixes.insert(name.to_owned(), prefix);
        }
        if matches!(callee, "httpx.Client" | "httpx.AsyncClient") {
            bindings.httpx_clients.insert(name.to_owned());
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_python_bindings(child, source, bindings);
    }
}

fn collect_api_endpoints(
    decorated: Node<'_>,
    function: Node<'_>,
    source: &[u8],
    bindings: &PythonBindings,
) -> Vec<ApiEndpoint> {
    let mut endpoints = Vec::new();
    let mut cursor = decorated.walk();
    for decorator in decorated
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
    {
        let Some(call) = first_descendant_of_kind(decorator, "call") else {
            continue;
        };
        let Some(callee) = call
            .child_by_field_name("function")
            .and_then(|node| node.utf8_text(source).ok())
        else {
            continue;
        };
        let Some((owner, operation)) = callee.rsplit_once('.') else {
            continue;
        };
        let Some(arguments) = call.child_by_field_name("arguments") else {
            continue;
        };
        let Some(path_node) = first_positional_argument(arguments) else {
            continue;
        };
        if path_node.kind() != "string" {
            continue;
        }
        let Some(path) = path_node
            .utf8_text(source)
            .ok()
            .and_then(static_string_content)
        else {
            continue;
        };
        let (methods, framework, prefix) = if operation == "route" {
            (flask_methods(arguments, source), "flask", String::new())
        } else if let Some(method) = http_method(operation) {
            (
                vec![method],
                "fastapi",
                bindings
                    .router_prefixes
                    .get(owner)
                    .cloned()
                    .unwrap_or_default(),
            )
        } else {
            continue;
        };
        let path = join_route_paths(&prefix, &path);
        let params = api_parameters(function, &path, source);
        let return_type = function
            .child_by_field_name("return_type")
            .and_then(|node| node.utf8_text(source).ok())
            .map(str::to_owned);
        endpoints.extend(methods.into_iter().map(|method| ApiEndpoint {
            path: path.clone(),
            method,
            params: params.clone(),
            return_type: return_type.clone(),
            framework: framework.to_owned(),
            range: source_range(decorator),
        }));
    }
    endpoints.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.method.cmp(&right.method))
    });
    endpoints.dedup_by(|left, right| left.path == right.path && left.method == right.method);
    endpoints
}

fn first_descendant_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = first_descendant_of_kind(child, kind) {
            return Some(found);
        }
    }
    None
}

fn first_positional_argument(arguments: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .find(|argument| argument.kind() != "keyword_argument")
}

fn keyword_argument<'a>(arguments: Node<'a>, name: &str, source: &[u8]) -> Option<Node<'a>> {
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter(|argument| argument.kind() == "keyword_argument")
        .find_map(|argument| {
            let argument_name = argument
                .child_by_field_name("name")?
                .utf8_text(source)
                .ok()?;
            (argument_name == name)
                .then(|| argument.child_by_field_name("value"))
                .flatten()
        })
}

fn http_method(value: &str) -> Option<HttpMethod> {
    match value.to_ascii_lowercase().as_str() {
        "get" => Some(HttpMethod::Get),
        "post" => Some(HttpMethod::Post),
        "put" => Some(HttpMethod::Put),
        "delete" => Some(HttpMethod::Delete),
        "patch" => Some(HttpMethod::Patch),
        _ => None,
    }
}

fn flask_methods(arguments: Node<'_>, source: &[u8]) -> Vec<HttpMethod> {
    let Some(methods) = keyword_argument(arguments, "methods", source) else {
        return vec![HttpMethod::Get];
    };
    let mut cursor = methods.walk();
    let mut result = methods
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "string")
        .filter_map(|node| node.utf8_text(source).ok())
        .filter_map(static_string_content)
        .filter_map(|method| http_method(&method))
        .collect::<Vec<_>>();
    result.sort_unstable();
    result.dedup();
    result
}

fn join_route_paths(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    match (prefix.is_empty(), path.is_empty()) {
        (true, true) => "/".to_owned(),
        (true, false) => format!("/{path}"),
        (false, true) => format!("{prefix}/"),
        (false, false) => format!("{prefix}/{path}"),
    }
}

fn api_parameters(function: Node<'_>, path: &str, source: &[u8]) -> Vec<ApiParam> {
    let Some(parameters) = function.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut cursor = parameters.walk();
    parameters
        .named_children(&mut cursor)
        .filter_map(|parameter| api_parameter(parameter, path, source))
        .collect()
}

fn api_parameter(parameter: Node<'_>, path: &str, source: &[u8]) -> Option<ApiParam> {
    let text = parameter.utf8_text(source).ok()?;
    let name_node = parameter
        .child_by_field_name("name")
        .or_else(|| first_descendant_of_kind(parameter, "identifier"))?;
    let name = name_node.utf8_text(source).ok()?.to_owned();
    if matches!(name.as_str(), "self" | "cls") {
        return None;
    }
    let type_hint = parameter
        .child_by_field_name("type")
        .or_else(|| first_descendant_field(parameter, "type"))
        .and_then(|node| node.utf8_text(source).ok())
        .map(str::to_owned);
    if type_hint
        .as_deref()
        .is_some_and(|hint| hint.rsplit('.').next() == Some("Request"))
        || text.contains("Depends(")
    {
        return None;
    }
    let has_default = matches!(
        parameter.kind(),
        "default_parameter" | "typed_default_parameter"
    ) || parameter.child_by_field_name("value").is_some();
    let optional_type = type_hint
        .as_deref()
        .is_some_and(|hint| hint.contains("Optional") || hint.contains("None"));
    let source_kind = if path_has_parameter(path, &name) {
        ParamSource::Path
    } else if type_hint.as_deref().is_some_and(is_body_type_hint) && !has_default {
        ParamSource::Body
    } else {
        ParamSource::Query
    };
    Some(ApiParam {
        name,
        type_hint,
        source: source_kind,
        required: !has_default && !optional_type,
    })
}

fn first_descendant_field<'a>(node: Node<'a>, field: &str) -> Option<Node<'a>> {
    if let Some(found) = node.child_by_field_name(field) {
        return Some(found);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = first_descendant_field(child, field) {
            return Some(found);
        }
    }
    None
}

fn path_has_parameter(path: &str, name: &str) -> bool {
    path.contains(&format!("{{{name}}}"))
        || path.contains(&format!("<{name}>"))
        || path.contains(&format!(":{name}>"))
}

fn is_body_type_hint(hint: &str) -> bool {
    let base = hint
        .trim()
        .trim_start_matches("Optional[")
        .trim_end_matches(']');
    !matches!(
        base,
        "str" | "int" | "float" | "bool" | "bytes" | "date" | "datetime" | "UUID"
    )
}

fn collect_external_calls(
    node: Node<'_>,
    source: &[u8],
    bindings: &PythonBindings,
) -> Vec<ExternalCall> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    visit_external_calls(&mut cursor, source, bindings, &mut calls, true);
    calls.sort_by(|left, right| {
        left.method
            .cmp(&right.method)
            .then_with(|| left.url.cmp(&right.url))
            .then_with(|| left.range.start_byte.cmp(&right.range.start_byte))
    });
    calls.dedup_by(|left, right| left.method == right.method && left.url == right.url);
    calls
}

fn visit_external_calls(
    cursor: &mut TreeCursor<'_>,
    source: &[u8],
    bindings: &PythonBindings,
    calls: &mut Vec<ExternalCall>,
    root: bool,
) {
    let node = cursor.node();
    if !root && matches!(node.kind(), "function_definition" | "class_definition") {
        return;
    }
    if node.kind() == "call"
        && let Some(function) = node.child_by_field_name("function")
        && let Ok(callee) = function.utf8_text(source)
        && let Some((owner, operation)) = callee.rsplit_once('.')
        && let Some(method) = http_method(operation)
        && is_http_client(owner, bindings)
        && let Some(arguments) = node.child_by_field_name("arguments")
        && let Some(url_node) = first_positional_argument(arguments)
        && let Some(url) = static_url_template(url_node, source)
    {
        calls.push(ExternalCall {
            method,
            url,
            range: source_range(node),
        });
    }
    if cursor.goto_first_child() {
        loop {
            visit_external_calls(cursor, source, bindings, calls, false);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn is_http_client(owner: &str, bindings: &PythonBindings) -> bool {
    owner == "requests"
        || owner == "httpx"
        || owner == "httpx.Client()"
        || owner == "httpx.AsyncClient()"
        || bindings.httpx_clients.contains(owner)
}

fn static_url_template(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    if node.kind() == "string" {
        if let Some(literal) = static_string_content(text) {
            return is_absolute_http_url(&literal).then_some(literal);
        }
        let inner = python_string_inner(text)?;
        return normalize_dynamic_url(inner);
    }
    if node.kind() == "call" && text.contains(".format(") {
        let literal = text.split_once(".format(")?.0;
        let inner = python_string_inner(literal)?;
        return normalize_dynamic_url(inner);
    }
    None
}

fn python_string_inner(literal: &str) -> Option<&str> {
    let quote_index = literal.find(['\'', '"'])?;
    let quote = literal.as_bytes()[quote_index] as char;
    let triple = literal[quote_index..].starts_with(&quote.to_string().repeat(3));
    let opening = if triple { 3 } else { 1 };
    let closing = quote.to_string().repeat(opening);
    let start = quote_index + opening;
    let end = literal.strip_suffix(&closing)?.len();
    (end >= start).then(|| &literal[start..end])
}

fn normalize_dynamic_url(value: &str) -> Option<String> {
    if !is_absolute_http_url(value) {
        return None;
    }
    let scheme_end = value.find("://")? + 3;
    let authority_end = value[scheme_end..].find('/').map(|i| scheme_end + i)?;
    let mut normalized = String::new();
    let mut remaining = value;
    let mut offset = 0;
    let mut replaced = false;
    while let Some(open) = remaining.find('{') {
        let absolute_open = offset + open;
        let close = remaining[open + 1..].find('}')? + open + 1;
        let field = &remaining[open + 1..close];
        let next = remaining.as_bytes().get(close + 1).copied();
        let previous = remaining.as_bytes().get(open.wrapping_sub(1)).copied();
        if absolute_open <= authority_end
            || previous != Some(b'/')
            || !matches!(next, None | Some(b'/' | b'?' | b'#'))
            || field.contains(['!', ':'])
        {
            return None;
        }
        normalized.push_str(&remaining[..open]);
        normalized.push_str("{param}");
        let consumed = close + 1;
        offset += consumed;
        remaining = &remaining[consumed..];
        replaced = true;
    }
    normalized.push_str(remaining);
    replaced.then_some(normalized)
}

fn is_absolute_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn collect_definitions(
    node: Node<'_>,
    source: &[u8],
    module: &str,
    parent: Option<&str>,
    routing: &PythonBindings,
    symbols: &mut Vec<SymbolDefinition>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let definition = unwrap_decorated(child);
        if matches!(
            definition.kind(),
            "function_definition" | "class_definition"
        ) {
            let decorated = (child.kind() == "decorated_definition").then_some(child);
            if let Some(symbol) =
                parse_definition(definition, decorated, source, module, parent, routing)
            {
                let canonical = symbol.canonical_path.clone();
                symbols.push(symbol);
                if let Some(body) = definition.child_by_field_name("body") {
                    collect_definitions(body, source, module, Some(&canonical), routing, symbols);
                }
            }
        } else {
            collect_definitions(child, source, module, parent, routing, symbols);
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
    decorated: Option<Node<'_>>,
    source: &[u8],
    module: &str,
    parent: Option<&str>,
    routing: &PythonBindings,
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
    let api_endpoints = if is_class {
        Vec::new()
    } else {
        decorated.map_or_else(Vec::new, |decorated| {
            collect_api_endpoints(decorated, node, source, routing)
        })
    };
    let external_calls = if is_class {
        Vec::new()
    } else {
        collect_external_calls(body, source, routing)
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
        api_endpoints,
        external_calls,
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
    fn extracts_fastapi_router_and_flask_endpoint_contracts_without_guessing_dynamic_paths() {
        let source = r#"
router = APIRouter(prefix="/v1")

@app.get("/subscriptions/{subscription_id}")
def get_subscription(
    subscription_id: str,
    request: Request,
    database = Depends(get_database),
    expand: bool = False,
) -> Subscription:
    return load(subscription_id)

@router.post("/subscriptions")
def create_subscription(payload: CreateSubscription) -> Subscription:
    return create(payload)

@flask_app.route("/jobs/<int:job_id>", methods=["GET", "DELETE"])
def job(job_id: int):
    return find(job_id)

@app.get(prefix + "/dynamic")
def dynamic():
    return None
"#;
        let analysis = PythonAnalyzer::analyze_source("src/api.py", source).expect("Python API");
        let get = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "get_subscription")
            .expect("FastAPI handler");
        assert_eq!(get.api_endpoints.len(), 1);
        let endpoint = &get.api_endpoints[0];
        assert_eq!(
            (endpoint.method, endpoint.path.as_str()),
            (HttpMethod::Get, "/subscriptions/{subscription_id}")
        );
        assert_eq!(endpoint.return_type.as_deref(), Some("Subscription"));
        assert_eq!(
            endpoint
                .params
                .iter()
                .map(|param| (param.name.as_str(), param.source, param.required))
                .collect::<Vec<_>>(),
            vec![
                ("subscription_id", ParamSource::Path, true),
                ("expand", ParamSource::Query, false),
            ]
        );

        let create = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "create_subscription")
            .expect("router handler");
        assert_eq!(create.api_endpoints[0].path, "/v1/subscriptions");
        assert_eq!(create.api_endpoints[0].params[0].source, ParamSource::Body);

        let job = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "job")
            .expect("Flask handler");
        assert_eq!(
            job.api_endpoints
                .iter()
                .map(|endpoint| endpoint.method)
                .collect::<Vec<_>>(),
            vec![HttpMethod::Get, HttpMethod::Delete]
        );
        assert!(
            analysis
                .symbols
                .iter()
                .find(|symbol| symbol.name == "dynamic")
                .expect("dynamic handler symbol")
                .api_endpoints
                .is_empty()
        );
    }

    #[test]
    fn extracts_static_requests_and_httpx_calls_and_normalizes_dynamic_path_segments() {
        let source = r#"
client = httpx.Client()

def notify(subscription_id, dynamic_url):
    requests.get("https://billing.internal/health")
    httpx.post(f"https://billing.internal/subscriptions/{subscription_id}")
    client.patch("https://billing.internal/subscriptions/{}".format(subscription_id))
    requests.delete(dynamic_url)
"#;
        let analysis =
            PythonAnalyzer::analyze_source("src/caller.py", source).expect("HTTP callers");
        let notify = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "notify")
            .expect("caller");
        assert_eq!(
            notify
                .external_calls
                .iter()
                .map(|call| (call.method, call.url.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (HttpMethod::Get, "https://billing.internal/health"),
                (
                    HttpMethod::Post,
                    "https://billing.internal/subscriptions/{param}"
                ),
                (
                    HttpMethod::Patch,
                    "https://billing.internal/subscriptions/{param}"
                ),
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
