//! Orchestrates AI-agent-assisted knowledge extraction (prompt3.md
//! PR-AI-*): for each currently known external artifact not already cited as
//! evidence by a pending candidate, not already covered by a known parent
//! artifact, and whose content actually changed since its last analysis
//! (PR-INCR-002 basic level), assembles its bounded neighborhood
//! ([`ctx_core::neighborhood::build_neighborhood`]) and hands it to a
//! [`SemanticAgent`]. A candidate the agent proposes is persisted as
//! `pending` -- never auto-promoted to fact (PR-P02) -- for a human to
//! decide through the existing verification flow (Phase 6). Every analyzed
//! artifact is marked in the ledger regardless of outcome, so a
//! `not_relevant`/`insufficient_evidence` answer is never re-asked of the
//! agent on unchanged content either.
//!
//! A `Comment`/`ReviewComment` structurally `CommentsOn` its issue/MR, and a
//! `Commit` is always `ContainsCommit`-linked from the branch or merge
//! request that carries it, once that ingest has run -- a lone comment or
//! commit read in isolation is rarely meaningful, and its text is already
//! part of its parent's own neighborhood (`build_neighborhood` reads links
//! in either direction). Spending a separate agent call on it is mostly
//! redundant with the call already spent on its parent, so `run_with_progress`
//! skips it as its own analysis subject whenever that parent is already
//! known -- never when it isn't, since an orphaned comment/commit (ingested
//! before its parent, or one whose parent was never ingested at all) is its
//! only chance to be analyzed at all. A `Branch` is skipped the same way
//! only when a known merge/pull request already names it as `source_branch`
//! ([`is_covered_by_a_known_parent`]).

use std::collections::HashSet;

use ctx_core::{
    artifact::{Artifact, ArtifactKind, ArtifactLink, ArtifactLinkKind, ArtifactLinkTarget},
    domain::RepositoryId,
    knowledge::AgentOutcome,
    neighborhood::build_neighborhood,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::{
    ArtifactLinkStore, ArtifactRepository, GraphStore, KnowledgeCandidateStore, PortError,
    SemanticAgent,
};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnrichReport {
    pub neighborhoods_analyzed: usize,
    pub candidates_proposed: usize,
    pub artifacts_skipped_covered_by_parent: usize,
    pub artifacts_skipped_already_pending: usize,
    pub artifacts_skipped_unchanged: usize,
}

#[derive(Debug, Error)]
pub enum EnrichError {
    #[error("stored state could not be read: {0}")]
    Read(PortError),
    #[error("agent analysis failed: {0}")]
    Agent(PortError),
    #[error("candidates could not be persisted: {0}")]
    Store(PortError),
}

pub struct EnrichRunner<'a, A, S> {
    agent: &'a A,
    store: &'a mut S,
}

impl<'a, A, S> EnrichRunner<'a, A, S>
where
    A: SemanticAgent,
    S: ArtifactRepository + ArtifactLinkStore + KnowledgeCandidateStore + GraphStore,
{
    pub const fn new(agent: &'a A, store: &'a mut S) -> Self {
        Self { agent, store }
    }

    /// # Errors
    /// Returns [`EnrichError`] when stored state cannot be read, the agent
    /// fails, or resulting candidates cannot be persisted.
    pub fn run(
        &mut self,
        repository: &RepositoryId,
        produced_at: &str,
        allow_ungrounded_symbols: bool,
    ) -> Result<EnrichReport, EnrichError> {
        self.run_with_progress(
            repository,
            produced_at,
            allow_ungrounded_symbols,
            &mut |_, _, _| {},
        )
    }

    /// Same as [`Self::run`], but calls `on_progress(position, total,
    /// subject)` immediately before each real agent call -- the slow step,
    /// typically a real subprocess/LLM round trip -- so a caller with many
    /// ingested artifacts can show the user it is still making progress
    /// rather than looking indistinguishable from a hang. `position` and
    /// `total` are 1-based positions within the full set of known
    /// artifacts, including ones this run will end up skipping, so the
    /// count is stable and meaningful even while skipping.
    ///
    /// # Errors
    /// Returns [`EnrichError`] when stored state cannot be read, the agent
    /// fails, or resulting candidates cannot be persisted.
    pub fn run_with_progress(
        &mut self,
        repository: &RepositoryId,
        produced_at: &str,
        allow_ungrounded_symbols: bool,
        on_progress: &mut dyn FnMut(usize, usize, &Artifact),
    ) -> Result<EnrichReport, EnrichError> {
        let known_artifacts = self
            .store
            .list_artifacts(repository)
            .map_err(EnrichError::Read)?;
        let links = self
            .store
            .list_links(repository)
            .map_err(EnrichError::Read)?;
        let graph = self
            .store
            .load_graph(repository)
            .map_err(EnrichError::Read)?;
        let already_pending: HashSet<_> = self
            .store
            .pending_candidates(repository)
            .map_err(EnrichError::Read)?
            .iter()
            .flat_map(|candidate| candidate.evidence.iter())
            .map(|evidence| evidence.identity.clone())
            .collect();
        let analyzed_content_hashes = self
            .store
            .analyzed_content_hashes(repository)
            .map_err(EnrichError::Read)?;

        let total = known_artifacts.len();
        let mut report = EnrichReport::default();
        let mut proposed = Vec::new();
        for (index, subject) in known_artifacts.iter().enumerate() {
            if already_pending.contains(&subject.identity) {
                report.artifacts_skipped_already_pending += 1;
                continue;
            }
            if is_covered_by_a_known_parent(subject, &links, &known_artifacts) {
                report.artifacts_skipped_covered_by_parent += 1;
                continue;
            }
            if analyzed_content_hashes.get(&subject.identity) == Some(&subject.content_hash) {
                report.artifacts_skipped_unchanged += 1;
                continue;
            }
            on_progress(index + 1, total, subject);
            let neighborhood = build_neighborhood(subject, &links, &known_artifacts, &graph);
            let outcome = self
                .agent
                .analyze(&neighborhood, produced_at, allow_ungrounded_symbols)
                .map_err(EnrichError::Agent)?;
            report.neighborhoods_analyzed += 1;
            self.store
                .mark_analyzed(
                    repository,
                    &subject.identity,
                    &subject.content_hash,
                    produced_at,
                )
                .map_err(EnrichError::Store)?;
            if let AgentOutcome::Relevant(candidates) = outcome {
                report.candidates_proposed += candidates.len();
                proposed.extend(candidates);
            }
        }
        self.store
            .upsert_candidates(repository, &proposed)
            .map_err(EnrichError::Store)?;
        Ok(report)
    }
}

/// Whether `subject` is a leaf kind whose text is already part of a known
/// richer parent's own neighborhood, so analyzing it separately would be
/// mostly redundant with the call already spent on that parent -- see the
/// module doc comment. Never true for a kind with no such structural
/// relationship (an `Issue`, `Documentation`, `CodeComment`, ...), and never
/// true when the parent itself isn't among `known_artifacts` (an orphan
/// gets its own chance to be analyzed).
fn is_covered_by_a_known_parent(
    subject: &Artifact,
    links: &[ArtifactLink],
    known_artifacts: &[Artifact],
) -> bool {
    let has_known_counterpart =
        |identity| known_artifacts.iter().any(|artifact| &artifact.identity == identity);
    match subject.identity.kind {
        ArtifactKind::Comment | ArtifactKind::ReviewComment => links.iter().any(|link| {
            link.source == subject.identity
                && link.kind == ArtifactLinkKind::CommentsOn
                && matches!(&link.target, ArtifactLinkTarget::Artifact(parent) if has_known_counterpart(parent))
        }),
        ArtifactKind::Commit => links.iter().any(|link| {
            link.kind == ArtifactLinkKind::ContainsCommit
                && link.target == ArtifactLinkTarget::Artifact(subject.identity.clone())
                && has_known_counterpart(&link.source)
        }),
        ArtifactKind::Branch => links.iter().any(|link| {
            link.kind == ArtifactLinkKind::References
                && link.target == ArtifactLinkTarget::Artifact(subject.identity.clone())
                && matches!(
                    link.source.kind,
                    ArtifactKind::MergeRequest | ArtifactKind::PullRequest
                )
                && has_known_counterpart(&link.source)
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use ctx_core::{
        artifact::{Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactProvider},
        business::BusinessKind,
        graph::GraphSnapshot,
        knowledge::{AgentProvenance, KnowledgeCandidate},
    };

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        artifacts: Vec<Artifact>,
        links: Vec<ArtifactLink>,
        pending: RefCell<Vec<KnowledgeCandidate>>,
        analyzed: RefCell<std::collections::HashMap<ArtifactIdentity, String>>,
    }

    impl ArtifactRepository for FakeStore {
        fn upsert_artifact(
            &mut self,
            _repository: &RepositoryId,
            _artifact: &Artifact,
            _ingested_at: &str,
            _ingest_version: &str,
        ) -> Result<(), PortError> {
            unreachable!("enrich never upserts artifacts")
        }

        fn list_artifacts(&self, _repository: &RepositoryId) -> Result<Vec<Artifact>, PortError> {
            Ok(self.artifacts.clone())
        }

        fn mark_analyzed(
            &mut self,
            _repository: &RepositoryId,
            identity: &ArtifactIdentity,
            content_hash: &str,
            _analyzed_at: &str,
        ) -> Result<(), PortError> {
            self.analyzed
                .borrow_mut()
                .insert(identity.clone(), content_hash.to_owned());
            Ok(())
        }

        fn analyzed_content_hashes(
            &self,
            _repository: &RepositoryId,
        ) -> Result<std::collections::HashMap<ArtifactIdentity, String>, PortError> {
            Ok(self.analyzed.borrow().clone())
        }
    }

    impl ArtifactLinkStore for FakeStore {
        fn persist_links(
            &mut self,
            _repository: &RepositoryId,
            _links: &[ArtifactLink],
        ) -> Result<(), PortError> {
            unreachable!("enrich never persists links")
        }

        fn list_links(&self, _repository: &RepositoryId) -> Result<Vec<ArtifactLink>, PortError> {
            Ok(self.links.clone())
        }
    }

    impl GraphStore for FakeStore {
        fn load_graph(&self, _repository: &RepositoryId) -> Result<GraphSnapshot, PortError> {
            Ok(GraphSnapshot::default())
        }
    }

    impl KnowledgeCandidateStore for FakeStore {
        fn upsert_candidates(
            &mut self,
            _repository: &RepositoryId,
            candidates: &[KnowledgeCandidate],
        ) -> Result<(), PortError> {
            self.pending.borrow_mut().extend_from_slice(candidates);
            Ok(())
        }

        fn pending_candidates(
            &self,
            _repository: &RepositoryId,
        ) -> Result<Vec<KnowledgeCandidate>, PortError> {
            Ok(self.pending.borrow().clone())
        }

        fn record_decision(
            &mut self,
            _repository: &RepositoryId,
            _fingerprint: &str,
            _decision: &ctx_core::knowledge::KnowledgeDecision,
            _author: &str,
            _timestamp: &str,
        ) -> Result<(), PortError> {
            unreachable!("enrich never records knowledge decisions")
        }

        fn accepted_evidence(
            &self,
            _repository: &RepositoryId,
        ) -> Result<
            std::collections::BTreeMap<String, Vec<ctx_core::artifact::ArtifactRef>>,
            PortError,
        > {
            unreachable!("enrich never reads accepted evidence")
        }

        fn accepted_record_for_document(
            &self,
            _repository: &RepositoryId,
            _document_id: &str,
        ) -> Result<Option<ctx_core::knowledge::AcceptedKnowledgeRecord>, PortError> {
            unreachable!("enrich never reads accepted candidate records")
        }
    }

    struct FakeAgent {
        outcome: RefCell<BTreeMap<String, AgentOutcome>>,
        calls: RefCell<usize>,
    }

    impl SemanticAgent for FakeAgent {
        fn analyze(
            &self,
            neighborhood: &ctx_core::neighborhood::ArtifactNeighborhood,
            _produced_at: &str,
            _allow_ungrounded_symbols: bool,
        ) -> Result<AgentOutcome, PortError> {
            *self.calls.borrow_mut() += 1;
            Ok(self
                .outcome
                .borrow_mut()
                .remove(&neighborhood.subject.identity.external_id)
                .unwrap_or(AgentOutcome::NotRelevant))
        }
    }

    fn artifact(external_id: &str) -> Artifact {
        artifact_of_kind(external_id, ArtifactKind::Issue)
    }

    fn artifact_of_kind(external_id: &str, kind: ArtifactKind) -> Artifact {
        Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind,
                external_id: external_id.to_owned(),
            },
            project: ctx_core::domain::Project("billing/subscriptions".to_owned()),
            title: "title".to_owned(),
            body: "body".to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: ctx_core::domain::Url(format!("gitlab:{external_id}")),
            content_hash: "hash".to_owned(),
        }
    }

    fn candidate(evidence_identity: &ArtifactIdentity) -> KnowledgeCandidate {
        KnowledgeCandidate {
            fingerprint: KnowledgeCandidate::fingerprint_for(
                BusinessKind::Requirement,
                "statement",
            ),
            kind: BusinessKind::Requirement,
            statement: "statement".to_owned(),
            evidence: vec![ctx_core::artifact::ArtifactRef {
                identity: evidence_identity.clone(),
                locator: "body".to_owned(),
                excerpt: "excerpt".to_owned(),
            }],
            implementation_candidates: Vec::new(),
            test_candidates: Vec::new(),
            provenance: AgentProvenance {
                producer: "test".to_owned(),
                model: None,
                input_artifact_ids: Vec::new(),
                produced_at: "2026-08-21T00:00:00Z".to_owned(),
                fingerprint: "fp".to_owned(),
            },
        }
    }

    #[test]
    fn analyzes_every_artifact_and_persists_only_relevant_candidates() {
        let issue = artifact("317");
        let mut store = FakeStore {
            artifacts: vec![issue.clone()],
            ..FakeStore::default()
        };
        let mut outcomes = BTreeMap::new();
        outcomes.insert(
            "317".to_owned(),
            AgentOutcome::Relevant(vec![candidate(&issue.identity)]),
        );
        let agent = FakeAgent {
            outcome: RefCell::new(outcomes),
            calls: RefCell::new(0),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = EnrichRunner::new(&agent, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z", false)
            .expect("enrich run");

        assert_eq!(report.neighborhoods_analyzed, 1);
        assert_eq!(report.candidates_proposed, 1);
        assert_eq!(*agent.calls.borrow(), 1);
        assert_eq!(store.pending.borrow().len(), 1);
    }

    #[test]
    fn an_artifact_already_cited_by_a_pending_candidate_is_skipped() {
        let issue = artifact("317");
        let mut store = FakeStore {
            artifacts: vec![issue.clone()],
            pending: RefCell::new(vec![candidate(&issue.identity)]),
            ..FakeStore::default()
        };
        let agent = FakeAgent {
            outcome: RefCell::new(BTreeMap::new()),
            calls: RefCell::new(0),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = EnrichRunner::new(&agent, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z", false)
            .expect("enrich run");

        assert_eq!(report.neighborhoods_analyzed, 0);
        assert_eq!(report.artifacts_skipped_already_pending, 1);
        assert_eq!(*agent.calls.borrow(), 0);
    }

    #[test]
    fn not_relevant_outcomes_never_reach_the_candidate_store() {
        let mut store = FakeStore {
            artifacts: vec![artifact("317")],
            ..FakeStore::default()
        };
        let agent = FakeAgent {
            outcome: RefCell::new(BTreeMap::new()),
            calls: RefCell::new(0),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = EnrichRunner::new(&agent, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z", false)
            .expect("enrich run");

        assert_eq!(report.candidates_proposed, 0);
        assert!(store.pending.borrow().is_empty());
    }

    #[test]
    fn an_artifact_analyzed_at_its_current_content_hash_is_skipped_next_run() {
        let issue = artifact("317");
        let mut store = FakeStore {
            artifacts: vec![issue.clone()],
            ..FakeStore::default()
        };
        let agent = FakeAgent {
            outcome: RefCell::new(BTreeMap::new()),
            calls: RefCell::new(0),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let first = EnrichRunner::new(&agent, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z", false)
            .expect("first run");
        assert_eq!(first.neighborhoods_analyzed, 1);
        assert_eq!(*agent.calls.borrow(), 1);

        let second = EnrichRunner::new(&agent, &mut store)
            .run(&repository, "2026-08-21T01:00:00Z", false)
            .expect("second run");

        assert_eq!(second.neighborhoods_analyzed, 0);
        assert_eq!(second.artifacts_skipped_unchanged, 1);
        assert_eq!(
            *agent.calls.borrow(),
            1,
            "the agent must not be re-asked about unchanged content"
        );
        assert_eq!(
            store.analyzed.borrow().get(&issue.identity),
            Some(&issue.content_hash)
        );
    }

    #[test]
    fn run_with_progress_reports_position_and_total_only_for_real_analysis() {
        let first = artifact("1");
        let second = artifact("2"); // will be skipped: already pending
        let third = artifact("3");
        let mut store = FakeStore {
            artifacts: vec![first.clone(), second.clone(), third.clone()],
            pending: RefCell::new(vec![candidate(&second.identity)]),
            ..FakeStore::default()
        };
        let agent = FakeAgent {
            outcome: RefCell::new(BTreeMap::new()),
            calls: RefCell::new(0),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let mut seen = Vec::new();

        EnrichRunner::new(&agent, &mut store)
            .run_with_progress(
                &repository,
                "2026-08-22T00:00:00Z",
                false,
                &mut |position, total, subject| {
                    seen.push((position, total, subject.identity.external_id.clone()));
                },
            )
            .expect("enrich run");

        assert_eq!(
            seen,
            vec![(1, 3, "1".to_owned()), (3, 3, "3".to_owned())],
            "reported once per real analysis, positioned within the full known-artifact count, skipping the already-pending one silently"
        );
    }

    #[test]
    fn a_comment_with_a_known_parent_is_skipped_but_its_parent_is_still_analyzed() {
        let issue = artifact("317");
        let comment = artifact_of_kind("317-comment-1", ArtifactKind::Comment);
        let mut store = FakeStore {
            artifacts: vec![issue.clone(), comment.clone()],
            links: vec![ArtifactLink {
                source: comment.identity.clone(),
                target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
                kind: ArtifactLinkKind::CommentsOn,
                evidence_locator: "gitlab notes API: 317".to_owned(),
            }],
            ..FakeStore::default()
        };
        let agent = FakeAgent {
            outcome: RefCell::new(BTreeMap::new()),
            calls: RefCell::new(0),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = EnrichRunner::new(&agent, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z", false)
            .expect("enrich run");

        assert_eq!(report.artifacts_skipped_covered_by_parent, 1);
        assert_eq!(report.neighborhoods_analyzed, 1);
        assert_eq!(*agent.calls.borrow(), 1);
    }

    #[test]
    fn an_orphaned_comment_with_no_known_parent_is_still_analyzed() {
        let comment = artifact_of_kind("999-comment-1", ArtifactKind::Comment);
        let mut store = FakeStore {
            artifacts: vec![comment.clone()],
            links: vec![ArtifactLink {
                source: comment.identity.clone(),
                target: ArtifactLinkTarget::Artifact(ArtifactIdentity {
                    provider: ArtifactProvider::GitLab,
                    kind: ArtifactKind::Issue,
                    external_id: "999".to_owned(),
                }),
                kind: ArtifactLinkKind::CommentsOn,
                evidence_locator: "gitlab notes API: 999".to_owned(),
            }],
            ..FakeStore::default()
        };
        let agent = FakeAgent {
            outcome: RefCell::new(BTreeMap::new()),
            calls: RefCell::new(0),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = EnrichRunner::new(&agent, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z", false)
            .expect("enrich run");

        assert_eq!(report.artifacts_skipped_covered_by_parent, 0);
        assert_eq!(report.neighborhoods_analyzed, 1);
    }

    #[test]
    fn a_branch_named_by_a_known_merge_requests_source_branch_is_skipped() {
        let branch = artifact_of_kind("feature/PAY-317-cancel", ArtifactKind::Branch);
        let merge_request = artifact_of_kind("842", ArtifactKind::MergeRequest);
        let mut store = FakeStore {
            artifacts: vec![branch.clone(), merge_request.clone()],
            links: vec![ArtifactLink {
                source: merge_request.identity.clone(),
                target: ArtifactLinkTarget::Artifact(branch.identity.clone()),
                kind: ArtifactLinkKind::References,
                evidence_locator: "merge_request.source_branch".to_owned(),
            }],
            ..FakeStore::default()
        };
        let agent = FakeAgent {
            outcome: RefCell::new(BTreeMap::new()),
            calls: RefCell::new(0),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = EnrichRunner::new(&agent, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z", false)
            .expect("enrich run");

        assert_eq!(report.artifacts_skipped_covered_by_parent, 1);
        assert_eq!(report.neighborhoods_analyzed, 1);
    }

    #[test]
    fn a_commit_contained_by_a_known_branch_or_merge_request_is_skipped() {
        let commit = artifact_of_kind("abc123", ArtifactKind::Commit);
        let branch = artifact_of_kind("feature/x", ArtifactKind::Branch);
        let mut store = FakeStore {
            artifacts: vec![commit.clone(), branch.clone()],
            links: vec![ArtifactLink {
                source: branch.identity.clone(),
                target: ArtifactLinkTarget::Artifact(commit.identity.clone()),
                kind: ArtifactLinkKind::ContainsCommit,
                evidence_locator: "branch:feature/x".to_owned(),
            }],
            ..FakeStore::default()
        };
        let agent = FakeAgent {
            outcome: RefCell::new(BTreeMap::new()),
            calls: RefCell::new(0),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = EnrichRunner::new(&agent, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z", false)
            .expect("enrich run");

        assert_eq!(report.artifacts_skipped_covered_by_parent, 1);
        assert_eq!(report.neighborhoods_analyzed, 1);
    }
}
