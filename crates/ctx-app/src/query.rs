use ctx_core::{
    context_pack::{ContextCompileError, ContextPack, ContextRequest, compile_context_pack},
    domain::{NodeKind, RepositoryId, StableKey},
    explain::{ExplainError, Explanation, KnowledgeProvenance, explain},
    graph::{NodeSummary, SymbolMatch, find_requirements, find_symbols},
    impact::{ImpactError, ImpactReport, analyze_impact},
    neighborhood::artifact_history,
};
use thiserror::Error;

use crate::ports::{
    ArtifactLinkStore, ArtifactRepository, GraphStore, KnowledgeCandidateStore, PortError,
};

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("graph could not be loaded: {0}")]
    Store(PortError),
    #[error(transparent)]
    Impact(#[from] ImpactError),
    #[error(transparent)]
    Explain(#[from] ExplainError),
    #[error(transparent)]
    Context(#[from] ContextCompileError),
}

pub struct QueryService<'a, S> {
    store: &'a S,
}

impl<'a, S> QueryService<'a, S>
where
    S: GraphStore + KnowledgeCandidateStore + ArtifactRepository + ArtifactLinkStore,
{
    pub const fn new(store: &'a S) -> Self {
        Self { store }
    }

    /// Returns bounded product and implementation impact for every distinct
    /// node the seed resolves to (several exact matches are not an error;
    /// each gets its own independent report — PR-LOOKUP-002/003).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph loading fails or the seed resolves
    /// to nothing.
    pub fn impact(
        &self,
        repository: &RepositoryId,
        target: &str,
    ) -> Result<Vec<ImpactReport>, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        analyze_impact(target, &graph).map_err(QueryError::from)
    }

    /// Explains every node the query resolves to (or the single directed
    /// relationship it names) from persisted evidence. Several exact matches
    /// are not an error; each gets its own independent explanation
    /// (PR-LOOKUP-002/003). A single-subject intent result additionally
    /// carries the full external-artifact -> agent-inference ->
    /// human-verification chain (Phase 9) when it originated from an
    /// accepted `ctx verify --knowledge` candidate.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph loading fails or the query resolves
    /// to nothing.
    pub fn explain(
        &self,
        repository: &RepositoryId,
        target: &str,
    ) -> Result<Vec<Explanation>, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        let mut explanations = explain(target, &graph)?;
        for explanation in &mut explanations {
            explanation.knowledge_provenance =
                self.knowledge_provenance_for(repository, explanation)?;
            explanation.artifact_history = self.artifact_history_for(repository, explanation)?;
        }
        Ok(explanations)
    }

    fn knowledge_provenance_for(
        &self,
        repository: &RepositoryId,
        explanation: &Explanation,
    ) -> Result<Option<KnowledgeProvenance>, QueryError> {
        let [subject] = explanation.subjects.as_slice() else {
            return Ok(None);
        };
        if !matches!(
            subject.kind,
            NodeKind::Feature | NodeKind::Requirement | NodeKind::Invariant | NodeKind::Decision
        ) {
            return Ok(None);
        }
        let record = self
            .store
            .accepted_record_for_document(repository, &subject.identifier)
            .map_err(QueryError::Store)?;
        Ok(record.map(|record| KnowledgeProvenance {
            derived_from: record.candidate.provenance.input_artifact_ids,
            agent_producer: record.candidate.provenance.producer,
            agent_model: record.candidate.provenance.model,
            decided_by: record.decided_by,
            decided_at: record.decided_at,
            decision_method: record.decision_method,
        }))
    }

    /// The artifacts that structurally touched a single `CodeSymbol`
    /// subject's history — empty for any other subject shape (several
    /// subjects, or one that isn't code), since artifact links never target
    /// a business node.
    fn artifact_history_for(
        &self,
        repository: &RepositoryId,
        explanation: &Explanation,
    ) -> Result<Vec<ctx_core::neighborhood::LinkedArtifact>, QueryError> {
        let [subject] = explanation.subjects.as_slice() else {
            return Ok(Vec::new());
        };
        if subject.kind != NodeKind::CodeSymbol {
            return Ok(Vec::new());
        }
        let Ok(stable_key) = StableKey::new(&subject.stable_key) else {
            return Ok(Vec::new());
        };
        let links = self
            .store
            .list_links(repository)
            .map_err(QueryError::Store)?;
        let known_artifacts = self
            .store
            .list_artifacts(repository)
            .map_err(QueryError::Store)?;
        Ok(artifact_history(&stable_key, &links, &known_artifacts))
    }

    /// Compiles a bounded context pack from task and explicit seeds.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph loading or context compilation fails.
    pub fn context(
        &self,
        repository: &RepositoryId,
        request: &ContextRequest,
    ) -> Result<ContextPack, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        compile_context_pack(&graph, request).map_err(QueryError::from)
    }

    /// Finds product requirements by ID or terms.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph state cannot be loaded.
    pub fn find_requirements(
        &self,
        repository: &RepositoryId,
        query: &str,
    ) -> Result<Vec<NodeSummary>, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        Ok(find_requirements(query, &graph))
    }

    /// Discovery lookup for a short or exact name: every distinct match,
    /// with no ambiguity error (PR-LOOKUP-007).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph state cannot be loaded.
    pub fn find(
        &self,
        repository: &RepositoryId,
        query: &str,
    ) -> Result<Vec<SymbolMatch>, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        Ok(find_symbols(query, &graph))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ctx_core::{
        artifact::ArtifactRef,
        business::BusinessKind,
        domain::{ClaimClass, ClaimStatus, Confidence, SourceKind, StableKey},
        graph::{GraphEdge, GraphNode, GraphSnapshot},
        indexing::PlannedNodeAttributes,
        knowledge::{
            AcceptedKnowledgeRecord, AgentProvenance, KnowledgeCandidate, KnowledgeDecision,
        },
    };

    use super::*;
    use crate::ports::PortError;

    #[derive(Default)]
    struct FakeStore {
        graph: GraphSnapshot,
        accepted: BTreeMap<String, AcceptedKnowledgeRecord>,
        artifacts: Vec<ctx_core::artifact::Artifact>,
        links: Vec<ctx_core::artifact::ArtifactLink>,
    }

    impl GraphStore for FakeStore {
        fn load_graph(&self, _repository: &RepositoryId) -> Result<GraphSnapshot, PortError> {
            Ok(self.graph.clone())
        }
    }

    impl ArtifactRepository for FakeStore {
        fn upsert_artifact(
            &mut self,
            _repository: &RepositoryId,
            _artifact: &ctx_core::artifact::Artifact,
            _ingested_at: &str,
            _ingest_version: &str,
        ) -> Result<(), PortError> {
            unreachable!("explain never upserts artifacts")
        }

        fn list_artifacts(
            &self,
            _repository: &RepositoryId,
        ) -> Result<Vec<ctx_core::artifact::Artifact>, PortError> {
            Ok(self.artifacts.clone())
        }

        fn mark_analyzed(
            &mut self,
            _repository: &RepositoryId,
            _identity: &ctx_core::artifact::ArtifactIdentity,
            _content_hash: &str,
            _input_fingerprint: &str,
            _analyzed_at: &str,
        ) -> Result<(), PortError> {
            unreachable!("explain never marks artifacts analyzed")
        }

        fn analyzed_input_fingerprints(
            &self,
            _repository: &RepositoryId,
        ) -> Result<
            std::collections::HashMap<ctx_core::artifact::ArtifactIdentity, String>,
            PortError,
        > {
            unreachable!("explain never reads analyzed content hashes")
        }
    }

    impl ArtifactLinkStore for FakeStore {
        fn persist_links(
            &mut self,
            _repository: &RepositoryId,
            _links: &[ctx_core::artifact::ArtifactLink],
        ) -> Result<(), PortError> {
            unreachable!("explain never persists links")
        }

        fn list_links(
            &self,
            _repository: &RepositoryId,
        ) -> Result<Vec<ctx_core::artifact::ArtifactLink>, PortError> {
            Ok(self.links.clone())
        }
    }

    impl KnowledgeCandidateStore for FakeStore {
        fn upsert_candidates(
            &mut self,
            _repository: &RepositoryId,
            _candidates: &[KnowledgeCandidate],
        ) -> Result<(), PortError> {
            unreachable!("explain never upserts candidates")
        }

        fn pending_candidates(
            &self,
            _repository: &RepositoryId,
        ) -> Result<Vec<KnowledgeCandidate>, PortError> {
            unreachable!("explain never reads pending candidates")
        }

        fn record_decision(
            &mut self,
            _repository: &RepositoryId,
            _fingerprint: &str,
            _decision: &KnowledgeDecision,
            _author: &str,
            _timestamp: &str,
        ) -> Result<(), PortError> {
            unreachable!("explain never records decisions")
        }

        fn accepted_evidence(
            &self,
            _repository: &RepositoryId,
        ) -> Result<BTreeMap<String, Vec<ArtifactRef>>, PortError> {
            unreachable!("explain never reads accepted evidence")
        }

        fn accepted_record_for_document(
            &self,
            _repository: &RepositoryId,
            document_id: &str,
        ) -> Result<Option<AcceptedKnowledgeRecord>, PortError> {
            Ok(self.accepted.get(document_id).cloned())
        }
    }

    fn intent_node(key: &str, kind: ctx_core::domain::NodeKind, id: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind,
            name: id.to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: id.to_owned(),
                status: "active".to_owned(),
                visibility: ctx_core::business::Visibility::Private,
                implementation_expected: true,
                body: "Cancellation preserves paid access.".to_owned(),
                feature: None,
                source_uri: "requirement.yaml".to_owned(),
            },
        }
    }

    fn symbol_node(key: &str, canonical: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: ctx_core::domain::NodeKind::CodeSymbol,
            name: canonical.to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: "billing.py".to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind: ctx_core::ir::SymbolKind::Function,
                range: ctx_core::ir::SourceRange {
                    start_byte: 0,
                    end_byte: 1,
                    start_line: 1,
                    end_line: 1,
                },
                signature: None,
                structural_fingerprint: "shape".to_owned(),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            },
        }
    }

    fn edge(source: &GraphNode, target: &GraphNode) -> GraphEdge {
        GraphEdge {
            source: source.stable_key.clone(),
            target: target.stable_key.clone(),
            kind: ctx_core::domain::RelationKind::Implements,
            claim_class: ClaimClass::Assertion,
            source_kind: SourceKind::Documentation,
            confidence: Confidence::CERTAIN,
            status: ClaimStatus::Active,
            valid_from: "commit".to_owned(),
            valid_to: None,
            producer: "test".to_owned(),
            fingerprint: format!("{}:implements", source.stable_key),
            stale_reason: None,
            evidence: Vec::new(),
        }
    }

    fn accepted_record(input_artifact_ids: Vec<String>) -> AcceptedKnowledgeRecord {
        AcceptedKnowledgeRecord {
            candidate: KnowledgeCandidate {
                fingerprint: KnowledgeCandidate::fingerprint_for(
                    BusinessKind::Requirement,
                    "Cancellation preserves paid access.",
                ),
                kind: BusinessKind::Requirement,
                statement: "Cancellation preserves paid access.".to_owned(),
                evidence: Vec::new(),
                implementation_candidates: Vec::new(),
                test_candidates: Vec::new(),
                provenance: AgentProvenance {
                    producer: "claude-code".to_owned(),
                    model: Some("claude-sonnet-5".to_owned()),
                    input_artifact_ids,
                    produced_at: "2026-08-21T00:00:00Z".to_owned(),
                    fingerprint: "prompt:v1".to_owned(),
                },
            },
            decided_by: "alice".to_owned(),
            decided_at: "2026-08-21T01:00:00Z".to_owned(),
            decision_method: ctx_core::knowledge::DecisionMethod::Human,
        }
    }

    #[test]
    fn a_requirement_from_an_accepted_candidate_carries_its_full_provenance_chain() {
        let requirement = intent_node(
            "req",
            ctx_core::domain::NodeKind::Requirement,
            "REQ-SUB-014",
        );
        let symbol = symbol_node("cancel", "SubscriptionService.cancel");
        let store = FakeStore {
            graph: GraphSnapshot {
                nodes: [requirement.clone(), symbol.clone()]
                    .into_iter()
                    .map(|node| (node.stable_key.clone(), node))
                    .collect(),
                edges: vec![edge(&symbol, &requirement)],
            },
            accepted: BTreeMap::from([(
                "REQ-SUB-014".to_owned(),
                accepted_record(vec!["gitlab:issue:317".to_owned()]),
            )]),
            ..FakeStore::default()
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let explanations = QueryService::new(&store)
            .explain(&repository, "REQ-SUB-014")
            .expect("explanation");

        assert_eq!(explanations.len(), 1);
        let provenance = explanations[0]
            .knowledge_provenance
            .as_ref()
            .expect("knowledge provenance");
        assert_eq!(provenance.derived_from, vec!["gitlab:issue:317".to_owned()]);
        assert_eq!(provenance.agent_producer, "claude-code");
        assert_eq!(provenance.decided_by, "alice");
        assert_eq!(
            provenance.decision_method,
            ctx_core::knowledge::DecisionMethod::Human
        );
    }

    #[test]
    fn a_hand_authored_requirement_carries_no_provenance_chain() {
        let requirement = intent_node(
            "req",
            ctx_core::domain::NodeKind::Requirement,
            "REQ-SUB-014",
        );
        let store = FakeStore {
            graph: GraphSnapshot {
                nodes: [(requirement.stable_key.clone(), requirement)]
                    .into_iter()
                    .collect(),
                edges: Vec::new(),
            },
            accepted: BTreeMap::new(),
            ..FakeStore::default()
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let explanations = QueryService::new(&store)
            .explain(&repository, "REQ-SUB-014")
            .expect("explanation");

        assert!(explanations[0].knowledge_provenance.is_none());
    }

    #[test]
    fn a_relationship_query_never_carries_a_provenance_chain() {
        let requirement = intent_node(
            "req",
            ctx_core::domain::NodeKind::Requirement,
            "REQ-SUB-014",
        );
        let symbol = symbol_node("cancel", "SubscriptionService.cancel");
        let store = FakeStore {
            graph: GraphSnapshot {
                nodes: [requirement.clone(), symbol.clone()]
                    .into_iter()
                    .map(|node| (node.stable_key.clone(), node))
                    .collect(),
                edges: vec![edge(&symbol, &requirement)],
            },
            accepted: BTreeMap::from([(
                "REQ-SUB-014".to_owned(),
                accepted_record(vec!["gitlab:issue:317".to_owned()]),
            )]),
            ..FakeStore::default()
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let explanations = QueryService::new(&store)
            .explain(&repository, "SubscriptionService.cancel -> REQ-SUB-014")
            .expect("explanation");

        assert_eq!(explanations.len(), 1);
        assert!(explanations[0].knowledge_provenance.is_none());
    }

    #[test]
    fn explaining_a_code_symbol_lists_the_artifacts_that_touched_it() {
        let symbol = symbol_node("cancel", "SubscriptionService.cancel");
        let commit = ctx_core::artifact::Artifact {
            identity: ctx_core::artifact::ArtifactIdentity {
                provider: ctx_core::artifact::ArtifactProvider::Git,
                kind: ctx_core::artifact::ArtifactKind::Commit,
                external_id: "abc123".to_owned(),
            },
            project: ctx_core::domain::Project("billing/subscriptions".to_owned()),
            title: "fix cancellation".to_owned(),
            body: "fix cancellation".to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: ctx_core::domain::Url("git:commit:abc123".to_owned()),
            content_hash: "hash".to_owned(),
        };
        let store = FakeStore {
            graph: GraphSnapshot {
                nodes: [(symbol.stable_key.clone(), symbol.clone())]
                    .into_iter()
                    .collect(),
                edges: Vec::new(),
            },
            artifacts: vec![commit.clone()],
            links: vec![ctx_core::artifact::ArtifactLink {
                source: commit.identity.clone(),
                target: ctx_core::artifact::ArtifactLinkTarget::CodeSymbol(
                    symbol.stable_key.clone(),
                ),
                kind: ctx_core::artifact::ArtifactLinkKind::ChangedSymbol,
                evidence_locator: "changed_file:billing.py".to_owned(),
            }],
            ..FakeStore::default()
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let explanations = QueryService::new(&store)
            .explain(&repository, "SubscriptionService.cancel")
            .expect("explanation");

        assert_eq!(explanations.len(), 1);
        assert_eq!(explanations[0].artifact_history.len(), 1);
        assert_eq!(
            explanations[0].artifact_history[0].artifact.identity,
            commit.identity
        );
    }
}
