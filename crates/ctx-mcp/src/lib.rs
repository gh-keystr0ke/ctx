//! Thin Model Context Protocol adapter over the existing `ctx-app` use cases.

use std::io::{self, BufRead, Write};

use ctx_adapters::{analyzer::AnalyzerRegistry, git::GitRepo, sqlite::SqliteStore};
use ctx_app::{
    ports::{GitRepository, PortError},
    query::QueryService,
    review::ReviewRunner,
};
use ctx_core::{context_pack::ContextRequest, domain::RepositoryId};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

const MODERN_PROTOCOL: &str = "2026-07-28";
const LEGACY_PROTOCOL: &str = "2025-11-25";
const SUPPORTED_PROTOCOLS: &[&str] =
    &[MODERN_PROTOCOL, LEGACY_PROTOCOL, "2025-06-18", "2025-03-26"];

#[derive(Debug, Error)]
pub enum McpServerError {
    #[error(transparent)]
    Git(#[from] ctx_adapters::git::GitError),
    #[error(transparent)]
    Sqlite(#[from] ctx_adapters::sqlite::SqliteStoreError),
    #[error("MCP transport IO failed: {0}")]
    Io(#[from] io::Error),
    #[error("MCP JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("repository metadata could not be loaded: {0}")]
    Port(#[from] PortError),
    #[error("ctx is not initialized; run 'ctx init' before starting MCP")]
    NotInitialized,
}

pub struct McpServer<'a> {
    git: &'a GitRepo,
    analyzer: AnalyzerRegistry,
    store: SqliteStore,
    repository: RepositoryId,
}

impl<'a> McpServer<'a> {
    /// Builds a server bound to the repository's existing local index.
    ///
    /// # Errors
    ///
    /// Returns [`McpServerError`] when the repository is not initialized or its
    /// store/metadata cannot be opened.
    pub fn new(git: &'a GitRepo) -> Result<Self, McpServerError> {
        let database = git.root().join(".ctx").join("ctx.db");
        if !database.exists() {
            return Err(McpServerError::NotInitialized);
        }
        let repository = git.descriptor()?.id;
        Ok(Self {
            git,
            analyzer: AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?,
            store: SqliteStore::open(&database, git.context_root())?,
            repository,
        })
    }

    fn handle(&mut self, request: &Value) -> Option<Value> {
        let method = request.get("method")?.as_str()?;
        if method.starts_with("notifications/") {
            return None;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        match method {
            "server/discover" => Some(rpc_result(id, discover_result())),
            "initialize" => Some(rpc_result(id, initialize_result(request))),
            "ping" => Some(rpc_result(id, json!({}))),
            "tools/list" => Some(rpc_result(id, json!({"tools": tool_definitions()}))),
            "tools/call" => Some(self.call_tool(id, request)),
            _ => Some(rpc_error(
                id,
                -32601,
                format!("method '{method}' not found"),
            )),
        }
    }

    fn call_tool(&mut self, id: Value, request: &Value) -> Value {
        let name = request
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !tool_definitions().iter().any(|tool| tool["name"] == name) {
            return rpc_error(id, -32602, format!("unknown tool '{name}'"));
        }
        let arguments = request
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let result = self
            .execute_tool(name, arguments)
            .map_or_else(|error| tool_error(&error), tool_result);
        rpc_result(id, result)
    }

    fn execute_tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        match name {
            "get_context" => {
                let arguments: GetContextArguments = decode(arguments)?;
                let request = ContextRequest {
                    task: arguments.task,
                    files: arguments.files,
                    symbols: arguments.symbols,
                    token_budget: arguments.token_budget,
                };
                let result = QueryService::new(&self.store)
                    .context(&self.repository, &request)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(result).map_err(|error| error.to_string())
            }
            "get_impact" => {
                let arguments: TargetArguments = decode(arguments)?;
                let result = QueryService::new(&self.store)
                    .impact(&self.repository, &arguments.target)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(result).map_err(|error| error.to_string())
            }
            "explain_relation" => {
                let arguments: ExplainArguments = decode(arguments)?;
                let result = QueryService::new(&self.store)
                    .explain(&self.repository, &arguments.claim)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(result).map_err(|error| error.to_string())
            }
            "find_requirements" => {
                let arguments: SearchArguments = decode(arguments)?;
                let result = QueryService::new(&self.store)
                    .find_requirements(&self.repository, &arguments.query)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(result).map_err(|error| error.to_string())
            }
            "review_change" => {
                let arguments: ReviewArguments = decode(arguments)?;
                let result = ReviewRunner::new(self.git, &self.analyzer, &self.store)
                    .run(&self.repository, &arguments.base, arguments.verbose)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(result).map_err(|error| error.to_string())
            }
            _ => Err(format!("unknown tool '{name}'")),
        }
    }
}

/// Serves newline-delimited JSON-RPC over standard input/output.
///
/// Both the current `server/discover` era and legacy `initialize` clients are
/// supported so local coding agents can migrate independently.
///
/// # Errors
///
/// Returns [`McpServerError`] when repository setup, transport IO, or response
/// serialization fails.
pub fn serve_stdio(git: &GitRepo) -> Result<(), McpServerError> {
    let mut server = McpServer::new(git)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(&mut server, stdin.lock(), stdout.lock())
}

fn serve<R: BufRead, W: Write>(
    server: &mut McpServer<'_>,
    reader: R,
    mut writer: W,
) -> Result<(), McpServerError> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                write_message(
                    &mut writer,
                    &rpc_error(Value::Null, -32700, format!("parse error: {error}")),
                )?;
                continue;
            }
        };
        if let Some(response) = server.handle(&request) {
            write_message(&mut writer, &response)?;
        }
    }
    Ok(())
}

fn write_message(writer: &mut impl Write, value: &Value) -> Result<(), McpServerError> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn discover_result() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": SUPPORTED_PROTOCOLS,
        "capabilities": {"tools": {}},
        "serverInfo": server_info(),
        "instructions": "Use ctx tools to retrieve evidence-backed product context for local code changes."
    })
}

fn initialize_result(request: &Value) -> Value {
    let requested = request
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(LEGACY_PROTOCOL);
    let protocol = if SUPPORTED_PROTOCOLS.contains(&requested) && requested != MODERN_PROTOCOL {
        requested
    } else {
        LEGACY_PROTOCOL
    };
    json!({
        "protocolVersion": protocol,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": server_info(),
        "instructions": "Use ctx tools to retrieve evidence-backed product context for local code changes."
    })
}

fn server_info() -> Value {
    json!({"name": "ctx", "version": env!("CARGO_PKG_VERSION")})
}

fn rpc_result(id: Value, mut result: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "_meta".to_owned(),
            json!({"io.modelcontextprotocol/serverInfo": server_info()}),
        );
    }
    let mut response = serde_json::Map::new();
    response.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    response.insert("id".to_owned(), id);
    response.insert("result".to_owned(), result);
    Value::Object(response)
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    let mut error = serde_json::Map::new();
    error.insert("code".to_owned(), Value::from(code));
    error.insert("message".to_owned(), Value::String(message));
    let mut response = serde_json::Map::new();
    response.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    response.insert("id".to_owned(), id);
    response.insert("error".to_owned(), Value::Object(error));
    response.insert(
        "_meta".to_owned(),
        json!({"io.modelcontextprotocol/serverInfo": server_info()}),
    );
    Value::Object(response)
}

fn tool_result(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    let mut result = serde_json::Map::new();
    result.insert(
        "content".to_owned(),
        json!([{"type": "text", "text": text}]),
    );
    result.insert("structuredContent".to_owned(), value);
    result.insert("isError".to_owned(), Value::Bool(false));
    Value::Object(result)
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true
    })
}

fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| format!("invalid tool arguments: {error}"))
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "get_context",
            "title": "Compile ctx Context Pack",
            "description": "Compile bounded evidence-backed product and code context for a coding task.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string"},
                    "files": {"type": "array", "items": {"type": "string"}, "default": []},
                    "symbols": {"type": "array", "items": {"type": "string"}, "default": []},
                    "token_budget": {"type": "integer", "minimum": 1, "default": 4000}
                },
                "required": ["task"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "get_impact",
            "title": "Get product impact",
            "description": "Find bounded product intent, implementation, and tests related to a file, symbol, or stable ID.",
            "inputSchema": object_schema("target", "Indexed file, symbol, or product-context ID"),
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "explain_relation",
            "title": "Explain ctx claim",
            "description": "Explain a node or directed source -> target relationship from stored provenance and evidence.",
            "inputSchema": object_schema("claim", "Stable ID or directed source -> target claim"),
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "find_requirements",
            "title": "Find product requirements",
            "description": "Find indexed requirements by stable ID or lexical terms.",
            "inputSchema": object_schema("query", "Requirement ID or search terms"),
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }),
        json!({
            "name": "review_change",
            "title": "Review change against product contracts",
            "description": "Review a Git branch or working-tree diff using high-confidence product claims.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "base": {"type": "string", "default": "HEAD"},
                    "verbose": {"type": "boolean", "default": false}
                },
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }),
    ]
}

fn object_schema(field: &str, description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {field: {"type": "string", "description": description}},
        "required": [field],
        "additionalProperties": false
    })
}

#[derive(Debug, Deserialize)]
struct GetContextArguments {
    task: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    symbols: Vec<String>,
    #[serde(default = "default_token_budget")]
    token_budget: usize,
}

#[derive(Debug, Deserialize)]
struct TargetArguments {
    target: String,
}

#[derive(Debug, Deserialize)]
struct ExplainArguments {
    claim: String,
}

#[derive(Debug, Deserialize)]
struct SearchArguments {
    query: String,
}

#[derive(Debug, Deserialize)]
struct ReviewArguments {
    #[serde(default = "default_base")]
    base: String,
    #[serde(default)]
    verbose: bool,
}

const fn default_token_budget() -> usize {
    4_000
}

fn default_base() -> String {
    "HEAD".to_owned()
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Cursor, path::Path, process::Command};

    use ctx_adapters::business_context::YamlBusinessContextReader;
    use ctx_app::{context::ContextImporter, index::IndexRunner, ports::GitRepository};

    use super::*;

    #[test]
    fn advertises_exactly_the_five_product_tools_in_stable_order() {
        let tools = tool_definitions();
        let names = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "get_context",
                "get_impact",
                "explain_relation",
                "find_requirements",
                "review_change"
            ]
        );
        assert_eq!(
            tools[1]["inputSchema"]["properties"]["target"]["type"],
            "string"
        );
    }

    #[test]
    fn supports_modern_discovery_and_legacy_initialization() {
        let discover = discover_result();
        assert_eq!(discover["supportedVersions"][0], MODERN_PROTOCOL);
        let initialize = initialize_result(&json!({
            "params": {"protocolVersion": "2025-06-18"}
        }));
        assert_eq!(initialize["protocolVersion"], "2025-06-18");
        assert_eq!(initialize["capabilities"]["tools"]["listChanged"], false);
    }

    fn run_git(root: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(arguments)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn indexed_repository() -> (tempfile::TempDir, GitRepo) {
        let directory = tempfile::tempdir().expect("temporary repository");
        fs::create_dir_all(directory.path().join(".ctx")).expect("ctx directory");
        fs::create_dir_all(directory.path().join(".context/requirements"))
            .expect("requirements directory");
        fs::create_dir_all(directory.path().join("src")).expect("source directory");
        fs::write(
            directory.path().join(".ctx/config.toml"),
            "languages = [\"python\"]\n\n[paths]\ninclude = [\"src\"]\n",
        )
        .expect("configuration");
        fs::write(
            directory.path().join("src/app.py"),
            "def cancel():\n    return 'access preserved'\n",
        )
        .expect("source");
        fs::write(
            directory
                .path()
                .join(".context/requirements/preserve-access.yaml"),
            "id: REQ-TEST-001\ntype: requirement\nstatus: active\nstatement: Preserve paid access.\nimplementation:\n  - symbol: app.cancel\n",
        )
        .expect("requirement");
        run_git(directory.path(), &["init", "--quiet"]);
        run_git(directory.path(), &["config", "user.name", "ctx tests"]);
        run_git(
            directory.path(),
            &["config", "user.email", "ctx@example.invalid"],
        );
        run_git(directory.path(), &["add", "."]);
        run_git(directory.path(), &["commit", "--quiet", "-m", "baseline"]);

        let git = GitRepo::discover(directory.path()).expect("discover repository");
        let database = directory.path().join(".ctx/ctx.db");
        let mut store = SqliteStore::open(&database, git.context_root()).expect("open store");
        let analyzer =
            AnalyzerRegistry::builtins(directory.path(), &["python".to_owned()]).expect("analyzer");
        IndexRunner::new(&git, &analyzer, &mut store)
            .run("2026-08-29T00:00:00Z")
            .expect("index code");
        let repository = git.descriptor().expect("repository descriptor");
        let head = git.head().expect("head");
        let reader = YamlBusinessContextReader::new(git.context_root().to_path_buf());
        ContextImporter::new(&reader, &mut store)
            .run(&repository, &head, "2026-08-29T00:00:00Z")
            .expect("index context");
        drop(store);
        (directory, git)
    }

    #[test]
    fn all_five_tools_execute_over_the_stdio_json_rpc_transport() {
        let (_directory, git) = indexed_repository();
        let mut server = McpServer::new(&git).expect("MCP server");
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_context","arguments":{"task":"preserve access","token_budget":1000}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_impact","arguments":{"target":"app.cancel"}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"explain_relation","arguments":{"claim":"REQ-TEST-001"}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"find_requirements","arguments":{"query":"access"}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"review_change","arguments":{"base":"HEAD"}}}),
        ];
        let input = requests
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let mut output = Vec::new();

        serve(&mut server, Cursor::new(input), &mut output).expect("serve requests");

        let responses: Vec<Value> = String::from_utf8(output)
            .expect("UTF-8 output")
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON-RPC response"))
            .collect();
        assert_eq!(responses.len(), requests.len());
        for response in responses {
            assert!(response.get("error").is_none(), "{response}");
            assert_eq!(response["result"]["isError"], false, "{response}");
            assert!(
                response["result"]["structuredContent"].is_object()
                    || response["result"]["structuredContent"].is_array(),
                "{response}"
            );
        }
    }
}
