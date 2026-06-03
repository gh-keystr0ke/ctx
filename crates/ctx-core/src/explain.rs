use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    domain::{ClaimClass, ClaimStatus, SourceKind},
    graph::{GraphEdge, GraphEvidence, GraphSnapshot, NodeSummary},
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Explanation {
    pub query: String,
    pub subjects: Vec<NodeSummary>,
    pub claims: Vec<ClaimExplanation>,
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
/// # Errors
///
/// Returns [`ExplainError`] when the node or relationship is absent.
pub fn explain(query: &str, graph: &GraphSnapshot) -> Result<Explanation, ExplainError> {
    if let Some((source_query, target_query)) = query.split_once("->") {
        return explain_relationship(source_query.trim(), target_query.trim(), query, graph);
    }
    let nodes = graph.resolve(query);
    if nodes.is_empty() {
        return Err(ExplainError::NotFound(query.to_owned()));
    }
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
    Ok(Explanation {
        query: query.to_owned(),
        subjects: nodes.into_iter().map(NodeSummary::from).collect(),
        claims,
    })
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
