use std::{collections::BTreeMap, fs, path::PathBuf};

use ctx_app::ports::{LanguageAnalyzer, PortError};
use ctx_core::ir::{
    ApiEndpoint, ApiParam, FileAnalysis, HttpMethod, OpenApiOperation, OpenApiResponse,
    ParamSource, SourceRange, SymbolDefinition, SymbolKind,
};
use serde_json::Value;
use thiserror::Error;

use crate::analyzer::AnalyzerModule;

#[derive(Debug, Error)]
pub enum OpenApiAnalysisError {
    #[error("could not read OpenAPI specification '{path}': {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("OpenAPI specification '{path}' is invalid: {message}")]
    Invalid { path: String, message: String },
}

pub struct OpenApiAnalyzer {
    root: PathBuf,
}

impl OpenApiAnalyzer {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Parses `OpenAPI` 3.0/3.1 YAML or JSON into public HTTP operation
    /// contracts. Local `$ref` values are followed for parameters, request
    /// bodies, responses, and schemas; unresolved references remain explicit
    /// type names rather than causing data to be guessed.
    ///
    /// # Errors
    /// Returns [`OpenApiAnalysisError`] when the document is not valid YAML,
    /// is not `OpenAPI` 3.x, or has no object-shaped `paths` member.
    pub fn analyze_source(
        relative_path: &str,
        source: &str,
    ) -> Result<FileAnalysis, OpenApiAnalysisError> {
        let root: Value =
            yaml_serde::from_str(source).map_err(|error| OpenApiAnalysisError::Invalid {
                path: relative_path.to_owned(),
                message: error.to_string(),
            })?;
        let version = root.get("openapi").and_then(Value::as_str).ok_or_else(|| {
            OpenApiAnalysisError::Invalid {
                path: relative_path.to_owned(),
                message: "missing string field 'openapi'".to_owned(),
            }
        })?;
        if !version.starts_with("3.0.") && !version.starts_with("3.1.") {
            return Err(OpenApiAnalysisError::Invalid {
                path: relative_path.to_owned(),
                message: format!("unsupported OpenAPI version '{version}'; expected 3.0 or 3.1"),
            });
        }
        let paths = root
            .get("paths")
            .and_then(Value::as_object)
            .ok_or_else(|| OpenApiAnalysisError::Invalid {
                path: relative_path.to_owned(),
                message: "missing object field 'paths'".to_owned(),
            })?;
        let global_security = security(&root, root.get("security"));
        let global_servers = servers(root.get("servers"));
        let namespace = specification_namespace(relative_path);
        let mut symbols = Vec::new();
        for (path, raw_path_item) in paths {
            let path_item = dereference(&root, raw_path_item);
            let path_parameters = parse_parameters(&root, path_item.get("parameters"));
            let path_servers = servers(path_item.get("servers"));
            for (method_name, method) in openapi_methods() {
                let Some(raw_operation) = path_item.get(method_name) else {
                    continue;
                };
                let operation = dereference(&root, raw_operation);
                if !operation.is_object() {
                    return Err(OpenApiAnalysisError::Invalid {
                        path: relative_path.to_owned(),
                        message: format!("operation '{method_name} {path}' must be an object"),
                    });
                }
                let endpoint = operation_contract(OperationContext {
                    root: &root,
                    source,
                    path,
                    method_name,
                    method,
                    path_item,
                    operation,
                    path_parameters: &path_parameters,
                    path_servers: &path_servers,
                    global_security: &global_security,
                    global_servers: &global_servers,
                });
                symbols.push(operation_symbol(&namespace, endpoint));
            }
        }
        symbols.sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
        Ok(FileAnalysis {
            path: relative_path.to_owned(),
            language: "openapi".to_owned(),
            analysis_version: "openapi-v1".to_owned(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            symbols,
        })
    }
}

impl LanguageAnalyzer for OpenApiAnalyzer {
    fn analysis_version(&self, _relative_path: &str) -> Result<String, PortError> {
        Ok("openapi-v1".to_owned())
    }

    fn analyze(&self, relative_path: &str) -> Result<FileAnalysis, PortError> {
        let path = self.root.join(relative_path);
        let source = fs::read_to_string(&path).map_err(|source| {
            PortError::new(
                OpenApiAnalysisError::Read {
                    path: path.display().to_string(),
                    source,
                }
                .to_string(),
            )
        })?;
        Self::analyze_source(relative_path, &source)
            .map_err(|error| PortError::new(error.to_string()))
    }

    fn analyze_text(&self, relative_path: &str, source: &str) -> Result<FileAnalysis, PortError> {
        Self::analyze_source(relative_path, source)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

impl AnalyzerModule for OpenApiAnalyzer {
    fn language(&self) -> &'static str {
        "openapi"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml", "json"]
    }
}

#[derive(Clone, Copy)]
struct OperationContext<'a> {
    root: &'a Value,
    source: &'a str,
    path: &'a str,
    method_name: &'a str,
    method: HttpMethod,
    path_item: &'a Value,
    operation: &'a Value,
    path_parameters: &'a [ApiParam],
    path_servers: &'a [String],
    global_security: &'a [String],
    global_servers: &'a [String],
}

fn operation_contract(context: OperationContext<'_>) -> ApiEndpoint {
    let OperationContext {
        root,
        source,
        path,
        method_name,
        method,
        path_item,
        operation,
        path_parameters,
        path_servers,
        global_security,
        global_servers,
    } = context;
    let mut parameters = path_parameters
        .iter()
        .map(|parameter| {
            (
                (parameter.source, parameter.name.clone()),
                parameter.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for parameter in parse_parameters(root, operation.get("parameters")) {
        parameters.insert((parameter.source, parameter.name.clone()), parameter);
    }
    let (body, request_content_types) = request_body(root, operation.get("requestBody"));
    if let Some(body) = body {
        parameters.insert((body.source, body.name.clone()), body);
    }
    let responses = responses(root, operation.get("responses"));
    let return_type = response_type(&responses);
    let security = if operation.get("security").is_some() {
        security(root, operation.get("security"))
    } else {
        global_security.to_vec()
    };
    let operation_servers = servers(operation.get("servers"));
    let selected_servers = if !operation_servers.is_empty() {
        operation_servers
    } else if !path_servers.is_empty() {
        path_servers.to_vec()
    } else {
        global_servers.to_vec()
    };
    let mut tags = string_array(operation.get("tags"));
    tags.sort();
    tags.dedup();
    let range = operation_range(source, path, method_name);
    ApiEndpoint {
        path: path.to_owned(),
        method,
        params: parameters.into_values().collect(),
        return_type,
        framework: "openapi".to_owned(),
        range,
        openapi: Some(OpenApiOperation {
            operation_id: string(operation.get("operationId")),
            summary: string(operation.get("summary")).or_else(|| string(path_item.get("summary"))),
            description: string(operation.get("description"))
                .or_else(|| string(path_item.get("description"))),
            deprecated: operation
                .get("deprecated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            tags,
            security,
            servers: selected_servers,
            request_content_types,
            responses,
        }),
    }
}

fn operation_symbol(namespace: &str, endpoint: ApiEndpoint) -> SymbolDefinition {
    let operation_id = endpoint
        .openapi
        .as_ref()
        .and_then(|metadata| metadata.operation_id.clone());
    let fallback = format!(
        "{}_{}",
        endpoint.method.as_str().to_ascii_lowercase(),
        slug(&endpoint.path)
    );
    let local_name = operation_id.clone().unwrap_or(fallback);
    let canonical_path = format!("openapi.{namespace}.{}", slug(&local_name));
    let normalized = serde_json::to_vec(&endpoint).expect("ApiEndpoint serialization cannot fail");
    let body_hash = blake3::hash(&normalized).to_hex().to_string();
    SymbolDefinition {
        name: operation_id
            .unwrap_or_else(|| format!("{} {}", endpoint.method.as_str(), endpoint.path)),
        canonical_path,
        kind: SymbolKind::Function,
        range: endpoint.range,
        signature: Some(format!("{} {}", endpoint.method.as_str(), endpoint.path)),
        body_hash: body_hash.clone(),
        structural_fingerprint: body_hash,
        calls: Vec::new(),
        database_accesses: Vec::new(),
        schema_tables: Vec::new(),
        api_endpoints: vec![endpoint],
        external_calls: Vec::new(),
    }
}

const fn openapi_methods() -> [(&'static str, HttpMethod); 8] {
    [
        ("get", HttpMethod::Get),
        ("post", HttpMethod::Post),
        ("put", HttpMethod::Put),
        ("delete", HttpMethod::Delete),
        ("patch", HttpMethod::Patch),
        ("head", HttpMethod::Head),
        ("options", HttpMethod::Options),
        ("trace", HttpMethod::Trace),
    ]
}

fn parse_parameters(root: &Value, value: Option<&Value>) -> Vec<ApiParam> {
    let mut parameters = Vec::new();
    for raw in value.and_then(Value::as_array).into_iter().flatten() {
        let parameter = dereference(root, raw);
        let Some(name) = parameter.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(location) = parameter.get("in").and_then(Value::as_str) else {
            continue;
        };
        let source = match location {
            "path" => ParamSource::Path,
            "query" => ParamSource::Query,
            "header" => ParamSource::Header,
            "cookie" => ParamSource::Cookie,
            _ => continue,
        };
        let schema = parameter
            .get("schema")
            .or_else(|| first_content_schema(parameter.get("content")));
        parameters.push(ApiParam {
            name: name.to_owned(),
            type_hint: schema.and_then(|schema| schema_type(root, schema)),
            source,
            required: source == ParamSource::Path
                || parameter
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
        });
    }
    parameters
}

fn request_body(root: &Value, value: Option<&Value>) -> (Option<ApiParam>, Vec<String>) {
    let Some(body) = value.map(|value| dereference(root, value)) else {
        return (None, Vec::new());
    };
    let content = body.get("content").and_then(Value::as_object);
    let mut content_types = content
        .into_iter()
        .flat_map(|content| content.keys().cloned())
        .collect::<Vec<_>>();
    content_types.sort();
    let type_hint = content.and_then(|content| {
        content.values().find_map(|media| {
            media
                .get("schema")
                .and_then(|schema| schema_type(root, schema))
        })
    });
    (
        Some(ApiParam {
            name: "body".to_owned(),
            type_hint,
            source: ParamSource::Body,
            required: body
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        content_types,
    )
}

fn responses(root: &Value, value: Option<&Value>) -> Vec<OpenApiResponse> {
    let Some(responses) = value.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut parsed = responses
        .iter()
        .map(|(status, raw)| {
            let response = dereference(root, raw);
            let content = response.get("content").and_then(Value::as_object);
            let mut content_types = content
                .into_iter()
                .flat_map(|content| content.keys().cloned())
                .collect::<Vec<_>>();
            content_types.sort();
            let type_hint = content.and_then(|content| {
                content.values().find_map(|media| {
                    media
                        .get("schema")
                        .and_then(|schema| schema_type(root, schema))
                })
            });
            OpenApiResponse {
                status: status.clone(),
                description: string(response.get("description")),
                content_types,
                type_hint,
            }
        })
        .collect::<Vec<_>>();
    parsed.sort_by(|left, right| left.status.cmp(&right.status));
    parsed
}

fn response_type(responses: &[OpenApiResponse]) -> Option<String> {
    let mut types = responses
        .iter()
        .filter(|response| response.status.starts_with('2'))
        .filter_map(|response| response.type_hint.clone())
        .collect::<Vec<_>>();
    types.sort();
    types.dedup();
    match types.as_slice() {
        [] => None,
        [only] => Some(only.clone()),
        _ => Some(format!("oneOf<{}>", types.join("|"))),
    }
}

fn schema_type(root: &Value, raw: &Value) -> Option<String> {
    if let Some(reference) = raw.get("$ref").and_then(Value::as_str) {
        return reference
            .rsplit('/')
            .next()
            .map(decode_json_pointer_segment);
    }
    let schema = dereference(root, raw);
    for combinator in ["oneOf", "anyOf", "allOf"] {
        if let Some(items) = schema.get(combinator).and_then(Value::as_array) {
            let types = items
                .iter()
                .filter_map(|item| schema_type(root, item))
                .collect::<Vec<_>>();
            if !types.is_empty() {
                return Some(format!("{combinator}<{}>", types.join("|")));
            }
        }
    }
    let kind = schema.get("type")?;
    if let Some(types) = kind.as_array() {
        let types = types
            .iter()
            .filter_map(Value::as_str)
            .filter(|kind| *kind != "null")
            .collect::<Vec<_>>();
        return (!types.is_empty()).then(|| types.join("|"));
    }
    let kind = kind.as_str()?;
    if kind == "array" {
        return schema
            .get("items")
            .and_then(|items| schema_type(root, items))
            .map(|item| format!("array<{item}>"))
            .or_else(|| Some("array".to_owned()));
    }
    if kind == "object"
        && let Some(values) = schema.get("additionalProperties")
        && values.is_object()
    {
        return schema_type(root, values).map(|value| format!("map<string,{value}>"));
    }
    schema.get("format").and_then(Value::as_str).map_or_else(
        || Some(kind.to_owned()),
        |format| Some(format!("{kind}({format})")),
    )
}

fn first_content_schema(value: Option<&Value>) -> Option<&Value> {
    value
        .and_then(Value::as_object)
        .and_then(|content| content.values().find_map(|media| media.get("schema")))
}

fn security(root: &Value, value: Option<&Value>) -> Vec<String> {
    let mut requirements = Vec::new();
    for requirement in value.and_then(Value::as_array).into_iter().flatten() {
        let requirement = dereference(root, requirement);
        let Some(object) = requirement.as_object() else {
            continue;
        };
        for (scheme, scopes) in object {
            let scopes = string_array(Some(scopes));
            if scopes.is_empty() {
                requirements.push(scheme.clone());
            } else {
                requirements.push(format!("{scheme}:[{}]", scopes.join(",")));
            }
        }
    }
    requirements.sort();
    requirements.dedup();
    requirements
}

fn servers(value: Option<&Value>) -> Vec<String> {
    let mut urls = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|server| server.get("url").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    urls.sort();
    urls.dedup();
    urls
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn dereference<'a>(root: &'a Value, value: &'a Value) -> &'a Value {
    let mut current = value;
    for _ in 0..16 {
        let Some(reference) = current.get("$ref").and_then(Value::as_str) else {
            break;
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            break;
        };
        let Some(resolved) = root.pointer(pointer) else {
            break;
        };
        current = resolved;
    }
    current
}

fn operation_range(source: &str, path: &str, method: &str) -> SourceRange {
    let mut path_indent = None;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if path_indent.is_none()
            && (trimmed == format!("{path}:") || trimmed == format!("\"{path}\":"))
        {
            path_indent = Some(line.len() - line.trim_start().len());
            continue;
        }
        if let Some(indent) = path_indent {
            let line_indent = line.len() - line.trim_start().len();
            if line_indent <= indent && !trimmed.is_empty() && !trimmed.starts_with('#') {
                break;
            }
            if trimmed.starts_with(&format!("{method}:"))
                || trimmed.starts_with(&format!("\"{method}\":"))
            {
                return SourceRange {
                    start_byte: 0,
                    end_byte: source.len(),
                    start_line: index + 1,
                    end_line: index + 1,
                };
            }
        }
    }
    SourceRange {
        start_byte: 0,
        end_byte: source.len(),
        start_line: 1,
        end_line: 1,
    }
}

fn specification_namespace(relative_path: &str) -> String {
    let without_extension = relative_path
        .rsplit_once('.')
        .map_or(relative_path, |(path, _)| path);
    let without_openapi = without_extension
        .strip_suffix("/openapi")
        .unwrap_or(without_extension);
    let namespace = slug(without_openapi);
    if namespace.is_empty() || namespace == "openapi" {
        "root".to_owned()
    } else {
        namespace
    }
}

fn slug(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            slug.push(character);
            separator = false;
        } else if !separator && !slug.is_empty() {
            slug.push('_');
            separator = true;
        }
    }
    slug.trim_matches('_').to_owned()
}

fn decode_json_pointer_segment(value: &str) -> String {
    value.replace("~1", "/").replace("~0", "~")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r"
openapi: 3.1.0
servers:
  - url: https://billing.example.test/v1
security:
  - oauth: [subscriptions:read]
paths:
  /subscriptions/{id}:
    parameters:
      - $ref: '#/components/parameters/SubscriptionId'
    get:
      operationId: getSubscription
      summary: Read a subscription
      tags: [subscriptions]
      parameters:
        - name: X-Request-ID
          in: header
          required: true
          schema: {type: string, format: uuid}
      responses:
        '200':
          description: Found
          content:
            application/json:
              schema: {$ref: '#/components/schemas/Subscription'}
    patch:
      operationId: patchSubscription
      security: []
      requestBody:
        required: true
        content:
          application/json:
            schema: {$ref: '#/components/schemas/SubscriptionPatch'}
      responses:
        '204': {description: Updated}
components:
  parameters:
    SubscriptionId:
      name: id
      in: path
      required: true
      schema: {type: string}
  schemas:
    Subscription: {type: object}
    SubscriptionPatch: {type: object}
";

    #[test]
    fn parses_every_operation_and_retains_openapi_contract_metadata() {
        let analysis = OpenApiAnalyzer::analyze_source("openapi.yaml", SPEC).expect("analysis");

        assert_eq!(analysis.language, "openapi");
        assert_eq!(analysis.symbols.len(), 2);
        let get = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "getSubscription")
            .expect("GET operation");
        assert_eq!(get.canonical_path, "openapi.root.getSubscription");
        let endpoint = &get.api_endpoints[0];
        assert_eq!(endpoint.method, HttpMethod::Get);
        assert_eq!(endpoint.return_type.as_deref(), Some("Subscription"));
        assert!(endpoint.params.iter().any(|parameter| {
            parameter.name == "id" && parameter.source == ParamSource::Path && parameter.required
        }));
        assert!(endpoint.params.iter().any(|parameter| {
            parameter.name == "X-Request-ID"
                && parameter.source == ParamSource::Header
                && parameter.type_hint.as_deref() == Some("string(uuid)")
        }));
        let metadata = endpoint.openapi.as_ref().expect("OpenAPI metadata");
        assert_eq!(metadata.security, vec!["oauth:[subscriptions:read]"]);
        assert_eq!(metadata.servers, vec!["https://billing.example.test/v1"]);
        assert_eq!(
            metadata.responses[0].type_hint.as_deref(),
            Some("Subscription")
        );
    }

    #[test]
    fn operation_security_overrides_global_security_and_request_body_is_typed() {
        let analysis = OpenApiAnalyzer::analyze_source("api/openapi.yml", SPEC).expect("analysis");
        let patch = analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name == "patchSubscription")
            .expect("PATCH operation");
        let endpoint = &patch.api_endpoints[0];

        assert_eq!(patch.canonical_path, "openapi.api.patchSubscription");
        assert!(
            endpoint
                .openapi
                .as_ref()
                .expect("metadata")
                .security
                .is_empty()
        );
        assert!(endpoint.params.iter().any(|parameter| {
            parameter.source == ParamSource::Body
                && parameter.required
                && parameter.type_hint.as_deref() == Some("SubscriptionPatch")
        }));
    }

    #[test]
    fn rejects_non_openapi_and_unsupported_swagger_documents_explicitly() {
        let missing = OpenApiAnalyzer::analyze_source("openapi.yaml", "paths: {}")
            .expect_err("missing version must fail");
        assert!(
            missing
                .to_string()
                .contains("missing string field 'openapi'")
        );

        let swagger =
            OpenApiAnalyzer::analyze_source("openapi.yaml", "openapi: 2.0.0\npaths: {}\n")
                .expect_err("unsupported version must fail");
        assert!(swagger.to_string().contains("unsupported OpenAPI version"));
    }
}
