use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use ctx_core::{
    business::{BusinessDocument, BusinessKind, Visibility},
    graph::GraphEvidence,
    ir::{ApiEndpoint, ApiParam, HttpMethod, OpenApiOperation},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::git::{GitError, GitRepo};

pub const FEDERATION_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Error)]
pub enum FederationError {
    #[error(
        "federation requires `[service].name` in .ctx/config.toml; nothing can be exported without a service identity"
    )]
    MissingServiceName,
    #[error("neighbor path '{0}' does not exist or is not a directory")]
    MissingNeighbor(String),
    #[error("neighbor path '{path}' is not a Git repository root (discovered '{discovered}')")]
    NotRepositoryRoot { path: String, discovered: String },
    #[error("neighbor '{name}' is already registered at '{existing}', not '{requested}'")]
    DuplicateName {
        name: String,
        existing: String,
        requested: String,
    },
    #[error(
        "neighbor name '{provided}' does not match its configured `[service].name` '{configured}'"
    )]
    ServiceNameMismatch {
        provided: String,
        configured: String,
    },
    #[error("neighbor '{0}' is not registered")]
    UnknownNeighbor(String),
    #[error("could not read federation file '{path}': {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("could not write federation file '{path}': {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid federation TOML in '{path}': {message}")]
    Toml { path: String, message: String },
    #[error("invalid federation JSON in '{path}': {message}")]
    Json { path: String, message: String },
    #[error(transparent)]
    Git(#[from] GitError),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistryNeighbor {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NeighborRegistry {
    #[serde(default, rename = "neighbor")]
    pub neighbors: Vec<RegistryNeighbor>,
}

impl NeighborRegistry {
    /// Reads the repository-local neighbor registry in deterministic order.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry cannot be read or parsed.
    pub fn load(root: &Path) -> Result<Self, FederationError> {
        let path = registry_path(root);
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = read_to_string(&path)?;
        let mut registry =
            toml::from_str::<Self>(&content).map_err(|error| FederationError::Toml {
                path: path.display().to_string(),
                message: error.to_string(),
            })?;
        registry.sort();
        Ok(registry)
    }

    /// Adds a checked-out neighboring repository, resolving its service name.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is missing, is not a repository root,
    /// has no usable service identity, conflicts with an existing entry, or
    /// the registry cannot be persisted.
    pub fn add(
        root: &Path,
        requested_path: &Path,
        provided_name: Option<&str>,
    ) -> Result<(Self, RegistryNeighbor, bool), FederationError> {
        let canonical = canonical_neighbor(root, requested_path)?;
        let repository = GitRepo::discover(&canonical)?;
        let discovered =
            fs::canonicalize(repository.root()).map_err(|source| FederationError::Read {
                path: repository.root().display().to_string(),
                source,
            })?;
        if discovered != canonical {
            return Err(FederationError::NotRepositoryRoot {
                path: canonical.display().to_string(),
                discovered: discovered.display().to_string(),
            });
        }
        let configured_name = repository.service_name();
        if let (Some(provided), Some(configured)) = (provided_name, configured_name)
            && provided != configured
        {
            return Err(FederationError::ServiceNameMismatch {
                provided: provided.to_owned(),
                configured: configured.to_owned(),
            });
        }
        let name = provided_name
            .or(configured_name)
            .ok_or(FederationError::MissingServiceName)?;
        if name.trim().is_empty() {
            return Err(FederationError::MissingServiceName);
        }
        let path = canonical.display().to_string();
        let entry = RegistryNeighbor {
            name: name.to_owned(),
            path,
        };
        let mut registry = Self::load(root)?;
        if let Some(existing) = registry
            .neighbors
            .iter()
            .find(|item| item.path == entry.path)
        {
            return Ok((registry.clone(), existing.clone(), false));
        }
        if let Some(existing) = registry
            .neighbors
            .iter()
            .find(|item| item.name == entry.name)
        {
            return Err(FederationError::DuplicateName {
                name: entry.name,
                existing: existing.path.clone(),
                requested: entry.path,
            });
        }
        registry.neighbors.push(entry.clone());
        registry.sort();
        registry.save(root)?;
        Ok((registry, entry, true))
    }

    /// Removes a registered neighbor by service name.
    ///
    /// # Errors
    ///
    /// Returns an error when the name is unknown or the registry cannot be
    /// read or persisted.
    pub fn remove(root: &Path, name: &str) -> Result<RegistryNeighbor, FederationError> {
        let mut registry = Self::load(root)?;
        let index = registry
            .neighbors
            .iter()
            .position(|neighbor| neighbor.name == name)
            .ok_or_else(|| FederationError::UnknownNeighbor(name.to_owned()))?;
        let removed = registry.neighbors.remove(index);
        registry.save(root)?;
        Ok(removed)
    }

    fn sort(&mut self) {
        self.neighbors
            .sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
    }

    fn save(&self, root: &Path) -> Result<(), FederationError> {
        let path = registry_path(root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| FederationError::Write {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let serialized = toml::to_string_pretty(self).map_err(|error| FederationError::Toml {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
        fs::write(&path, serialized).map_err(|source| FederationError::Write {
            path: path.display().to_string(),
            source,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportedDocument {
    pub id: String,
    pub kind: BusinessKind,
    pub title: String,
    pub body: String,
    pub status: String,
    pub visibility: Visibility,
    pub source_uri: String,
    pub content_hash: String,
}

impl From<&BusinessDocument> for ExportedDocument {
    fn from(document: &BusinessDocument) -> Self {
        Self {
            id: document.id.clone(),
            kind: document.kind,
            title: document.title.clone(),
            body: document.body.clone(),
            status: document.status.clone(),
            visibility: document.visibility,
            source_uri: document.source_uri.clone(),
            content_hash: document.content_hash.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportedEvidence {
    pub source_uri: String,
    pub locator: String,
    pub commit: Option<String>,
}

impl From<&GraphEvidence> for ExportedEvidence {
    fn from(evidence: &GraphEvidence) -> Self {
        Self {
            source_uri: evidence.source_uri.clone(),
            locator: evidence.locator.clone(),
            commit: evidence.commit.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportedEndpoint {
    pub handler: String,
    pub method: HttpMethod,
    pub path: String,
    pub params: Vec<ApiParam>,
    pub return_type: Option<String>,
    pub framework: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openapi: Option<OpenApiOperation>,
    pub evidence: Vec<ExportedEvidence>,
}

impl ExportedEndpoint {
    pub fn from_contract(
        handler: String,
        endpoint: &ApiEndpoint,
        evidence: &[GraphEvidence],
    ) -> Self {
        let mut evidence = evidence
            .iter()
            .map(ExportedEvidence::from)
            .collect::<Vec<_>>();
        evidence.sort_by(|left, right| {
            left.source_uri
                .cmp(&right.source_uri)
                .then(left.locator.cmp(&right.locator))
                .then(left.commit.cmp(&right.commit))
        });
        Self {
            handler,
            method: endpoint.method,
            path: endpoint.path.clone(),
            params: endpoint.params.clone(),
            return_type: endpoint.return_type.clone(),
            framework: endpoint.framework.clone(),
            openapi: endpoint.openapi.clone(),
            evidence,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportManifest {
    pub service_name: String,
    pub source_commit: String,
    pub schema_version: u32,
    pub documents: Vec<ExportedDocument>,
    pub endpoints: Vec<ExportedEndpoint>,
}

impl ExportManifest {
    pub fn new(
        service_name: String,
        source_commit: String,
        mut documents: Vec<ExportedDocument>,
        mut endpoints: Vec<ExportedEndpoint>,
    ) -> Self {
        documents.sort_by(|left, right| left.id.cmp(&right.id));
        endpoints.sort_by(|left, right| {
            left.method
                .cmp(&right.method)
                .then(left.path.cmp(&right.path))
                .then(left.handler.cmp(&right.handler))
        });
        Self {
            service_name,
            source_commit,
            schema_version: FEDERATION_SCHEMA_VERSION,
            documents,
            endpoints,
        }
    }

    /// Reads and decodes one exported federation manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or is not valid manifest
    /// JSON.
    pub fn read(path: &Path) -> Result<Self, FederationError> {
        let content = read_to_string(path)?;
        serde_json::from_str(&content).map_err(|error| FederationError::Json {
            path: path.display().to_string(),
            message: error.to_string(),
        })
    }

    /// Writes the manifest as deterministic, pretty JSON with one final newline.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be serialized or its target
    /// directory/file cannot be written.
    pub fn write(&self, path: &Path) -> Result<(), FederationError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| FederationError::Write {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let mut serialized =
            serde_json::to_string_pretty(self).map_err(|error| FederationError::Json {
                path: path.display().to_string(),
                message: error.to_string(),
            })?;
        serialized.push('\n');
        fs::write(path, serialized).map_err(|source| FederationError::Write {
            path: path.display().to_string(),
            source,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalCallContract {
    pub stable_key: String,
    pub handler: String,
    pub method: HttpMethod,
    pub url: String,
    pub path_template: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FederatedResolution {
    pub source_repo: String,
    pub source_commit: String,
    pub local_commit: String,
    pub synced_at: String,
    pub status: String,
    pub call: ExternalCallContract,
    pub endpoint: ExportedEndpoint,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FederationSyncState {
    pub source_repo: String,
    pub source_path: String,
    pub source_commit: String,
    pub synced_at: String,
    pub schema_version: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FederatedRepositoryData {
    pub state: Option<FederationSyncState>,
    pub documents: Vec<ExportedDocument>,
    pub endpoints: Vec<ExportedEndpoint>,
    pub resolutions: Vec<FederatedResolution>,
}

/// Returns the configured service identity required by federation commands.
///
/// # Errors
///
/// Returns [`FederationError::MissingServiceName`] when `[service].name` is
/// absent.
pub fn require_service_name(repository: &GitRepo) -> Result<&str, FederationError> {
    repository
        .service_name()
        .ok_or(FederationError::MissingServiceName)
}

pub fn registry_path(root: &Path) -> PathBuf {
    root.join(".ctx/registry.toml")
}

pub fn default_export_path(root: &Path) -> PathBuf {
    root.join(".ctx/export.json")
}

pub fn neighbor_head(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn path_template(url_or_path: &str) -> Option<String> {
    let path = if let Some(rest) = url_or_path
        .strip_prefix("https://")
        .or_else(|| url_or_path.strip_prefix("http://"))
    {
        let slash = rest.find('/');
        slash.map_or("/", |index| &rest[index..])
    } else if url_or_path.starts_with('/') {
        url_or_path
    } else {
        return None;
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let normalized = path
        .split('/')
        .map(|segment| {
            if segment.starts_with('{') && segment.ends_with('}') && segment.len() > 2 {
                "{param}"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    Some(if normalized.is_empty() {
        "/".to_owned()
    } else {
        normalized
    })
}

pub fn matching_resolutions(
    source_repo: &str,
    source_commit: &str,
    local_commit: &str,
    synced_at: &str,
    calls: &[ExternalCallContract],
    endpoints: &[ExportedEndpoint],
) -> Vec<FederatedResolution> {
    let mut matches = Vec::new();
    for call in calls {
        for endpoint in endpoints {
            if call.method == endpoint.method
                && path_template(&endpoint.path).as_deref() == Some(call.path_template.as_str())
            {
                matches.push(FederatedResolution {
                    source_repo: source_repo.to_owned(),
                    source_commit: source_commit.to_owned(),
                    local_commit: local_commit.to_owned(),
                    synced_at: synced_at.to_owned(),
                    status: "FEDERATED_MATCH".to_owned(),
                    call: call.clone(),
                    endpoint: endpoint.clone(),
                });
            }
        }
    }
    matches.sort_by(|left, right| {
        left.call
            .stable_key
            .cmp(&right.call.stable_key)
            .then(left.endpoint.method.cmp(&right.endpoint.method))
            .then(left.endpoint.path.cmp(&right.endpoint.path))
            .then(left.endpoint.handler.cmp(&right.endpoint.handler))
    });
    matches
}

fn canonical_neighbor(root: &Path, requested: &Path) -> Result<PathBuf, FederationError> {
    let path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    if !path.is_dir() {
        return Err(FederationError::MissingNeighbor(
            requested.display().to_string(),
        ));
    }
    fs::canonicalize(&path).map_err(|source| FederationError::Read {
        path: path.display().to_string(),
        source,
    })
}

fn read_to_string(path: &Path) -> Result<String, FederationError> {
    fs::read_to_string(path).map_err(|source| FederationError::Read {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_templates_ignore_placeholder_names_and_query_strings() {
        assert_eq!(
            path_template("https://billing.internal/subscriptions/{param}?expand=true"),
            Some("/subscriptions/{param}".to_owned())
        );
        assert_eq!(
            path_template("/subscriptions/{subscription_id}"),
            Some("/subscriptions/{param}".to_owned())
        );
        assert_eq!(path_template("dynamic_url"), None);
    }

    #[test]
    fn manifests_are_sorted_and_end_with_one_newline() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("export.json");
        let manifest = ExportManifest::new(
            "billing".to_owned(),
            "abcd".to_owned(),
            vec![
                ExportedDocument {
                    id: "REQ-Z".to_owned(),
                    kind: BusinessKind::Requirement,
                    title: "Z".to_owned(),
                    body: "z".to_owned(),
                    status: "active".to_owned(),
                    visibility: Visibility::Public,
                    source_uri: ".context/z.yaml".to_owned(),
                    content_hash: "z".to_owned(),
                },
                ExportedDocument {
                    id: "REQ-A".to_owned(),
                    kind: BusinessKind::Requirement,
                    title: "A".to_owned(),
                    body: "a".to_owned(),
                    status: "active".to_owned(),
                    visibility: Visibility::Public,
                    source_uri: ".context/a.yaml".to_owned(),
                    content_hash: "a".to_owned(),
                },
            ],
            Vec::new(),
        );

        manifest.write(&path).expect("write manifest");
        let bytes = fs::read(&path).expect("manifest bytes");
        assert!(bytes.ends_with(b"\n"));
        assert!(!bytes.ends_with(b"\n\n"));
        let loaded = ExportManifest::read(&path).expect("read manifest");
        assert_eq!(loaded.documents[0].id, "REQ-A");
    }
}
