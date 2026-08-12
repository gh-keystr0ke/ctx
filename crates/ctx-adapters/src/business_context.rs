use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use ctx_app::ports::{BusinessContextReader, BusinessContextWriter, PortError};
use ctx_core::business::{BusinessDocument, BusinessKind, ExplicitSymbolLink, Visibility};
use serde::{Deserialize, Serialize};
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
    #[error(
        "business context '{path}' has invalid visibility '{visibility}'; expected 'public' or 'private'"
    )]
    InvalidVisibility { path: String, visibility: String },
    #[error("business context ID '{id}' is declared more than once")]
    DuplicateId { id: String },
    #[error("Markdown context '{0}' must start and end YAML front matter with '---'")]
    InvalidFrontMatter(String),
    #[error("a business context document already exists at '{0}'")]
    AlreadyExists(String),
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
    visibility: Option<String>,
    feature: Option<String>,
    #[serde(default = "default_implementation_expected")]
    implementation_expected: bool,
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

const fn default_implementation_expected() -> bool {
    true
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
        visibility: parse_visibility(&display_path, raw.visibility.as_deref())?,
        feature: raw.feature,
        implementation_expected: raw.implementation_expected,
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

impl BusinessContextWriter for YamlBusinessContextReader {
    /// Writes `document` as a brand-new `.context/{kind}s/{id}.yaml` file
    /// (never an update to an existing one -- an accepted candidate always
    /// allocates a fresh ID) using exactly the field shape [`read_document`]
    /// parses back, so the very next `ctx index` picks it up like any
    /// hand-authored document. Returns the path written, relative to the
    /// repository root.
    fn write_document(&self, document: &BusinessDocument) -> Result<String, PortError> {
        let directory = self
            .root
            .join(".context")
            .join(kind_directory(document.kind));
        let filename = format!("{}.yaml", slugify(&document.id));
        let path = directory.join(&filename);
        if path.exists() {
            return Err(port_error(BusinessContextError::AlreadyExists(
                path.display().to_string(),
            )));
        }
        fs::create_dir_all(&directory).map_err(|source| {
            port_error(BusinessContextError::Io {
                path: directory.display().to_string(),
                source,
            })
        })?;
        let yaml = serde_yaml::to_string(&written_document(document)).map_err(|error| {
            port_error(BusinessContextError::Yaml {
                path: path.display().to_string(),
                message: error.to_string(),
            })
        })?;
        fs::write(&path, yaml).map_err(|source| {
            port_error(BusinessContextError::Io {
                path: path.display().to_string(),
                source,
            })
        })?;
        Ok(path
            .strip_prefix(&self.root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/"))
    }
}

const fn kind_directory(kind: BusinessKind) -> &'static str {
    match kind {
        BusinessKind::Feature => "features",
        BusinessKind::Requirement => "requirements",
        BusinessKind::Invariant => "invariants",
        BusinessKind::Decision => "decisions",
    }
}

/// A conservative, portable filename: lowercase ASCII alphanumerics with
/// every other byte collapsed to `-`, matching how every existing
/// `.context/*.yaml` filename in this repository is already shaped.
fn slugify(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

#[derive(Serialize)]
struct WrittenLink<'a> {
    symbol: &'a str,
}

#[derive(Serialize)]
struct WrittenDocument<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    feature: Option<&'a str>,
    status: &'a str,
    visibility: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    statement: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    implementation: Vec<WrittenLink<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tests: Vec<WrittenLink<'a>>,
}

/// Maps `document`'s single normalized `body`/`title` back onto the
/// per-kind field names [`read_document`] expects (`statement` for
/// Requirement/Invariant, `name`/`description` for Feature, `title`/
/// `decision` for Decision) so writing and reading stay each other's exact
/// inverse.
fn written_document(document: &BusinessDocument) -> WrittenDocument<'_> {
    let kind = kind_directory(document.kind)
        .strip_suffix('s')
        .unwrap_or_default();
    let (name, title, statement, description, decision) = match document.kind {
        BusinessKind::Feature => (
            Some(document.title.as_str()),
            None,
            None,
            Some(document.body.as_str()),
            None,
        ),
        BusinessKind::Requirement | BusinessKind::Invariant => {
            (None, None, Some(document.body.as_str()), None, None)
        }
        BusinessKind::Decision => (
            None,
            Some(document.title.as_str()),
            None,
            None,
            Some(document.body.as_str()),
        ),
    };
    WrittenDocument {
        id: &document.id,
        kind,
        feature: document.feature.as_deref(),
        status: &document.status,
        visibility: document.visibility.as_str(),
        name,
        title,
        statement,
        description,
        decision,
        implementation: document
            .implementation
            .iter()
            .map(|link| WrittenLink {
                symbol: &link.symbol,
            })
            .collect(),
        tests: document
            .tests
            .iter()
            .map(|link| WrittenLink {
                symbol: &link.symbol,
            })
            .collect(),
    }
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

fn parse_visibility(
    path: &str,
    visibility: Option<&str>,
) -> Result<Visibility, BusinessContextError> {
    match visibility {
        None | Some("private") => Ok(Visibility::Private),
        Some("public") => Ok(Visibility::Public),
        Some(visibility) => Err(BusinessContextError::InvalidVisibility {
            path: path.to_owned(),
            visibility: visibility.to_owned(),
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

    fn document(kind: BusinessKind, id: &str, title: &str, body: &str) -> BusinessDocument {
        BusinessDocument {
            id: id.to_owned(),
            kind,
            title: title.to_owned(),
            body: body.to_owned(),
            status: "active".to_owned(),
            visibility: Visibility::Private,
            feature: Some("FEAT-INDEXING".to_owned()),
            implementation_expected: true,
            implementation: vec![ExplicitSymbolLink {
                symbol: "billing.subscription.SubscriptionService.cancel".to_owned(),
                locator: "implementation[0]".to_owned(),
            }],
            tests: Vec::new(),
            source_uri: String::new(),
            content_hash: String::new(),
        }
    }

    #[test]
    fn a_written_requirement_reads_back_with_the_same_statement_and_links() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let reader = YamlBusinessContextReader::new(directory.path().to_path_buf());
        let original = document(
            BusinessKind::Requirement,
            "REQ-SUB-014",
            "Cancellation preserves paid access.",
            "Cancellation preserves paid access.",
        );

        let path = reader.write_document(&original).expect("write document");
        assert_eq!(path, ".context/requirements/req-sub-014.yaml");

        let read_back = reader.read_all().expect("read all");
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0].id, "REQ-SUB-014");
        assert_eq!(read_back[0].kind, BusinessKind::Requirement);
        assert_eq!(read_back[0].body, original.body);
        assert_eq!(
            read_back[0].implementation[0].symbol,
            "billing.subscription.SubscriptionService.cancel"
        );
    }

    #[test]
    fn visibility_defaults_private_and_accepts_both_explicit_values() {
        for (yaml, expected) in [
            (
                "id: REQ-PRIVATE\nstatement: Keep access.\n",
                Visibility::Private,
            ),
            (
                "id: REQ-PUBLIC\nvisibility: public\nstatement: Keep access.\n",
                Visibility::Public,
            ),
            (
                "id: REQ-EXPLICIT-PRIVATE\nvisibility: private\nstatement: Keep access.\n",
                Visibility::Private,
            ),
        ] {
            let raw: RawDocument = serde_yaml::from_str(yaml).expect("YAML");
            let document = normalize_document(
                Path::new("/repo"),
                Path::new("/repo/.context/requirements/access.yaml"),
                yaml,
                raw,
            )
            .expect("document");
            assert_eq!(document.visibility, expected);
        }
    }

    #[test]
    fn invalid_visibility_is_a_clear_parse_error() {
        let yaml = "id: REQ-INVALID\nvisibility: internal\nstatement: Keep access.\n";
        let raw: RawDocument = serde_yaml::from_str(yaml).expect("YAML");
        let error = normalize_document(
            Path::new("/repo"),
            Path::new("/repo/.context/requirements/access.yaml"),
            yaml,
            raw,
        )
        .expect_err("unsupported visibility must fail");
        assert!(error.to_string().contains("invalid visibility 'internal'"));
    }

    /// A design-spike ADR with no code to point at opts out of the
    /// needs-mappings check by setting `implementation_expected: false`;
    /// every other document defaults to `true` so absence never silently
    /// exempts it (PR-MAP-003).
    #[test]
    fn implementation_expected_defaults_true_and_accepts_an_explicit_false() {
        for (yaml, expected) in [
            (
                "id: ADR-DEFAULT\ntype: decision\ntitle: T\ndecision: Keep access.\n",
                true,
            ),
            (
                "id: ADR-SPIKE\ntype: decision\ntitle: T\ndecision: Keep access.\nimplementation_expected: false\n",
                false,
            ),
            (
                "id: ADR-EXPLICIT-TRUE\ntype: decision\ntitle: T\ndecision: Keep access.\nimplementation_expected: true\n",
                true,
            ),
        ] {
            let raw: RawDocument = serde_yaml::from_str(yaml).expect("YAML");
            let document = normalize_document(
                Path::new("/repo"),
                Path::new("/repo/.context/decisions/adr.yaml"),
                yaml,
                raw,
            )
            .expect("document");
            assert_eq!(document.implementation_expected, expected);
        }
    }

    #[test]
    fn a_written_decision_reads_back_with_its_title_and_decision_text() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let reader = YamlBusinessContextReader::new(directory.path().to_path_buf());
        let original = document(
            BusinessKind::Decision,
            "ADR-SUB-002",
            "Cancellation stays reversible until period end",
            "A cancelled subscription remains reversible until paid_until.",
        );

        reader.write_document(&original).expect("write document");

        let read_back = reader.read_all().expect("read all");
        assert_eq!(read_back[0].title, original.title);
        assert_eq!(read_back[0].body, original.body);
    }

    #[test]
    fn writing_the_same_id_twice_is_rejected_not_silently_overwritten() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let reader = YamlBusinessContextReader::new(directory.path().to_path_buf());
        let original = document(
            BusinessKind::Invariant,
            "INV-SUB-003",
            "statement",
            "statement",
        );
        reader.write_document(&original).expect("first write");

        let error = reader
            .write_document(&original)
            .expect_err("a duplicate ID must not overwrite the existing file");
        assert!(error.to_string().contains("already exists"));
    }
}
