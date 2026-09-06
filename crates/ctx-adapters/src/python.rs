use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use ctx_app::ports::{LanguageAnalyzer, PortError, PythonTypeCandidateExtractor};
use ctx_core::ir::{
    ApiEndpoint, ApiParam, CallSite, DatabaseAccess, DatabaseAccessKind, ExternalCall,
    FileAnalysis, ForeignKeyRef, HttpMethod, OrmModelAccess, ParamSource, SchemaColumn,
    SchemaTableDefinition, SourceRange, SymbolDefinition, SymbolKind,
};
use ctx_core::type_inference::{TypePosition, TypeProbe, TypeWriteCandidate, TypeWriteForm};
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
        let bindings = collect_python_bindings(tree.root_node(), source.as_bytes(), &module);
        let mut symbols = Vec::new();
        collect_definitions(
            tree.root_node(),
            source.as_bytes(),
            &module,
            None,
            &bindings,
            &mut symbols,
        );
        symbols.sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
        Ok(FileAnalysis {
            path: relative_path.to_owned(),
            language: "python".to_owned(),
            analysis_version: "python-tree-sitter-v8".to_owned(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            symbols,
        })
    }

    /// Extracts permissive write-site candidates for a separate type-backed
    /// enrichment pass. Candidate extraction alone never implies ORM or
    /// database semantics.
    ///
    /// # Errors
    ///
    /// Returns [`PythonAnalysisError`] when Tree-sitter cannot be configured or
    /// does not produce a syntax tree.
    pub fn type_write_candidates(
        relative_path: &str,
        source: &str,
    ) -> Result<Vec<TypeWriteCandidate>, PythonAnalysisError> {
        let mut parser = Parser::new();
        let language = tree_sitter_python::LANGUAGE.into();
        parser
            .set_language(&language)
            .map_err(|_| PythonAnalysisError::Language)?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| PythonAnalysisError::Parse(relative_path.to_owned()))?;
        let mut candidates = Vec::new();
        visit_type_write_candidates(tree.root_node(), relative_path, source, &mut candidates);
        candidates.sort_by(|left, right| {
            left.write_range
                .start_byte
                .cmp(&right.write_range.start_byte)
                .then_with(|| {
                    left.probe
                        .range
                        .start_byte
                        .cmp(&right.probe.range.start_byte)
                })
                .then_with(|| left.form.cmp(&right.form))
        });
        Ok(candidates)
    }
}

fn visit_type_write_candidates(
    node: Node<'_>,
    relative_path: &str,
    source: &str,
    candidates: &mut Vec<TypeWriteCandidate>,
) {
    if matches!(node.kind(), "assignment" | "augmented_assignment") {
        if let Some(candidate) = attr_assignment_candidate(node, relative_path, source) {
            candidates.push(candidate);
        }
    } else if node.kind() == "call" {
        collect_unit_of_work_candidates(node, relative_path, source, candidates);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_type_write_candidates(child, relative_path, source, candidates);
    }
}

fn attr_assignment_candidate(
    assignment: Node<'_>,
    relative_path: &str,
    source: &str,
) -> Option<TypeWriteCandidate> {
    let left = assignment.child_by_field_name("left")?;
    if left.kind() != "attribute" {
        return None;
    }
    let object = left.child_by_field_name("object")?;
    let column = left
        .child_by_field_name("attribute")?
        .utf8_text(source.as_bytes())
        .ok()?
        .to_owned();
    Some(type_write_candidate(
        relative_path,
        TypeWriteForm::AttrAssign,
        object,
        None,
        Some(column),
        assignment,
        source,
    ))
}

fn collect_unit_of_work_candidates(
    call: Node<'_>,
    relative_path: &str,
    source: &str,
    candidates: &mut Vec<TypeWriteCandidate>,
) {
    let Some(function) = call.child_by_field_name("function") else {
        return;
    };
    if function.kind() != "attribute" {
        return;
    }
    let Some(operation) = function
        .child_by_field_name("attribute")
        .and_then(|attribute| attribute.utf8_text(source.as_bytes()).ok())
    else {
        return;
    };
    let form = match operation {
        "add" => TypeWriteForm::Add,
        "add_all" => TypeWriteForm::AddAll,
        "merge" => TypeWriteForm::Merge,
        "delete" => TypeWriteForm::Delete,
        _ => return,
    };
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return;
    };
    let Some(argument) = first_positional_argument(arguments) else {
        return;
    };
    if form == TypeWriteForm::AddAll {
        if argument.kind() != "list" {
            return;
        }
        let mut cursor = argument.walk();
        for element in argument
            .named_children(&mut cursor)
            .filter(|element| element.kind() != "list_splat")
        {
            candidates.push(type_write_candidate(
                relative_path,
                form,
                element,
                Some(function),
                None,
                call,
                source,
            ));
        }
    } else {
        candidates.push(type_write_candidate(
            relative_path,
            form,
            argument,
            Some(function),
            None,
            call,
            source,
        ));
    }
}

fn type_write_candidate(
    relative_path: &str,
    form: TypeWriteForm,
    probe: Node<'_>,
    method_probe: Option<Node<'_>>,
    column: Option<String>,
    write: Node<'_>,
    source: &str,
) -> TypeWriteCandidate {
    TypeWriteCandidate {
        file_path: relative_path.to_owned(),
        form,
        probe: type_probe(probe, source),
        method_probe: method_probe.map(|probe| type_probe(probe, source)),
        column,
        write_range: source_range(write),
        statement_hash: blake3::hash(&source.as_bytes()[write.byte_range()])
            .to_hex()
            .to_string(),
    }
}

fn type_probe(node: Node<'_>, source: &str) -> TypeProbe {
    TypeProbe {
        expression: source[node.byte_range()].to_owned(),
        range: source_range(node),
        start: type_position(source, node.start_byte(), node.start_position().row),
        end: type_position(source, node.end_byte(), node.end_position().row),
    }
}

fn type_position(source: &str, byte: usize, row: usize) -> TypePosition {
    let line_start = source[..byte].rfind('\n').map_or(0, |offset| offset + 1);
    TypePosition {
        line: row,
        character: source[line_start..byte].encode_utf16().count(),
    }
}

impl LanguageAnalyzer for PythonAnalyzer {
    fn analysis_version(&self, _relative_path: &str) -> Result<String, PortError> {
        Ok("python-tree-sitter-v8".to_owned())
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

impl PythonTypeCandidateExtractor for PythonAnalyzer {
    fn candidates(&self, relative_path: &str) -> Result<Vec<TypeWriteCandidate>, PortError> {
        let path = self.root.join(relative_path);
        let source = fs::read_to_string(&path).map_err(|source| {
            PortError::new(
                PythonAnalysisError::Read {
                    path: path.display().to_string(),
                    source,
                }
                .to_string(),
            )
        })?;
        Self::type_write_candidates(relative_path, &source)
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SqlAlchemyVerb {
    Select,
    Insert,
    Update,
    Delete,
}

impl SqlAlchemyVerb {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "select" => Some(Self::Select),
            "insert" => Some(Self::Insert),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    fn access_kind(self) -> DatabaseAccessKind {
        match self {
            Self::Select => DatabaseAccessKind::Read,
            Self::Insert | Self::Update | Self::Delete => DatabaseAccessKind::Write,
        }
    }
}

#[derive(Default)]
struct PythonBindings {
    router_prefixes: BTreeMap<String, String>,
    /// Variables bound to an `httpx`/`aiohttp` client whose HTTP verb is the
    /// method name (`client.post(url)`), by either plain assignment or a
    /// `with <constructor>() as name:` context manager.
    http_client_bindings: BTreeSet<String>,
    /// Variables bound to a `urllib3`/`http.client` connection whose HTTP
    /// verb is a string literal passed to `.request(verb, url)`, tracked
    /// the same two ways as `http_client_bindings`.
    request_style_bindings: BTreeSet<String>,
    sqlalchemy_expr_names: BTreeMap<String, SqlAlchemyVerb>,
    sqlalchemy_module_aliases: BTreeSet<String>,
    imported_symbols: BTreeMap<String, String>,
    same_file_classes: BTreeMap<String, String>,
}

fn collect_python_bindings(root: Node<'_>, source: &[u8], module: &str) -> PythonBindings {
    let mut bindings = PythonBindings::default();
    visit_python_bindings(root, source, &mut bindings);
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        let definition = unwrap_decorated(child);
        if definition.kind() != "class_definition" {
            continue;
        }
        let Some(name) = definition
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(source).ok())
        else {
            continue;
        };
        bindings
            .same_file_classes
            .insert(name.to_owned(), format!("{module}.{name}"));
    }
    bindings
}

fn visit_python_bindings(node: Node<'_>, source: &[u8], bindings: &mut PythonBindings) {
    match node.kind() {
        "import_from_statement" => collect_from_imports(node, source, bindings),
        "import_statement" => collect_module_imports(node, source, bindings),
        _ => {}
    }
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
        register_http_binding(bindings, name, callee);
    }
    if node.kind() == "as_pattern"
        && let Some(alias) = node.child_by_field_name("alias")
        && let Some(name_node) = first_descendant_of_kind(alias, "identifier")
        && let Ok(name) = name_node.utf8_text(source)
        && let Some(call) = {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|child| child.kind() == "call")
        }
        && let Some(function) = call.child_by_field_name("function")
        && let Ok(callee) = function.utf8_text(source)
    {
        register_http_binding(bindings, name, callee);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_python_bindings(child, source, bindings);
    }
}

fn collect_from_imports(node: Node<'_>, source: &[u8], bindings: &mut PythonBindings) {
    let Some(module_node) = node.child_by_field_name("module_name") else {
        return;
    };
    if module_node.kind() != "dotted_name" {
        return;
    }
    let Ok(module) = module_node.utf8_text(source) else {
        return;
    };
    let mut cursor = node.walk();
    for imported in node.children_by_field_name("name", &mut cursor) {
        let Some((name, local_name)) = import_name_and_binding(imported, source, true) else {
            continue;
        };
        bindings
            .imported_symbols
            .insert(local_name.clone(), format!("{module}.{name}"));
        if matches!(module, "sqlalchemy" | "sqlalchemy.sql")
            && let Some(verb) = SqlAlchemyVerb::from_name(&name)
        {
            bindings.sqlalchemy_expr_names.insert(local_name, verb);
        }
    }
}

fn collect_module_imports(node: Node<'_>, source: &[u8], bindings: &mut PythonBindings) {
    let mut cursor = node.walk();
    for imported in node.children_by_field_name("name", &mut cursor) {
        let Some((name, local_name)) = import_name_and_binding(imported, source, false) else {
            continue;
        };
        if matches!(name.as_str(), "sqlalchemy" | "sqlalchemy.sql") {
            bindings.sqlalchemy_module_aliases.insert(local_name);
        }
    }
}

fn import_name_and_binding(
    imported: Node<'_>,
    source: &[u8],
    from_import: bool,
) -> Option<(String, String)> {
    let (name_node, alias) = if imported.kind() == "aliased_import" {
        (
            imported.child_by_field_name("name")?,
            imported
                .child_by_field_name("alias")?
                .utf8_text(source)
                .ok()
                .map(str::to_owned),
        )
    } else {
        (imported, None)
    };
    let name = name_node.utf8_text(source).ok()?.to_owned();
    let local_name = alias.unwrap_or_else(|| {
        if from_import {
            name.rsplit('.').next().unwrap_or(&name).to_owned()
        } else {
            name.clone()
        }
    });
    Some((name, local_name))
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
            openapi: None,
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
    positional_argument(arguments, 0)
}

fn positional_argument(arguments: Node<'_>, index: usize) -> Option<Node<'_>> {
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter(|argument| argument.kind() != "keyword_argument")
        .nth(index)
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
            .then_with(|| left.host_expr.cmp(&right.host_expr))
            .then_with(|| left.range.start_byte.cmp(&right.range.start_byte))
    });
    calls.dedup_by(|left, right| {
        left.method == right.method && left.url == right.url && left.host_expr == right.host_expr
    });
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
        && let Some((url, host_expr)) = resolve_call_url(url_node, source)
    {
        calls.push(ExternalCall {
            method,
            url,
            range: source_range(node),
            host_expr,
            request_fields: request_body_fields(arguments, source),
            response_fields: response_fields(node, source),
        });
    }
    if node.kind() == "call"
        && let Some(function) = node.child_by_field_name("function")
        && let Ok(callee) = function.utf8_text(source)
        && let Some((owner, "request")) = callee.rsplit_once('.')
        && bindings.request_style_bindings.contains(owner)
        && let Some(arguments) = node.child_by_field_name("arguments")
        && let Some(method_node) = first_positional_argument(arguments)
        && method_node.kind() == "string"
        && let Ok(method_text) = method_node.utf8_text(source)
        && let Some(method_literal) = static_string_content(method_text)
        && let Some(method) = http_method(&method_literal)
        && let Some(url_node) = positional_argument(arguments, 1)
        && let Some((url, host_expr)) = resolve_call_url(url_node, source)
    {
        calls.push(ExternalCall {
            method,
            url,
            range: source_range(node),
            host_expr,
            request_fields: Vec::new(),
            response_fields: Vec::new(),
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

/// Records `name` as a client-like binding when `callee` is a recognized
/// HTTP-client constructor, whether reached by plain assignment
/// (`name = httpx.Client()`) or a `with ... as name:` context manager.
fn register_http_binding(bindings: &mut PythonBindings, name: &str, callee: &str) {
    if matches!(
        callee,
        "httpx.Client" | "httpx.AsyncClient" | "aiohttp.ClientSession"
    ) {
        bindings.http_client_bindings.insert(name.to_owned());
    }
    if matches!(
        callee,
        "urllib3.PoolManager" | "http.client.HTTPSConnection" | "http.client.HTTPConnection"
    ) {
        bindings.request_style_bindings.insert(name.to_owned());
    }
}

fn is_http_client(owner: &str, bindings: &PythonBindings) -> bool {
    owner == "requests"
        || owner == "httpx"
        || owner == "httpx.Client()"
        || owner == "httpx.AsyncClient()"
        || owner == "aiohttp.ClientSession()"
        || bindings.http_client_bindings.contains(owner)
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

/// Resolves a call's URL argument, additionally trying an opaque-authority
/// shape (`f"{self._host}/v1/charges"`, typical of a config/environment-
/// injected client host) when [`static_url_template`] finds no fully
/// literal-or-known-authority URL. Returns the URL (a literal path when the
/// authority is opaque) and, when the authority was opaque, the raw source
/// text of the expression that supplied it -- never its resolved value.
fn resolve_call_url(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    if let Some(url) = static_url_template(node, source) {
        return Some((url, None));
    }
    if node.kind() != "string" {
        return None;
    }
    let text = node.utf8_text(source).ok()?;
    let inner = python_string_inner(text)?;
    let (host_expr, path) = resolve_opaque_host_url(inner)?;
    Some((path, Some(host_expr)))
}

/// Whether `text` is a dotted-identifier chain (`self._host`, `cls.HOST`)
/// and nothing else -- no call, subscript, or operator. This is the only
/// expression shape trusted as an opaque, unresolved authority; anything
/// else risks silently mischaracterizing a call's target.
fn is_dotted_identifier_chain(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|segment| {
            let mut chars = segment.chars();
            chars
                .next()
                .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
                && chars.all(|rest| rest == '_' || rest.is_ascii_alphanumeric())
        })
}

/// Resolves an f-string interpolation body (already stripped of its quotes)
/// shaped as `{<dotted-expr>}/literal/path`. Returns the raw expression text
/// and the normalized path, or `None` when the leading interpolation is not
/// exactly one dotted-identifier expression, when anything but `/` follows
/// it immediately, or when a later path-segment interpolation is malformed.
fn resolve_opaque_host_url(inner: &str) -> Option<(String, String)> {
    let rest = inner.strip_prefix('{')?;
    let close = rest.find('}')?;
    let host_expr = &rest[..close];
    if !is_dotted_identifier_chain(host_expr) {
        return None;
    }
    let path = &rest[close + 1..];
    if !path.starts_with('/') {
        return None;
    }
    let normalized_path = normalize_path_segments(path)?;
    Some((host_expr.to_owned(), normalized_path))
}

/// Rewrites every well-formed `{field}` path-segment interpolation in
/// `path` (immediately preceded by `/`, immediately followed by `/`, `?`,
/// `#`, or the end, and containing neither `!` nor `:`) to the literal
/// placeholder `{param}`. Unlike a "does this URL have any dynamic part"
/// check, an all-literal `path` is a valid result on its own, so this
/// returns `Some` even when it replaces nothing -- only a malformed
/// interpolation returns `None`.
fn normalize_path_segments(path: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut remaining = path;
    while let Some(open) = remaining.find('{') {
        let close = remaining[open + 1..].find('}')? + open + 1;
        let field = &remaining[open + 1..close];
        let next = remaining.as_bytes().get(close + 1).copied();
        let previous = remaining.as_bytes().get(open.wrapping_sub(1)).copied();
        if previous != Some(b'/')
            || !matches!(next, None | Some(b'/' | b'?' | b'#'))
            || field.contains(['!', ':'])
        {
            return None;
        }
        normalized.push_str(&remaining[..open]);
        normalized.push_str("{param}");
        remaining = &remaining[close + 1..];
    }
    normalized.push_str(remaining);
    Some(normalized)
}

/// Field names of a request body passed as a `json=`/`data=` keyword
/// argument dict literal directly at the call site. Empty when the body is
/// not a keyword argument, not a dict literal, or has any non-string-literal
/// key -- never inspected past the call site itself.
fn request_body_fields(arguments: Node<'_>, source: &[u8]) -> Vec<String> {
    for name in ["json", "data"] {
        if let Some(value) = keyword_argument(arguments, name, source) {
            let fields = dictionary_string_keys(value, source);
            if !fields.is_empty() {
                return fields;
            }
        }
    }
    Vec::new()
}

/// Response field names statically read from an HTTP call: either chained
/// directly off it (`...post(...).json()["field"]`), or through exactly one
/// local variable assigned directly from the call and never reassigned
/// before the read, within the same function. Empty when neither shape
/// matches -- never traced through more than this one local hop.
fn response_fields(call: Node<'_>, source: &[u8]) -> Vec<String> {
    if let Some(field) = json_subscript_key(call, source) {
        return vec![field];
    }
    let Some(assignment) = call.parent().filter(|parent| {
        parent.kind() == "assignment" && parent.child_by_field_name("right") == Some(call)
    }) else {
        return Vec::new();
    };
    let Some(target) = assignment.child_by_field_name("left") else {
        return Vec::new();
    };
    if target.kind() != "identifier" {
        return Vec::new();
    }
    let Ok(name) = target.utf8_text(source) else {
        return Vec::new();
    };
    let Some(body) = enclosing_function_body(assignment) else {
        return Vec::new();
    };
    if is_rebound_elsewhere(body, name, assignment, source) {
        return Vec::new();
    }
    let mut fields = Vec::new();
    collect_identifier_json_reads(body, name, source, &mut fields);
    fields.sort();
    fields.dedup();
    fields
}

/// `<node>.json()["key"]` chained directly off `node` -- one call, one
/// attribute, one subscript, nothing more.
fn json_subscript_key(node: Node<'_>, source: &[u8]) -> Option<String> {
    let attribute = node.parent()?;
    if attribute.kind() != "attribute" || attribute.child_by_field_name("object") != Some(node) {
        return None;
    }
    let is_json = attribute
        .child_by_field_name("attribute")
        .and_then(|name| name.utf8_text(source).ok())
        == Some("json");
    if !is_json {
        return None;
    }
    let outer_call = attribute.parent()?;
    if outer_call.kind() != "call" || outer_call.child_by_field_name("function") != Some(attribute)
    {
        return None;
    }
    let subscript = outer_call.parent()?;
    if subscript.kind() != "subscript" || subscript.child_by_field_name("value") != Some(outer_call)
    {
        return None;
    }
    let key = subscript.child_by_field_name("subscript")?;
    if key.kind() != "string" {
        return None;
    }
    static_string_content(key.utf8_text(source).ok()?)
}

fn enclosing_function_body(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.parent();
    while let Some(candidate) = current {
        if candidate.kind() == "function_definition" {
            return candidate.child_by_field_name("body");
        }
        current = candidate.parent();
    }
    None
}

/// Whether `name` is bound to anything else within `scope` (an assignment,
/// augmented assignment, `for` target, or `as`-pattern target other than
/// `exclude`), stopping at any nested function/class boundary -- a binding
/// inside a closure is a distinct local, not a rebinding of this one.
fn is_rebound_elsewhere(scope: Node<'_>, name: &str, exclude: Node<'_>, source: &[u8]) -> bool {
    fn walk(node: Node<'_>, name: &str, exclude: Node<'_>, source: &[u8], root: bool) -> bool {
        if !root && matches!(node.kind(), "function_definition" | "class_definition") {
            return false;
        }
        let is_target = match node.kind() {
            "assignment" | "augmented_assignment" => {
                node.child_by_field_name("left").is_some_and(|left| {
                    left.kind() == "identifier" && left.utf8_text(source) == Ok(name)
                })
            }
            "for_statement" => node.child_by_field_name("left").is_some_and(|left| {
                left.kind() == "identifier" && left.utf8_text(source) == Ok(name)
            }),
            "as_pattern_target" => node.utf8_text(source) == Ok(name),
            _ => false,
        };
        if is_target && node != exclude {
            return true;
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .any(|child| walk(child, name, exclude, source, false))
    }
    walk(scope, name, exclude, source, true)
}

/// Every `name.json()["key"]` read within `scope`, stopping at any nested
/// function/class boundary.
fn collect_identifier_json_reads(
    scope: Node<'_>,
    name: &str,
    source: &[u8],
    fields: &mut Vec<String>,
) {
    fn walk(node: Node<'_>, name: &str, source: &[u8], fields: &mut Vec<String>, root: bool) {
        if !root && matches!(node.kind(), "function_definition" | "class_definition") {
            return;
        }
        if node.kind() == "identifier"
            && node.utf8_text(source) == Ok(name)
            && let Some(field) = json_subscript_key(node, source)
        {
            fields.push(field);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            walk(child, name, source, fields, false);
        }
    }
    walk(scope, name, source, fields, true);
}

fn collect_definitions(
    node: Node<'_>,
    source: &[u8],
    module: &str,
    parent: Option<&str>,
    bindings: &PythonBindings,
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
                parse_definition(definition, decorated, source, module, parent, bindings)
            {
                let canonical = symbol.canonical_path.clone();
                symbols.push(symbol);
                if let Some(body) = definition.child_by_field_name("body") {
                    collect_definitions(body, source, module, Some(&canonical), bindings, symbols);
                }
            }
        } else {
            collect_definitions(child, source, module, parent, bindings, symbols);
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
    bindings: &PythonBindings,
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
    let orm_accesses = if is_class {
        Vec::new()
    } else {
        collect_orm_accesses(body, source, bindings)
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
            collect_api_endpoints(decorated, node, source, bindings)
        })
    };
    let external_calls = if is_class {
        Vec::new()
    } else {
        collect_external_calls(body, source, bindings)
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
        orm_accesses,
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

fn collect_orm_accesses(
    node: Node<'_>,
    source: &[u8],
    bindings: &PythonBindings,
) -> Vec<OrmModelAccess> {
    let mut accesses = Vec::new();
    let mut cursor = node.walk();
    visit_orm_calls(&mut cursor, source, bindings, &mut accesses, true);
    accesses.sort_by(|left, right| {
        left.model_ref
            .cmp(&right.model_ref)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.range.start_byte.cmp(&right.range.start_byte))
    });
    accesses.dedup_by(|left, right| {
        left.model_ref == right.model_ref
            && left.kind == right.kind
            && left.statement_hash == right.statement_hash
    });
    accesses
}

fn visit_orm_calls(
    cursor: &mut TreeCursor<'_>,
    source: &[u8],
    bindings: &PythonBindings,
    accesses: &mut Vec<OrmModelAccess>,
    root: bool,
) {
    let node = cursor.node();
    if !root && matches!(node.kind(), "function_definition" | "class_definition") {
        return;
    }
    if node.kind() == "call" && !is_nested_in_execute_call(node, source) {
        if let Some((verb_call, values_arguments)) = sqlalchemy_values_call(node, source, bindings)
        {
            if let Some(access) =
                orm_access_from_call(node, verb_call, Some(values_arguments), source, bindings)
            {
                accesses.push(access);
            }
        } else if sqlalchemy_verb_call(node, source, bindings).is_some()
            && !is_values_chain_inner(node, source, bindings)
            && let Some(access) = orm_access_from_call(node, node, None, source, bindings)
        {
            accesses.push(access);
        }
    }
    if cursor.goto_first_child() {
        loop {
            visit_orm_calls(cursor, source, bindings, accesses, false);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn sqlalchemy_verb_call<'a>(
    call: Node<'a>,
    source: &[u8],
    bindings: &PythonBindings,
) -> Option<(SqlAlchemyVerb, Node<'a>)> {
    if call.kind() != "call" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    let verb = if function.kind() == "identifier" {
        let local_name = function.utf8_text(source).ok()?;
        bindings.sqlalchemy_expr_names.get(local_name).copied()
    } else if function.kind() == "attribute" {
        let owner = function.child_by_field_name("object")?;
        let operation = function
            .child_by_field_name("attribute")?
            .utf8_text(source)
            .ok()?;
        owner
            .utf8_text(source)
            .ok()
            .filter(|owner| bindings.sqlalchemy_module_aliases.contains(*owner))
            .and_then(|_| SqlAlchemyVerb::from_name(operation))
    } else {
        None
    }?;
    Some((verb, call.child_by_field_name("arguments")?))
}

fn sqlalchemy_values_call<'a>(
    call: Node<'a>,
    source: &[u8],
    bindings: &PythonBindings,
) -> Option<(Node<'a>, Node<'a>)> {
    if call.kind() != "call" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    if function.kind() != "attribute"
        || function
            .child_by_field_name("attribute")?
            .utf8_text(source)
            .ok()?
            != "values"
    {
        return None;
    }
    let verb_call = function.child_by_field_name("object")?;
    let (verb, _) = sqlalchemy_verb_call(verb_call, source, bindings)?;
    if !matches!(verb, SqlAlchemyVerb::Insert | SqlAlchemyVerb::Update) {
        return None;
    }
    Some((verb_call, call.child_by_field_name("arguments")?))
}

fn is_values_chain_inner(call: Node<'_>, source: &[u8], bindings: &PythonBindings) -> bool {
    let Some(attribute) = call.parent().filter(|parent| parent.kind() == "attribute") else {
        return false;
    };
    if attribute.child_by_field_name("object") != Some(call) {
        return false;
    }
    let Some(outer_call) = attribute.parent().filter(|parent| parent.kind() == "call") else {
        return false;
    };
    sqlalchemy_values_call(outer_call, source, bindings)
        .is_some_and(|(verb_call, _)| verb_call == call)
}

fn is_nested_in_execute_call(call: Node<'_>, source: &[u8]) -> bool {
    let mut ancestor = call.parent();
    while let Some(node) = ancestor {
        if matches!(node.kind(), "function_definition" | "class_definition") {
            return false;
        }
        if node.kind() == "call"
            && node
                .child_by_field_name("function")
                .and_then(|function| function.utf8_text(source).ok())
                .is_some_and(|callee| callee.rsplit('.').next() == Some("execute"))
        {
            return true;
        }
        ancestor = node.parent();
    }
    false
}

fn orm_access_from_call(
    expression: Node<'_>,
    verb_call: Node<'_>,
    values_arguments: Option<Node<'_>>,
    source: &[u8],
    bindings: &PythonBindings,
) -> Option<OrmModelAccess> {
    let (verb, verb_arguments) = sqlalchemy_verb_call(verb_call, source, bindings)?;
    let (model_name, mut columns) = orm_model_and_columns(verb, verb_arguments, source)?;
    if matches!(verb, SqlAlchemyVerb::Insert | SqlAlchemyVerb::Update) {
        columns =
            values_arguments.map_or_else(Vec::new, |arguments| values_columns(arguments, source));
    } else if verb == SqlAlchemyVerb::Delete {
        columns.clear();
    }
    columns.sort_unstable();
    columns.dedup();
    Some(OrmModelAccess {
        kind: verb.access_kind(),
        model_ref: resolve_model_ref(&model_name, bindings),
        columns,
        range: source_range(expression),
        statement_hash: blake3::hash(&source[expression.byte_range()])
            .to_hex()
            .to_string(),
    })
}

fn orm_model_and_columns(
    verb: SqlAlchemyVerb,
    arguments: Node<'_>,
    source: &[u8],
) -> Option<(String, Vec<String>)> {
    let mut cursor = arguments.walk();
    let arguments = arguments.named_children(&mut cursor).collect::<Vec<_>>();
    if arguments.is_empty() || (verb != SqlAlchemyVerb::Select && arguments.len() != 1) {
        return None;
    }
    let mut model_name = None::<String>;
    let mut columns = Vec::new();
    for argument in arguments {
        let (candidate, column) = if argument.kind() == "identifier" {
            (argument.utf8_text(source).ok()?.to_owned(), None)
        } else if argument.kind() == "attribute" {
            let object = argument.child_by_field_name("object")?;
            if object.kind() != "identifier" {
                return None;
            }
            (
                object.utf8_text(source).ok()?.to_owned(),
                argument
                    .child_by_field_name("attribute")?
                    .utf8_text(source)
                    .ok()
                    .map(str::to_owned),
            )
        } else {
            return None;
        };
        if model_name.as_ref().is_some_and(|model| model != &candidate) {
            return None;
        }
        model_name = Some(candidate);
        if verb == SqlAlchemyVerb::Select
            && let Some(column) = column
        {
            columns.push(column);
        }
    }
    Some((model_name?, columns))
}

fn values_columns(arguments: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut cursor = arguments.walk();
    let arguments = arguments.named_children(&mut cursor).collect::<Vec<_>>();
    if !arguments.is_empty()
        && arguments
            .iter()
            .all(|argument| argument.kind() == "keyword_argument")
    {
        return arguments
            .iter()
            .filter_map(|argument| argument.child_by_field_name("name"))
            .filter_map(|name| name.utf8_text(source).ok())
            .map(str::to_owned)
            .collect();
    }
    let [dictionary] = arguments.as_slice() else {
        return Vec::new();
    };
    dictionary_string_keys(*dictionary, source)
}

/// String keys of a dict literal, when every entry is a plain `key: value`
/// pair keyed by a static string literal. Empty for a non-dictionary node,
/// or when any entry has a non-literal or non-string key — never a partial
/// list, since a partial list would misrepresent which fields are known.
fn dictionary_string_keys(dictionary: Node<'_>, source: &[u8]) -> Vec<String> {
    if dictionary.kind() != "dictionary" {
        return Vec::new();
    }
    let mut cursor = dictionary.walk();
    let entries = dictionary.named_children(&mut cursor).collect::<Vec<_>>();
    if entries.iter().any(|entry| entry.kind() != "pair") {
        return Vec::new();
    }
    entries
        .iter()
        .map(|entry| entry.child_by_field_name("key"))
        .map(|key| {
            key.filter(|key| key.kind() == "string")
                .and_then(|key| key.utf8_text(source).ok())
                .and_then(static_string_content)
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}

fn resolve_model_ref(model_name: &str, bindings: &PythonBindings) -> String {
    bindings
        .imported_symbols
        .get(model_name)
        .or_else(|| bindings.same_file_classes.get(model_name))
        .cloned()
        .unwrap_or_else(|| model_name.to_owned())
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

    const fn position(line: usize, character: usize) -> TypePosition {
        TypePosition { line, character }
    }

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
    fn extracts_same_file_sqlalchemy_expression_accesses_and_static_columns() {
        let source = r#"
from sqlalchemy import insert, select, update

class Subscription(Base):
    __tablename__ = "subscriptions"
    id = Column(String, primary_key=True)
    status = Column(String)

def persist(status):
    select(Subscription)
    select(Subscription.id, Subscription.status)
    update(Subscription).values(status=status)
    insert(Subscription).values(a=1, b=2)
    insert(Subscription).values({"created_at": now(), "status": status})
"#;
        let analysis = PythonAnalyzer::analyze_source("src/app/models.py", source)
            .expect("SQLAlchemy expressions");
        let persist = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "persist")
            .expect("persist function");
        assert!(
            persist
                .orm_accesses
                .iter()
                .all(|access| access.model_ref == "app.models.Subscription")
        );
        assert_eq!(
            persist
                .orm_accesses
                .iter()
                .map(|access| (access.kind, access.columns.as_slice()))
                .collect::<Vec<_>>(),
            vec![
                (DatabaseAccessKind::Read, &[][..]),
                (
                    DatabaseAccessKind::Read,
                    &["id".to_owned(), "status".to_owned()][..]
                ),
                (DatabaseAccessKind::Write, &["status".to_owned()][..]),
                (
                    DatabaseAccessKind::Write,
                    &["a".to_owned(), "b".to_owned()][..]
                ),
                (
                    DatabaseAccessKind::Write,
                    &["created_at".to_owned(), "status".to_owned()][..],
                ),
            ]
        );
    }

    #[test]
    fn dynamic_values_payloads_leave_columns_wholly_unknown() {
        let source = r#"
from sqlalchemy import update

class Subscription(Base):
    __tablename__ = "subscriptions"
    id = Column(String)

def persist(payload, some_var, key):
    update(Subscription).values(**payload)
    update(Subscription).values(some_var)
    update(Subscription).values(build())
    update(Subscription).values({"status": payload, key: some_var})
"#;
        let analysis = PythonAnalyzer::analyze_source("models.py", source).expect("dynamic values");
        let persist = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "persist")
            .expect("persist function");
        assert_eq!(persist.orm_accesses.len(), 4);
        assert!(
            persist
                .orm_accesses
                .iter()
                .all(|access| access.columns.is_empty())
        );
    }

    #[test]
    fn sqlalchemy_import_aliases_gate_expression_recognition() {
        let source = r#"
from sqlalchemy import select as sel
import sqlalchemy as sa
import sqlalchemy
import sqlalchemy.sql as sasql
from app.models import Subscription as Sub

def access():
    sel(Sub)
    sa.update(Sub).values(status="active")
    sqlalchemy.delete(Sub)
    sasql.insert(Sub)
"#;
        let analysis =
            PythonAnalyzer::analyze_source("src/app/service.py", source).expect("import aliases");
        assert_eq!(analysis.analysis_version, "python-tree-sitter-v8");
        let access = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "access")
            .expect("access function");
        assert_eq!(access.orm_accesses.len(), 4);
        assert!(
            access
                .orm_accesses
                .iter()
                .all(|orm| orm.model_ref == "app.models.Subscription")
        );
        assert_eq!(
            access
                .orm_accesses
                .iter()
                .map(|orm| orm.kind)
                .collect::<Vec<_>>(),
            vec![
                DatabaseAccessKind::Read,
                DatabaseAccessKind::Write,
                DatabaseAccessKind::Write,
                DatabaseAccessKind::Write,
            ]
        );
    }

    #[test]
    fn unbound_or_dynamic_sqlalchemy_expression_shapes_emit_no_access() {
        let source = r#"
from sqlalchemy import select as sa_select

class Model(Base):
    __tablename__ = "models"
    id = Column(String)

def select(value):
    return value

def local_select():
    select(Model)

def dynamic_model():
    sa_select(get_model())

def wrapped(session):
    session.execute(sa_select(Model))
"#;
        let analysis =
            PythonAnalyzer::analyze_source("models.py", source).expect("negative expressions");
        assert!(
            analysis
                .symbols
                .iter()
                .all(|symbol| symbol.orm_accesses.is_empty())
        );
    }

    #[test]
    fn unit_of_work_session_add_is_not_a_static_orm_access() {
        // session.add(Model()) is a unit-of-work write: only Pyright type
        // inference can prove the receiver is a SQLAlchemy Session (defect
        // A/C boundary). The static extractor only recognizes SQL-expression
        // verbs (select/insert/update/delete); this test locks in that a
        // syntactic `.add`/`.add_all` call is deliberately NOT treated as a
        // static ORM access, so a future change can't silently start
        // guessing it from the method name alone.
        let source = r#"
from sqlalchemy.ext.asyncio import AsyncSession

class Model(Base):
    __tablename__ = "models"
    id = Column(String)

async def create(session: AsyncSession) -> None:
    row = Model()
    session.add(row)
    session.add_all([row])
"#;
        let analysis =
            PythonAnalyzer::analyze_source("models.py", source).expect("unit-of-work write");
        let create = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "create")
            .expect("create function");
        assert!(create.orm_accesses.is_empty());
        assert!(create.database_accesses.is_empty());
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
    fn recognizes_aiohttp_session_calls_from_plain_assignment_and_with_as() {
        let source = r#"
async def notify_a():
    session = aiohttp.ClientSession()
    await session.post("https://billing.internal/events")

async def notify_b():
    async with aiohttp.ClientSession() as session:
        await session.get("https://billing.internal/health")
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        let calls = analysis
            .symbols
            .iter()
            .flat_map(|symbol| &symbol.external_calls)
            .map(|call| (call.method, call.url.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![
                (HttpMethod::Post, "https://billing.internal/events"),
                (HttpMethod::Get, "https://billing.internal/health"),
            ]
        );
    }

    #[test]
    fn recognizes_httpx_client_bound_via_with_as() {
        let source = r#"
def notify():
    with httpx.Client() as client:
        client.post("https://billing.internal/events")
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        assert_eq!(
            analysis.symbols[0].external_calls[0].url,
            "https://billing.internal/events"
        );
    }

    #[test]
    fn recognizes_urllib3_pool_manager_request_calls() {
        let source = r#"
def notify():
    http = urllib3.PoolManager()
    http.request("POST", "https://billing.internal/events")
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        let call = &analysis.symbols[0].external_calls[0];
        assert_eq!(call.method, HttpMethod::Post);
        assert_eq!(call.url, "https://billing.internal/events");
    }

    #[test]
    fn recognizes_http_client_connection_request_calls() {
        let source = r#"
def notify():
    conn = http.client.HTTPSConnection("billing.internal")
    conn.request("GET", "/health")
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        assert!(analysis.symbols[0].external_calls.is_empty());
    }

    #[test]
    fn urllib3_request_with_a_non_literal_verb_produces_no_fact() {
        let source = r#"
def notify(verb):
    http = urllib3.PoolManager()
    http.request(verb, "https://billing.internal/events")
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        assert!(analysis.symbols[0].external_calls.is_empty());
    }

    #[test]
    fn resolves_opaque_host_prefix_with_a_literal_path_suffix() {
        let source = r#"
class StripeClient:
    def __init__(self):
        self._host = "https://api.stripe.com"

    def charge(self):
        requests.post(f"{self._host}/v1/charges")
"#;
        let analysis = PythonAnalyzer::analyze_source("clients.py", source).expect("client");
        let charge = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "charge")
            .expect("charge method");
        let call = &charge.external_calls[0];
        assert_eq!(call.url, "/v1/charges");
        assert_eq!(call.host_expr.as_deref(), Some("self._host"));
    }

    #[test]
    fn resolves_opaque_host_prefix_with_a_dynamic_path_segment() {
        let source = r#"
def notify(subscription_id):
    requests.get(f"{self._host}/v1/subscriptions/{subscription_id}")
"#;
        let analysis = PythonAnalyzer::analyze_source("clients.py", source).expect("client");
        let call = &analysis.symbols[0].external_calls[0];
        assert_eq!(call.url, "/v1/subscriptions/{param}");
        assert_eq!(call.host_expr.as_deref(), Some("self._host"));
    }

    #[test]
    fn opaque_host_prefix_produces_no_fact_when_not_a_dotted_identifier() {
        let source = r#"
def notify():
    requests.post(f"{self._build_host()}/v1/charges")
"#;
        let analysis = PythonAnalyzer::analyze_source("clients.py", source).expect("client");
        assert!(analysis.symbols[0].external_calls.is_empty());
    }

    #[test]
    fn opaque_host_prefix_produces_no_fact_when_anything_follows_before_the_slash() {
        let source = r#"
def notify():
    requests.post(f"{self._host}.example.com/v1/charges")
"#;
        let analysis = PythonAnalyzer::analyze_source("clients.py", source).expect("client");
        assert!(analysis.symbols[0].external_calls.is_empty());
    }

    #[test]
    fn different_opaque_hosts_with_the_same_path_get_distinct_host_expr() {
        let source = r#"
class StripeClient:
    def charge(self):
        requests.post(f"{self._stripe_host}/v1/items")

class InventoryClient:
    def charge(self):
        requests.post(f"{self._inventory_host}/v1/items")
"#;
        let analysis = PythonAnalyzer::analyze_source("clients.py", source).expect("client");
        let mut host_exprs = analysis
            .symbols
            .iter()
            .flat_map(|symbol| &symbol.external_calls)
            .map(|call| call.host_expr.as_deref())
            .collect::<Vec<_>>();
        host_exprs.sort_unstable();
        assert_eq!(
            host_exprs,
            vec![Some("self._inventory_host"), Some("self._stripe_host")]
        );
    }

    #[test]
    fn extracts_request_fields_from_static_dict_literal_body() {
        let source = r#"
def notify():
    requests.post("https://billing.internal/events", json={"kind": "renewed", "amount": 10})
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        let call = &analysis.symbols[0].external_calls[0];
        assert_eq!(call.request_fields, vec!["kind", "amount"]);
    }

    #[test]
    fn request_fields_stay_empty_for_non_literal_or_mixed_key_body() {
        let source = r#"
def notify(payload):
    requests.post("https://billing.internal/events", json=payload)
    requests.post("https://billing.internal/events", json={"kind": "renewed", 1: "x"})
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        for call in &analysis.symbols[0].external_calls {
            assert!(call.request_fields.is_empty());
        }
    }

    #[test]
    fn extracts_response_fields_chained_directly_off_the_call() {
        let source = r#"
def notify():
    requests.post("https://billing.internal/events").json()["status"]
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        let call = &analysis.symbols[0].external_calls[0];
        assert_eq!(call.response_fields, vec!["status"]);
    }

    #[test]
    fn extracts_response_fields_through_one_unreassigned_local_variable() {
        let source = r#"
def notify():
    resp = requests.post("https://billing.internal/events")
    status = resp.json()["status"]
    reason = resp.json()["reason"]
    return status, reason
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        let call = &analysis.symbols[0].external_calls[0];
        assert_eq!(call.response_fields, vec!["reason", "status"]);
    }

    #[test]
    fn response_fields_stay_empty_when_the_local_variable_is_reassigned() {
        let source = r#"
def notify():
    resp = requests.post("https://billing.internal/events")
    resp = other_call()
    status = resp.json()["status"]
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        let call = &analysis.symbols[0].external_calls[0];
        assert!(call.response_fields.is_empty());
    }

    #[test]
    fn response_fields_stay_empty_when_the_local_variable_is_passed_elsewhere() {
        let source = r#"
def notify():
    resp = requests.post("https://billing.internal/events")
    handle(resp)
"#;
        let analysis = PythonAnalyzer::analyze_source("src/caller.py", source).expect("caller");
        let call = &analysis.symbols[0].external_calls[0];
        assert!(call.response_fields.is_empty());
    }

    #[test]
    fn old_format_external_call_json_without_new_fields_still_deserializes() {
        let old = r#"{"method":"post","url":"https://billing.internal/events","range":{"start_byte":0,"end_byte":1,"start_line":0,"end_line":0}}"#;
        let call: ctx_core::ir::ExternalCall =
            serde_json::from_str(old).expect("old-format ExternalCall still deserializes");
        assert_eq!(call.host_expr, None);
        assert!(call.request_fields.is_empty());
        assert!(call.response_fields.is_empty());
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

    #[test]
    fn extracts_type_write_candidates_with_exact_probe_ranges() {
        let source = concat!(
            "x.status = value\n",
            "x.count += 1\n",
            "session.add(row)\n",
            "session.add_all([a, b])\n",
            "session.merge(row)\n",
            "session.delete(row)\n",
        );
        let candidates =
            PythonAnalyzer::type_write_candidates("service.py", source).expect("write candidates");

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| (
                    candidate.form,
                    candidate.probe.expression.as_str(),
                    candidate
                        .method_probe
                        .as_ref()
                        .map(|probe| probe.expression.as_str()),
                    candidate.column.as_deref(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (TypeWriteForm::AttrAssign, "x", None, Some("status"),),
                (TypeWriteForm::AttrAssign, "x", None, Some("count"),),
                (TypeWriteForm::Add, "row", Some("session.add"), None,),
                (TypeWriteForm::AddAll, "a", Some("session.add_all"), None,),
                (TypeWriteForm::AddAll, "b", Some("session.add_all"), None,),
                (TypeWriteForm::Merge, "row", Some("session.merge"), None,),
                (TypeWriteForm::Delete, "row", Some("session.delete"), None,),
            ]
        );
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| (candidate.probe.start, candidate.probe.end))
                .collect::<Vec<_>>(),
            vec![
                (position(0, 0), position(0, 1)),
                (position(1, 0), position(1, 1)),
                (position(2, 12), position(2, 15)),
                (position(3, 17), position(3, 18)),
                (position(3, 20), position(3, 21)),
                (position(4, 14), position(4, 17)),
                (position(5, 15), position(5, 18)),
            ]
        );
        assert_eq!(candidates[0].write_range.start_line, 1);
        assert_eq!(candidates[6].write_range.start_line, 6);
    }

    #[test]
    fn add_all_requires_a_statically_enumerated_list() {
        let source = concat!(
            "session.add_all(items)\n",
            "session.add_all([first, *rest, second])\n",
        );
        let candidates =
            PythonAnalyzer::type_write_candidates("service.py", source).expect("write candidates");

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.probe.expression.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn type_probe_positions_use_utf16_code_units() {
        let source = "label = \"😀\"; row.status = value\n";
        let candidate = PythonAnalyzer::type_write_candidates("service.py", source)
            .expect("write candidates")
            .pop()
            .expect("attribute assignment");

        assert_eq!(candidate.probe.expression, "row");
        assert_eq!(candidate.probe.start.character, 14);
        assert_eq!(candidate.probe.end.character, 17);
    }
}
