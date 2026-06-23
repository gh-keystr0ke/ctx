use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    domain::{ClaimStatus, NodeKind, RelationKind, StableKey},
    graph::{GraphNode, GraphSnapshot},
    indexing::PlannedNodeAttributes,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolutionScore {
    pub explicit: f32,
    pub alias: f32,
    pub lexical: f32,
    pub structural: f32,
    pub test_correlation: f32,
    pub data_interaction: f32,
    pub semantic_similarity: Option<f32>,
    pub total: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticCandidate {
    pub fingerprint: String,
    pub source: StableKey,
    pub source_identifier: String,
    pub target: StableKey,
    pub target_identifier: String,
    pub relation: RelationKind,
    pub score: ResolutionScore,
    pub evidence: Vec<String>,
    pub impact_priority: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationDecision {
    Accept,
    Reject,
}

/// Generates conservative heuristic candidates that remain inferences until a
/// human records a separate assertion.
pub fn semantic_candidates(graph: &GraphSnapshot) -> Vec<SemanticCandidate> {
    let linked = existing_semantic_pairs(graph);
    let intents = graph
        .nodes
        .values()
        .filter(|node| is_intent(node.kind))
        .collect::<Vec<_>>();
    let symbols = graph
        .nodes
        .values()
        .filter(|node| node.kind == NodeKind::CodeSymbol && !node.is_test())
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for intent in intents {
        for symbol in &symbols {
            let relation = relation_for_intent(intent.kind);
            if linked.contains(&(
                symbol.stable_key.clone(),
                intent.stable_key.clone(),
                relation,
            )) {
                continue;
            }
            let score = score_candidate(graph, symbol, intent);
            if score.total < 0.65 {
                continue;
            }
            let evidence = score_evidence(&score);
            candidates.push(SemanticCandidate {
                fingerprint: format!(
                    "candidate:{}:{relation:?}:{}",
                    symbol.stable_key, intent.stable_key
                ),
                source: symbol.stable_key.clone(),
                source_identifier: symbol.identifier().to_owned(),
                target: intent.stable_key.clone(),
                target_identifier: intent.identifier().to_owned(),
                relation,
                score,
                evidence,
                impact_priority: intent_priority(intent.kind),
            });
        }
    }
    candidates.sort_by(|left, right| {
        right
            .impact_priority
            .cmp(&left.impact_priority)
            .then_with(|| right.score.total.total_cmp(&left.score.total))
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
    });
    candidates
}

fn score_candidate(
    graph: &GraphSnapshot,
    symbol: &GraphNode,
    intent: &GraphNode,
) -> ResolutionScore {
    let symbol_terms = node_terms(symbol);
    let intent_terms = node_terms(intent);
    let overlap = symbol_terms.intersection(&intent_terms).count();
    let lexical: f32 = match overlap {
        0 => 0.0,
        1 => 0.25,
        2 => 0.5,
        3 => 0.75,
        _ => 1.0,
    };
    let structural = structural_signal(graph, symbol, intent);
    let test_correlation = test_signal(graph, symbol, intent);
    let data_interaction = data_interaction_signal(graph, symbol, intent);
    let total = lexical.mul_add(
        0.40,
        structural.mul_add(
            0.35,
            test_correlation.mul_add(0.15, data_interaction * 0.10),
        ),
    );
    ResolutionScore {
        explicit: 0.0,
        alias: 0.0,
        lexical,
        structural,
        test_correlation,
        data_interaction,
        semantic_similarity: None,
        total,
    }
}

fn structural_signal(graph: &GraphSnapshot, symbol: &GraphNode, intent: &GraphNode) -> f32 {
    let verified_symbols = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.target == intent.stable_key
                && edge.status == ClaimStatus::Active
                && matches!(
                    edge.kind,
                    RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
                )
        })
        .map(|edge| &edge.source)
        .collect::<BTreeSet<_>>();
    if graph.edges.iter().any(|edge| {
        edge.kind == RelationKind::Calls
            && edge.status == ClaimStatus::Active
            && ((edge.source == symbol.stable_key && verified_symbols.contains(&edge.target))
                || (edge.target == symbol.stable_key && verified_symbols.contains(&edge.source)))
    }) {
        return 1.0;
    }
    let file_path = symbol_file(symbol);
    let same_file = verified_symbols.iter().any(|key| {
        graph
            .nodes
            .get(*key)
            .and_then(symbol_file)
            .zip(file_path)
            .is_some_and(|(verified, candidate)| verified == candidate)
    });
    if same_file { 0.6 } else { 0.0 }
}

fn test_signal(graph: &GraphSnapshot, symbol: &GraphNode, intent: &GraphNode) -> f32 {
    let linked_tests = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.source == intent.stable_key
                && edge.kind == RelationKind::CoveredBy
                && edge.status == ClaimStatus::Active
        })
        .map(|edge| &edge.target)
        .collect::<BTreeSet<_>>();
    if graph.edges.iter().any(|edge| {
        edge.kind == RelationKind::Calls
            && ((edge.source == symbol.stable_key && linked_tests.contains(&edge.target))
                || (edge.target == symbol.stable_key && linked_tests.contains(&edge.source)))
    }) {
        1.0
    } else {
        0.0
    }
}

fn data_interaction_signal(graph: &GraphSnapshot, symbol: &GraphNode, intent: &GraphNode) -> f32 {
    let verified_symbols = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.target == intent.stable_key
                && edge.status == ClaimStatus::Active
                && matches!(
                    edge.kind,
                    RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
                )
        })
        .map(|edge| &edge.source)
        .collect::<BTreeSet<_>>();
    let candidate_interactions = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.source == symbol.stable_key
                && edge.status == ClaimStatus::Active
                && matches!(edge.kind, RelationKind::ReadsFrom | RelationKind::WritesTo)
        })
        .map(|edge| (&edge.target, edge.kind))
        .collect::<BTreeSet<_>>();
    let verified_interactions = graph
        .edges
        .iter()
        .filter(|edge| {
            verified_symbols.contains(&edge.source)
                && edge.status == ClaimStatus::Active
                && matches!(edge.kind, RelationKind::ReadsFrom | RelationKind::WritesTo)
        })
        .map(|edge| (&edge.target, edge.kind))
        .collect::<BTreeSet<_>>();
    if candidate_interactions
        .intersection(&verified_interactions)
        .next()
        .is_some()
    {
        return 1.0;
    }
    if candidate_interactions.iter().any(|(candidate, _)| {
        verified_interactions
            .iter()
            .any(|(verified, _)| candidate == verified)
    }) {
        0.6
    } else {
        0.0
    }
}

fn score_evidence(score: &ResolutionScore) -> Vec<String> {
    let mut evidence = Vec::new();
    if score.lexical > 0.0 {
        evidence.push(format!("lexical signal {:.2}", score.lexical));
    }
    if score.structural > 0.0 {
        evidence.push(format!("structural graph signal {:.2}", score.structural));
    }
    if score.test_correlation > 0.0 {
        evidence.push(format!("test correlation {:.2}", score.test_correlation));
    }
    if score.data_interaction > 0.0 {
        evidence.push(format!(
            "shared database interaction {:.2}",
            score.data_interaction
        ));
    }
    evidence
}

fn existing_semantic_pairs(
    graph: &GraphSnapshot,
) -> BTreeSet<(StableKey, StableKey, RelationKind)> {
    graph
        .edges
        .iter()
        .filter(|edge| {
            matches!(
                edge.kind,
                RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
            )
        })
        .map(|edge| (edge.source.clone(), edge.target.clone(), edge.kind))
        .collect()
}

fn node_terms(node: &GraphNode) -> BTreeSet<String> {
    let content = match &node.attributes {
        PlannedNodeAttributes::Business { body, .. } => body.as_str(),
        PlannedNodeAttributes::Symbol { canonical_path, .. } => canonical_path.as_str(),
        PlannedNodeAttributes::File { path, .. } => path.as_str(),
        PlannedNodeAttributes::Interaction { identifier } => identifier.as_str(),
    };
    format!("{} {} {content}", node.identifier(), node.name)
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.len() >= 3)
        .map(str::to_ascii_lowercase)
        .filter(|term| !STOP_WORDS.contains(&term.as_str()))
        .collect()
}

fn symbol_file(node: &GraphNode) -> Option<&str> {
    match &node.attributes {
        PlannedNodeAttributes::Symbol { file_path, .. } => Some(file_path),
        _ => None,
    }
}

const fn is_intent(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Feature | NodeKind::Requirement | NodeKind::Invariant | NodeKind::Decision
    )
}

const fn relation_for_intent(kind: NodeKind) -> RelationKind {
    match kind {
        NodeKind::Invariant => RelationKind::Enforces,
        NodeKind::Decision => RelationKind::Satisfies,
        NodeKind::Feature
        | NodeKind::Requirement
        | NodeKind::DomainConcept
        | NodeKind::ExternalSystem
        | NodeKind::File
        | NodeKind::CodeSymbol
        | NodeKind::Endpoint
        | NodeKind::DbEntity
        | NodeKind::Event => RelationKind::Implements,
    }
}

const fn intent_priority(kind: NodeKind) -> usize {
    match kind {
        NodeKind::Invariant => 4,
        NodeKind::Requirement => 3,
        NodeKind::Feature | NodeKind::Decision => 2,
        _ => 0,
    }
}

const STOP_WORDS: &[&str] = &[
    "and", "are", "for", "from", "must", "not", "the", "this", "until", "when", "with",
];

#[cfg(test)]
mod tests {
    use crate::{
        domain::{ClaimClass, Confidence, SourceKind},
        graph::{GraphEdge, GraphNode},
        ir::{SourceRange, SymbolKind},
    };

    use super::*;

    #[test]
    fn candidate_score_exposes_individual_heuristic_signals() {
        let intent = intent_node();
        let existing = symbol_node("existing", "subscription.cancel_access_existing");
        let candidate = symbol_node("candidate", "subscription.cancel_access_handler");
        let database = interaction_node("db:subscriptions", "subscriptions");
        let graph = GraphSnapshot {
            nodes: [
                intent.clone(),
                existing.clone(),
                candidate.clone(),
                database.clone(),
            ]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect(),
            edges: vec![
                edge(&existing, &intent, RelationKind::Implements),
                edge(&candidate, &existing, RelationKind::Calls),
                edge(&existing, &database, RelationKind::WritesTo),
                edge(&candidate, &database, RelationKind::WritesTo),
            ],
        };

        let candidates = semantic_candidates(&graph);
        let proposal = candidates
            .iter()
            .find(|item| item.source == candidate.stable_key)
            .expect("candidate proposal");

        assert!(proposal.score.lexical > 0.0);
        assert!((proposal.score.structural - 1.0).abs() < f32::EPSILON);
        assert!((proposal.score.data_interaction - 1.0).abs() < f32::EPSILON);
        assert!(proposal.score.total >= 0.65);
        assert!(proposal.evidence.len() >= 3);
    }

    fn intent_node() -> GraphNode {
        GraphNode {
            stable_key: StableKey::new("intent").expect("intent key"),
            kind: NodeKind::Requirement,
            name: "Subscription cancel access".to_owned(),
            content_hash: "intent".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: "REQ-SUB-001".to_owned(),
                status: "active".to_owned(),
                body: "Subscription cancel access remains available".to_owned(),
                feature: None,
                source_uri: "requirement.yaml".to_owned(),
            },
        }
    }

    fn symbol_node(key: &str, canonical: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("symbol key"),
            kind: NodeKind::CodeSymbol,
            name: canonical.rsplit('.').next().unwrap_or(canonical).to_owned(),
            content_hash: "symbol".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: "subscription.py".to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind: SymbolKind::Function,
                range: SourceRange {
                    start_byte: 0,
                    end_byte: 1,
                    start_line: 1,
                    end_line: 1,
                },
                signature: None,
                structural_fingerprint: key.to_owned(),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                schema_tables: Vec::new(),
            },
        }
    }

    fn interaction_node(key: &str, identifier: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("interaction key"),
            kind: NodeKind::DbEntity,
            name: identifier.to_owned(),
            content_hash: identifier.to_owned(),
            attributes: PlannedNodeAttributes::Interaction {
                identifier: identifier.to_owned(),
            },
        }
    }

    fn edge(source: &GraphNode, target: &GraphNode, kind: RelationKind) -> GraphEdge {
        GraphEdge {
            source: source.stable_key.clone(),
            target: target.stable_key.clone(),
            kind,
            claim_class: if kind.is_semantic() {
                ClaimClass::Assertion
            } else {
                ClaimClass::Fact
            },
            source_kind: SourceKind::StaticAnalysis,
            confidence: Confidence::CERTAIN,
            status: ClaimStatus::Active,
            valid_from: "commit".to_owned(),
            valid_to: None,
            producer: "test".to_owned(),
            fingerprint: format!("{kind:?}:{}", source.stable_key),
            stale_reason: None,
            evidence: Vec::new(),
        }
    }
}
