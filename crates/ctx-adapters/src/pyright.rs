//! Generic Python type-oracle adapter backed by Pyright's Type Server
//! Protocol. This module translates source probes into structured type
//! identity and deliberately contains no ORM or graph semantics.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use ctx_app::ports::{PortError, PythonTypeOracle};
use ctx_core::type_inference::{
    PythonClassType, PythonDeclaration, PythonFunctionType, PythonType, TypePosition, TypeProbe,
};
use serde_json::{Value, json};
use thiserror::Error;

const SUPPORTED_PROTOCOL_MINOR: u32 = 4;
const STDERR_LIMIT: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum PyrightError {
    #[error("Pyright Type Server executable '{executable}' was not found")]
    NotFound { executable: String },
    #[error("could not start Pyright Type Server '{executable}': {source}")]
    Spawn {
        executable: String,
        source: io::Error,
    },
    #[error("Pyright Type Server request '{method}' timed out after {timeout_ms}ms")]
    Timeout { method: String, timeout_ms: u128 },
    #[error("Pyright Type Server protocol error: {0}")]
    Protocol(String),
    #[error("Pyright Type Server request '{method}' failed ({code}): {message}")]
    Request {
        method: String,
        code: i64,
        message: String,
    },
    #[error("Pyright Type Server protocol version '{0}' is unsupported; ctx requires 0.4.x")]
    UnsupportedProtocol(String),
    #[error("Pyright Type Server exited unexpectedly{details}")]
    Exited { details: String },
    #[error("could not read Python source '{}': {source}", path.display())]
    ReadSource { path: PathBuf, source: io::Error },
}

impl PyrightError {
    #[must_use]
    pub const fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }
}

enum ServerEvent {
    Message(Value),
    Malformed(String),
    Closed,
}

/// One warm Pyright Type Server process, shared by every query in an
/// inference command invocation.
pub struct PyrightTypeServer {
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<ServerEvent>,
    stderr: Arc<Mutex<String>>,
    timeout: Duration,
    next_request_id: u64,
    snapshot: i64,
    open_files: BTreeSet<PathBuf>,
    stopped: bool,
    failed: bool,
}

impl PyrightTypeServer {
    /// Starts and initializes `pyright-typeserver --stdio` for one workspace.
    ///
    /// # Errors
    /// Returns [`PyrightError`] for process, protocol, timeout, and version
    /// negotiation failures.
    pub fn start(
        executable: &Path,
        workspace_root: &Path,
        timeout: Duration,
    ) -> Result<Self, PyrightError> {
        let executable_label = executable.display().to_string();
        let mut child = Command::new(executable)
            .arg("--stdio")
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| {
                if source.kind() == io::ErrorKind::NotFound {
                    PyrightError::NotFound {
                        executable: executable_label.clone(),
                    }
                } else {
                    PyrightError::Spawn {
                        executable: executable_label.clone(),
                        source,
                    }
                }
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| PyrightError::Protocol("server stdin was not available".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PyrightError::Protocol("server stdout was not available".to_owned()))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| PyrightError::Protocol("server stderr was not available".to_owned()))?;
        let (sender, events) = mpsc::channel();
        thread::spawn(move || read_server_output(stdout, &sender));
        let stderr = Arc::new(Mutex::new(String::new()));
        capture_stderr(stderr_pipe, Arc::clone(&stderr));

        let mut server = Self {
            child,
            stdin,
            events,
            stderr,
            timeout,
            next_request_id: 1,
            snapshot: 0,
            open_files: BTreeSet::new(),
            stopped: false,
            failed: false,
        };
        let root_uri = file_uri(workspace_root)?;
        server.request(
            "initialize",
            Some(json!({
                "processId": std::process::id(),
                "clientInfo": {"name": "ctx", "version": env!("CARGO_PKG_VERSION")},
                "rootUri": root_uri,
                "workspaceFolders": [{"uri": root_uri, "name": "workspace"}],
                "capabilities": {},
            })),
        )?;
        server.notify("initialized", json!({}))?;
        let version = server
            .request("typeServer/getSupportedProtocolVersion", None)?
            .as_str()
            .ok_or_else(|| PyrightError::Protocol("protocol version was not a string".to_owned()))?
            .to_owned();
        if !supported_protocol(&version) {
            return Err(PyrightError::UnsupportedProtocol(version));
        }
        server.snapshot = server.read_snapshot()?;
        Ok(server)
    }

    #[must_use]
    pub const fn snapshot(&self) -> i64 {
        self.snapshot
    }

    /// Requests a clean server shutdown and waits briefly for process exit.
    ///
    /// # Errors
    /// Returns [`PyrightError`] when shutdown negotiation fails.
    pub fn shutdown(&mut self) -> Result<(), PyrightError> {
        if self.stopped {
            return Ok(());
        }
        self.request("shutdown", None)?;
        self.notify("exit", json!({}))?;
        let deadline = Instant::now() + self.timeout;
        while Instant::now() < deadline {
            if self
                .child
                .try_wait()
                .map_err(|error| PyrightError::Protocol(error.to_string()))?
                .is_some()
            {
                self.stopped = true;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }
        Err(PyrightError::Timeout {
            method: "shutdown".to_owned(),
            timeout_ms: self.timeout.as_millis(),
        })
    }

    fn ensure_open(&mut self, file: &Path) -> Result<String, PyrightError> {
        let canonical = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
        let uri = file_uri(&canonical)?;
        if self.open_files.insert(canonical.clone()) {
            let text =
                fs::read_to_string(&canonical).map_err(|source| PyrightError::ReadSource {
                    path: canonical,
                    source,
                })?;
            self.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "python",
                        "version": 1,
                        "text": text,
                    }
                }),
            )?;
            self.snapshot = self.read_snapshot()?;
        }
        Ok(uri)
    }

    fn read_snapshot(&mut self) -> Result<i64, PyrightError> {
        self.request("typeServer/getSnapshot", None)?
            .as_i64()
            .filter(|snapshot| *snapshot >= 0)
            .ok_or_else(|| PyrightError::Protocol("snapshot was not non-negative".to_owned()))
    }

    fn computed_type(&mut self, uri: &str, probe: &TypeProbe) -> Result<PythonType, PyrightError> {
        let value = match self.request(
            "typeServer/getComputedType",
            Some(computed_type_params(uri, probe, self.snapshot)),
        ) {
            Err(PyrightError::Request { code: -32802, .. }) => {
                self.snapshot = self.read_snapshot()?;
                self.request(
                    "typeServer/getComputedType",
                    Some(computed_type_params(uri, probe, self.snapshot)),
                )?
            }
            result => result?,
        };
        if value.is_null() {
            return Ok(PythonType::Unknown);
        }
        decode_type(&value)
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value, PyrightError> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        let mut payload = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
        });
        if let Some(params) = params {
            payload["params"] = params;
        }
        self.send(&payload)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = self
                .events
                .recv_timeout(remaining)
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => PyrightError::Timeout {
                        method: method.to_owned(),
                        timeout_ms: self.timeout.as_millis(),
                    },
                    mpsc::RecvTimeoutError::Disconnected => self.exited_error(),
                })?;
            match event {
                ServerEvent::Malformed(error) => return Err(PyrightError::Protocol(error)),
                ServerEvent::Closed => return Err(self.exited_error()),
                ServerEvent::Message(message) => {
                    if message.get("method").is_some() && message.get("id").is_some() {
                        self.respond_to_server_request(&message)?;
                        continue;
                    }
                    if message.get("method").is_some() {
                        if message.get("method").and_then(Value::as_str)
                            == Some("typeServer/snapshotChanged")
                            && let Some(snapshot) =
                                message.pointer("/params/new").and_then(Value::as_i64)
                        {
                            self.snapshot = snapshot;
                        }
                        continue;
                    }
                    if message.get("id").and_then(Value::as_u64) != Some(request_id) {
                        return Err(PyrightError::Protocol(format!(
                            "unexpected response id while waiting for '{method}'"
                        )));
                    }
                    if let Some(error) = message.get("error") {
                        return Err(PyrightError::Request {
                            method: method.to_owned(),
                            code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                            message: error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown server error")
                                .to_owned(),
                        });
                    }
                    return message.get("result").cloned().ok_or_else(|| {
                        PyrightError::Protocol(format!(
                            "response to '{method}' had neither result nor error"
                        ))
                    });
                }
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), PyrightError> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn send(&mut self, value: &Value) -> Result<(), PyrightError> {
        let body =
            serde_json::to_vec(value).map_err(|error| PyrightError::Protocol(error.to_string()))?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())
            .and_then(|()| self.stdin.write_all(&body))
            .and_then(|()| self.stdin.flush())
            .map_err(|error| PyrightError::Protocol(format!("could not write request: {error}")))
    }

    fn respond_to_server_request(&mut self, request: &Value) -> Result<(), PyrightError> {
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "workspace/configuration" => {
                let count = request
                    .pointer("/params/items")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                Value::Array(vec![Value::Null; count])
            }
            "workspace/workspaceFolders" => Value::Array(Vec::new()),
            _ => Value::Null,
        };
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": result,
        }))
    }

    fn exited_error(&mut self) -> PyrightError {
        let status = self.child.try_wait().ok().flatten();
        let stderr = self
            .stderr
            .lock()
            .map_or_else(|_| String::new(), |stderr| stderr.trim().to_owned());
        let mut parts = Vec::new();
        if let Some(status) = status {
            parts.push(format!("status {status}"));
        }
        if !stderr.is_empty() {
            parts.push(format!("stderr: {stderr}"));
        }
        PyrightError::Exited {
            details: if parts.is_empty() {
                String::new()
            } else {
                format!(" ({})", parts.join("; "))
            },
        }
    }
}

impl PythonTypeOracle for PyrightTypeServer {
    fn inferred_type(&mut self, file: &Path, probe: &TypeProbe) -> Result<PythonType, PortError> {
        let result = self
            .ensure_open(file)
            .and_then(|uri| self.computed_type(&uri, probe));
        result.map_err(|error| {
            self.failed |= fatal_error(&error);
            PortError::new(error.to_string())
        })
    }

    fn resolve_import(
        &mut self,
        from_file: &Path,
        module: &str,
    ) -> Result<Option<String>, PortError> {
        let uri = self.ensure_open(from_file).map_err(|error| {
            self.failed |= fatal_error(&error);
            PortError::new(error.to_string())
        })?;
        let value = match self.request(
            "typeServer/resolveImport",
            Some(resolve_import_params(&uri, module, self.snapshot)),
        ) {
            Err(PyrightError::Request { code: -32802, .. }) => {
                self.snapshot = self
                    .read_snapshot()
                    .map_err(|error| PortError::new(error.to_string()))?;
                self.request(
                    "typeServer/resolveImport",
                    Some(resolve_import_params(&uri, module, self.snapshot)),
                )
            }
            result => result,
        }
        .map_err(|error| {
            self.failed |= fatal_error(&error);
            PortError::new(error.to_string())
        })?;
        if value.is_null() {
            Ok(None)
        } else {
            value
                .as_str()
                .map(|uri| Some(uri.to_owned()))
                .ok_or_else(|| PortError::new("Pyright resolveImport result was not a URI string"))
        }
    }

    fn is_healthy(&mut self) -> bool {
        !self.failed && self.child.try_wait().is_ok_and(|status| status.is_none())
    }
}

impl Drop for PyrightTypeServer {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stopped = true;
    }
}

fn read_server_output(stdout: impl Read, sender: &mpsc::Sender<ServerEvent>) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_framed_message(&mut reader) {
            Ok(Some(value)) => {
                if sender.send(ServerEvent::Message(value)).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = sender.send(ServerEvent::Closed);
                return;
            }
            Err(error) => {
                let _ = sender.send(ServerEvent::Malformed(error.to_string()));
                return;
            }
        }
    }
}

fn read_framed_message(reader: &mut impl BufRead) -> Result<Option<Value>, io::Error> {
    let mut content_length = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Ok(None);
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        let Some((name, value)) = header.trim_end().split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed JSON-RPC header",
            ));
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            content_length = Some(value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length")
            })?);
        }
    }
    let content_length = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn capture_stderr(stderr: impl Read + Send + 'static, output: Arc<Mutex<String>>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buffer = [0_u8; 1024];
        loop {
            let Ok(read) = reader.read(&mut buffer) else {
                return;
            };
            if read == 0 {
                return;
            }
            if let Ok(mut output) = output.lock()
                && output.len() < STDERR_LIMIT
            {
                let remaining = STDERR_LIMIT - output.len();
                output.push_str(&String::from_utf8_lossy(&buffer[..read.min(remaining)]));
            }
        }
    });
}

fn supported_protocol(version: &str) -> bool {
    let mut parts = version.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("0"), Some(minor), Some(_), None)
            if minor.parse::<u32>() == Ok(SUPPORTED_PROTOCOL_MINOR)
    )
}

const fn fatal_error(error: &PyrightError) -> bool {
    matches!(
        error,
        PyrightError::Timeout { .. }
            | PyrightError::Protocol(_)
            | PyrightError::Exited { .. }
            | PyrightError::ReadSource { .. }
    )
}

fn position_value(position: TypePosition) -> Value {
    json!({"line": position.line, "character": position.character})
}

fn computed_type_params(uri: &str, probe: &TypeProbe, snapshot: i64) -> Value {
    json!({
        "arg": {
            "uri": uri,
            "range": {
                "start": position_value(probe.start),
                "end": position_value(probe.end),
            }
        },
        "snapshot": snapshot,
    })
}

fn resolve_import_params(uri: &str, module: &str, snapshot: i64) -> Value {
    json!({
        "sourceUri": uri,
        "moduleDescriptor": {
            "leadingDots": 0,
            "nameParts": module.split('.').collect::<Vec<_>>(),
        },
        "snapshot": snapshot,
    })
}

fn decode_type(value: &Value) -> Result<PythonType, PyrightError> {
    decode_type_inner(value, &mut BTreeMap::new())
}

fn decode_type_inner(
    value: &Value,
    decoded: &mut BTreeMap<i64, PythonType>,
) -> Result<PythonType, PyrightError> {
    let kind = value
        .get("kind")
        .and_then(Value::as_i64)
        .ok_or_else(|| PyrightError::Protocol("type result had no numeric kind".to_owned()))?;
    if kind == 9 {
        let reference = value
            .get("typeReferenceId")
            .and_then(Value::as_i64)
            .ok_or_else(|| PyrightError::Protocol("type reference had no target id".to_owned()))?;
        return decoded.get(&reference).cloned().ok_or_else(|| {
            PyrightError::Protocol(format!("type reference {reference} was unresolved"))
        });
    }
    let id = value
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| PyrightError::Protocol("type result had no numeric id".to_owned()))?;
    decoded.insert(
        id,
        PythonType::Other {
            oracle_kind: format!("recursive_type_{id}"),
        },
    );
    let result = match kind {
        0 => match value.get("name").and_then(Value::as_str) {
            Some("any") => PythonType::Any,
            Some("unknown") => PythonType::Unknown,
            Some(name) => PythonType::Other {
                oracle_kind: name.to_owned(),
            },
            None => {
                return Err(PyrightError::Protocol(
                    "built-in type had no name".to_owned(),
                ));
            }
        },
        2 => PythonType::Function(PythonFunctionType {
            declaration: decode_declaration(value.get("declaration"))?,
            bound_to: value
                .get("boundToType")
                .map(|bound| decode_type_inner(bound, decoded).map(Box::new))
                .transpose()?,
        }),
        3 => {
            let flags = value.get("flags").and_then(Value::as_u64).unwrap_or(0);
            PythonType::Class(PythonClassType {
                declaration: decode_declaration(value.get("declaration"))?,
                is_instance: flags & (1 << 1) != 0,
                type_arguments: value
                    .get("typeArgs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|argument| decode_type_inner(argument, decoded))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        }
        4 => PythonType::Union {
            members: value
                .get("subTypes")
                .and_then(Value::as_array)
                .ok_or_else(|| PyrightError::Protocol("union had no subTypes".to_owned()))?
                .iter()
                .map(|member| decode_type_inner(member, decoded))
                .collect::<Result<Vec<_>, _>>()?,
        },
        _ => PythonType::Other {
            oracle_kind: format!("type_kind_{kind}"),
        },
    };
    decoded.insert(id, result.clone());
    Ok(result)
}

fn decode_declaration(value: Option<&Value>) -> Result<PythonDeclaration, PyrightError> {
    let value = value
        .ok_or_else(|| PyrightError::Protocol("declared type had no declaration".to_owned()))?;
    let kind = value
        .get("kind")
        .and_then(Value::as_i64)
        .ok_or_else(|| PyrightError::Protocol("declaration had no numeric kind".to_owned()))?;
    if kind == 0 {
        let node = value
            .get("node")
            .ok_or_else(|| PyrightError::Protocol("regular declaration had no node".to_owned()))?;
        let uri = node
            .get("uri")
            .and_then(Value::as_str)
            .ok_or_else(|| PyrightError::Protocol("declaration had no URI".to_owned()))?
            .to_owned();
        let start = decode_position(node.pointer("/range/start"))?;
        let end = decode_position(node.pointer("/range/end"))?;
        Ok(PythonDeclaration {
            path: file_uri_path(&uri),
            uri,
            name: value.get("name").and_then(Value::as_str).map(str::to_owned),
            range: Some((start, end)),
            category: value
                .get("category")
                .and_then(Value::as_u64)
                .and_then(|category| u8::try_from(category).ok()),
        })
    } else if kind == 1 {
        let uri = value
            .get("uri")
            .and_then(Value::as_str)
            .ok_or_else(|| PyrightError::Protocol("synthesized declaration had no URI".to_owned()))?
            .to_owned();
        Ok(PythonDeclaration {
            path: file_uri_path(&uri),
            uri,
            name: None,
            range: None,
            category: None,
        })
    } else {
        Err(PyrightError::Protocol(format!(
            "unknown declaration kind {kind}"
        )))
    }
}

fn decode_position(value: Option<&Value>) -> Result<TypePosition, PyrightError> {
    let value = value.ok_or_else(|| PyrightError::Protocol("missing position".to_owned()))?;
    Ok(TypePosition {
        line: value
            .get("line")
            .and_then(Value::as_u64)
            .and_then(|line| usize::try_from(line).ok())
            .ok_or_else(|| PyrightError::Protocol("invalid position line".to_owned()))?,
        character: value
            .get("character")
            .and_then(Value::as_u64)
            .and_then(|character| usize::try_from(character).ok())
            .ok_or_else(|| PyrightError::Protocol("invalid position character".to_owned()))?,
    })
}

fn file_uri(path: &Path) -> Result<String, PyrightError> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path = path.to_string_lossy().replace('\\', "/");
    if !path.starts_with('/') && !path.as_bytes().get(1).is_some_and(|byte| *byte == b':') {
        return Err(PyrightError::Protocol(format!(
            "cannot create a file URI from relative path '{path}'"
        )));
    }
    let prefix = if path.starts_with('/') {
        "file://"
    } else {
        "file:///"
    };
    Ok(format!("{prefix}{}", percent_encode_path(path.as_bytes())))
}

fn percent_encode_path(path: &[u8]) -> String {
    let mut encoded = String::new();
    for byte in path {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~')
        {
            encoded.push(char::from(*byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn file_uri_path(uri: &str) -> Option<String> {
    let encoded = uri.strip_prefix("file://")?;
    let encoded = encoded.strip_prefix("localhost").unwrap_or(encoded);
    percent_decode(encoded)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push((hex_value(high)? << 4) | hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_structured_class_union_without_hover_text() {
        let value = json!({
            "kind": 4, "id": 1, "flags": 0,
            "subTypes": [
                {
                    "kind": 3, "id": 2, "flags": 2,
                    "declaration": {
                        "kind": 0, "category": 6, "name": "OfferDB",
                        "node": {
                            "uri": "file:///workspace/models.py",
                            "range": {
                                "start": {"line": 3, "character": 6},
                                "end": {"line": 3, "character": 13}
                            }
                        }
                    }
                },
                {"kind": 0, "id": 3, "flags": 0, "name": "unknown"}
            ]
        });

        let PythonType::Union { members } = decode_type(&value).expect("structured type") else {
            panic!("expected union");
        };
        let PythonType::Class(class) = &members[0] else {
            panic!("expected class");
        };
        assert!(class.is_instance);
        assert_eq!(class.declaration.name.as_deref(), Some("OfferDB"));
        assert_eq!(
            class.declaration.path.as_deref(),
            Some("/workspace/models.py")
        );
        assert_eq!(members[1], PythonType::Unknown);
    }

    #[test]
    fn reads_content_length_framing_and_rejects_malformed_json() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":7}"#;
        let input = format!("Content-Length: {}\r\n\r\n", body.len())
            .into_bytes()
            .into_iter()
            .chain(body.iter().copied())
            .collect::<Vec<_>>();
        let value = read_framed_message(&mut io::Cursor::new(input))
            .expect("frame")
            .expect("message");
        assert_eq!(value["result"], 7);

        let malformed = b"Content-Length: 1\r\n\r\n{";
        assert!(read_framed_message(&mut io::Cursor::new(malformed)).is_err());
    }

    #[test]
    fn protocol_support_is_explicit_for_zero_major_versions() {
        assert!(supported_protocol("0.4.0"));
        assert!(supported_protocol("0.4.9"));
        assert!(!supported_protocol("0.3.9"));
        assert!(!supported_protocol("1.4.0"));
        assert!(!supported_protocol("0.4"));
    }

    #[test]
    fn file_uri_round_trip_preserves_spaces_and_unicode() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("space π.py");
        fs::write(&path, "").expect("fixture");
        let uri = file_uri(&path).expect("file URI");
        assert_eq!(file_uri_path(&uri).as_deref(), path.to_str());
    }

    #[test]
    fn missing_executable_is_reported_separately() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let error = PyrightTypeServer::start(
            &temporary.path().join("missing-pyright-typeserver"),
            temporary.path(),
            Duration::from_millis(50),
        )
        .err()
        .expect("missing executable");
        assert!(error.is_not_found());
    }

    #[cfg(unix)]
    #[test]
    fn warm_server_answers_structured_type_and_import_queries() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source_path = temporary.path().join("model.py");
        fs::write(&source_path, "row = Model()\n").expect("source fixture");
        let executable = executable_fixture(
            temporary.path(),
            "server.py",
            r#"
import json
import sys

def read_message():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b'\r\n', b'\n'):
            break
        name, value = line.decode().strip().split(':', 1)
        if name.lower() == 'content-length':
            length = int(value.strip())
    return json.loads(sys.stdin.buffer.read(length))

def send(message):
    body = json.dumps(message, separators=(',', ':')).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get('method')
    if 'id' not in message:
        if method == 'exit':
            break
        continue
    if method == 'initialize':
        result = {'capabilities': {}}
    elif method == 'typeServer/getSupportedProtocolVersion':
        result = '0.4.1'
    elif method == 'typeServer/getSnapshot':
        result = 7
    elif method == 'typeServer/resolveImport':
        result = 'file:///site-packages/sqlalchemy/orm/session.py'
    elif method == 'typeServer/getComputedType':
        result = {
            'kind': 3, 'id': 10, 'flags': 2,
            'declaration': {
                'kind': 0, 'category': 6, 'name': 'Model',
                'node': {
                    'uri': message['params']['arg']['uri'],
                    'range': {
                        'start': {'line': 0, 'character': 6},
                        'end': {'line': 0, 'character': 11},
                    },
                },
            },
        }
    elif method == 'shutdown':
        result = None
    else:
        send({'jsonrpc': '2.0', 'id': message['id'], 'error': {'code': -32601, 'message': method}})
        continue
    send({'jsonrpc': '2.0', 'id': message['id'], 'result': result})
"#,
        );
        let mut server =
            PyrightTypeServer::start(&executable, temporary.path(), Duration::from_secs(1))
                .expect("type server starts");
        let probe = TypeProbe {
            expression: "row".to_owned(),
            range: ctx_core::ir::SourceRange {
                start_byte: 0,
                end_byte: 3,
                start_line: 1,
                end_line: 1,
            },
            start: TypePosition {
                line: 0,
                character: 0,
            },
            end: TypePosition {
                line: 0,
                character: 3,
            },
        };

        let resolved = server
            .inferred_type(&source_path, &probe)
            .expect("computed type");
        let PythonType::Class(class) = resolved else {
            panic!("expected class");
        };
        assert_eq!(class.declaration.name.as_deref(), Some("Model"));
        assert!(class.is_instance);
        assert_eq!(server.snapshot(), 7);
        assert_eq!(
            server
                .resolve_import(&source_path, "sqlalchemy.orm.session")
                .expect("import query")
                .as_deref(),
            Some("file:///site-packages/sqlalchemy/orm/session.py")
        );
        server.shutdown().expect("clean shutdown");
    }

    #[cfg(unix)]
    #[test]
    fn server_crash_aborts_startup() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let executable = executable_fixture(temporary.path(), "crash.py", "raise SystemExit(3)\n");
        let error =
            PyrightTypeServer::start(&executable, temporary.path(), Duration::from_millis(200))
                .err()
                .expect("server crash");
        assert!(matches!(error, PyrightError::Exited { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn malformed_server_response_aborts_startup() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let executable = executable_fixture(
            temporary.path(),
            "malformed.py",
            "import sys\nsys.stdout.buffer.write(b'Content-Length: 1\\r\\n\\r\\n{')\nsys.stdout.buffer.flush()\n",
        );
        let error =
            PyrightTypeServer::start(&executable, temporary.path(), Duration::from_millis(200))
                .err()
                .expect("malformed response");
        assert!(matches!(error, PyrightError::Protocol(_)));
    }

    #[cfg(unix)]
    #[test]
    fn unresponsive_server_query_times_out() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let executable = executable_fixture(
            temporary.path(),
            "timeout.py",
            "import time\ntime.sleep(1)\n",
        );
        let error =
            PyrightTypeServer::start(&executable, temporary.path(), Duration::from_millis(30))
                .err()
                .expect("timeout");
        assert!(matches!(error, PyrightError::Timeout { .. }));
    }

    #[cfg(unix)]
    fn executable_fixture(directory: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = directory.join(name);
        fs::write(&path, format!("#!/usr/bin/env python3\n{body}")).expect("server fixture");
        let mut permissions = fs::metadata(&path).expect("fixture metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("executable fixture");
        path
    }
}
