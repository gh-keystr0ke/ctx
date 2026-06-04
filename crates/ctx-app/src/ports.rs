use std::fmt;

use ctx_core::{
    business::{BusinessDocument, ContextImportStats},
    domain::{CommitOid, RepositoryId},
    graph::GraphSnapshot,
    indexing::{FileChange, IndexPlan, RepositorySnapshot},
    ir::FileAnalysis,
    verification::{SemanticCandidate, VerificationDecision},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortError {
    message: String,
}

impl PortError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for PortError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitMetadata {
    pub oid: CommitOid,
    pub parent_oid: Option<CommitOid>,
    pub authored_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryDescriptor {
    pub id: RepositoryId,
    pub root_path: String,
    pub remote_url: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepositoryStatus {
    pub last_indexed_commit: Option<CommitOid>,
    pub files: usize,
    pub symbols: usize,
    pub active_edges: usize,
    pub stale_semantic_edges: usize,
}

pub trait GitRepository {
    /// Returns the repository's stable local descriptor.
    ///
    /// # Errors
    /// Returns [`PortError`] when repository metadata cannot be inspected.
    fn descriptor(&self) -> Result<RepositoryDescriptor, PortError>;
    /// Returns metadata for the current `HEAD` commit.
    ///
    /// # Errors
    /// Returns [`PortError`] when `HEAD` cannot be inspected or is invalid.
    fn head(&self) -> Result<CommitMetadata, PortError>;
    /// Lists supported source files in deterministic order.
    ///
    /// # Errors
    /// Returns [`PortError`] when tracked files cannot be enumerated.
    fn all_source_files(&self) -> Result<Vec<String>, PortError>;
    /// Returns supported source-file changes from `oid` through `HEAD`.
    ///
    /// # Errors
    /// Returns [`PortError`] when the diff cannot be read or parsed.
    fn changes_since(&self, oid: &CommitOid) -> Result<Vec<FileChange>, PortError>;
}

pub trait LanguageAnalyzer {
    /// Produces normalized IR for a complete source-file version.
    ///
    /// # Errors
    /// Returns [`PortError`] when the source cannot be read or parsed.
    fn analyze(&self, relative_path: &str) -> Result<FileAnalysis, PortError>;
    /// Produces normalized IR for caller-supplied source text.
    ///
    /// # Errors
    /// Returns [`PortError`] when the source cannot be parsed.
    fn analyze_text(&self, relative_path: &str, source: &str) -> Result<FileAnalysis, PortError>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReviewChangeSet {
    pub source_changes: Vec<FileChange>,
    pub changed_context_files: Vec<String>,
}

pub trait ReviewRepository {
    /// Returns source and context changes between `base` and the working tree.
    ///
    /// # Errors
    /// Returns [`PortError`] when Git cannot resolve or diff the base.
    fn review_changes(&self, base: &str) -> Result<ReviewChangeSet, PortError>;
    /// Returns a file as it existed at `revision`, or `None` when absent.
    ///
    /// # Errors
    /// Returns [`PortError`] when Git cannot read repository data.
    fn source_at(&self, revision: &str, path: &str) -> Result<Option<String>, PortError>;
}

pub trait BusinessContextReader {
    /// Reads every supported business-context document in stable order.
    ///
    /// # Errors
    /// Returns [`PortError`] when a document cannot be read, parsed, or
    /// normalized.
    fn read_all(&self) -> Result<Vec<BusinessDocument>, PortError>;
}

pub trait BusinessContextStore {
    /// Synchronizes the complete context snapshot and explicit claims.
    ///
    /// # Errors
    /// Returns [`PortError`] when documents, provenance, or claims cannot be
    /// persisted atomically.
    fn sync_context(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        indexed_at: &str,
        documents: &[BusinessDocument],
    ) -> Result<ContextImportStats, PortError>;
}

pub trait GraphStore {
    /// Loads the current version of all nodes, claims, and evidence.
    ///
    /// # Errors
    /// Returns [`PortError`] when graph state cannot be read or decoded.
    fn load_graph(&self, repository: &RepositoryId) -> Result<GraphSnapshot, PortError>;
}

pub trait VerificationStore {
    /// Persists the original inference, human annotation, and—on acceptance—a
    /// separate human assertion.
    ///
    /// # Errors
    /// Returns [`PortError`] when the atomic verification record cannot be
    /// persisted.
    fn record_verification(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        candidate: &SemanticCandidate,
        decision: VerificationDecision,
        author: &str,
        timestamp: &str,
    ) -> Result<(), PortError>;
}

pub trait IndexStore {
    /// Ensures local repository metadata exists.
    ///
    /// # Errors
    /// Returns [`PortError`] when metadata cannot be persisted.
    fn ensure_repository(
        &mut self,
        repository: &RepositoryDescriptor,
        created_at: &str,
    ) -> Result<(), PortError>;
    /// Loads the last successfully indexed commit.
    ///
    /// # Errors
    /// Returns [`PortError`] when commit metadata cannot be read.
    fn latest_commit(&self, repository: &RepositoryId)
    -> Result<Option<CommitMetadata>, PortError>;
    /// Loads current file and symbol versions for pure planning.
    ///
    /// # Errors
    /// Returns [`PortError`] when stored state is inaccessible or invalid.
    fn load_snapshot(&self, repository: &RepositoryId) -> Result<RepositorySnapshot, PortError>;
    /// Applies a complete plan and commit marker atomically.
    ///
    /// # Errors
    /// Returns [`PortError`] when any persistence effect fails.
    fn apply_index(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        indexed_at: &str,
        plan: &IndexPlan,
    ) -> Result<(), PortError>;
    /// Returns current index health counters.
    ///
    /// # Errors
    /// Returns [`PortError`] when counters cannot be queried.
    fn status(&self, repository: &RepositoryId) -> Result<RepositoryStatus, PortError>;
}
