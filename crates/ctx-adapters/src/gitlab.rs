//! GitLab issue/merge-request ingestion (prompt3.md PR-EXT-001 MUST list,
//! the end-to-end provider chosen for this sprint): reads issues, merge
//! requests, and their comments through GitLab's REST API and normalizes
//! them into [`Artifact`]s, with deterministic (never AI-derived) links for
//! facts the API reports directly — a comment belongs to its issue/MR, an
//! MR's changeset includes a given commit.
//!
//! HTTP access goes through [`GitLabTransport`] so the client can be tested
//! against canned responses instead of a live GitLab instance.

use std::{fs, path::Path};

use ctx_app::ports::{
    ExternalArtifactBatch, ExternalArtifactRequest, ExternalArtifactSource, PortError,
};
use ctx_core::artifact::{
    Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkKind, ArtifactLinkTarget,
    ArtifactProvider,
};
use serde::Deserialize;
use thiserror::Error;

use crate::http_retry::{self, Attempt, RetryError, RetryPolicy, ThreadSleeper};

#[derive(Debug, Error)]
pub enum GitLabError {
    #[error("GitLab request to '{path}' failed: {message}")]
    Transport { path: String, message: String },
    #[error("GitLab response for '{path}' was not valid JSON: {source}")]
    InvalidJson {
        path: String,
        source: serde_json::Error,
    },
    #[error("invalid ctx GitLab configuration at '{path}': {message}")]
    Config { path: String, message: String },
}

/// Minimal HTTP transport boundary: the real implementation
/// ([`UreqTransport`]) makes live requests, while tests inject canned
/// per-path responses instead of reaching a real GitLab instance.
pub trait GitLabTransport {
    /// # Errors
    /// Returns [`GitLabError::Transport`] when the request fails or returns
    /// a non-success status.
    fn get(&self, path: &str) -> Result<String, GitLabError>;
}

pub struct UreqTransport {
    base_url: String,
    token: Option<String>,
    agent: ureq::Agent,
    retry_policy: RetryPolicy,
}

impl UreqTransport {
    #[must_use]
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self {
            base_url: base_url.into(),
            token,
            agent,
            retry_policy: RetryPolicy::default(),
        }
    }

    #[must_use]
    pub fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }
}

impl GitLabTransport for UreqTransport {
    fn get(&self, path: &str) -> Result<String, GitLabError> {
        let url = format!("{}{path}", self.base_url);
        http_retry::run(self.retry_policy, &ThreadSleeper, || {
            let mut request = self.agent.get(&url);
            if let Some(token) = &self.token {
                request = request.header("PRIVATE-TOKEN", token);
            }
            let mut response = request.call().map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = response
                .body_mut()
                .read_to_string()
                .map_err(|error| error.to_string())?;
            Ok(Attempt {
                status,
                retry_after,
                value: body,
            })
        })
        .map_err(|error| GitLabError::Transport {
            path: path.to_owned(),
            message: match error {
                RetryError::Request(message) => message,
                RetryError::Status { status, attempts } => {
                    format!("HTTP {status} after {attempts} attempt(s)")
                }
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLabConfig {
    pub base_url: String,
    pub project: String,
    pub token: Option<String>,
}

const DEFAULT_BASE_URL: &str = "https://gitlab.com/api/v4";

impl GitLabConfig {
    /// Reads the `[gitlab]` table from `.ctx/config.toml` (`project`
    /// required, `base_url` optional) and the access token from the
    /// `CTX_GITLAB_TOKEN` environment variable — deliberately never from a
    /// repository-committed file, so a token is never accidentally checked
    /// in.
    ///
    /// # Errors
    /// Returns [`GitLabError::Config`] when the file exists but is invalid,
    /// or `project` is missing.
    pub fn load(root: &Path) -> Result<Self, GitLabError> {
        let path = root.join(".ctx").join("config.toml");
        let content = fs::read_to_string(&path).map_err(|error| GitLabError::Config {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
        let file: RawConfigFile =
            toml::from_str(&content).map_err(|error| GitLabError::Config {
                path: path.display().to_string(),
                message: error.to_string(),
            })?;
        let gitlab = file.gitlab.ok_or_else(|| GitLabError::Config {
            path: path.display().to_string(),
            message: "missing [gitlab] section (needs at least `project`)".to_owned(),
        })?;
        Ok(Self {
            base_url: gitlab
                .base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned()),
            project: gitlab.project,
            token: std::env::var("CTX_GITLAB_TOKEN").ok(),
        })
    }
}

#[derive(Deserialize)]
struct RawConfigFile {
    gitlab: Option<RawGitLabConfig>,
}

#[derive(Deserialize)]
struct RawGitLabConfig {
    project: String,
    base_url: Option<String>,
}

pub struct GitLabClient<T> {
    transport: T,
    project: String,
}

const PAGE_SIZE: usize = 100;

impl<T: GitLabTransport> GitLabClient<T> {
    pub fn new(transport: T, project: impl Into<String>) -> Self {
        Self {
            transport,
            project: project.into(),
        }
    }

    /// Fetches every issue and merge request for the configured project, or
    /// (when `since` is given) only those GitLab itself reports as updated
    /// at or after that RFC3339 timestamp (prompt3.md PR-INCR-001, T8.1) --
    /// each with its own comments, and every merge request's associated
    /// commit SHAs. Returns the normalized artifacts and the deterministic
    /// (provider-reported, never AI-derived) links between them: a
    /// comment `comments_on` its issue/MR, an MR `contains_commit` for
    /// each of its commits.
    ///
    /// # Errors
    /// Returns [`GitLabError`] when a request fails or its response is not
    /// valid JSON.
    pub fn fetch_issue_and_mr_artifacts(
        &self,
        since: Option<&str>,
    ) -> Result<(Vec<Artifact>, Vec<ArtifactLink>), GitLabError> {
        let mut artifacts = Vec::new();
        let mut links = Vec::new();
        let updated_after = since
            .map(|cursor| format!("&updated_after={}", encode_query_value(cursor)))
            .unwrap_or_default();

        for issue in self.get_all_pages::<RawIssue>(&format!(
            "/projects/{}/issues?per_page={PAGE_SIZE}&order_by=iid&sort=asc{updated_after}",
            encoded_project(&self.project)
        ))? {
            let identity = Self::issue_identity(issue.iid);
            artifacts.push(self.issue_artifact(&identity, issue));
            let notes = self.get_all_pages::<RawNote>(&format!(
                "/projects/{}/issues/{}/notes?per_page={PAGE_SIZE}",
                encoded_project(&self.project),
                identity.external_id
            ))?;
            self.push_notes(&identity, notes, &mut artifacts, &mut links);
        }

        for merge_request in self.get_all_pages::<RawMergeRequest>(&format!(
            "/projects/{}/merge_requests?per_page={PAGE_SIZE}&order_by=iid&sort=asc{updated_after}",
            encoded_project(&self.project)
        ))? {
            let identity = Self::merge_request_identity(merge_request.iid);
            artifacts.push(self.merge_request_artifact(&identity, merge_request));
            let notes = self.get_all_pages::<RawNote>(&format!(
                "/projects/{}/merge_requests/{}/notes?per_page={PAGE_SIZE}",
                encoded_project(&self.project),
                identity.external_id
            ))?;
            self.push_notes(&identity, notes, &mut artifacts, &mut links);

            let commits = self.get_all_pages::<RawCommitRef>(&format!(
                "/projects/{}/merge_requests/{}/commits?per_page={PAGE_SIZE}",
                encoded_project(&self.project),
                identity.external_id
            ))?;
            for commit in commits {
                links.push(ArtifactLink {
                    source: identity.clone(),
                    target: ArtifactLinkTarget::Artifact(ArtifactIdentity {
                        provider: ArtifactProvider::Git,
                        kind: ArtifactKind::Commit,
                        external_id: commit.id,
                    }),
                    kind: ArtifactLinkKind::ContainsCommit,
                    evidence_locator: format!("merge_request:{}", identity.external_id),
                });
            }
        }

        Ok((artifacts, links))
    }

    fn push_notes(
        &self,
        parent: &ArtifactIdentity,
        notes: Vec<RawNote>,
        artifacts: &mut Vec<Artifact>,
        links: &mut Vec<ArtifactLink>,
    ) {
        for note in notes {
            if note.system {
                // A GitLab-generated system note ("assigned to @alice",
                // "changed the description") is not human-authored source
                // material and carries no evidence text of its own.
                continue;
            }
            let identity = ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: if parent.kind == ArtifactKind::MergeRequest {
                    ArtifactKind::ReviewComment
                } else {
                    ArtifactKind::Comment
                },
                external_id: format!("{}-note-{}", parent.external_id, note.id),
            };
            let body = note.body;
            artifacts.push(Artifact {
                title: body.lines().next().unwrap_or_default().to_owned(),
                content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
                body,
                author: note.author.map(|user| user.username),
                external_created_at: note.created_at.map(ctx_core::domain::Timestamp),
                external_updated_at: note.updated_at.map(ctx_core::domain::Timestamp),
                source_locator: ctx_core::domain::Url(format!(
                    "gitlab:note:{}",
                    identity.external_id
                )),
                project: ctx_core::domain::Project(self.project.clone()),
                identity: identity.clone(),
            });
            links.push(ArtifactLink {
                source: identity,
                target: ArtifactLinkTarget::Artifact(parent.clone()),
                kind: ArtifactLinkKind::CommentsOn,
                evidence_locator: format!("gitlab notes API: {}", parent.external_id),
            });
        }
    }

    fn issue_identity(iid: u64) -> ArtifactIdentity {
        ArtifactIdentity {
            provider: ArtifactProvider::GitLab,
            kind: ArtifactKind::Issue,
            external_id: iid.to_string(),
        }
    }

    fn merge_request_identity(iid: u64) -> ArtifactIdentity {
        ArtifactIdentity {
            provider: ArtifactProvider::GitLab,
            kind: ArtifactKind::MergeRequest,
            external_id: iid.to_string(),
        }
    }

    fn issue_artifact(&self, identity: &ArtifactIdentity, issue: RawIssue) -> Artifact {
        let body = issue.description.unwrap_or_default();
        Artifact {
            title: issue.title,
            content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
            body,
            author: issue.author.map(|user| user.username),
            external_created_at: issue.created_at.map(ctx_core::domain::Timestamp),
            external_updated_at: issue.updated_at.map(ctx_core::domain::Timestamp),
            source_locator: ctx_core::domain::Url(issue.web_url.unwrap_or_default()),
            project: ctx_core::domain::Project(self.project.clone()),
            identity: identity.clone(),
        }
    }

    fn merge_request_artifact(
        &self,
        identity: &ArtifactIdentity,
        merge_request: RawMergeRequest,
    ) -> Artifact {
        let body = merge_request.description.unwrap_or_default();
        Artifact {
            title: merge_request.title,
            content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
            body,
            author: merge_request.author.map(|user| user.username),
            external_created_at: merge_request.created_at.map(ctx_core::domain::Timestamp),
            external_updated_at: merge_request.updated_at.map(ctx_core::domain::Timestamp),
            source_locator: ctx_core::domain::Url(merge_request.web_url.unwrap_or_default()),
            project: ctx_core::domain::Project(self.project.clone()),
            identity: identity.clone(),
        }
    }

    fn get_json<D: serde::de::DeserializeOwned>(&self, path: &str) -> Result<D, GitLabError> {
        let body = self.transport.get(path)?;
        serde_json::from_str(&body).map_err(|error| GitLabError::InvalidJson {
            path: path.to_owned(),
            source: error,
        })
    }

    fn get_all_pages<D: serde::de::DeserializeOwned>(
        &self,
        base_path: &str,
    ) -> Result<Vec<D>, GitLabError> {
        let mut items = Vec::new();
        let mut page = 1usize;
        loop {
            let page_items: Vec<D> = self.get_json(&format!("{base_path}&page={page}"))?;
            let fetched = page_items.len();
            items.extend(page_items);
            if fetched < PAGE_SIZE {
                return Ok(items);
            }
            page += 1;
        }
    }
}

impl<T: GitLabTransport> ExternalArtifactSource for GitLabClient<T> {
    fn fetch(
        &self,
        request: ExternalArtifactRequest<'_>,
    ) -> Result<ExternalArtifactBatch, PortError> {
        let ExternalArtifactRequest::UpdatedSince(since) = request else {
            return Err(PortError::new(
                "GitLab accepts only the updated-since artifact request mode",
            ));
        };
        let (artifacts, links) = self
            .fetch_issue_and_mr_artifacts(since)
            .map_err(|error| PortError::new(error.to_string()))?;
        Ok(ExternalArtifactBatch { artifacts, links })
    }
}

fn encoded_project(project: &str) -> String {
    // GitLab's REST API accepts a namespaced path as the `:id` segment only
    // when it is percent-encoded; a purely numeric project ID passes
    // through unchanged since `/` is the only character this replaces.
    project.replace('/', "%2F")
}

/// Percent-encodes the characters an RFC3339 timestamp can actually contain
/// that are unsafe in a raw query string (`:` and, for a numeric UTC offset
/// rather than `Z`, `+`) -- not a general query-string encoder, since this
/// is the only kind of value this module ever puts in one.
fn encode_query_value(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            ':' => "%3A".to_owned(),
            '+' => "%2B".to_owned(),
            other => other.to_string(),
        })
        .collect()
}

#[derive(Deserialize)]
struct RawUser {
    username: String,
}

#[derive(Deserialize)]
struct RawIssue {
    iid: u64,
    title: String,
    #[serde(default)]
    description: Option<String>,
    author: Option<RawUser>,
    created_at: Option<String>,
    updated_at: Option<String>,
    web_url: Option<String>,
}

#[derive(Deserialize)]
struct RawMergeRequest {
    iid: u64,
    title: String,
    #[serde(default)]
    description: Option<String>,
    author: Option<RawUser>,
    created_at: Option<String>,
    updated_at: Option<String>,
    web_url: Option<String>,
}

#[derive(Deserialize)]
struct RawNote {
    id: u64,
    body: String,
    author: Option<RawUser>,
    created_at: Option<String>,
    updated_at: Option<String>,
    #[serde(default)]
    system: bool,
}

#[derive(Deserialize)]
struct RawCommitRef {
    id: String,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::*;

    struct FakeTransport {
        responses: BTreeMap<String, String>,
    }

    impl GitLabTransport for FakeTransport {
        fn get(&self, path: &str) -> Result<String, GitLabError> {
            self.responses
                .get(path)
                .cloned()
                .ok_or_else(|| GitLabError::Transport {
                    path: path.to_owned(),
                    message: "no fixture response for this path".to_owned(),
                })
        }
    }

    fn insert_pages(responses: &mut BTreeMap<String, String>, base_path: &str, items: &[Value]) {
        for (index, page) in items.chunks(PAGE_SIZE).enumerate() {
            responses.insert(
                format!("{base_path}&page={}", index + 1),
                Value::Array(page.to_vec()).to_string(),
            );
        }
    }

    #[test]
    fn every_gitlab_collection_reads_more_than_one_page() {
        let issues: Vec<_> = (1..=101)
            .map(|iid| json!({"iid": iid, "title": format!("issue {iid}")}))
            .collect();
        let merge_requests: Vec<_> = (1..=101)
            .map(|iid| json!({"iid": iid, "title": format!("MR {iid}")}))
            .collect();
        let notes: Vec<_> = (1..=101)
            .map(|id| json!({"id": id, "body": format!("note {id}"), "system": false}))
            .collect();
        let commits: Vec<_> = (1..=101)
            .map(|id| json!({"id": format!("commit-{id}")}))
            .collect();
        let mut responses = BTreeMap::new();
        insert_pages(
            &mut responses,
            "/projects/example/issues?per_page=100",
            &issues,
        );
        insert_pages(
            &mut responses,
            "/projects/example/merge_requests?per_page=100",
            &merge_requests,
        );
        insert_pages(
            &mut responses,
            "/projects/example/issues/1/notes?per_page=100",
            &notes,
        );
        insert_pages(
            &mut responses,
            "/projects/example/merge_requests/1/commits?per_page=100",
            &commits,
        );
        let client = GitLabClient::new(FakeTransport { responses }, "example");

        assert_eq!(
            client
                .get_all_pages::<RawIssue>("/projects/example/issues?per_page=100")
                .expect("issues")
                .len(),
            101
        );
        assert_eq!(
            client
                .get_all_pages::<RawMergeRequest>("/projects/example/merge_requests?per_page=100",)
                .expect("merge requests")
                .len(),
            101
        );
        assert_eq!(
            client
                .get_all_pages::<RawNote>("/projects/example/issues/1/notes?per_page=100")
                .expect("notes")
                .len(),
            101
        );
        assert_eq!(
            client
                .get_all_pages::<RawCommitRef>(
                    "/projects/example/merge_requests/1/commits?per_page=100",
                )
                .expect("commits")
                .len(),
            101
        );
    }

    #[test]
    fn fetches_issues_and_merge_requests_with_their_comments_and_commits() {
        let mut responses = BTreeMap::new();
        responses.insert(
            "/projects/billing%2Fsubscriptions/issues?per_page=100&order_by=iid&sort=asc&page=1"
                .to_owned(),
            r#"[{"iid":317,"title":"Cancellation removes prepaid access","description":"A cancelled prepaid subscription must remain usable until paid_until.","author":{"username":"alice"},"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-01T00:00:00Z","web_url":"https://gitlab.example/billing/subscriptions/-/issues/317"}]"#
                .to_owned(),
        );
        responses.insert(
            "/projects/billing%2Fsubscriptions/issues/317/notes?per_page=100&page=1".to_owned(),
            r#"[{"id":1,"body":"Do not revoke an already paid entitlement immediately.","author":{"username":"bob"},"created_at":"2026-08-01T01:00:00Z","updated_at":"2026-08-01T01:00:00Z","system":false},{"id":2,"body":"assigned to @alice","author":{"username":"bot"},"created_at":"2026-08-01T02:00:00Z","updated_at":"2026-08-01T02:00:00Z","system":true}]"#
                .to_owned(),
        );
        responses.insert(
            "/projects/billing%2Fsubscriptions/merge_requests?per_page=100&order_by=iid&sort=asc&page=1"
                .to_owned(),
            r#"[{"iid":842,"title":"Fix cancellation semantics","description":"Fixes #317.","author":{"username":"alice"},"created_at":"2026-08-02T00:00:00Z","updated_at":"2026-08-02T00:00:00Z","web_url":"https://gitlab.example/billing/subscriptions/-/merge_requests/842"}]"#
                .to_owned(),
        );
        responses.insert(
            "/projects/billing%2Fsubscriptions/merge_requests/842/notes?per_page=100&page=1"
                .to_owned(),
            "[]".to_owned(),
        );
        responses.insert(
            "/projects/billing%2Fsubscriptions/merge_requests/842/commits?per_page=100&page=1"
                .to_owned(),
            r#"[{"id":"abc123def456"}]"#.to_owned(),
        );
        let client = GitLabClient::new(FakeTransport { responses }, "billing/subscriptions");

        let (artifacts, links) = client
            .fetch_issue_and_mr_artifacts(None)
            .expect("issues and merge requests");

        assert_eq!(artifacts.len(), 3); // issue + 1 human comment + MR
        let issue = artifacts
            .iter()
            .find(|artifact| artifact.identity.kind == ArtifactKind::Issue)
            .expect("issue artifact");
        assert_eq!(issue.title, "Cancellation removes prepaid access");
        assert!(
            artifacts
                .iter()
                .all(|artifact| !artifact.body.contains("assigned to"))
        );

        let comments_on = links
            .iter()
            .find(|link| link.kind == ArtifactLinkKind::CommentsOn)
            .expect("comment link");
        assert_eq!(
            comments_on.target,
            ArtifactLinkTarget::Artifact(ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: ArtifactKind::Issue,
                external_id: "317".to_owned(),
            })
        );

        let contains_commit = links
            .iter()
            .find(|link| link.kind == ArtifactLinkKind::ContainsCommit)
            .expect("commit link");
        assert_eq!(
            contains_commit.target,
            ArtifactLinkTarget::Artifact(ArtifactIdentity {
                provider: ArtifactProvider::Git,
                kind: ArtifactKind::Commit,
                external_id: "abc123def456".to_owned(),
            })
        );
    }

    #[test]
    fn a_sync_cursor_becomes_an_updated_after_query_parameter() {
        let mut responses = BTreeMap::new();
        responses.insert(
            "/projects/billing%2Fsubscriptions/issues?per_page=100&order_by=iid&sort=asc&updated_after=2026-08-21T00%3A00%3A00Z&page=1"
                .to_owned(),
            "[]".to_owned(),
        );
        responses.insert(
            "/projects/billing%2Fsubscriptions/merge_requests?per_page=100&order_by=iid&sort=asc&updated_after=2026-08-21T00%3A00%3A00Z&page=1"
                .to_owned(),
            "[]".to_owned(),
        );
        let client = GitLabClient::new(FakeTransport { responses }, "billing/subscriptions");

        let (artifacts, links) = client
            .fetch_issue_and_mr_artifacts(Some("2026-08-21T00:00:00Z"))
            .expect("incremental fetch");

        assert!(artifacts.is_empty());
        assert!(links.is_empty());
    }
}
