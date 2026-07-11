//! Orchestrates AI-agent-assisted knowledge extraction (prompt3.md
//! PR-AI-*): for each currently known external artifact not already cited as
//! evidence by a pending candidate, assembles its bounded neighborhood
//! ([`ctx_core::neighborhood::build_neighborhood`]) and hands it to a
//! [`SemanticAgent`]. A candidate the agent proposes is persisted as
//! `pending` -- never auto-promoted to fact (PR-P02) -- for a human to
//! decide through the existing verification flow (Phase 6).
//!
//! Full incremental "already analyzed, nothing new since" skip logic is
//! Phase 8's job (PR-INCR-*); this runner only avoids re-proposing a
//! candidate an earlier `ctx enrich` run already left pending for the same
//! artifact.

use std::collections::HashSet;

use ctx_core::{domain::RepositoryId, knowledge::AgentOutcome, neighborhood::build_neighborhood};
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
    pub artifacts_skipped_already_pending: usize,
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

        let mut report = EnrichReport::default();
        let mut proposed = Vec::new();
        for subject in &known_artifacts {
            if already_pending.contains(&subject.identity) {
                report.artifacts_skipped_already_pending += 1;
                continue;
            }
            let neighborhood = build_neighborhood(subject, &links, &known_artifacts, &graph);
            let outcome = self
                .agent
                .analyze(&neighborhood, produced_at)
                .map_err(EnrichError::Agent)?;
            report.neighborhoods_analyzed += 1;
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
        Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: ArtifactKind::Issue,
                external_id: external_id.to_owned(),
            },
            project: "billing/subscriptions".to_owned(),
            title: "title".to_owned(),
            body: "body".to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: format!("gitlab:{external_id}"),
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
            .run(&repository, "2026-08-21T00:00:00Z")
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
            .run(&repository, "2026-08-21T00:00:00Z")
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
            .run(&repository, "2026-08-21T00:00:00Z")
            .expect("enrich run");

        assert_eq!(report.candidates_proposed, 0);
        assert!(store.pending.borrow().is_empty());
    }
}
