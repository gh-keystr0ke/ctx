use ctx_core::{
    business::{BusinessDocument, BusinessKind, ExplicitSymbolLink},
    domain::RepositoryId,
    knowledge::{KnowledgeCandidate, KnowledgeDecision},
    verification::{
        ArtifactEvidenceContext, SemanticCandidate, VerificationDecision, possible_duplicate,
        semantic_candidates,
    },
};
use thiserror::Error;

use crate::ports::{
    ArtifactLinkStore, BusinessContextWriter, CommitMetadata, GraphStore, KnowledgeCandidateStore,
    PortError, VerificationStore,
};

#[derive(Debug, Error)]
pub enum VerificationError {
    #[error("verification candidates could not be loaded: {0}")]
    Store(PortError),
    #[error("verification candidate '{0}' was not found")]
    CandidateNotFound(String),
    #[error(
        "'{statement}' looks like a restatement of already-active {existing_id} -- attach as evidence to it instead, or pass force to create a new document anyway"
    )]
    PossibleDuplicate {
        existing_id: String,
        statement: String,
    },
}

pub struct VerificationService<'a, S> {
    store: &'a mut S,
}

impl<'a, S> VerificationService<'a, S>
where
    S: GraphStore + VerificationStore + ArtifactLinkStore + KnowledgeCandidateStore,
{
    pub const fn new(store: &'a mut S) -> Self {
        Self { store }
    }

    /// Returns deterministic, impact-prioritized semantic candidates,
    /// including the artifact-evidence signal (PR-MAP-001) for any intent
    /// that originated from an accepted AI-derived candidate.
    ///
    /// # Errors
    ///
    /// Returns [`VerificationError`] when current graph or artifact state
    /// cannot be loaded.
    pub fn candidates(
        &self,
        repository: &RepositoryId,
    ) -> Result<Vec<SemanticCandidate>, VerificationError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(VerificationError::Store)?;
        let artifact_context = ArtifactEvidenceContext {
            links: self
                .store
                .list_links(repository)
                .map_err(VerificationError::Store)?,
            accepted_evidence: self
                .store
                .accepted_evidence(repository)
                .map_err(VerificationError::Store)?,
        };
        Ok(semantic_candidates(&graph, &artifact_context))
    }

    /// Records a decision for a current candidate.
    ///
    /// # Errors
    ///
    /// Returns [`VerificationError`] when the fingerprint is no longer a
    /// current candidate or persistence fails.
    pub fn decide(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        fingerprint: &str,
        decision: VerificationDecision,
        author: &str,
        timestamp: &str,
    ) -> Result<(), VerificationError> {
        let candidate = self
            .candidates(repository)?
            .into_iter()
            .find(|candidate| candidate.fingerprint == fingerprint)
            .ok_or_else(|| VerificationError::CandidateNotFound(fingerprint.to_owned()))?;
        self.store
            .record_verification(repository, commit, &candidate, decision, author, timestamp)
            .map_err(VerificationError::Store)
    }
}

/// Human verification for AI-derived [`KnowledgeCandidate`]s
/// (`ctx verify --knowledge`, prompt3.md PR-VERIFY-001) — deliberately a
/// sibling service, not a shared queue with [`VerificationService`]:
/// accepting a heuristic `SemanticCandidate` only asserts an already-known
/// claim, while accepting a `KnowledgeCandidate` creates a brand-new
/// product-knowledge entity and needs a human-chosen stable ID, so the two
/// flows have genuinely different shapes rather than one interchangeable
/// accept/reject action.
pub struct KnowledgeVerificationService<'a, S, W> {
    store: &'a mut S,
    writer: &'a W,
}

impl<'a, S, W> KnowledgeVerificationService<'a, S, W>
where
    S: KnowledgeCandidateStore + GraphStore,
    W: BusinessContextWriter,
{
    pub const fn new(store: &'a mut S, writer: &'a W) -> Self {
        Self { store, writer }
    }

    /// Returns every candidate still awaiting a human decision.
    ///
    /// # Errors
    /// Returns [`VerificationError`] when stored candidates cannot be read.
    pub fn candidates(
        &self,
        repository: &RepositoryId,
    ) -> Result<Vec<KnowledgeCandidate>, VerificationError> {
        self.store
            .pending_candidates(repository)
            .map_err(VerificationError::Store)
    }

    /// Accepts a pending candidate under `document_id`: writes the resulting
    /// `.context/*.yaml` file (the next `ctx index` absorbs it like any
    /// hand-authored document) and records the decision, keeping the
    /// original candidate row -- status `accepted`, pointing at this ID --
    /// rather than discarding the artifact-to-inference chain (PR-VERIFY-002).
    ///
    /// Unless `force`, refuses when the statement looks like a restatement
    /// of an already-active document of the same kind (prompt3.md §13 MUST:
    /// "restating REQ-17 must not silently become REQ-94") -- a lexical
    /// similarity check against the current graph, advisory only, never a
    /// second AI call.
    ///
    /// # Errors
    /// Returns [`VerificationError`] when `fingerprint` is not currently
    /// pending, a likely duplicate exists and `force` is false, the document
    /// file already exists, or persistence fails.
    pub fn accept(
        &mut self,
        repository: &RepositoryId,
        fingerprint: &str,
        document_id: &str,
        author: &str,
        timestamp: &str,
        force: bool,
    ) -> Result<String, VerificationError> {
        let candidate = self
            .candidates(repository)?
            .into_iter()
            .find(|candidate| candidate.fingerprint == fingerprint)
            .ok_or_else(|| VerificationError::CandidateNotFound(fingerprint.to_owned()))?;
        if !force {
            let graph = self
                .store
                .load_graph(repository)
                .map_err(VerificationError::Store)?;
            if let Some(existing_id) =
                possible_duplicate(&graph, candidate.kind, &candidate.statement)
            {
                return Err(VerificationError::PossibleDuplicate {
                    existing_id,
                    statement: candidate.statement,
                });
            }
        }
        let document = candidate_to_document(&candidate, document_id);
        let path = self
            .writer
            .write_document(&document)
            .map_err(VerificationError::Store)?;
        self.store
            .record_decision(
                repository,
                fingerprint,
                &KnowledgeDecision::Accept {
                    document_id: document_id.to_owned(),
                },
                author,
                timestamp,
            )
            .map_err(VerificationError::Store)?;
        Ok(path)
    }

    /// Rejects a pending candidate; it is never proposed again once a future
    /// `ctx enrich` run recognizes the same fingerprint.
    ///
    /// # Errors
    /// Returns [`VerificationError`] when `fingerprint` is not currently
    /// pending or persistence fails.
    pub fn reject(
        &mut self,
        repository: &RepositoryId,
        fingerprint: &str,
        author: &str,
        timestamp: &str,
    ) -> Result<(), VerificationError> {
        self.store
            .record_decision(
                repository,
                fingerprint,
                &KnowledgeDecision::Reject,
                author,
                timestamp,
            )
            .map_err(VerificationError::Store)
    }
}

fn candidate_to_document(candidate: &KnowledgeCandidate, document_id: &str) -> BusinessDocument {
    let title = match candidate.kind {
        BusinessKind::Requirement | BusinessKind::Invariant => candidate.statement.clone(),
        BusinessKind::Feature | BusinessKind::Decision => candidate.derived_title(),
    };
    let to_links = |symbols: &[String]| {
        symbols
            .iter()
            .map(|symbol| ExplicitSymbolLink {
                symbol: symbol.clone(),
                locator: String::new(),
            })
            .collect()
    };
    BusinessDocument {
        id: document_id.to_owned(),
        kind: candidate.kind,
        title,
        body: candidate.statement.clone(),
        status: "active".to_owned(),
        feature: None,
        implementation: to_links(&candidate.implementation_candidates),
        tests: to_links(&candidate.test_candidates),
        source_uri: String::new(),
        content_hash: String::new(),
    }
}

#[cfg(test)]
mod knowledge_tests {
    use std::cell::RefCell;

    use ctx_core::{
        artifact::{ArtifactIdentity, ArtifactKind, ArtifactProvider, ArtifactRef},
        knowledge::AgentProvenance,
    };

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        pending: Vec<KnowledgeCandidate>,
        decisions: RefCell<Vec<(String, KnowledgeDecision)>>,
        graph: ctx_core::graph::GraphSnapshot,
    }

    impl GraphStore for FakeStore {
        fn load_graph(
            &self,
            _repository: &RepositoryId,
        ) -> Result<ctx_core::graph::GraphSnapshot, PortError> {
            Ok(self.graph.clone())
        }
    }

    impl KnowledgeCandidateStore for FakeStore {
        fn upsert_candidates(
            &mut self,
            _repository: &RepositoryId,
            _candidates: &[KnowledgeCandidate],
        ) -> Result<(), PortError> {
            unreachable!("verification never upserts candidates")
        }

        fn pending_candidates(
            &self,
            _repository: &RepositoryId,
        ) -> Result<Vec<KnowledgeCandidate>, PortError> {
            Ok(self.pending.clone())
        }

        fn record_decision(
            &mut self,
            _repository: &RepositoryId,
            fingerprint: &str,
            decision: &KnowledgeDecision,
            _author: &str,
            _timestamp: &str,
        ) -> Result<(), PortError> {
            self.decisions
                .borrow_mut()
                .push((fingerprint.to_owned(), decision.clone()));
            Ok(())
        }

        fn accepted_evidence(
            &self,
            _repository: &RepositoryId,
        ) -> Result<std::collections::BTreeMap<String, Vec<ArtifactRef>>, PortError> {
            unreachable!("knowledge verification never reads accepted evidence")
        }

        fn accepted_record_for_document(
            &self,
            _repository: &RepositoryId,
            _document_id: &str,
        ) -> Result<Option<ctx_core::knowledge::AcceptedKnowledgeRecord>, PortError> {
            unreachable!("knowledge verification never reads accepted candidate records")
        }
    }

    #[derive(Default)]
    struct FakeWriter {
        written: RefCell<Vec<BusinessDocument>>,
    }

    impl BusinessContextWriter for FakeWriter {
        fn write_document(&self, document: &BusinessDocument) -> Result<String, PortError> {
            self.written.borrow_mut().push(document.clone());
            Ok(format!(".context/fake/{}.yaml", document.id))
        }
    }

    fn candidate(kind: BusinessKind, statement: &str) -> KnowledgeCandidate {
        KnowledgeCandidate {
            fingerprint: KnowledgeCandidate::fingerprint_for(kind, statement),
            kind,
            statement: statement.to_owned(),
            evidence: vec![ArtifactRef {
                identity: ArtifactIdentity {
                    provider: ArtifactProvider::GitLab,
                    kind: ArtifactKind::Issue,
                    external_id: "317".to_owned(),
                },
                locator: "body".to_owned(),
                excerpt: "excerpt".to_owned(),
            }],
            implementation_candidates: vec!["SubscriptionService.cancel".to_owned()],
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
    fn accepting_writes_the_document_and_records_the_decision() {
        let candidate = candidate(BusinessKind::Requirement, "Cancellation preserves access.");
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let path = KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                &candidate.fingerprint,
                "REQ-SUB-014",
                "alice",
                "2026-08-21T00:00:00Z",
                false,
            )
            .expect("accept");

        assert_eq!(path, ".context/fake/REQ-SUB-014.yaml");
        let written = writer.written.borrow();
        assert_eq!(written[0].id, "REQ-SUB-014");
        assert_eq!(written[0].title, "Cancellation preserves access.");
        assert_eq!(
            written[0].implementation[0].symbol,
            "SubscriptionService.cancel"
        );
        assert_eq!(
            store.decisions.borrow()[0],
            (
                candidate.fingerprint.clone(),
                KnowledgeDecision::Accept {
                    document_id: "REQ-SUB-014".to_owned()
                }
            )
        );
    }

    #[test]
    fn a_decision_kind_gets_a_derived_title_distinct_from_its_body() {
        let candidate = candidate(
            BusinessKind::Decision,
            "Cancellation stays reversible until period end. Detailed reasoning follows.",
        );
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                &candidate.fingerprint,
                "ADR-SUB-002",
                "alice",
                "2026-08-21T00:00:00Z",
                false,
            )
            .expect("accept");

        let written = writer.written.borrow();
        assert_eq!(
            written[0].title,
            "Cancellation stays reversible until period end"
        );
        assert_eq!(written[0].body, candidate.statement);
    }

    #[test]
    fn accepting_an_unknown_fingerprint_fails_clearly() {
        let mut store = FakeStore::default();
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let error = KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                "missing",
                "REQ-X",
                "alice",
                "2026-08-21T00:00:00Z",
                false,
            )
            .expect_err("unknown fingerprint must fail");

        assert!(matches!(error, VerificationError::CandidateNotFound(_)));
        assert!(writer.written.borrow().is_empty());
    }

    #[test]
    fn accepting_a_likely_restatement_is_refused_unless_forced() {
        let candidate = candidate(
            BusinessKind::Requirement,
            "Cancellation preserves paid access until the period ends.",
        );
        let existing = ctx_core::graph::GraphNode {
            stable_key: ctx_core::domain::StableKey::new("intent:REQ-SUB-001").expect("stable key"),
            kind: ctx_core::domain::NodeKind::Requirement,
            name: "Cancellation preserves access".to_owned(),
            content_hash: "hash".to_owned(),
            attributes: ctx_core::indexing::PlannedNodeAttributes::Business {
                id: "REQ-SUB-001".to_owned(),
                status: "active".to_owned(),
                body: "Cancellation preserves paid access until the period ends.".to_owned(),
                feature: None,
                source_uri: "requirement.yaml".to_owned(),
            },
        };
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            graph: ctx_core::graph::GraphSnapshot {
                nodes: [(existing.stable_key.clone(), existing)]
                    .into_iter()
                    .collect(),
                edges: Vec::new(),
            },
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let error = KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                &candidate.fingerprint,
                "REQ-SUB-002",
                "alice",
                "2026-08-21T00:00:00Z",
                false,
            )
            .expect_err("a likely restatement must be refused without force");
        assert!(matches!(
            error,
            VerificationError::PossibleDuplicate { existing_id, .. } if existing_id == "REQ-SUB-001"
        ));
        assert!(writer.written.borrow().is_empty());

        // force overrides the check.
        let path = KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                &candidate.fingerprint,
                "REQ-SUB-002",
                "alice",
                "2026-08-21T00:00:00Z",
                true,
            )
            .expect("force overrides the duplicate check");
        assert_eq!(path, ".context/fake/REQ-SUB-002.yaml");
    }

    #[test]
    fn rejecting_records_a_reject_decision_and_writes_nothing() {
        let candidate = candidate(BusinessKind::Invariant, "Never delete paid history.");
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        KnowledgeVerificationService::new(&mut store, &writer)
            .reject(
                &repository,
                &candidate.fingerprint,
                "alice",
                "2026-08-21T00:00:00Z",
            )
            .expect("reject");

        assert!(writer.written.borrow().is_empty());
        assert_eq!(
            store.decisions.borrow()[0],
            (candidate.fingerprint, KnowledgeDecision::Reject)
        );
    }
}
