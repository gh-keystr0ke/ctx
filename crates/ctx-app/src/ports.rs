use std::fmt;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ctx_core::{
    artifact::{
        Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactProvider, ArtifactRef,
    },
    business::{BusinessDocument, ContextImportStats},
    domain::{CommitOid, RepositoryId},
    graph::GraphSnapshot,
    indexing::{FileChange, IndexPlan, RepositorySnapshot},
    ir::FileAnalysis,
    knowledge::{
        AcceptedKnowledgeRecord, AgentOutcome, ClusterReview, KnowledgeCandidate, KnowledgeDecision,
    },
    neighborhood::ArtifactNeighborhood,
    verification::{SemanticCandidate, StaleClaim, StaleClaimVerdict, VerificationDecision},
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
    pub db_entities: usize,
    pub features: usize,
    pub requirements: usize,
    pub invariants: usize,
    pub decisions: usize,
    pub public_documents: usize,
    pub active_edges: usize,
    pub structural_facts: usize,
    pub active_assertions: usize,
    pub active_inferences: usize,
    pub stale_semantic_edges: usize,
    pub rejected_semantic_edges: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceScope {
    pub languages: Vec<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
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
    /// Lists uncommitted source or business-context files that would make a
    /// commit-labelled index misleading.
    ///
    /// # Errors
    /// Returns [`PortError`] when working-tree state cannot be inspected.
    fn uncommitted_index_inputs(&self) -> Result<Vec<String>, PortError>;
    /// Returns the effective deterministic source configuration.
    fn source_scope(&self) -> SourceScope;
}

pub trait LanguageAnalyzer {
    /// Returns the cache/schema version for the analyzer selected for a path.
    ///
    /// # Errors
    /// Returns [`PortError`] when no enabled analyzer handles the path.
    fn analysis_version(&self, relative_path: &str) -> Result<String, PortError>;
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

#[derive(Clone, Debug, Default, PartialEq)]
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

/// One branch artifact together with the commits that branch alone
/// contains relative to the repository's default branch. Deliberately not
/// "every commit reachable from its tip" -- for the default branch itself
/// that would be the whole repository history.
#[derive(Clone, Debug, PartialEq)]
pub struct BranchArtifact {
    pub artifact: Artifact,
    /// Newest-first commit OIDs unique to this branch. Empty for the
    /// default branch and for any branch when no default branch could be
    /// resolved.
    pub own_commits: Vec<CommitOid>,
}

/// One commit artifact together with the source files that commit itself
/// changed, read in the same history walk that produced the artifact —
/// never a per-commit subprocess.
#[derive(Clone, Debug, PartialEq)]
pub struct CommitArtifact {
    pub artifact: Artifact,
    pub changed_paths: BTreeSet<String>,
}

/// Reads Git-native development artifacts — commit messages and branch
/// names — as normalized [`Artifact`]s (prompt3.md PR-EXT-001 MUST list),
/// with no network or provider account required.
pub trait GitArtifactSource {
    /// Returns one entry per commit reachable from `HEAD`, or from `HEAD`
    /// back to (exclusive of) `since` when given, in deterministic
    /// (newest-first) order.
    ///
    /// # Errors
    /// Returns [`PortError`] when Git history cannot be read.
    fn commit_artifacts(&self, since: Option<&CommitOid>)
    -> Result<Vec<CommitArtifact>, PortError>;

    /// Returns one entry per local branch, in deterministic (name-sorted)
    /// order.
    ///
    /// # Errors
    /// Returns [`PortError`] when Git refs cannot be read.
    fn branch_artifacts(&self) -> Result<Vec<BranchArtifact>, PortError>;
}

/// Provider-independent input modes supported by external artifact sources.
/// A connector declares its behavior by accepting the appropriate variant;
/// adding another provider no longer requires adding another app-layer port.
#[derive(Clone, Copy, Debug)]
pub enum ExternalArtifactRequest<'a> {
    /// Fetch everything updated at or after an optional RFC3339 cursor.
    UpdatedSince(Option<&'a str>),
    /// Fetch the explicitly referenced tracker keys and provider-linked
    /// neighbors allowed by that connector's bounded traversal policy.
    ReferencedKeys(&'a BTreeSet<String>),
}

/// Normalized output shared by every network artifact connector.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExternalArtifactBatch {
    pub artifacts: Vec<Artifact>,
    pub links: Vec<ArtifactLink>,
}

/// Reads provider artifacts behind one normalized application port.
/// Provider-specific authentication, pagination and response shapes remain
/// adapter concerns; runners only select a request mode and persist a batch.
pub trait ExternalArtifactSource {
    /// # Errors
    /// Returns [`PortError`] when the request mode is unsupported, a provider
    /// request fails, or its response is invalid.
    fn fetch(
        &self,
        request: ExternalArtifactRequest<'_>,
    ) -> Result<ExternalArtifactBatch, PortError>;
}

/// Per-provider "last synced at" cursor (prompt3.md PR-INCR-001, T8.1):
/// lets `ctx ingest <source>` ask a provider for only what changed since the
/// previous run instead of re-fetching everything every time, mirroring
/// [`IndexStore::latest_commit`]'s pattern for Git itself. Distinct from
/// [`ArtifactRepository`]'s per-artifact analysis ledger (Phase 8's
/// `REQ-INCR-002`): this cursor is about what to *ask a provider for*, not
/// whether to re-analyze what is already stored.
pub trait IngestCursorStore {
    /// # Errors
    /// Returns [`PortError`] when the stored cursor cannot be read.
    fn sync_cursor(
        &self,
        repository: &RepositoryId,
        provider: &str,
    ) -> Result<Option<String>, PortError>;

    /// # Errors
    /// Returns [`PortError`] when the cursor cannot be persisted.
    fn set_sync_cursor(
        &mut self,
        repository: &RepositoryId,
        provider: &str,
        cursor: &str,
    ) -> Result<(), PortError>;
}

/// Persists raw external artifacts (prompt3.md PR-EXT-*), kept separate from
/// the graph so an imported artifact never automatically becomes product
/// knowledge (PR-EXT-002).
pub trait ArtifactRepository {
    /// Idempotently persists one artifact keyed by its identity
    /// (`provider`, `kind`, `external_id`): re-syncing the same external
    /// object versions the stored record rather than creating a logically
    /// new artifact (PR-EXT-003).
    ///
    /// # Errors
    /// Returns [`PortError`] when the artifact cannot be persisted.
    fn upsert_artifact(
        &mut self,
        repository: &RepositoryId,
        artifact: &Artifact,
        ingested_at: &str,
        ingest_version: &str,
    ) -> Result<(), PortError>;

    /// Lists every artifact currently stored for a repository, in
    /// deterministic order.
    ///
    /// # Errors
    /// Returns [`PortError`] when stored artifacts cannot be read.
    fn list_artifacts(&self, repository: &RepositoryId) -> Result<Vec<Artifact>, PortError>;

    /// Records that `identity` was analyzed by `ctx enrich` with the complete
    /// `input_fingerprint`. `content_hash` is retained separately for audit;
    /// incremental skipping keys on the complete input so changed links or
    /// neighboring artifacts trigger re-analysis even when the subject text
    /// itself is unchanged.
    ///
    /// # Errors
    /// Returns [`PortError`] when the record cannot be persisted.
    fn mark_analyzed(
        &mut self,
        repository: &RepositoryId,
        identity: &ArtifactIdentity,
        content_hash: &str,
        input_fingerprint: &str,
        analyzed_at: &str,
    ) -> Result<(), PortError>;

    /// The complete input fingerprint each artifact was last analyzed with,
    /// keyed by identity.
    ///
    /// # Errors
    /// Returns [`PortError`] when stored analysis records cannot be read.
    fn analyzed_input_fingerprints(
        &self,
        repository: &RepositoryId,
    ) -> Result<HashMap<ArtifactIdentity, String>, PortError>;
}

/// Persists deterministic, non-AI relationships between artifacts or
/// between an artifact and code (PR-LINK-001/002).
pub trait ArtifactLinkStore {
    /// Persists links, skipping any already-stored identical link so
    /// repeated ingestion of the same artifact stays idempotent.
    ///
    /// # Errors
    /// Returns [`PortError`] when links cannot be persisted.
    fn persist_links(
        &mut self,
        repository: &RepositoryId,
        links: &[ArtifactLink],
    ) -> Result<(), PortError>;

    /// Lists every stored link for a repository, in deterministic order.
    ///
    /// # Errors
    /// Returns [`PortError`] when stored links cannot be read.
    fn list_links(&self, repository: &RepositoryId) -> Result<Vec<ArtifactLink>, PortError>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtifactReconcileReport {
    pub removed: Vec<ArtifactIdentity>,
}

/// Destructive artifact maintenance is deliberately separated from ordinary
/// upsert/read operations. Callers must opt into this port when they own a
/// complete provider snapshot or an explicit prune plan.
pub trait ArtifactMaintenanceStore {
    /// Replaces every outgoing link for one stored source artifact as one
    /// transaction. Links targeting unknown artifacts or nodes are skipped,
    /// matching [`ArtifactLinkStore::persist_links`].
    ///
    /// # Errors
    /// Returns [`PortError`] when links cannot be reconciled.
    fn replace_outgoing_links(
        &mut self,
        repository: &RepositoryId,
        source: &ArtifactIdentity,
        links: &[ArtifactLink],
    ) -> Result<(), PortError>;

    /// Reconciles a complete provider/kind snapshot, deleting stored
    /// artifacts in that scope which are absent from `current`.
    ///
    /// # Errors
    /// Returns [`PortError`] when the snapshot cannot be reconciled.
    fn reconcile_snapshot(
        &mut self,
        repository: &RepositoryId,
        provider: ArtifactProvider,
        kinds: &[ArtifactKind],
        current: &HashSet<ArtifactIdentity>,
    ) -> Result<ArtifactReconcileReport, PortError>;

    /// Deletes exactly the named stored artifacts and all incoming/outgoing
    /// artifact links and analysis-ledger rows that depend on them.
    ///
    /// # Errors
    /// Returns [`PortError`] when deletion cannot be completed atomically.
    fn delete_artifacts(
        &mut self,
        repository: &RepositoryId,
        identities: &[ArtifactIdentity],
    ) -> Result<ArtifactReconcileReport, PortError>;
}

/// Persists AI-derived typed knowledge candidates awaiting human
/// verification (PR-VERIFY-001/002). Distinct from the heuristic
/// [`SemanticCandidate`] pipeline, which is cheap enough to recompute on
/// demand; an agent call is not, so its output must survive between runs.
pub trait KnowledgeCandidateStore {
    /// Idempotently persists candidates keyed by `fingerprint`: an already
    /// `accepted`/`rejected` candidate is left untouched rather than
    /// reverted to `pending` by a later re-analysis (PR-INCR-001/002).
    ///
    /// # Errors
    /// Returns [`PortError`] when candidates cannot be persisted.
    fn upsert_candidates(
        &mut self,
        repository: &RepositoryId,
        candidates: &[KnowledgeCandidate],
    ) -> Result<(), PortError>;

    /// Lists every candidate still awaiting a human decision, in
    /// deterministic order.
    ///
    /// # Errors
    /// Returns [`PortError`] when stored candidates cannot be read.
    fn pending_candidates(
        &self,
        repository: &RepositoryId,
    ) -> Result<Vec<KnowledgeCandidate>, PortError>;

    /// Records a human decision on a still-pending candidate (PR-VERIFY-001).
    /// The candidate row survives with `status` set and, for an accept, the
    /// resulting document's ID attached (PR-VERIFY-002) — it is never
    /// deleted.
    ///
    /// # Errors
    /// Returns [`PortError`] when `fingerprint` is not currently pending or
    /// the decision cannot be persisted.
    fn record_decision(
        &mut self,
        repository: &RepositoryId,
        fingerprint: &str,
        decision: &KnowledgeDecision,
        author: &str,
        timestamp: &str,
    ) -> Result<(), PortError>;

    /// The evidence artifacts behind every currently *accepted* candidate,
    /// keyed by the resulting document's ID (prompt3.md PR-MAP-001): lets a
    /// heuristic implementation-mapping pass see which artifact backs an
    /// AI-derived intent, without a hand-authored `.context/*.yaml` intent
    /// (which has no candidate row at all) needing any special case.
    ///
    /// # Errors
    /// Returns [`PortError`] when stored candidates cannot be read.
    fn accepted_evidence(
        &self,
        repository: &RepositoryId,
    ) -> Result<BTreeMap<String, Vec<ArtifactRef>>, PortError>;

    /// The accepted candidate and human decision behind `document_id`, if
    /// it originated from `ctx verify --knowledge --accept` (prompt3.md
    /// §16/§19, Phase 9) rather than a hand-authored `.context/*.yaml` file.
    ///
    /// # Errors
    /// Returns [`PortError`] when stored candidates cannot be read.
    fn accepted_record_for_document(
        &self,
        repository: &RepositoryId,
        document_id: &str,
    ) -> Result<Option<AcceptedKnowledgeRecord>, PortError>;
}

/// Materializes an accepted [`KnowledgeCandidate`] as a new `.context/*.yaml`
/// file (prompt3.md's own recommendation, ADR-EXT/PR-VERIFY-002): the
/// existing `BusinessContextReader`/`ContextImporter` path stays the single
/// source of truth for product knowledge, so an accepted candidate needs no
/// second, parallel storage mechanism to become visible to `ctx impact`/`ctx
/// explain`/`ctx review` -- the very next `ctx index` picks it up like any
/// hand-authored document.
pub trait BusinessContextWriter {
    /// # Errors
    /// Returns [`PortError`] when `document.id` is already used by an
    /// existing file, or the file cannot be written.
    fn write_document(&self, document: &BusinessDocument) -> Result<String, PortError>;
}

/// The interchangeable AI-agent boundary (prompt3.md PR-AGENT-001): every
/// concrete agent -- Claude Code CLI today, any other CLI- or API-backed
/// model later -- is referenced only through this trait outside its own
/// adapter module (PR-P05). `ctx-core`/`ctx-app` never name a specific
/// vendor or model.
pub trait SemanticAgent {
    /// Analyzes one bounded artifact neighborhood and returns what the agent
    /// found (PR-AI-002). Absence of extracted knowledge (`NotRelevant`/
    /// `InsufficientEvidence`) is always preferred to a fabricated candidate
    /// (PR-P02, FR-01). Evidence artifact-id grounding is always strict;
    /// `allow_ungrounded_symbols` only relaxes which implementation/test
    /// candidate paths are accepted, letting the agent name paths outside
    /// the neighborhood's changed symbols/nearby tests when set.
    ///
    /// # Errors
    /// Returns [`PortError`] when the agent cannot be reached, or its output
    /// cannot be parsed and validated as the expected contract.
    fn analyze(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
        allow_ungrounded_symbols: bool,
    ) -> Result<AgentOutcome, PortError>;
}

/// The interchangeable AI-agent boundary for `ctx verify --knowledge
/// --auto`'s independent second-opinion review -- deliberately a sibling
/// trait to [`SemanticAgent`], not a shared one: extraction decides "does
/// this bounded neighborhood state new knowledge," review decides "should
/// this already-extracted candidate actually be accepted," a genuinely
/// different question with a different input shape (a candidate cluster,
/// not an artifact neighborhood) and a different output shape (per-candidate
/// verdicts plus an optional merge, not new candidates).
pub trait KnowledgeReviewAgent {
    /// Independently reviews one [`ctx_core::verification::CandidateCluster`]'s
    /// candidates and returns a verdict on each, plus an optional merged
    /// statement when two or more accepted candidates genuinely restate the
    /// same knowledge.
    ///
    /// # Errors
    /// Returns [`PortError`] when the agent cannot be reached, or its output
    /// cannot be parsed and validated as the expected contract.
    fn review(&self, candidates: &[KnowledgeCandidate]) -> Result<ClusterReview, PortError>;
}

/// The interchangeable AI-agent boundary for `ctx verify --stale`'s
/// independent re-review of already-stale semantic claims -- another
/// sibling to [`SemanticAgent`]/[`KnowledgeReviewAgent`], not a shared
/// trait: this reviews claims that already exist and once held, deciding
/// whether current code still supports them, not whether to extract or
/// accept something new.
pub trait StaleClaimReviewAgent {
    /// Independently reviews every claim in `claims` (typically every
    /// currently stale semantic relationship in one repository) and returns
    /// one verdict per fingerprint, with reasoning.
    ///
    /// # Errors
    /// Returns [`PortError`] when the agent cannot be reached, or its output
    /// cannot be parsed and validated as the expected contract.
    fn review_stale_claims(
        &self,
        claims: &[StaleClaim],
    ) -> Result<Vec<StaleClaimVerdict>, PortError>;
}

/// Applies a [`StaleClaimReviewAgent`]'s binding `Accept` verdicts: a stale
/// claim's own storage-layer reactivation, precise to the one edge
/// identified by `fingerprint` -- never a whole-document re-touch (which
/// would refresh every other claim that document happens to declare too,
/// stale or not). A `Reject` verdict is never applied through this trait at
/// all; it is only ever surfaced to a human as a suggestion.
pub trait StaleClaimStore {
    /// Reactivates the stale edge identified by `fingerprint`, if one still
    /// exists in that state, and records an audit annotation. Returns
    /// `false` (not an error) when no matching stale edge is found -- for
    /// example, a concurrent `ctx index` already changed it -- since a
    /// caller reviewing a possibly-stale snapshot should not treat that as
    /// fatal.
    ///
    /// # Errors
    /// Returns [`PortError`] when the reactivation cannot be persisted.
    fn reactivate_stale_claim(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        fingerprint: &str,
        reviewer: &str,
        reasoning: &str,
        timestamp: &str,
    ) -> Result<bool, PortError>;
}
