//! Jira Cloud issue ingestion (mirrors `crate::gitlab`'s architecture and
//! rationale -- see `ADR-EXT-003`/its Jira counterpart, `ADR-EXT-005`):
//! reads issues and their comments through Jira's REST API v3 and
//! normalizes them into [`Artifact`]s.
//!
//! Unlike GitLab (one project per repository, so "fetch the whole project"
//! is the right scope), a single Jira project routinely spans many
//! unrelated services/repositories. Fetching the whole project into every
//! one of them would mean mostly-irrelevant noise and needless API load.
//! Instead, this module fetches only two things: (1) issues whose key is
//! actually mentioned somewhere in artifacts this repository already knows
//! about (commits, branches, GitLab issues/MRs, prior Jira issues), and (2)
//! one hop further out, whatever Jira's own `issuelinks`/`parent` fields
//! report as directly related to one of those -- never recursing past that
//! single hop, so one mention can never transitively drag in an unbounded
//! slice of the project.
//!
//! An issue's [`ArtifactIdentity::external_id`] is its human-readable key
//! (`"PSI-1122"`), never Jira's internal numeric id. This is deliberate,
//! not cosmetic: `ctx_core::linking::ReferenceKind::TicketKey` already
//! recognizes exactly this `PROJECT-123` shape in commit messages, branch
//! names, and MR bodies and links it to an `ArtifactKind::Issue` by
//! `external_id` equality alone -- and it is also how this module decides
//! which keys are even candidates to fetch in the first place.
//!
//! HTTP access goes through [`JiraTransport`] so the client can be tested
//! against canned responses instead of a live Jira instance -- only Jira
//! Cloud is supported (Basic auth via an account email + API token, REST
//! API v3); Jira Server/Data Center is out of scope.

use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::Path,
};

use base64::Engine as _;
use ctx_app::ports::{
    ExternalArtifactBatch, ExternalArtifactRequest, ExternalArtifactSource, PortError,
};
use ctx_core::artifact::{
    Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkKind, ArtifactLinkTarget,
    ArtifactProvider,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::http_retry::{self, Attempt, RetryError, RetryPolicy, ThreadSleeper};

#[derive(Debug, Error)]
pub enum JiraError {
    #[error("Jira request to '{path}' failed: {message}")]
    Transport { path: String, message: String },
    #[error("Jira request to '{path}' returned HTTP {status}: {message}")]
    Http {
        path: String,
        status: u16,
        message: String,
    },
    #[error("Jira response for '{path}' was not valid JSON: {source}")]
    InvalidJson {
        path: String,
        source: serde_json::Error,
    },
    #[error("invalid ctx Jira configuration at '{path}': {message}")]
    Config { path: String, message: String },
}

/// Minimal HTTP transport boundary: the real implementation
/// ([`UreqTransport`]) makes live requests, while tests inject canned
/// per-path responses instead of reaching a live Jira instance.
pub trait JiraTransport {
    /// # Errors
    /// Returns [`JiraError::Transport`] when the request fails and
    /// [`JiraError::Http`] for a non-success status.
    fn get(&self, path: &str) -> Result<String, JiraError>;

    /// # Errors
    /// Returns [`JiraError::Transport`] when the request fails and
    /// [`JiraError::Http`] for a non-success status.
    fn post(&self, path: &str, body: &str) -> Result<String, JiraError>;
}

pub struct UreqTransport {
    base_url: String,
    authorization: String,
    agent: ureq::Agent,
    retry_policy: RetryPolicy,
}

impl UreqTransport {
    #[must_use]
    pub fn new(base_url: impl Into<String>, email: &str, token: &str) -> Self {
        let credentials =
            base64::engine::general_purpose::STANDARD.encode(format!("{email}:{token}"));
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self {
            base_url: base_url.into(),
            authorization: format!("Basic {credentials}"),
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

impl JiraTransport for UreqTransport {
    fn get(&self, path: &str) -> Result<String, JiraError> {
        let url = format!("{}{path}", self.base_url);
        tracing::debug!(method = "GET", url, "Jira request started");
        let result = http_retry::run(self.retry_policy, &ThreadSleeper, || {
            let mut response = self
                .agent
                .get(&url)
                .header("Authorization", &self.authorization)
                .header("Accept", "application/json")
                .call()
                .map_err(|error| error.to_string())?;
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
        .map_err(|error| match error {
            RetryError::Request(message) => JiraError::Transport {
                path: path.to_owned(),
                message,
            },
            RetryError::Status {
                status,
                attempts,
                value,
            } => JiraError::Http {
                path: path.to_owned(),
                status,
                message: http_error_message(&value, attempts),
            },
        });
        if let Ok(body) = &result {
            tracing::trace!(
                method = "GET",
                url,
                response = body,
                "Jira response received"
            );
        }
        result
    }

    fn post(&self, path: &str, body: &str) -> Result<String, JiraError> {
        let url = format!("{}{path}", self.base_url);
        tracing::debug!(method = "POST", url, "Jira request started");
        tracing::trace!(method = "POST", url, request = body, "Jira request body");
        let result = http_retry::run(self.retry_policy, &ThreadSleeper, || {
            let mut response = self
                .agent
                .post(&url)
                .header("Authorization", &self.authorization)
                .header("Accept", "application/json")
                .content_type("application/json")
                .send(body)
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let response_body = response
                .body_mut()
                .read_to_string()
                .map_err(|error| error.to_string())?;
            Ok(Attempt {
                status,
                retry_after,
                value: response_body,
            })
        })
        .map_err(|error| match error {
            RetryError::Request(message) => JiraError::Transport {
                path: path.to_owned(),
                message,
            },
            RetryError::Status {
                status,
                attempts,
                value,
            } => JiraError::Http {
                path: path.to_owned(),
                status,
                message: http_error_message(&value, attempts),
            },
        });
        if let Ok(response) = &result {
            tracing::trace!(method = "POST", url, response, "Jira response received");
        }
        result
    }
}

fn http_error_message(body: &str, attempts: usize) -> String {
    let body = body.trim();
    if body.is_empty() {
        format!("request failed after {attempts} attempt(s)")
    } else {
        format!("{body} (after {attempts} attempt(s))")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JiraConfig {
    pub base_url: String,
    pub project: String,
    pub email: String,
    pub token: String,
}

impl JiraConfig {
    /// Reads the `[jira]` table from `.ctx/config.toml` (`base_url` and the
    /// backward-compatible informational `project` value are both required)
    /// and the account email/API token from the
    /// `CTX_JIRA_EMAIL`/`CTX_JIRA_TOKEN` environment variables --
    /// deliberately never from a repository-committed file, so a token is
    /// never accidentally checked in.
    ///
    /// # Errors
    /// Returns [`JiraError::Config`] when the file is missing or invalid,
    /// `[jira]` is absent, `base_url`/`project` is missing, or either
    /// credential environment variable is unset.
    pub fn load(root: &Path) -> Result<Self, JiraError> {
        let path = root.join(".ctx").join("config.toml");
        let content = fs::read_to_string(&path).map_err(|error| JiraError::Config {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
        let file: RawConfigFile = toml::from_str(&content).map_err(|error| JiraError::Config {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
        let jira = file.jira.ok_or_else(|| JiraError::Config {
            path: path.display().to_string(),
            message: "missing [jira] section (needs `base_url` and `project`)".to_owned(),
        })?;
        Self::resolve(
            path.display().to_string(),
            jira,
            std::env::var("CTX_JIRA_EMAIL").ok(),
            std::env::var("CTX_JIRA_TOKEN").ok(),
        )
    }

    /// Combines a parsed `[jira]` table with credentials read from the
    /// environment. Split out from [`Self::load`] so credential-resolution
    /// errors are unit-testable without mutating process-global env vars
    /// (which `std::env::set_var`/`remove_var` require `unsafe` for, since
    /// Rust 2024, and which this workspace forbids -- `unsafe_code =
    /// "forbid"` -- and which would be racy across parallel tests anyway).
    fn resolve(
        config_path: String,
        jira: RawJiraConfig,
        email: Option<String>,
        token: Option<String>,
    ) -> Result<Self, JiraError> {
        let email = email.ok_or_else(|| JiraError::Config {
            path: config_path.clone(),
            message: "CTX_JIRA_EMAIL is not set".to_owned(),
        })?;
        let token = token.ok_or_else(|| JiraError::Config {
            path: config_path,
            message: "CTX_JIRA_TOKEN is not set".to_owned(),
        })?;
        Ok(Self {
            base_url: jira.base_url,
            project: jira.project,
            email,
            token,
        })
    }
}

#[derive(Deserialize)]
struct RawConfigFile {
    jira: Option<RawJiraConfig>,
}

#[derive(Deserialize)]
struct RawJiraConfig {
    base_url: String,
    project: String,
}

/// How many issue keys go into one `key in (...)` JQL request. Keeps the
/// query string (and URL) bounded regardless of how many tickets a
/// repository's history happens to mention. Also means a single
/// `/rest/api/3/search/jql` page (`maxResults` 100) always holds every
/// match, since `key in (...)` can never return more issues than keys
/// named -- so [`MAX_SEARCH_PAGES`]'s loop below is a defensive backstop,
/// not a path this batch size is expected to exercise.
const KEY_BATCH_SIZE: usize = 50;

/// Hard cap on pages read per `key in (...)` batch. Atlassian Community
/// has reported the enhanced JQL search endpoint occasionally reissuing a
/// fresh `nextPageToken` without ever terminating pagination (the
/// documented `isLast` response flag has been reported stuck at `false`
/// through the same failure, so it isn't a trustworthy alternative signal
/// either); this cap turns that into a loud, bounded error instead of
/// `ctx ingest jira` hanging indefinitely.
const MAX_SEARCH_PAGES: usize = 1000;

pub struct JiraClient<T> {
    transport: T,
    base_url: String,
}

impl<T: JiraTransport> JiraClient<T> {
    pub fn new(
        transport: T,
        legacy_project: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let _ = legacy_project.into();
        Self {
            transport,
            base_url: base_url.into(),
        }
    }

    /// Fetches exactly the issues in `candidate_keys`, regardless of Jira
    /// project, plus one hop of Jira-reported related issues. Inaccessible or
    /// nonexistent keys are returned in [`ExternalArtifactBatch::unavailable_keys`].
    ///
    /// # Errors
    /// Returns [`JiraError`] when a request fails or its response is not
    /// valid JSON.
    pub fn fetch_issue_artifacts_for_keys(
        &self,
        candidate_keys: &BTreeSet<String>,
    ) -> Result<ExternalArtifactBatch, JiraError> {
        self.fetch_issue_artifacts_for_keys_with_depth(candidate_keys, 1)
    }

    fn fetch_issue_artifacts_for_keys_with_depth(
        &self,
        candidate_keys: &BTreeSet<String>,
        related_depth: usize,
    ) -> Result<ExternalArtifactBatch, JiraError> {
        self.fetch_issue_artifacts_for_keys_with_depth_and_known(
            candidate_keys,
            related_depth,
            &HashSet::new(),
            &HashSet::new(),
        )
    }

    fn fetch_issue_artifacts_for_keys_with_depth_and_known(
        &self,
        candidate_keys: &BTreeSet<String>,
        related_depth: usize,
        known_artifacts: &HashSet<ArtifactIdentity>,
        unavailable_artifacts: &HashSet<ArtifactIdentity>,
    ) -> Result<ExternalArtifactBatch, JiraError> {
        let known_issue_keys: BTreeSet<_> = known_artifacts
            .iter()
            .filter(|identity| {
                identity.provider == ArtifactProvider::Jira && identity.kind == ArtifactKind::Issue
            })
            .map(|identity| identity.external_id.clone())
            .collect();
        let unavailable_issue_keys: BTreeSet<_> = unavailable_artifacts
            .iter()
            .filter(|identity| {
                identity.provider == ArtifactProvider::Jira && identity.kind == ArtifactKind::Issue
            })
            .map(|identity| identity.external_id.clone())
            .collect();
        let mut frontier_keys: BTreeSet<_> = candidate_keys
            .difference(&known_issue_keys)
            .filter(|key| !unavailable_issue_keys.contains(*key))
            .cloned()
            .collect();
        if frontier_keys.is_empty() {
            return Ok(ExternalArtifactBatch::default());
        }

        let mut artifacts = Vec::new();
        let mut links = Vec::new();
        let mut unavailable_keys = BTreeSet::new();
        let mut visited_keys = candidate_keys
            .union(&known_issue_keys)
            .cloned()
            .collect::<BTreeSet<_>>();
        visited_keys.extend(unavailable_issue_keys);
        let mut pending_related = Vec::new();
        for depth in 0..=related_depth {
            let resolution = self.fetch_issues_by_keys(&frontier_keys)?;
            unavailable_keys.extend(resolution.unavailable_keys);
            let mut next_frontier = BTreeSet::new();
            for issue in resolution.issues {
                let issue_key = issue.key.clone();
                let related_keys = if depth < related_depth {
                    linked_keys(&issue)
                } else {
                    Vec::new()
                };
                let Some(identity) = self.ingest_one_issue(issue, &mut artifacts, &mut links)?
                else {
                    unavailable_keys.insert(issue_key);
                    continue;
                };
                for related_key in related_keys {
                    pending_related.push((identity.clone(), related_key.clone()));
                    if visited_keys.insert(related_key.clone()) {
                        next_frontier.insert(related_key);
                    }
                }
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier_keys = next_frontier;
        }

        let mut available_issue_keys: BTreeSet<_> = artifacts
            .iter()
            .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
            .map(|artifact| artifact.identity.external_id.clone())
            .collect();
        available_issue_keys.extend(known_issue_keys);
        for (source, target_key) in pending_related {
            if available_issue_keys.contains(&target_key) {
                links.push(ArtifactLink {
                    evidence_locator: format!("jira issuelinks/parent: {}", source.external_id),
                    source,
                    target: ArtifactLinkTarget::Artifact(Self::issue_identity(&target_key)),
                    kind: ArtifactLinkKind::RelatedIssue,
                });
            }
        }

        Ok(ExternalArtifactBatch {
            artifacts,
            links,
            unavailable_keys,
        })
    }

    /// Fetches every issue named in `keys`, batched into
    /// [`KEY_BATCH_SIZE`]-sized `key in (...)` JQL queries. Issue keys are
    /// always `[A-Z]{2,10}-[0-9]{1,6}` (either validated by
    /// `ctx_core::linking::match_ticket_key` before reaching this module,
    /// or reported by Jira's own API), so none can smuggle JQL syntax.
    ///
    /// Uses the enhanced JQL search endpoint (`POST
    /// /rest/api/3/search/jql`), not the legacy `GET /rest/api/3/search`:
    /// Atlassian has sunset the latter (it now answers with HTTP 410) in
    /// favor of this one, which also drops the `startAt`/`total`
    /// offset-pagination model for an opaque `nextPageToken` cursor.
    fn fetch_issues_by_keys(&self, keys: &BTreeSet<String>) -> Result<IssueResolution, JiraError> {
        let ordered: Vec<String> = keys.iter().cloned().collect();
        let mut resolution = IssueResolution::default();
        for chunk in ordered.chunks(KEY_BATCH_SIZE) {
            resolution.extend(self.resolve_issue_batch(chunk)?);
        }
        Ok(resolution)
    }

    fn resolve_issue_batch(&self, keys: &[String]) -> Result<IssueResolution, JiraError> {
        match self.search_issue_batch(keys) {
            Ok(issues) => {
                let returned: BTreeSet<_> = issues.iter().map(|issue| issue.key.clone()).collect();
                let mut resolution = IssueResolution {
                    issues,
                    unavailable_keys: BTreeSet::new(),
                };
                for key in keys {
                    if !returned.contains(key) {
                        resolution.extend(self.probe_issue(key)?);
                    }
                }
                Ok(resolution)
            }
            Err(error) if is_isolatable_search_error(&error) && keys.len() > 1 => {
                let middle = keys.len() / 2;
                let mut resolution = self.resolve_issue_batch(&keys[..middle])?;
                resolution.extend(self.resolve_issue_batch(&keys[middle..])?);
                Ok(resolution)
            }
            Err(error) if is_isolatable_search_error(&error) => self.probe_issue(&keys[0]),
            Err(error) => Err(error),
        }
    }

    fn search_issue_batch(&self, keys: &[String]) -> Result<Vec<RawIssue>, JiraError> {
        let jql = format!("key in ({}) ORDER BY key ASC", keys.join(","));
        let mut issues = Vec::new();
        let mut next_page_token: Option<String> = None;
        for page_number in 1..=MAX_SEARCH_PAGES {
            let request_body = serde_json::to_string(&RawSearchRequest {
                jql: &jql,
                max_results: 100,
                fields: SEARCH_FIELDS,
                next_page_token: next_page_token.as_deref(),
            })
            .expect("search request body is always representable as JSON");
            let page: RawSearchResponse =
                self.post_json("/rest/api/3/search/jql", &request_body)?;
            let fetched = page.issues.len();
            issues.extend(page.issues);
            next_page_token = page.next_page_token;
            if fetched == 0 || next_page_token.is_none() {
                break;
            }
            if page_number == MAX_SEARCH_PAGES {
                return Err(JiraError::Transport {
                    path: "/rest/api/3/search/jql".to_owned(),
                    message: format!(
                        "did not terminate after {MAX_SEARCH_PAGES} pages (still returning a nextPageToken) -- likely the reported Jira Cloud pagination bug"
                    ),
                });
            }
        }
        Ok(issues)
    }

    fn probe_issue(&self, key: &str) -> Result<IssueResolution, JiraError> {
        let path = format!("/rest/api/3/issue/{key}?fields={}", SEARCH_FIELDS.join(","));
        match self.get_json(&path) {
            Ok(issue) => Ok(IssueResolution {
                issues: vec![issue],
                unavailable_keys: BTreeSet::new(),
            }),
            Err(error) if is_unavailable_error(&error) => Ok(IssueResolution {
                issues: Vec::new(),
                unavailable_keys: BTreeSet::from([key.to_owned()]),
            }),
            Err(error) => Err(error),
        }
    }

    fn ingest_one_issue(
        &self,
        issue: RawIssue,
        artifacts: &mut Vec<Artifact>,
        links: &mut Vec<ArtifactLink>,
    ) -> Result<Option<ArtifactIdentity>, JiraError> {
        let identity = Self::issue_identity(&issue.key);
        let project = issue.fields.project.key.clone();
        let comments = match self.fetch_comments(&identity.external_id) {
            Ok(comments) => comments,
            Err(error) if is_unavailable_error(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        artifacts.push(self.issue_artifact(&identity, issue));
        self.push_comments(&identity, &project, comments, artifacts, links);
        Ok(Some(identity))
    }

    fn fetch_comments(&self, issue_key: &str) -> Result<Vec<RawComment>, JiraError> {
        let mut comments = Vec::new();
        let mut start_at = 0u32;
        loop {
            let page: RawCommentPage = self.get_json(&format!(
                "/rest/api/3/issue/{issue_key}/comment?startAt={start_at}&maxResults=100"
            ))?;
            let fetched = page.comments.len();
            comments.extend(page.comments);
            start_at += u32::try_from(fetched).unwrap_or(u32::MAX);
            if fetched == 0 || start_at >= page.total {
                break;
            }
        }
        Ok(comments)
    }

    fn push_comments(
        &self,
        parent: &ArtifactIdentity,
        project: &str,
        comments: Vec<RawComment>,
        artifacts: &mut Vec<Artifact>,
        links: &mut Vec<ArtifactLink>,
    ) {
        for comment in comments {
            let identity = ArtifactIdentity {
                provider: ArtifactProvider::Jira,
                kind: ArtifactKind::Comment,
                external_id: format!("{}-comment-{}", parent.external_id, comment.id),
            };
            let body = flatten_adf(&comment.body);
            artifacts.push(Artifact {
                title: body.lines().next().unwrap_or_default().to_owned(),
                content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
                body,
                author: comment.author.and_then(|user| user.display_name),
                external_created_at: comment.created.map(ctx_core::domain::Timestamp),
                external_updated_at: comment.updated.map(ctx_core::domain::Timestamp),
                source_locator: ctx_core::domain::Url(format!(
                    "{}/browse/{}?focusedCommentId={}",
                    self.base_url, parent.external_id, comment.id
                )),
                project: ctx_core::domain::Project(project.to_owned()),
                identity: identity.clone(),
            });
            links.push(ArtifactLink {
                source: identity,
                target: ArtifactLinkTarget::Artifact(parent.clone()),
                kind: ArtifactLinkKind::CommentsOn,
                evidence_locator: format!("jira comment API: {}", parent.external_id),
            });
        }
    }

    fn issue_identity(key: &str) -> ArtifactIdentity {
        ArtifactIdentity {
            provider: ArtifactProvider::Jira,
            kind: ArtifactKind::Issue,
            external_id: key.to_owned(),
        }
    }

    fn issue_artifact(&self, identity: &ArtifactIdentity, issue: RawIssue) -> Artifact {
        let body = issue
            .fields
            .description
            .as_ref()
            .map(flatten_adf)
            .unwrap_or_default();
        Artifact {
            title: issue.fields.summary,
            content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
            body,
            author: issue.fields.creator.and_then(|user| user.display_name),
            external_created_at: issue.fields.created.map(ctx_core::domain::Timestamp),
            external_updated_at: issue.fields.updated.map(ctx_core::domain::Timestamp),
            source_locator: ctx_core::domain::Url(format!(
                "{}/browse/{}",
                self.base_url, identity.external_id
            )),
            project: ctx_core::domain::Project(issue.fields.project.key),
            identity: identity.clone(),
        }
    }

    fn get_json<D: serde::de::DeserializeOwned>(&self, path: &str) -> Result<D, JiraError> {
        let body = self.transport.get(path)?;
        serde_json::from_str(&body).map_err(|error| JiraError::InvalidJson {
            path: path.to_owned(),
            source: error,
        })
    }

    fn post_json<D: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        request_body: &str,
    ) -> Result<D, JiraError> {
        let body = self.transport.post(path, request_body)?;
        serde_json::from_str(&body).map_err(|error| JiraError::InvalidJson {
            path: path.to_owned(),
            source: error,
        })
    }
}

impl<T: JiraTransport> ExternalArtifactSource for JiraClient<T> {
    fn fetch(
        &self,
        request: ExternalArtifactRequest<'_>,
    ) -> Result<ExternalArtifactBatch, PortError> {
        match request {
            ExternalArtifactRequest::ReferencedKeys {
                keys,
                known_artifacts,
                unavailable_artifacts,
            } => self.fetch_issue_artifacts_for_keys_with_depth_and_known(
                keys,
                1,
                known_artifacts,
                unavailable_artifacts,
            ),
            ExternalArtifactRequest::BusinessLinkedKeys {
                keys,
                related_depth,
                known_artifacts,
                unavailable_artifacts,
            } => self.fetch_issue_artifacts_for_keys_with_depth_and_known(
                keys,
                related_depth,
                known_artifacts,
                unavailable_artifacts,
            ),
            _ => {
                return Err(PortError::new(
                    "Jira accepts only referenced-key artifact requests",
                ));
            }
        }
        .map_err(|error| PortError::new(error.to_string()))
    }
}

#[derive(Default)]
struct IssueResolution {
    issues: Vec<RawIssue>,
    unavailable_keys: BTreeSet<String>,
}

impl IssueResolution {
    fn extend(&mut self, other: Self) {
        self.issues.extend(other.issues);
        self.unavailable_keys.extend(other.unavailable_keys);
    }
}

fn is_isolatable_search_error(error: &JiraError) -> bool {
    matches!(
        error,
        JiraError::Http {
            status: 400 | 403 | 404,
            ..
        }
    )
}

fn is_unavailable_error(error: &JiraError) -> bool {
    matches!(
        error,
        JiraError::Http {
            status: 403 | 404,
            ..
        }
    )
}

/// Every issue key `issue`'s own `issuelinks` (`blocks`/`relates
/// to`/`duplicates`, in either direction) and `parent` (subtask or,
/// team-managed project, epic) fields report -- Jira's own structural
/// relationship data, not a text-derived guess. Classic (company-managed)
/// project epic links, which live behind an instance-specific custom field
/// rather than `parent`, are not covered; that would require per-instance
/// configuration this module deliberately doesn't ask for in v1.
fn linked_keys(issue: &RawIssue) -> Vec<String> {
    let mut keys = Vec::new();
    for link in &issue.fields.issuelinks {
        if let Some(reference) = &link.outward_issue {
            keys.push(reference.key.clone());
        }
        if let Some(reference) = &link.inward_issue {
            keys.push(reference.key.clone());
        }
    }
    if let Some(parent) = &issue.fields.parent {
        keys.push(parent.key.clone());
    }
    keys
}

/// Flattens an Atlassian Document Format value (Jira Cloud v3's
/// `description`/comment `body` shape) to plain text: every field this
/// module treats as evidence text is ADF JSON, not a string, and passing
/// the raw JSON through as `body` would make it both unreadable and
/// useless as `ctx enrich` evidence. Walks `content` recursively,
/// concatenating `text` nodes and inserting paragraph/line breaks at block
/// boundaries -- not a full ADF renderer (marks/tables/media are ignored),
/// just enough to recover readable prose.
fn flatten_adf(node: &JsonValue) -> String {
    let mut buffer = String::new();
    flatten_adf_into(node, &mut buffer);
    buffer.trim().to_owned()
}

fn flatten_adf_into(node: &JsonValue, buffer: &mut String) {
    let Some(node_type) = node.get("type").and_then(JsonValue::as_str) else {
        return;
    };
    if node_type == "text" {
        if let Some(text) = node.get("text").and_then(JsonValue::as_str) {
            buffer.push_str(text);
        }
        return;
    }
    if let Some(content) = node.get("content").and_then(JsonValue::as_array) {
        for child in content {
            flatten_adf_into(child, buffer);
        }
    }
    match node_type {
        "paragraph" | "heading" | "codeBlock" | "blockquote" | "listItem" => {
            buffer.push('\n');
            buffer.push('\n');
        }
        "hardBreak" => buffer.push('\n'),
        _ => {}
    }
}

/// The fixed field selection every `/rest/api/3/search/jql` request asks
/// for -- exactly what [`JiraClient::issue_artifact`]/[`linked_keys`]
/// consume, kept as one constant so the request-builder and the
/// human-readable rationale for the selection live in one place.
const SEARCH_FIELDS: &[&str] = &[
    "project",
    "summary",
    "description",
    "creator",
    "created",
    "updated",
    "issuelinks",
    "parent",
];

#[derive(serde::Serialize)]
struct RawSearchRequest<'a> {
    jql: &'a str,
    #[serde(rename = "maxResults")]
    max_results: u32,
    fields: &'a [&'a str],
    #[serde(rename = "nextPageToken", skip_serializing_if = "Option::is_none")]
    next_page_token: Option<&'a str>,
}

#[derive(Deserialize)]
struct RawSearchResponse {
    issues: Vec<RawIssue>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct RawIssue {
    key: String,
    fields: RawIssueFields,
}

#[derive(Deserialize)]
struct RawIssueFields {
    project: RawProject,
    summary: String,
    #[serde(default)]
    description: Option<JsonValue>,
    creator: Option<RawUser>,
    created: Option<String>,
    updated: Option<String>,
    #[serde(default)]
    issuelinks: Vec<RawIssueLink>,
    #[serde(default)]
    parent: Option<RawIssueRef>,
}

#[derive(Deserialize)]
struct RawProject {
    key: String,
}

#[derive(Deserialize)]
struct RawIssueLink {
    #[serde(rename = "outwardIssue", default)]
    outward_issue: Option<RawIssueRef>,
    #[serde(rename = "inwardIssue", default)]
    inward_issue: Option<RawIssueRef>,
}

#[derive(Deserialize)]
struct RawIssueRef {
    key: String,
}

#[derive(Deserialize)]
struct RawUser {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct RawCommentPage {
    comments: Vec<RawComment>,
    total: u32,
}

#[derive(Deserialize)]
struct RawComment {
    id: String,
    body: JsonValue,
    author: Option<RawUser>,
    created: Option<String>,
    updated: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct FakeTransport {
        responses: BTreeMap<String, String>,
        errors: BTreeMap<String, u16>,
    }

    impl JiraTransport for FakeTransport {
        fn get(&self, path: &str) -> Result<String, JiraError> {
            if let Some(status) = self.errors.get(path) {
                return Err(JiraError::Http {
                    path: path.to_owned(),
                    status: *status,
                    message: "fixture HTTP error".to_owned(),
                });
            }
            self.responses
                .get(path)
                .cloned()
                .ok_or_else(|| JiraError::Transport {
                    path: path.to_owned(),
                    message: "no fixture response for this path".to_owned(),
                })
        }

        fn post(&self, path: &str, body: &str) -> Result<String, JiraError> {
            let key = format!("POST {path}\n{body}");
            if let Some(status) = self.errors.get(&key) {
                return Err(JiraError::Http {
                    path: path.to_owned(),
                    status: *status,
                    message: "fixture HTTP error".to_owned(),
                });
            }
            self.responses
                .get(&key)
                .cloned()
                .ok_or_else(|| JiraError::Transport {
                    path: path.to_owned(),
                    message: "no fixture response for this request body".to_owned(),
                })
        }
    }

    fn adf_paragraph(text: &str) -> String {
        format!(
            r#"{{"type":"doc","version":1,"content":[{{"type":"paragraph","content":[{{"type":"text","text":"{text}"}}]}}]}}"#
        )
    }

    fn keys(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn empty_comment_page(issue_key: &str) -> (String, String) {
        (
            format!("/rest/api/3/issue/{issue_key}/comment?startAt=0&maxResults=100"),
            r#"{"total":0,"comments":[]}"#.to_owned(),
        )
    }

    fn issue_path(issue_key: &str) -> String {
        format!(
            "/rest/api/3/issue/{issue_key}?fields={}",
            SEARCH_FIELDS.join(",")
        )
    }

    /// Builds the fixture key for a `POST /rest/api/3/search/jql` request:
    /// the exact serialized request body [`JiraClient::fetch_issues_by_keys`]
    /// sends for this `jql`/`next_page_token` pair, so a fixture only
    /// matches the one request it was written for.
    fn search_key(jql: &str, next_page_token: Option<&str>) -> String {
        let body = serde_json::to_string(&RawSearchRequest {
            jql,
            max_results: 100,
            fields: SEARCH_FIELDS,
            next_page_token,
        })
        .expect("search request body always serializes");
        format!("POST /rest/api/3/search/jql\n{body}")
    }

    #[test]
    fn jira_search_and_comments_read_every_page() {
        let jql = "key in (PSI-1) ORDER BY key ASC";
        let first_issues: Vec<_> = (1..=100)
            .map(|id| json!({"key": format!("PSI-{id}"), "fields": {"project": {"key": "PSI"}, "summary": format!("issue {id}")}}))
            .collect();
        let last_issue = json!({"key": "PSI-101", "fields": {"project": {"key": "PSI"}, "summary": "issue 101"}});
        let first_comments: Vec<_> = (1..=100)
            .map(|id| json!({"id": id.to_string(), "body": {"type": "doc", "content": []}}))
            .collect();
        let last_comment = json!({"id": "101", "body": {"type": "doc", "content": []}});
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key(jql, None),
            json!({"issues": first_issues, "nextPageToken": "page-2"}).to_string(),
        );
        responses.insert(
            search_key(jql, Some("page-2")),
            json!({"issues": [last_issue]}).to_string(),
        );
        responses.insert(
            "/rest/api/3/issue/PSI-1/comment?startAt=0&maxResults=100".to_owned(),
            json!({"total": 101, "comments": first_comments}).to_string(),
        );
        responses.insert(
            "/rest/api/3/issue/PSI-1/comment?startAt=100&maxResults=100".to_owned(),
            json!({"total": 101, "comments": [last_comment]}).to_string(),
        );
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let issues = client
            .fetch_issues_by_keys(&keys(&["PSI-1"]))
            .expect("all issue pages");
        let comments = client.fetch_comments("PSI-1").expect("all comment pages");

        assert_eq!(issues.issues.len(), 101);
        assert_eq!(comments.len(), 101);
    }

    #[test]
    fn a_search_that_never_stops_paginating_fails_loudly_instead_of_hanging() {
        // Reproduces the pagination bug Atlassian Community has reported
        // against this endpoint: every page keeps handing back the same
        // `nextPageToken`, so a client that only stops on an absent token
        // would loop forever.
        let jql = "key in (PSI-1) ORDER BY key ASC";
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key(jql, None),
            json!({"issues": [{"key": "PSI-1", "fields": {"project": {"key": "PSI"}, "summary": "Seed"}}], "nextPageToken": "stuck"})
                .to_string(),
        );
        responses.insert(
            search_key(jql, Some("stuck")),
            json!({"issues": [{"key": "PSI-1", "fields": {"project": {"key": "PSI"}, "summary": "Seed"}}], "nextPageToken": "stuck"})
                .to_string(),
        );
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );

        match client.fetch_issues_by_keys(&keys(&["PSI-1"])) {
            Err(JiraError::Transport { .. }) => {}
            other => panic!(
                "pagination that never terminates must be a reported transport error, not a hang: {}",
                other.is_ok()
            ),
        }
    }

    #[test]
    fn fetches_referenced_issues_across_projects() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key("key in (PSI-1122,UTF-8) ORDER BY key ASC", None),
            format!(
                r#"{{"issues":[{{"key":"PSI-1122","fields":{{"project":{{"key":"PSI"}},"summary":"Cancellation removes prepaid access","description":{},"creator":{{"displayName":"alice"}},"created":"2026-08-01T00:00:00Z","updated":"2026-08-01T00:00:00Z"}}}},{{"key":"UTF-8","fields":{{"project":{{"key":"UTF"}},"summary":"Cross-project issue"}}}}]}}"#,
                adf_paragraph("A cancelled prepaid subscription must remain usable until paid_until.")
            ),
        );
        responses.extend([empty_comment_page("UTF-8")]);
        responses.insert(
            "/rest/api/3/issue/PSI-1122/comment?startAt=0&maxResults=100".to_owned(),
            format!(
                r#"{{"total":1,"comments":[{{"id":"1","body":{},"author":{{"displayName":"bob"}},"created":"2026-08-01T01:00:00Z","updated":"2026-08-01T01:00:00Z"}}]}}"#,
                adf_paragraph("Do not revoke an already paid entitlement immediately.")
            ),
        );
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys(&keys(&["PSI-1122", "UTF-8"]))
            .expect("issues and comments");

        assert_eq!(batch.artifacts.len(), 3); // two issues + one comment
        let issue = batch
            .artifacts
            .iter()
            .find(|artifact| artifact.identity.external_id == "PSI-1122")
            .expect("issue artifact");
        // The issue's external_id must be the human-readable key, not an
        // internal numeric id, so ReferenceKind::TicketKey resolves a
        // "PSI-1122" mention in a branch name or commit message to this
        // artifact for free (crates/ctx-core/src/linking.rs).
        assert_eq!(issue.identity.external_id, "PSI-1122");
        assert_eq!(issue.title, "Cancellation removes prepaid access");
        assert_eq!(
            issue.body,
            "A cancelled prepaid subscription must remain usable until paid_until."
        );
        assert_eq!(
            issue.source_locator.as_str(),
            "https://example.atlassian.net/browse/PSI-1122"
        );
        assert_eq!(issue.project.as_str(), "PSI");
        let cross_project = batch
            .artifacts
            .iter()
            .find(|artifact| artifact.identity.external_id == "UTF-8")
            .expect("cross-project issue artifact");
        assert_eq!(cross_project.project.as_str(), "UTF");

        let comments_on = batch
            .links
            .iter()
            .find(|link| link.kind == ArtifactLinkKind::CommentsOn)
            .expect("comment link");
        assert_eq!(
            comments_on.target,
            ArtifactLinkTarget::Artifact(ArtifactIdentity {
                provider: ArtifactProvider::Jira,
                kind: ArtifactKind::Issue,
                external_id: "PSI-1122".to_owned(),
            })
        );
    }

    #[test]
    fn no_referenced_keys_means_no_request_at_all() {
        let client = JiraClient::new(
            FakeTransport::default(),
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys(&BTreeSet::new())
            .expect("no candidates is not an error");

        assert!(batch.artifacts.is_empty());
        assert!(batch.links.is_empty());
    }

    #[test]
    fn a_negative_cached_key_is_omitted_from_jira_requests() {
        let client = JiraClient::new(
            FakeTransport::default(),
            "PSI",
            "https://example.atlassian.net",
        );
        let unavailable = HashSet::from([ArtifactIdentity {
            provider: ArtifactProvider::Jira,
            kind: ArtifactKind::Issue,
            external_id: "OPS-404".to_owned(),
        }]);

        let batch = client
            .fetch(ExternalArtifactRequest::ReferencedKeys {
                keys: &keys(&["OPS-404"]),
                known_artifacts: &HashSet::new(),
                unavailable_artifacts: &unavailable,
            })
            .expect("cached key must need no transport fixture");

        assert_eq!(batch, ExternalArtifactBatch::default());
    }

    #[test]
    fn a_partially_local_candidate_set_fetches_only_the_missing_issue() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key("key in (PSI-2) ORDER BY key ASC", None),
            r#"{"issues":[{"key":"PSI-2","fields":{"project":{"key":"PSI"},"summary":"Missing issue"}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-2")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );
        let known = HashSet::from([ArtifactIdentity {
            provider: ArtifactProvider::Jira,
            kind: ArtifactKind::Issue,
            external_id: "PSI-1".to_owned(),
        }]);

        let batch = client
            .fetch(ExternalArtifactRequest::ReferencedKeys {
                keys: &keys(&["PSI-1", "PSI-2"]),
                known_artifacts: &known,
                unavailable_artifacts: &HashSet::new(),
            })
            .expect("only missing issue is requested");

        assert_eq!(
            batch
                .artifacts
                .iter()
                .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
                .map(|artifact| artifact.identity.external_id.as_str())
                .collect::<Vec<_>>(),
            vec!["PSI-2"]
        );
    }

    #[test]
    fn an_entirely_local_candidate_set_makes_no_jira_request() {
        let client = JiraClient::new(
            FakeTransport::default(),
            "PSI",
            "https://example.atlassian.net",
        );
        let known = HashSet::from([ArtifactIdentity {
            provider: ArtifactProvider::Jira,
            kind: ArtifactKind::Issue,
            external_id: "PSI-1".to_owned(),
        }]);

        let batch = client
            .fetch(ExternalArtifactRequest::ReferencedKeys {
                keys: &keys(&["PSI-1"]),
                known_artifacts: &known,
                unavailable_artifacts: &HashSet::new(),
            })
            .expect("local issue needs no transport fixture");

        assert_eq!(batch, ExternalArtifactBatch::default());
    }

    #[test]
    fn expands_one_hop_through_jira_reported_issue_links_but_no_further() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key("key in (PSI-1) ORDER BY key ASC", None),
            r#"{"issues":[{"key":"PSI-1","fields":{"project":{"key":"PSI"},"summary":"Seed","description":null,"creator":null,"created":null,"updated":null,"issuelinks":[{"outwardIssue":{"key":"PSI-2"}}],"parent":{"key":"PSI-3"}}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-1")]);
        responses.insert(
            search_key("key in (PSI-2,PSI-3) ORDER BY key ASC", None),
            // PSI-2 itself links further to PSI-4 -- this must NOT be
            // followed (one hop only), so no fixture exists for a PSI-4
            // request; if the client tried, the test would fail on a
            // missing-fixture error.
            r#"{"issues":[{"key":"PSI-2","fields":{"project":{"key":"PSI"},"summary":"Related via issuelinks","description":null,"creator":null,"created":null,"updated":null,"issuelinks":[{"outwardIssue":{"key":"PSI-4"}}]}},{"key":"PSI-3","fields":{"project":{"key":"PSI"},"summary":"Related via parent","description":null,"creator":null,"created":null,"updated":null}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-2"), empty_comment_page("PSI-3")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys(&keys(&["PSI-1"]))
            .expect("seed plus one-hop expansion");

        let mut issue_keys: Vec<_> = batch
            .artifacts
            .iter()
            .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
            .map(|artifact| artifact.identity.external_id.clone())
            .collect();
        issue_keys.sort();
        assert_eq!(
            issue_keys,
            vec!["PSI-1".to_owned(), "PSI-2".to_owned(), "PSI-3".to_owned()],
            "PSI-4 (a link of a link) must not be pulled in"
        );

        let related_links: Vec<_> = batch
            .links
            .iter()
            .filter(|link| link.kind == ArtifactLinkKind::RelatedIssue)
            .collect();
        assert_eq!(related_links.len(), 2);
        assert!(related_links.iter().any(|link| {
            link.source.external_id == "PSI-1"
                && link.target
                    == ArtifactLinkTarget::Artifact(ArtifactIdentity {
                        provider: ArtifactProvider::Jira,
                        kind: ArtifactKind::Issue,
                        external_id: "PSI-2".to_owned(),
                    })
        }));
        assert!(related_links.iter().any(|link| {
            link.source.external_id == "PSI-1"
                && link.target
                    == ArtifactLinkTarget::Artifact(ArtifactIdentity {
                        provider: ArtifactProvider::Jira,
                        kind: ArtifactKind::Issue,
                        external_id: "PSI-3".to_owned(),
                    })
        }));
    }

    #[test]
    fn two_new_issues_preserve_both_relations_to_the_same_target() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key("key in (PSI-1,PSI-2) ORDER BY key ASC", None),
            r#"{"issues":[{"key":"PSI-1","fields":{"project":{"key":"PSI"},"summary":"First","issuelinks":[{"outwardIssue":{"key":"PSI-3"}}]}},{"key":"PSI-2","fields":{"project":{"key":"PSI"},"summary":"Second","issuelinks":[{"outwardIssue":{"key":"PSI-3"}}]}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-1"), empty_comment_page("PSI-2")]);
        responses.insert(
            search_key("key in (PSI-3) ORDER BY key ASC", None),
            r#"{"issues":[{"key":"PSI-3","fields":{"project":{"key":"PSI"},"summary":"Shared target"}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-3")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys(&keys(&["PSI-1", "PSI-2"]))
            .expect("shared target expansion");
        let mut sources: Vec<_> = batch
            .links
            .iter()
            .filter(|link| {
                link.kind == ArtifactLinkKind::RelatedIssue
                    && link.target
                        == ArtifactLinkTarget::Artifact(ArtifactIdentity {
                            provider: ArtifactProvider::Jira,
                            kind: ArtifactKind::Issue,
                            external_id: "PSI-3".to_owned(),
                        })
            })
            .map(|link| link.source.external_id.as_str())
            .collect();
        sources.sort_unstable();
        assert_eq!(sources, vec!["PSI-1", "PSI-2"]);
    }

    #[test]
    fn a_related_issue_already_stored_locally_is_linked_without_refetching_it() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key("key in (PSI-1) ORDER BY key ASC", None),
            r#"{"issues":[{"key":"PSI-1","fields":{"project":{"key":"PSI"},"summary":"Seed","issuelinks":[{"outwardIssue":{"key":"PSI-2"}}]}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-1")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );
        let known = HashSet::from([ArtifactIdentity {
            provider: ArtifactProvider::Jira,
            kind: ArtifactKind::Issue,
            external_id: "PSI-2".to_owned(),
        }]);

        let batch = client
            .fetch_issue_artifacts_for_keys_with_depth_and_known(
                &keys(&["PSI-1"]),
                1,
                &known,
                &HashSet::new(),
            )
            .expect("known related issue needs no Jira request");

        assert_eq!(
            batch
                .artifacts
                .iter()
                .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
                .count(),
            1
        );
        assert!(batch.links.iter().any(|link| {
            link.source.external_id == "PSI-1"
                && link.target
                    == ArtifactLinkTarget::Artifact(ArtifactIdentity {
                        provider: ArtifactProvider::Jira,
                        kind: ArtifactKind::Issue,
                        external_id: "PSI-2".to_owned(),
                    })
                && link.kind == ArtifactLinkKind::RelatedIssue
        }));
    }

    #[test]
    fn zero_related_depth_fetches_only_direct_repository_backed_keys() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key("key in (PSI-1) ORDER BY key ASC", None),
            r#"{"issues":[{"key":"PSI-1","fields":{"project":{"key":"PSI"},"summary":"Seed","description":null,"creator":null,"created":null,"updated":null,"issuelinks":[{"outwardIssue":{"key":"PSI-2"}}]}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-1")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys_with_depth(&keys(&["PSI-1"]), 0)
            .expect("direct keys only");

        assert_eq!(
            batch
                .artifacts
                .iter()
                .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
                .count(),
            1
        );
        assert!(
            batch
                .links
                .iter()
                .all(|link| link.kind != ArtifactLinkKind::RelatedIssue)
        );
    }

    #[test]
    fn a_related_issue_already_among_the_seeds_keeps_the_reported_link() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key("key in (PSI-1,PSI-2) ORDER BY key ASC", None),
            r#"{"issues":[{"key":"PSI-1","fields":{"project":{"key":"PSI"},"summary":"Seed one","description":null,"creator":null,"created":null,"updated":null,"issuelinks":[{"outwardIssue":{"key":"PSI-2"}}]}},{"key":"PSI-2","fields":{"project":{"key":"PSI"},"summary":"Seed two","description":null,"creator":null,"created":null,"updated":null}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-1"), empty_comment_page("PSI-2")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys(&keys(&["PSI-1", "PSI-2"]))
            .expect("both already seeds");

        assert_eq!(
            batch
                .artifacts
                .iter()
                .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
                .count(),
            2
        );
        assert_eq!(
            batch
                .links
                .iter()
                .filter(|link| link.kind == ArtifactLinkKind::RelatedIssue)
                .count(),
            1
        );
    }

    #[test]
    fn a_successful_search_probes_and_ingests_an_omitted_key() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key("key in (OPS-2,PSI-1) ORDER BY key ASC", None),
            json!({"issues": [{"key": "PSI-1", "fields": {"project": {"key": "PSI"}, "summary": "Visible seed"}}]}).to_string(),
        );
        responses.insert(
            issue_path("OPS-2"),
            json!({"key": "OPS-2", "fields": {"project": {"key": "OPS"}, "summary": "Search-lagged issue"}}).to_string(),
        );
        responses.extend([empty_comment_page("OPS-2"), empty_comment_page("PSI-1")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                ..FakeTransport::default()
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys_with_depth(&keys(&["OPS-2", "PSI-1"]), 0)
            .expect("omitted issue is resolved directly");

        let issue_keys: BTreeSet<_> = batch
            .artifacts
            .iter()
            .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
            .map(|artifact| artifact.identity.external_id.as_str())
            .collect();
        assert_eq!(issue_keys, BTreeSet::from(["OPS-2", "PSI-1"]));
        assert!(batch.unavailable_keys.is_empty());
    }

    #[test]
    fn a_successful_search_reports_a_probe_404_as_unavailable() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_key("key in (OPS-404,PSI-1) ORDER BY key ASC", None),
            json!({"issues": [{"key": "PSI-1", "fields": {"project": {"key": "PSI"}, "summary": "Visible seed"}}]}).to_string(),
        );
        responses.extend([empty_comment_page("PSI-1")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                errors: BTreeMap::from([(issue_path("OPS-404"), 404)]),
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys_with_depth(&keys(&["OPS-404", "PSI-1"]), 0)
            .expect("inaccessible issue does not fail the batch");

        assert_eq!(batch.unavailable_keys, keys(&["OPS-404"]));
        assert_eq!(
            batch
                .artifacts
                .iter()
                .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
                .count(),
            1
        );
    }

    #[test]
    fn a_failed_batch_is_bisected_and_only_the_inaccessible_key_is_reported() {
        let initial = search_key("key in (OPS-404,PSI-1) ORDER BY key ASC", None);
        let inaccessible = search_key("key in (OPS-404) ORDER BY key ASC", None);
        let visible = search_key("key in (PSI-1) ORDER BY key ASC", None);
        let mut responses = BTreeMap::new();
        responses.insert(
            visible,
            json!({"issues": [{"key": "PSI-1", "fields": {"project": {"key": "PSI"}, "summary": "Visible seed"}}]}).to_string(),
        );
        responses.extend([empty_comment_page("PSI-1")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                errors: BTreeMap::from([
                    (initial, 400),
                    (inaccessible, 400),
                    (issue_path("OPS-404"), 404),
                ]),
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys_with_depth(&keys(&["OPS-404", "PSI-1"]), 0)
            .expect("bisection preserves accessible issues");

        assert_eq!(batch.unavailable_keys, keys(&["OPS-404"]));
        assert!(batch.artifacts.iter().any(|artifact| {
            artifact.identity.kind == ArtifactKind::Issue
                && artifact.identity.external_id == "PSI-1"
        }));
    }

    #[test]
    fn a_singleton_search_400_is_not_misreported_when_the_probe_also_fails() {
        let client = JiraClient::new(
            FakeTransport {
                responses: BTreeMap::new(),
                errors: BTreeMap::from([
                    (search_key("key in (PSI-1) ORDER BY key ASC", None), 400),
                    (issue_path("PSI-1"), 400),
                ]),
            },
            "PSI",
            "https://example.atlassian.net",
        );

        assert!(matches!(
            client.fetch_issue_artifacts_for_keys_with_depth(&keys(&["PSI-1"]), 0),
            Err(JiraError::Http { status: 400, .. })
        ));
    }

    #[test]
    fn an_observed_401_stays_fatal() {
        let client = JiraClient::new(
            FakeTransport {
                responses: BTreeMap::new(),
                errors: BTreeMap::from([(
                    search_key("key in (PSI-1) ORDER BY key ASC", None),
                    401,
                )]),
            },
            "PSI",
            "https://example.atlassian.net",
        );

        assert!(matches!(
            client.fetch_issue_artifacts_for_keys_with_depth(&keys(&["PSI-1"]), 0),
            Err(JiraError::Http { status: 401, .. })
        ));
    }

    #[test]
    fn inaccessible_comments_make_only_that_issue_unavailable() {
        let search = search_key("key in (OPS-2,PSI-1) ORDER BY key ASC", None);
        let mut responses = BTreeMap::from([(
            search,
            json!({"issues": [
                {"key": "OPS-2", "fields": {"project": {"key": "OPS"}, "summary": "No comments access"}},
                {"key": "PSI-1", "fields": {"project": {"key": "PSI"}, "summary": "Visible seed"}}
            ]})
            .to_string(),
        )]);
        responses.extend([empty_comment_page("PSI-1")]);
        let client = JiraClient::new(
            FakeTransport {
                responses,
                errors: BTreeMap::from([(
                    "/rest/api/3/issue/OPS-2/comment?startAt=0&maxResults=100".to_owned(),
                    403,
                )]),
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let batch = client
            .fetch_issue_artifacts_for_keys_with_depth(&keys(&["OPS-2", "PSI-1"]), 0)
            .expect("one inaccessible comment collection does not fail other issues");

        assert_eq!(batch.unavailable_keys, keys(&["OPS-2"]));
        assert!(
            batch
                .artifacts
                .iter()
                .all(|artifact| { !artifact.identity.external_id.starts_with("OPS-2") })
        );
    }

    #[test]
    fn adf_description_is_flattened_to_plain_text() {
        let adf: JsonValue = serde_json::from_str(
            r#"{"type":"doc","version":1,"content":[
                {"type":"paragraph","content":[{"type":"text","text":"First paragraph."}]},
                {"type":"paragraph","content":[{"type":"text","text":"Second, with a "},{"type":"text","text":"hard"},{"type":"hardBreak"},{"type":"text","text":"break."}]}
            ]}"#,
        )
        .expect("valid ADF fixture");

        let text = flatten_adf(&adf);

        assert_eq!(text, "First paragraph.\n\nSecond, with a hard\nbreak.");
    }

    #[test]
    fn missing_email_or_token_env_var_is_a_config_error() {
        let jira = RawJiraConfig {
            base_url: "https://example.atlassian.net".to_owned(),
            project: "PSI".to_owned(),
        };

        let missing_email = JiraConfig::resolve(
            "config.toml".to_owned(),
            jira,
            None,
            Some("token".to_owned()),
        )
        .expect_err("missing email");
        assert!(matches!(missing_email, JiraError::Config { .. }));

        let jira = RawJiraConfig {
            base_url: "https://example.atlassian.net".to_owned(),
            project: "PSI".to_owned(),
        };
        let missing_token = JiraConfig::resolve(
            "config.toml".to_owned(),
            jira,
            Some("jane@example.com".to_owned()),
            None,
        )
        .expect_err("missing token");
        assert!(matches!(missing_token, JiraError::Config { .. }));
    }
}
