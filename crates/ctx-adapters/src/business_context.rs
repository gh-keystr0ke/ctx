use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use ctx_app::ports::{BusinessContextReader, PortError};
use ctx_core::business::{BusinessDocument, BusinessKind, ExplicitSymbolLink};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BusinessContextError {
    #[error("could not read business context at '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid business context YAML in '{path}': {message}")]
    Yaml { path: String, message: String },
    #[error("business context '{path}' is missing required field '{field}'")]
    MissingField { path: String, field: &'static str },
    #[error("business context '{path}' has unsupported type '{kind}'")]
    InvalidKind { path: String, kind: String },
    #[error("business context ID '{id}' is declared more than once")]
    DuplicateId { id: String },
    #[error("Markdown context '{0}' must start and end YAML front matter with '---'")]
    InvalidFrontMatter(String),
}

pub struct YamlBusinessContextReader {
    root: PathBuf,
}

impl YamlBusinessContextReader {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl BusinessContextReader for YamlBusinessContextReader {
    fn read_all(&self) -> Result<Vec<BusinessDocument>, PortError> {
        let context_root = self.root.join(".context");
        if !context_root.exists() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        collect_paths(&context_root, &mut paths).map_err(port_error)?;
        paths.retain(|path| is_context_file(path));
        paths.sort();

        let mut documents = Vec::new();
        let mut ids = BTreeSet::new();
        for path in paths {
            let document = read_document(&self.root, &path).map_err(port_error)?;
            if !ids.insert(document.id.clone()) {
                return Err(PortError::new(
                    BusinessContextError::DuplicateId { id: document.id }.to_string(),
                ));
            }
            documents.push(document);
        }
        Ok(documents)
    }
}

#[derive(Debug, Deserialize)]
struct RawDocument {
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    name: Option<String>,
    title: Option<String>,
    statement: Option<String>,
    description: Option<String>,
    decision: Option<String>,
    status: Option<String>,
    feature: Option<String>,
    #[serde(default)]
    implementation: Vec<RawLink>,
    #[serde(default)]
    tests: Vec<RawLink>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawLink {
    Name(String),
    Object { symbol: String },
}

impl RawLink {
    fn symbol(self) -> String {
        match self {
            Self::Name(symbol) | Self::Object { symbol } => symbol,
        }
    }
}

fn collect_paths(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), BusinessContextError> {
    let entries = fs::read_dir(directory).map_err(|source| BusinessContextError::Io {
        path: directory.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| BusinessContextError::Io {
            path: directory.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_paths(&path, paths)?;
        } else {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_context_file(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("yaml")
            || extension.eq_ignore_ascii_case("yml")
            || extension.eq_ignore_ascii_case("md")
    })
}

fn read_document(root: &Path, path: &Path) -> Result<BusinessDocument, BusinessContextError> {
    let content = fs::read_to_string(path).map_err(|source| BusinessContextError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let yaml = if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        markdown_front_matter(path, &content)?
    } else {
        content.as_str()
    };
    let raw: RawDocument =
        serde_yaml::from_str(yaml).map_err(|error| BusinessContextError::Yaml {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
    normalize_document(root, path, &content, raw)
}

fn markdown_front_matter<'a>(
    path: &Path,
    content: &'a str,
) -> Result<&'a str, BusinessContextError> {
    let Some(rest) = content.strip_prefix("---\n") else {
        return Err(BusinessContextError::InvalidFrontMatter(
            path.display().to_string(),
        ));
    };
    rest.split_once("\n---")
        .map(|(yaml, _)| yaml)
        .ok_or_else(|| BusinessContextError::InvalidFrontMatter(path.display().to_string()))
}

fn normalize_document(
    root: &Path,
    path: &Path,
    content: &str,
    raw: RawDocument,
) -> Result<BusinessDocument, BusinessContextError> {
    let display_path = path.display().to_string();
    let id = required(raw.id, &display_path, "id")?;
    let kind_text = raw
        .kind
        .or_else(|| inferred_kind(path).map(str::to_owned))
        .ok_or_else(|| BusinessContextError::MissingField {
            path: display_path.clone(),
            field: "type",
        })?;
    let kind = parse_kind(&display_path, &kind_text)?;
    let body = match kind {
        BusinessKind::Feature => raw.description.unwrap_or_default(),
        BusinessKind::Requirement | BusinessKind::Invariant => {
            required(raw.statement, &display_path, "statement")?
        }
        BusinessKind::Decision => required(raw.decision, &display_path, "decision")?,
    };
    let title = match kind {
        BusinessKind::Feature => required(raw.name, &display_path, "name")?,
        BusinessKind::Decision => required(raw.title, &display_path, "title")?,
        BusinessKind::Requirement | BusinessKind::Invariant => body.clone(),
    };
    let source_uri = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    Ok(BusinessDocument {
        id,
        kind,
        title,
        body,
        status: raw.status.unwrap_or_else(|| "active".to_owned()),
        feature: raw.feature,
        implementation: normalize_links(raw.implementation, "implementation"),
        tests: normalize_links(raw.tests, "tests"),
        source_uri,
        content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
    })
}

fn normalize_links(links: Vec<RawLink>, field: &str) -> Vec<ExplicitSymbolLink> {
    links
        .into_iter()
        .enumerate()
        .map(|(index, link)| ExplicitSymbolLink {
            symbol: link.symbol(),
            locator: format!("{field}[{index}]"),
        })
        .collect()
}

fn required<T>(
    value: Option<T>,
    path: &str,
    field: &'static str,
) -> Result<T, BusinessContextError> {
    value.ok_or_else(|| BusinessContextError::MissingField {
        path: path.to_owned(),
        field,
    })
}

fn inferred_kind(path: &Path) -> Option<&str> {
    path.parent()?.file_name()?.to_str()?.strip_suffix('s')
}

fn parse_kind(path: &str, kind: &str) -> Result<BusinessKind, BusinessContextError> {
    match kind.to_ascii_lowercase().as_str() {
        "feature" => Ok(BusinessKind::Feature),
        "requirement" => Ok(BusinessKind::Requirement),
        "invariant" => Ok(BusinessKind::Invariant),
        "decision" => Ok(BusinessKind::Decision),
        _ => Err(BusinessContextError::InvalidKind {
            path: path.to_owned(),
            kind: kind.to_owned(),
        }),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn port_error(error: BusinessContextError) -> PortError {
    PortError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_explicit_requirement_links() {
        let raw: RawDocument = serde_yaml::from_str(
            "id: REQ-SUB-014\nstatement: Keep access.\nimplementation:\n  - symbol: billing.cancel\n",
        )
        .expect("YAML");
        let document = normalize_document(
            Path::new("/repo"),
            Path::new("/repo/.context/requirements/cancel.yaml"),
            "source",
            raw,
        )
        .expect("document");
        assert_eq!(document.kind, BusinessKind::Requirement);
        assert_eq!(document.implementation[0].symbol, "billing.cancel");
        assert_eq!(document.source_uri, ".context/requirements/cancel.yaml");
    }
}
