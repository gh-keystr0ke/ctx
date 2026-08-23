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
    collections::{BTreeMap, BTreeSet},
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
    /// Returns [`JiraError::Transport`] when the request fails or returns a
    /// non-success status.
    fn get(&self, path: &str) -> Result<String, JiraError>;
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
        http_retry::run(self.retry_policy, &ThreadSleeper, || {
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
        .map_err(|error| JiraError::Transport {
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
pub struct JiraConfig {
    pub base_url: String,
    pub project: String,
    pub email: String,
    pub token: String,
}

impl JiraConfig {
    /// Reads the `[jira]` table from `.ctx/config.toml` (`base_url` and
    /// `project` both required -- unlike GitLab, Jira Cloud has no shared
    /// default host) and the account email/API token from the
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
/// repository's history happens to mention.
const KEY_BATCH_SIZE: usize = 50;

pub struct JiraClient<T> {
    transport: T,
    project: String,
    base_url: String,
}

impl<T: JiraTransport> JiraClient<T> {
    pub fn new(transport: T, project: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            transport,
            project: project.into(),
            base_url: base_url.into(),
        }
    }

    /// Fetches exactly the issues in `candidate_keys` that belong to this
    /// client's configured project -- others are silently ignored, since a
    /// repository's own text can plausibly reference a ticket from a
    /// different Jira project, or coincidentally match the ticket-key shape
    /// (`"UTF-8"` fits `PROJECT-123`) without naming a real one; only this
    /// client's own project is ever queried -- plus, one hop further out,
    /// every issue Jira's own `issuelinks`/`parent` fields report as
    /// directly related to one of those. Never recurses past that single
    /// hop: a related issue's own further links are not followed.
    ///
    /// # Errors
    /// Returns [`JiraError`] when a request fails or its response is not
    /// valid JSON.
    pub fn fetch_issue_artifacts_for_keys(
        &self,
        candidate_keys: &BTreeSet<String>,
    ) -> Result<(Vec<Artifact>, Vec<ArtifactLink>), JiraError> {
        let project_prefix = format!("{}-", self.project);
        let seed_keys: BTreeSet<String> = candidate_keys
            .iter()
            .filter(|key| key.starts_with(&project_prefix))
            .cloned()
            .collect();
        if seed_keys.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let mut artifacts = Vec::new();
        let mut links = Vec::new();

        let seed_issues = self.fetch_issues_by_keys(&seed_keys)?;
        let seed_key_set: BTreeSet<String> =
            seed_issues.iter().map(|issue| issue.key.clone()).collect();
        // The first seed to mention a given related key wins as its
        // recorded source; a related issue linked from several seeds still
        // only needs one auditable `RelatedIssue` edge to justify why it's
        // in the store at all.
        let mut related_sources: BTreeMap<String, ArtifactIdentity> = BTreeMap::new();
        for issue in &seed_issues {
            let identity = Self::issue_identity(&issue.key);
            for related_key in linked_keys(issue) {
                if !seed_key_set.contains(&related_key) {
                    related_sources
                        .entry(related_key)
                        .or_insert_with(|| identity.clone());
                }
            }
        }

        for issue in seed_issues {
            self.ingest_one_issue(issue, &mut artifacts, &mut links)?;
        }

        if !related_sources.is_empty() {
            let related_keys: BTreeSet<String> = related_sources.keys().cloned().collect();
            for issue in self.fetch_issues_by_keys(&related_keys)? {
                let source_identity = related_sources.get(&issue.key).cloned();
                let identity = self.ingest_one_issue(issue, &mut artifacts, &mut links)?;
                if let Some(source_identity) = source_identity {
                    links.push(ArtifactLink {
                        source: source_identity.clone(),
                        target: ArtifactLinkTarget::Artifact(identity),
                        kind: ArtifactLinkKind::RelatedIssue,
                        evidence_locator: format!(
                            "jira issuelinks/parent: {}",
                            source_identity.external_id
                        ),
                    });
                }
            }
        }

        Ok((artifacts, links))
    }

    /// Fetches every issue named in `keys`, batched into
    /// [`KEY_BATCH_SIZE`]-sized `key in (...)` JQL queries. Issue keys are
    /// always `[A-Z]{2,10}-[0-9]{1,6}` (either validated by
    /// `ctx_core::linking::match_ticket_key` before reaching this module,
    /// or reported by Jira's own API), so none can smuggle JQL syntax.
    fn fetch_issues_by_keys(&self, keys: &BTreeSet<String>) -> Result<Vec<RawIssue>, JiraError> {
        let ordered: Vec<&String> = keys.iter().collect();
        let mut issues = Vec::new();
        for chunk in ordered.chunks(KEY_BATCH_SIZE) {
            let jql = format!(
                "key in ({}) ORDER BY key ASC",
                chunk
                    .iter()
                    .map(|key| key.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let mut start_at = 0u32;
            loop {
                let page: RawSearchResponse = self.get_json(&format!(
                    "/rest/api/3/search?jql={}&startAt={start_at}&maxResults=100&fields=summary,description,creator,created,updated,issuelinks,parent",
                    encode_query_value(&jql)
                ))?;
                let fetched = page.issues.len();
                issues.extend(page.issues);
                start_at += u32::try_from(fetched).unwrap_or(u32::MAX);
                if fetched == 0 || start_at >= page.total {
                    break;
                }
            }
        }
        Ok(issues)
    }

    fn ingest_one_issue(
        &self,
        issue: RawIssue,
        artifacts: &mut Vec<Artifact>,
        links: &mut Vec<ArtifactLink>,
    ) -> Result<ArtifactIdentity, JiraError> {
        let identity = Self::issue_identity(&issue.key);
        artifacts.push(self.issue_artifact(&identity, issue));
        let comments = self.fetch_comments(&identity.external_id)?;
        self.push_comments(&identity, comments, artifacts, links);
        Ok(identity)
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
                external_created_at: comment.created,
                external_updated_at: comment.updated,
                source_locator: format!(
                    "{}/browse/{}?focusedCommentId={}",
                    self.base_url, parent.external_id, comment.id
                ),
                project: self.project.clone(),
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
            external_created_at: issue.fields.created,
            external_updated_at: issue.fields.updated,
            source_locator: format!("{}/browse/{}", self.base_url, identity.external_id),
            project: self.project.clone(),
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
}

impl<T: JiraTransport> ExternalArtifactSource for JiraClient<T> {
    fn fetch(
        &self,
        request: ExternalArtifactRequest<'_>,
    ) -> Result<ExternalArtifactBatch, PortError> {
        let ExternalArtifactRequest::ReferencedKeys(candidate_keys) = request else {
            return Err(PortError::new(
                "Jira accepts only the referenced-keys artifact request mode",
            ));
        };
        let (artifacts, links) = self
            .fetch_issue_artifacts_for_keys(candidate_keys)
            .map_err(|error| PortError::new(error.to_string()))?;
        Ok(ExternalArtifactBatch { artifacts, links })
    }
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

/// Percent-encodes a full query-string value: unlike GitLab's ingest path
/// (`crate::gitlab::encode_query_value`), which only ever escapes an
/// RFC3339 timestamp, a JQL expression can contain spaces, quotes, and
/// comparison operators, so every byte outside the unreserved set
/// (`A-Za-z0-9-_.~`) is escaped.
fn encode_query_value(value: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
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

#[derive(Deserialize)]
struct RawSearchResponse {
    issues: Vec<RawIssue>,
    total: u32,
}

#[derive(Deserialize)]
struct RawIssue {
    key: String,
    fields: RawIssueFields,
}

#[derive(Deserialize)]
struct RawIssueFields {
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
    use serde_json::json;

    use super::*;

    struct FakeTransport {
        responses: BTreeMap<String, String>,
    }

    impl JiraTransport for FakeTransport {
        fn get(&self, path: &str) -> Result<String, JiraError> {
            self.responses
                .get(path)
                .cloned()
                .ok_or_else(|| JiraError::Transport {
                    path: path.to_owned(),
                    message: "no fixture response for this path".to_owned(),
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

    fn search_path(jql: &str) -> String {
        search_path_at(jql, 0)
    }

    fn search_path_at(jql: &str, start_at: u32) -> String {
        format!(
            "/rest/api/3/search?jql={}&startAt={start_at}&maxResults=100&fields=summary,description,creator,created,updated,issuelinks,parent",
            encode_query_value(jql)
        )
    }

    #[test]
    fn jira_search_and_comments_read_every_page() {
        let jql = "key in (PSI-1) ORDER BY key ASC";
        let first_issues: Vec<_> = (1..=100)
            .map(|id| json!({"key": format!("PSI-{id}"), "fields": {"summary": format!("issue {id}")}}))
            .collect();
        let last_issue = json!({"key": "PSI-101", "fields": {"summary": "issue 101"}});
        let first_comments: Vec<_> = (1..=100)
            .map(|id| json!({"id": id.to_string(), "body": {"type": "doc", "content": []}}))
            .collect();
        let last_comment = json!({"id": "101", "body": {"type": "doc", "content": []}});
        let mut responses = BTreeMap::new();
        responses.insert(
            search_path_at(jql, 0),
            json!({"total": 101, "issues": first_issues}).to_string(),
        );
        responses.insert(
            search_path_at(jql, 100),
            json!({"total": 101, "issues": [last_issue]}).to_string(),
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
            FakeTransport { responses },
            "PSI",
            "https://example.atlassian.net",
        );

        let issues = client
            .fetch_issues_by_keys(&keys(&["PSI-1"]))
            .expect("all issue pages");
        let comments = client.fetch_comments("PSI-1").expect("all comment pages");

        assert_eq!(issues.len(), 101);
        assert_eq!(comments.len(), 101);
    }

    #[test]
    fn fetches_only_the_referenced_issues_not_the_whole_project() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_path("key in (PSI-1122) ORDER BY key ASC"),
            format!(
                r#"{{"total":1,"issues":[{{"key":"PSI-1122","fields":{{"summary":"Cancellation removes prepaid access","description":{},"creator":{{"displayName":"alice"}},"created":"2026-08-01T00:00:00Z","updated":"2026-08-01T00:00:00Z"}}}}]}}"#,
                adf_paragraph("A cancelled prepaid subscription must remain usable until paid_until.")
            ),
        );
        responses.insert(
            "/rest/api/3/issue/PSI-1122/comment?startAt=0&maxResults=100".to_owned(),
            format!(
                r#"{{"total":1,"comments":[{{"id":"1","body":{},"author":{{"displayName":"bob"}},"created":"2026-08-01T01:00:00Z","updated":"2026-08-01T01:00:00Z"}}]}}"#,
                adf_paragraph("Do not revoke an already paid entitlement immediately.")
            ),
        );
        let client = JiraClient::new(
            FakeTransport { responses },
            "PSI",
            "https://example.atlassian.net",
        );

        // "UTF-8" matches the same PROJECT-123 shape but isn't a PSI ticket
        // and must never trigger a request of its own -- the fixture above
        // has no fixture for it, so an unwanted request would fail the test.
        let (artifacts, links) = client
            .fetch_issue_artifacts_for_keys(&keys(&["PSI-1122", "UTF-8"]))
            .expect("issues and comments");

        assert_eq!(artifacts.len(), 2); // issue + 1 comment
        let issue = artifacts
            .iter()
            .find(|artifact| artifact.identity.kind == ArtifactKind::Issue)
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
            issue.source_locator,
            "https://example.atlassian.net/browse/PSI-1122"
        );

        let comments_on = links
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
            FakeTransport {
                responses: BTreeMap::new(),
            },
            "PSI",
            "https://example.atlassian.net",
        );

        let (artifacts, links) = client
            .fetch_issue_artifacts_for_keys(&BTreeSet::new())
            .expect("no candidates is not an error");

        assert!(artifacts.is_empty());
        assert!(links.is_empty());
    }

    #[test]
    fn expands_one_hop_through_jira_reported_issue_links_but_no_further() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_path("key in (PSI-1) ORDER BY key ASC"),
            r#"{"total":1,"issues":[{"key":"PSI-1","fields":{"summary":"Seed","description":null,"creator":null,"created":null,"updated":null,"issuelinks":[{"outwardIssue":{"key":"PSI-2"}}],"parent":{"key":"PSI-3"}}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-1")]);
        responses.insert(
            search_path("key in (PSI-2,PSI-3) ORDER BY key ASC"),
            // PSI-2 itself links further to PSI-4 -- this must NOT be
            // followed (one hop only), so no fixture exists for a PSI-4
            // request; if the client tried, the test would fail on a
            // missing-fixture error.
            r#"{"total":2,"issues":[{"key":"PSI-2","fields":{"summary":"Related via issuelinks","description":null,"creator":null,"created":null,"updated":null,"issuelinks":[{"outwardIssue":{"key":"PSI-4"}}]}},{"key":"PSI-3","fields":{"summary":"Related via parent","description":null,"creator":null,"created":null,"updated":null}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-2"), empty_comment_page("PSI-3")]);
        let client = JiraClient::new(
            FakeTransport { responses },
            "PSI",
            "https://example.atlassian.net",
        );

        let (artifacts, links) = client
            .fetch_issue_artifacts_for_keys(&keys(&["PSI-1"]))
            .expect("seed plus one-hop expansion");

        let mut issue_keys: Vec<_> = artifacts
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

        let related_links: Vec<_> = links
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
    fn a_related_issue_already_among_the_seeds_gets_no_duplicate_related_link() {
        let mut responses = BTreeMap::new();
        responses.insert(
            search_path("key in (PSI-1,PSI-2) ORDER BY key ASC"),
            r#"{"total":2,"issues":[{"key":"PSI-1","fields":{"summary":"Seed one","description":null,"creator":null,"created":null,"updated":null,"issuelinks":[{"outwardIssue":{"key":"PSI-2"}}]}},{"key":"PSI-2","fields":{"summary":"Seed two","description":null,"creator":null,"created":null,"updated":null}}]}"#
                .to_owned(),
        );
        responses.extend([empty_comment_page("PSI-1"), empty_comment_page("PSI-2")]);
        let client = JiraClient::new(
            FakeTransport { responses },
            "PSI",
            "https://example.atlassian.net",
        );

        let (artifacts, links) = client
            .fetch_issue_artifacts_for_keys(&keys(&["PSI-1", "PSI-2"]))
            .expect("both already seeds");

        assert_eq!(
            artifacts
                .iter()
                .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
                .count(),
            2
        );
        assert!(
            !links
                .iter()
                .any(|link| link.kind == ArtifactLinkKind::RelatedIssue),
            "PSI-2 was already a seed, so it must not also appear as a RelatedIssue edge"
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
