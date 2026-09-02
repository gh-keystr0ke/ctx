use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    domain::{ClaimClass, ClaimStatus, SourceKind},
    graph::{GraphEdge, GraphEvidence, GraphNode, GraphSnapshot, NodeSummary},
    knowledge::DecisionMethod,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimExplanation {
    pub claim: String,
    pub claim_class: ClaimClass,
    pub status: ClaimStatus,
    pub confidence: f32,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub provenance: SourceKind,
    pub producer: String,
    pub stale_reason: Option<String>,
    pub evidence: Vec<GraphEvidence>,
}

/// The full external-artifact -> agent-inference -> human-verification chain
/// behind a node that reached the graph through an accepted AI-derived
/// [`crate::knowledge::KnowledgeCandidate`] (prompt3.md §16/§19, Phase 9).
/// `None` for a hand-authored `.context/*.yaml` document, which has no such
/// candidate row at all -- `explain` never fabricates one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeProvenance {
    /// Formatted artifact ids (`gitlab:issue:317`) the agent's bounded input
    /// neighborhood was built from -- the accepted candidate's own
    /// `AgentProvenance.input_artifact_ids`.
    pub derived_from: Vec<String>,
    pub agent_producer: String,
    pub agent_model: Option<String>,
    pub decided_by: String,
    pub decided_at: String,
    /// Whether `decided_by` names a human (`ctx verify --knowledge`) or an
    /// agent's own independent second-opinion review (`--auto`) -- kept
    /// alongside `decided_by` so rendering never has to guess, and never
    /// silently presents an agent's own decision as a human review that
    /// never happened (`INV-EPISTEMIC-001`).
    pub decision_method: DecisionMethod,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Explanation {
    pub query: String,
    pub subjects: Vec<NodeSummary>,
    pub claims: Vec<ClaimExplanation>,
    /// Set only for a plain node query with exactly one subject that
    /// originated from an accepted AI-derived candidate; assembled at the
    /// `ctx-app` layer, since the candidate record lives outside the graph.
    pub knowledge_provenance: Option<KnowledgeProvenance>,
    /// Every artifact (commit, merge/pull request, comment, ...) whose
    /// changeset structurally touched or discussed this symbol, newest
    /// first (`ctx_core::neighborhood::artifact_history`). Set only for a
    /// plain node query with exactly one `CodeSymbol` subject; assembled at
    /// the `ctx-app` layer, since artifacts and their links live outside
    /// the graph.
    pub artifact_history: Vec<crate::neighborhood::LinkedArtifact>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ExplainError {
    #[error("nothing indexed matches '{0}'")]
    NotFound(String),
    #[error("no stored relationship matches '{0}'")]
    RelationshipNotFound(String),
}

/// Explains a node or `source -> target` relationship exclusively from stored
/// claims and evidence.
///
/// Several distinct nodes matching one query (a short name shared across
/// namespaces) are not an error and are not pooled together: each distinct
/// match gets its own independent [`Explanation`], per PR-LOOKUP-003 — the
/// caller sees the equivalent of running `explain` once per fully-qualified
/// match. A `source -> target` relationship query always yields exactly one
/// [`Explanation`], since it names a specific directed claim rather than an
/// ambiguous lookup.
///
/// # Errors
///
/// Returns [`ExplainError`] when the node or relationship is absent.
pub fn explain(query: &str, graph: &GraphSnapshot) -> Result<Vec<Explanation>, ExplainError> {
    if let Some((source_query, target_query)) = query.split_once("->") {
        return explain_relationship(source_query.trim(), target_query.trim(), query, graph)
            .map(|explanation| vec![explanation]);
    }
    let nodes = graph.resolve(query);
    if nodes.is_empty() {
        return Err(ExplainError::NotFound(query.to_owned()));
    }
    let mut grouped = BTreeMap::<&str, Vec<&GraphNode>>::new();
    for node in nodes {
        grouped.entry(node.identifier()).or_default().push(node);
    }
    Ok(grouped
        .into_values()
        .map(|group| explain_nodes(query, &group, graph))
        .collect())
}

fn explain_nodes(query: &str, nodes: &[&GraphNode], graph: &GraphSnapshot) -> Explanation {
    let mut claims = graph
        .edges
        .iter()
        .filter(|edge| {
            nodes
                .iter()
                .any(|node| edge.source == node.stable_key || edge.target == node.stable_key)
        })
        .map(|edge| claim_explanation(edge, graph))
        .collect::<Vec<_>>();
    sort_claims(&mut claims);
    Explanation {
        query: query.to_owned(),
        subjects: nodes.iter().map(|node| NodeSummary::from(*node)).collect(),
        claims,
        knowledge_provenance: None,
        artifact_history: Vec::new(),
    }
}

fn explain_relationship(
    source_query: &str,
    target_query: &str,
    original: &str,
    graph: &GraphSnapshot,
) -> Result<Explanation, ExplainError> {
    let sources = graph.resolve(source_query);
    let targets = graph.resolve(target_query);
    if sources.is_empty() || targets.is_empty() {
        return Err(ExplainError::NotFound(original.to_owned()));
    }
    let mut matching = graph
        .edges
        .iter()
        .filter(|edge| {
            sources
                .iter()
                .any(|source| source.stable_key == edge.source)
                && targets
                    .iter()
                    .any(|target| target.stable_key == edge.target)
        })
        .map(|edge| claim_explanation(edge, graph))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Err(ExplainError::RelationshipNotFound(original.to_owned()));
    }
    sort_claims(&mut matching);
    let mut subjects = sources
        .into_iter()
        .chain(targets)
        .map(NodeSummary::from)
        .collect::<Vec<_>>();
    subjects.sort_by(|left, right| left.identifier.cmp(&right.identifier));
    subjects.dedup_by(|left, right| left.stable_key == right.stable_key);
    Ok(Explanation {
        query: original.to_owned(),
        subjects,
        claims: matching,
        knowledge_provenance: None,
        artifact_history: Vec::new(),
    })
}

fn claim_explanation(edge: &GraphEdge, graph: &GraphSnapshot) -> ClaimExplanation {
    let source = graph.nodes.get(&edge.source).map_or_else(
        || edge.source.to_string(),
        |node| node.identifier().to_owned(),
    );
    let target = graph.nodes.get(&edge.target).map_or_else(
        || edge.target.to_string(),
        |node| node.identifier().to_owned(),
    );
    ClaimExplanation {
        claim: format!("{source} {:?} {target}", edge.kind),
        claim_class: edge.claim_class,
        status: edge.status,
        confidence: edge.confidence.get(),
        valid_from: edge.valid_from.clone(),
        valid_to: edge.valid_to.clone(),
        provenance: edge.source_kind,
        producer: edge.producer.clone(),
        stale_reason: edge.stale_reason.clone(),
        evidence: edge.evidence.clone(),
    }
}

fn sort_claims(claims: &mut [ClaimExplanation]) {
    claims.sort_by(|left, right| left.claim.cmp(&right.claim));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        domain::{Confidence, RelationKind, StableKey},
        graph::GraphEdge,
        indexing::PlannedNodeAttributes,
        ir::{SourceRange, SymbolKind},
    };

    use super::*;

    /// Mirrors prompt3.md's `Replication` example (PR-LOOKUP-002/003, FR-04):
    /// several distinct namespaces sharing one short name is not an error,
    /// and each match's claims stay in its own independent [`Explanation`]
    /// instead of being pooled into one merged neighborhood.
    #[test]
    fn multiple_short_name_matches_produce_independent_explanations() {
        let manager = symbol_node("manager", "internal.logic.manager.Replication");
        let storage = symbol_node("storage", "storage.replication.Replication");
        let manager_requirement = intent_node("manager-req", "REQ-MANAGER-001");
        let storage_requirement = intent_node("storage-req", "REQ-STORAGE-001");
        let nodes = [
            manager.clone(),
            storage.clone(),
            manager_requirement.clone(),
            storage_requirement.clone(),
        ]
        .into_iter()
        .map(|node| (node.stable_key.clone(), node))
        .collect::<BTreeMap<_, _>>();
        let edges = vec![
            edge(&manager, &manager_requirement),
            edge(&storage, &storage_requirement),
        ];

        let mut explanations = explain("Replication", &GraphSnapshot { nodes, edges })
            .expect("independent explanations per match");

        assert_eq!(explanations.len(), 2);
        explanations.sort_by(|left, right| {
            left.subjects[0]
                .identifier
                .cmp(&right.subjects[0].identifier)
        });
        assert_eq!(
            explanations[0].subjects[0].identifier,
            "internal.logic.manager.Replication"
        );
        assert_eq!(explanations[0].claims.len(), 1);
        assert!(explanations[0].claims[0].claim.contains("REQ-MANAGER-001"));
        assert_eq!(
            explanations[1].subjects[0].identifier,
            "storage.replication.Replication"
        );
        assert_eq!(explanations[1].claims.len(), 1);
        assert!(explanations[1].claims[0].claim.contains("REQ-STORAGE-001"));
    }

    fn symbol_node(key: &str, canonical: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: crate::domain::NodeKind::CodeSymbol,
            name: canonical.rsplit('.').next().unwrap_or(canonical).to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: "file.rs".to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind: SymbolKind::Struct,
                range: SourceRange {
                    start_byte: 0,
                    end_byte: 1,
                    start_line: 1,
                    end_line: 1,
                },
                signature: None,
                structural_fingerprint: "shape".to_owned(),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                orm_accesses: Vec::new(),
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            },
        }
    }

    fn intent_node(key: &str, id: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: crate::domain::NodeKind::Requirement,
            name: id.to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: id.to_owned(),
                status: "active".to_owned(),
                visibility: crate::business::Visibility::Private,
                implementation_expected: true,
                body: "body".to_owned(),
                feature: None,
                source_uri: "context.yaml".to_owned(),
            },
        }
    }

    fn edge(source: &GraphNode, target: &GraphNode) -> GraphEdge {
        GraphEdge {
            source: source.stable_key.clone(),
            target: target.stable_key.clone(),
            kind: RelationKind::Implements,
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
}
