//! External development-artifact model (prompt3.md PR-EXT-*): a normalized
//! representation of source material that already exists in a team's
//! development history — commits, branches, GitLab issues/merge requests and
//! their comments, code comments/docstrings — kept deliberately separate
//! from [`crate::domain::Node`]. An imported artifact never automatically
//! becomes a Feature/Requirement/Invariant/Decision (PR-EXT-002); it is raw
//! source material other passes may derive candidates from.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactProvider {
    Git,
    GitLab,
    GitHub,
    Jira,
    Code,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Commit,
    Branch,
    Issue,
    MergeRequest,
    PullRequest,
    Comment,
    ReviewComment,
    CodeComment,
    Docstring,
    Documentation,
}

/// Identifies one external object independently of any local database row,
/// so re-syncing the same object never creates a logically new artifact
/// (PR-EXT-003).
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ArtifactIdentity {
    pub provider: ArtifactProvider,
    pub kind: ArtifactKind,
    /// The provider's own identifier for this object (a commit OID, a
    /// GitLab issue/MR IID, a stable per-file/symbol/line locator for a
    /// code comment). Unique only in combination with `provider` and `kind`.
    pub external_id: String,
}

/// A normalized external artifact with its own provenance identity
/// (PR-EXT-003). Field set is provider-agnostic; a missing provider
/// integration never requires changing this shape (PR-EXT-001).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub identity: ArtifactIdentity,
    /// The repository/project the artifact belongs to, as the provider
    /// names it (a GitLab `namespace/project` path, a Jira project key).
    pub project: crate::domain::Project,
    pub title: String,
    pub body: String,
    pub author: Option<String>,
    pub external_created_at: Option<crate::domain::Timestamp>,
    pub external_updated_at: Option<crate::domain::Timestamp>,
    /// Where a human could go look at the original (a URL, a `path#Lstart-Lend`
    /// locator for a code comment).
    pub source_locator: crate::domain::Url,
    pub content_hash: String,
}

/// A pointer into one artifact's content, used as candidate evidence
/// (PR-AI-004: evidence must be a concrete excerpt, not free-form reasoning).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub identity: ArtifactIdentity,
    pub locator: String,
    pub excerpt: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactLinkKind {
    /// A merge/pull request's changeset includes this commit.
    ContainsCommit,
    /// One artifact's text names a deterministic reference to another
    /// (PR-LINK-002) without asserting what the reference means
    /// (PR-LINK-003/004) — for example a branch name containing a ticket
    /// key, or an MR body mentioning `#482`.
    References,
    /// The artifact's changeset touched this code symbol (a structural fact
    /// from already-indexed diff data, not an inference).
    ChangedSymbol,
    /// A code comment or docstring discusses the nearest enclosing symbol
    /// (PR-CODEDOC-002).
    Discusses,
    /// A comment/review-comment artifact belongs to the issue/merge-request
    /// it was posted on — a structural fact reported directly by the
    /// provider's own API, not a text-derived reference.
    CommentsOn,
    /// One issue is linked to another by the tracker's own reported
    /// relationship (Jira `issuelinks`/`parent`: blocks, relates to,
    /// subtask-of, epic-parent) — a structural fact from the provider's own
    /// API, not a text-derived reference, used to justify ingesting an
    /// issue that a repository's own artifacts never directly mention.
    RelatedIssue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ArtifactLinkTarget {
    Artifact(ArtifactIdentity),
    CodeSymbol(crate::domain::StableKey),
}

/// A deterministic, non-AI relationship between two artifacts or an
/// artifact and code (PR-LINK-001/002, PR-P01): established from evidence
/// literally present in the artifacts, never from AI inference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactLink {
    pub source: ArtifactIdentity,
    pub target: ArtifactLinkTarget,
    pub kind: ArtifactLinkKind,
    /// The literal text/locator that grounds this link (a matched ticket
    /// reference, a changed file path) so it stays auditable.
    pub evidence_locator: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_equality_ignores_unrelated_fields() {
        let identity = ArtifactIdentity {
            provider: ArtifactProvider::GitLab,
            kind: ArtifactKind::MergeRequest,
            external_id: "842".to_owned(),
        };
        let same = ArtifactIdentity {
            provider: ArtifactProvider::GitLab,
            kind: ArtifactKind::MergeRequest,
            external_id: "842".to_owned(),
        };
        let different_kind = ArtifactIdentity {
            kind: ArtifactKind::Issue,
            ..same.clone()
        };
        assert_eq!(identity, same);
        assert_ne!(identity, different_kind);
    }
}
