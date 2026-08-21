//! Jira Cloud issue ingestion (mirrors `crate::gitlab`'s architecture and
//! rationale -- see `ADR-EXT-003`/its Jira counterpart): reads issues and
//! their comments through Jira's REST API v3 and normalizes them into
//! [`Artifact`]s.
//!
//! An issue's [`ArtifactIdentity::external_id`] is its human-readable key
//! (`"PSI-1122"`), never Jira's internal numeric id. This is deliberate,
//! not cosmetic: `ctx_core::linking::ReferenceKind::TicketKey` already
//! recognizes exactly this `PROJECT-123` shape in commit messages, branch
//! names, and MR bodies and links it to an `ArtifactKind::Issue` by
//! `external_id` equality alone. Keying Jira issues by their visible ticket
//! key means that deterministic linking resolves branches like
//! `psi-1122-fix` to their Jira issue for free, with no change to
//! `ctx-core`'s linking module.
//!
//! HTTP access goes through [`JiraTransport`] so the client can be tested
//! against canned responses instead of a live Jira instance -- only Jira
//! Cloud is supported (Basic auth via an account email + API token, REST
//! API v3); Jira Server/Data Center is out of scope.

use std::{fs, path::Path};

use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use ctx_app::ports::{JiraArtifactSource, PortError};
use ctx_core::artifact::{
    Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkKind, ArtifactLinkTarget,
    ArtifactProvider,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use thiserror::Error;

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
}

impl UreqTransport {
    #[must_use]
    pub fn new(base_url: impl Into<String>, email: &str, token: &str) -> Self {
        let credentials = base64::engine::general_purpose::STANDARD.encode(format!(
            "{email}:{token}"
        ));
        Self {
            base_url: base_url.into(),
            authorization: format!("Basic {credentials}"),
        }
    }
}

impl JiraTransport for UreqTransport {
    fn get(&self, path: &str) -> Result<String, JiraError> {
        let url = format!("{}{path}", self.base_url);
        let mut response = ureq::get(&url)
            .header("Authorization", &self.authorization)
            .header("Accept", "application/json")
            .call()
            .map_err(|error| JiraError::Transport {
                path: path.to_owned(),
                message: error.to_string(),
            })?;
        response
            .body_mut()
            .read_to_string()
            .map_err(|error| JiraError::Transport {
                path: path.to_owned(),
                message: error.to_string(),
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

/// How far before a stored sync cursor to widen an incremental JQL
/// `updated >=` filter. Jira's JQL date literals have no timezone marker
/// and are interpreted in the *instance's own configured timezone*, not
/// UTC -- converting our UTC cursor naively could silently narrow the
/// window and miss issues updated between the intended cutoff and the
/// shifted one. A fixed margin comfortably wider than any real UTC offset
/// (max +/-14h) trades a bit of redundant, harmless re-fetch (re-ingesting
/// an unchanged issue is a no-op upsert) for the guarantee of never missing
/// an update because of a timezone mismatch we cannot observe from here.
const INCREMENTAL_SYNC_SAFETY_MARGIN_HOURS: i64 = 24;

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

    /// Fetches every issue for the configured project, or (when `since` is
    /// given) only those Jira reports as updated at or after that RFC3339
    /// timestamp, minus [`INCREMENTAL_SYNC_SAFETY_MARGIN_HOURS`] -- each
    /// with its own comments. Returns the normalized artifacts and the
    /// deterministic `CommentsOn` links between a comment and its issue.
    ///
    /// # Errors
    /// Returns [`JiraError`] when a request fails or its response is not
    /// valid JSON.
    pub fn fetch_issue_artifacts(
        &self,
        since: Option<&str>,
    ) -> Result<(Vec<Artifact>, Vec<ArtifactLink>), JiraError> {
        let mut artifacts = Vec::new();
        let mut links = Vec::new();
        let jql = self.build_jql(since);
        let mut start_at = 0u32;
        loop {
            let page: RawSearchResponse = self.get_json(&format!(
                "/rest/api/3/search?jql={}&startAt={start_at}&maxResults=100&fields=summary,description,creator,created,updated",
                encode_query_value(&jql)
            ))?;
            let fetched = page.issues.len();
            for issue in page.issues {
                let identity = Self::issue_identity(&issue.key);
                artifacts.push(self.issue_artifact(&identity, issue));
                let comments = self.fetch_comments(&identity.external_id)?;
                self.push_comments(&identity, comments, &mut artifacts, &mut links);
            }
            start_at += u32::try_from(fetched).unwrap_or(u32::MAX);
            if fetched == 0 || start_at >= page.total {
                break;
            }
        }
        Ok((artifacts, links))
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

    fn build_jql(&self, since: Option<&str>) -> String {
        let escaped_project = self.project.replace('"', "\\\"");
        match since.and_then(jql_lower_bound) {
            Some(lower_bound) => format!(
                "project = \"{escaped_project}\" AND updated >= \"{lower_bound}\" ORDER BY key ASC"
            ),
            None => format!("project = \"{escaped_project}\" ORDER BY key ASC"),
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

impl<T: JiraTransport> JiraArtifactSource for JiraClient<T> {
    fn issue_artifacts(
        &self,
        since: Option<&str>,
    ) -> Result<(Vec<Artifact>, Vec<ArtifactLink>), PortError> {
        self.fetch_issue_artifacts(since)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

/// Converts a stored RFC3339 sync cursor into Jira JQL's bare
/// `"yyyy/MM/dd HH:mm"` date literal, widened earlier by
/// [`INCREMENTAL_SYNC_SAFETY_MARGIN_HOURS`]. Returns `None` (falling back to
/// an unfiltered JQL query) only when the stored cursor fails to parse as
/// RFC3339 -- which never happens for a cursor this same module wrote,
/// since [`crate::jira`]'s ingest runner always passes through the RFC3339
/// `ingested_at` it was given.
fn jql_lower_bound(since: &str) -> Option<String> {
    let parsed = DateTime::parse_from_rfc3339(since).ok()?;
    let widened = parsed.with_timezone(&Utc) - Duration::hours(INCREMENTAL_SYNC_SAFETY_MARGIN_HOURS);
    Some(widened.format("%Y/%m/%d %H:%M").to_string())
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

    #[test]
    fn fetches_issues_with_their_comments() {
        let mut responses = BTreeMap::new();
        responses.insert(
            format!(
                "/rest/api/3/search?jql={}&startAt=0&maxResults=100&fields=summary,description,creator,created,updated",
                encode_query_value("project = \"PSI\" ORDER BY key ASC")
            ),
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

        let (artifacts, links) = client
            .fetch_issue_artifacts(None)
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
    fn adf_description_is_flattened_to_plain_text() {
        let adf: JsonValue = serde_json::from_str(
            r#"{"type":"doc","version":1,"content":[
                {"type":"paragraph","content":[{"type":"text","text":"First paragraph."}]},
                {"type":"paragraph","content":[{"type":"text","text":"Second, with a "},{"type":"text","text":"hard"},{"type":"hardBreak"},{"type":"text","text":"break."}]}
            ]}"#,
        )
        .expect("valid ADF fixture");

        let text = flatten_adf(&adf);

        assert_eq!(
            text,
            "First paragraph.\n\nSecond, with a hard\nbreak."
        );
    }

    #[test]
    fn a_sync_cursor_becomes_a_widened_jql_updated_filter() {
        let mut responses = BTreeMap::new();
        let expected_jql =
            "project = \"PSI\" AND updated >= \"2026/08/20 00:00\" ORDER BY key ASC";
        responses.insert(
            format!(
                "/rest/api/3/search?jql={}&startAt=0&maxResults=100&fields=summary,description,creator,created,updated",
                encode_query_value(expected_jql)
            ),
            r#"{"total":0,"issues":[]}"#.to_owned(),
        );
        let client = JiraClient::new(
            FakeTransport { responses },
            "PSI",
            "https://example.atlassian.net",
        );

        // The cursor is 2026-08-21T00:00:00Z; the 24h safety margin
        // (INCREMENTAL_SYNC_SAFETY_MARGIN_HOURS) widens the JQL lower bound
        // back to 2026-08-20T00:00.
        let (artifacts, links) = client
            .fetch_issue_artifacts(Some("2026-08-21T00:00:00Z"))
            .expect("incremental fetch");

        assert!(artifacts.is_empty());
        assert!(links.is_empty());
    }

    #[test]
    fn pagination_follows_start_at_until_exhausted() {
        let mut responses = BTreeMap::new();
        responses.insert(
            format!(
                "/rest/api/3/search?jql={}&startAt=0&maxResults=100&fields=summary,description,creator,created,updated",
                encode_query_value("project = \"PSI\" ORDER BY key ASC")
            ),
            r#"{"total":2,"issues":[{"key":"PSI-1","fields":{"summary":"First","description":null,"creator":null,"created":null,"updated":null}}]}"#
                .to_owned(),
        );
        responses.insert(
            "/rest/api/3/issue/PSI-1/comment?startAt=0&maxResults=100".to_owned(),
            r#"{"total":0,"comments":[]}"#.to_owned(),
        );
        responses.insert(
            format!(
                "/rest/api/3/search?jql={}&startAt=1&maxResults=100&fields=summary,description,creator,created,updated",
                encode_query_value("project = \"PSI\" ORDER BY key ASC")
            ),
            r#"{"total":2,"issues":[{"key":"PSI-2","fields":{"summary":"Second","description":null,"creator":null,"created":null,"updated":null}}]}"#
                .to_owned(),
        );
        responses.insert(
            "/rest/api/3/issue/PSI-2/comment?startAt=0&maxResults=100".to_owned(),
            r#"{"total":0,"comments":[]}"#.to_owned(),
        );
        let client = JiraClient::new(
            FakeTransport { responses },
            "PSI",
            "https://example.atlassian.net",
        );

        let (artifacts, _links) = client.fetch_issue_artifacts(None).expect("paginated fetch");

        let mut keys: Vec<_> = artifacts
            .iter()
            .filter(|artifact| artifact.identity.kind == ArtifactKind::Issue)
            .map(|artifact| artifact.identity.external_id.clone())
            .collect();
        keys.sort();
        assert_eq!(keys, vec!["PSI-1".to_owned(), "PSI-2".to_owned()]);
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
