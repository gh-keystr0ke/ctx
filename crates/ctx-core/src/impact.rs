use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    domain::{ClaimClass, ClaimStatus, NodeKind, RelationKind, StableKey},
    graph::{GraphEdge, GraphSnapshot, NodeSummary},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImpactUncertainty {
    pub relationship: String,
    pub reason: String,
    pub confidence: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImpactReport {
    pub query: String,
    pub selected: Vec<NodeSummary>,
    pub features: Vec<NodeSummary>,
    pub requirements: Vec<NodeSummary>,
    pub invariants: Vec<NodeSummary>,
    pub decisions: Vec<NodeSummary>,
    pub implementation: Vec<NodeSummary>,
    pub tests: Vec<NodeSummary>,
    pub uncertainties: Vec<ImpactUncertainty>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ImpactError {
    #[error("no indexed file, symbol, or product context matches '{0}'")]
    NotFound(String),
    #[error("'{query}' is ambiguous; matches: {matches}")]
    Ambiguous { query: String, matches: String },
}

/// Compiles a bounded intent-to-implementation impact view.
///
/// The policy expands file containment and calls only one hop, then follows at
/// most three semantic hops. An inference can never lead to another inference.
///
/// # Errors
///
/// Returns [`ImpactError`] when the exact/suffix seed is missing or ambiguous.
pub fn analyze_impact(query: &str, graph: &GraphSnapshot) -> Result<ImpactReport, ImpactError> {
    let seeds = resolve_unique(query, graph)?;
    let seed_keys = seeds
        .iter()
        .map(|node| node.stable_key.clone())
        .collect::<BTreeSet<_>>();
    let mut selected = seed_keys.clone();
    expand_structural_seed_neighborhood(graph, &seed_keys, &mut selected);
    let mut uncertainties = Vec::new();
    expand_semantics(graph, &mut selected, &mut uncertainties);

    let mut report = ImpactReport {
        query: query.to_owned(),
        selected: seeds.iter().map(|node| NodeSummary::from(*node)).collect(),
        uncertainties,
        ..ImpactReport::default()
    };
    for key in selected {
        let Some(node) = graph.nodes.get(&key) else {
            continue;
        };
        let summary = NodeSummary::from(node);
        match node.kind {
            NodeKind::Feature => report.features.push(summary),
            NodeKind::Requirement => report.requirements.push(summary),
            NodeKind::Invariant => report.invariants.push(summary),
            NodeKind::Decision => report.decisions.push(summary),
            NodeKind::CodeSymbol if node.is_test() => report.tests.push(summary),
            NodeKind::CodeSymbol | NodeKind::File => report.implementation.push(summary),
            _ => {}
        }
    }
    sort_report(&mut report);
    Ok(report)
}

fn resolve_unique<'a>(
    query: &str,
    graph: &'a GraphSnapshot,
) -> Result<Vec<&'a crate::graph::GraphNode>, ImpactError> {
    let nodes = graph.resolve(query);
    if nodes.is_empty() {
        return Err(ImpactError::NotFound(query.to_owned()));
    }
    let identifiers = nodes
        .iter()
        .map(|node| node.identifier())
        .collect::<BTreeSet<_>>();
    if identifiers.len() > 1 {
        return Err(ImpactError::Ambiguous {
            query: query.to_owned(),
            matches: identifiers.into_iter().collect::<Vec<_>>().join(", "),
        });
    }
    Ok(nodes)
}

fn expand_structural_seed_neighborhood(
    graph: &GraphSnapshot,
    seeds: &BTreeSet<StableKey>,
    selected: &mut BTreeSet<StableKey>,
) {
    for edge in graph
        .edges
        .iter()
        .filter(|edge| edge.status == ClaimStatus::Active)
    {
        let touches_seed = seeds.contains(&edge.source) || seeds.contains(&edge.target);
        if !touches_seed || !matches!(edge.kind, RelationKind::Contains | RelationKind::Calls) {
            continue;
        }
        selected.insert(edge.source.clone());
        selected.insert(edge.target.clone());
    }
}

fn expand_semantics(
    graph: &GraphSnapshot,
    selected: &mut BTreeSet<StableKey>,
    uncertainties: &mut Vec<ImpactUncertainty>,
) {
    let mut inferred_reached = BTreeSet::new();
    for _ in 0..3 {
        let before = selected.len();
        for edge in graph.edges.iter().filter(|edge| edge.kind.is_semantic()) {
            let source_selected = selected.contains(&edge.source);
            let target_selected = selected.contains(&edge.target);
            if source_selected == target_selected {
                continue;
            }
            let known = if source_selected {
                &edge.source
            } else {
                &edge.target
            };
            let candidate = if source_selected {
                &edge.target
            } else {
                &edge.source
            };
            if edge.status != ClaimStatus::Active {
                uncertainties.push(uncertainty(edge, "stale relationship"));
                selected.insert(candidate.clone());
                continue;
            }
            if edge.claim_class == ClaimClass::Inference {
                uncertainties.push(uncertainty(edge, "inferred relationship"));
                if inferred_reached.contains(known) || edge.confidence.get() < 0.65 {
                    continue;
                }
                inferred_reached.insert(candidate.clone());
            }
            selected.insert(candidate.clone());
        }
        if selected.len() == before {
            break;
        }
    }
}

fn uncertainty(edge: &GraphEdge, reason: &str) -> ImpactUncertainty {
    ImpactUncertainty {
        relationship: format!("{} -> {}", edge.source, edge.target),
        reason: edge
            .stale_reason
            .clone()
            .unwrap_or_else(|| reason.to_owned()),
        confidence: edge.confidence.get(),
    }
}

fn sort_report(report: &mut ImpactReport) {
    for items in [
        &mut report.selected,
        &mut report.features,
        &mut report.requirements,
        &mut report.invariants,
        &mut report.decisions,
        &mut report.implementation,
        &mut report.tests,
    ] {
        items.sort_by(|left, right| left.identifier.cmp(&right.identifier));
        items.dedup_by(|left, right| left.stable_key == right.stable_key);
    }
    report
        .uncertainties
        .sort_by(|left, right| left.relationship.cmp(&right.relationship));
    report
        .uncertainties
        .dedup_by(|left, right| left.relationship == right.relationship);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        domain::{Confidence, SourceKind},
        graph::{GraphEdge, GraphNode},
        indexing::PlannedNodeAttributes,
        ir::{SourceRange, SymbolKind},
    };

    use super::*;

    #[test]
    fn follows_only_the_product_chain_and_surfaces_staleness() {
        let code = symbol_node("code", "billing.cancel", SymbolKind::Method);
        let test = symbol_node("test", "tests.test_cancel", SymbolKind::Test);
        let requirement = intent_node("req", NodeKind::Requirement, "REQ-SUB-014");
        let invariant = intent_node("inv", NodeKind::Invariant, "INV-SUB-003");
        let feature = intent_node("feature", NodeKind::Feature, "FEAT-SUBSCRIPTIONS");
        let unrelated = intent_node("other", NodeKind::Requirement, "REQ-OTHER-001");
        let nodes = [
            code.clone(),
            test.clone(),
            requirement.clone(),
            invariant.clone(),
            feature.clone(),
            unrelated,
        ]
        .into_iter()
        .map(|node| (node.stable_key.clone(), node))
        .collect::<BTreeMap<_, _>>();
        let edges = vec![
            edge(
                &code,
                &requirement,
                RelationKind::Implements,
                ClaimStatus::Active,
            ),
            edge(
                &code,
                &invariant,
                RelationKind::Enforces,
                ClaimStatus::Stale,
            ),
            edge(
                &requirement,
                &feature,
                RelationKind::DependsOn,
                ClaimStatus::Active,
            ),
            edge(
                &requirement,
                &test,
                RelationKind::CoveredBy,
                ClaimStatus::Active,
            ),
        ];

        let report = analyze_impact("billing.cancel", &GraphSnapshot { nodes, edges })
            .expect("impact report");

        assert_eq!(report.requirements[0].identifier, "REQ-SUB-014");
        assert_eq!(report.invariants[0].identifier, "INV-SUB-003");
        assert_eq!(report.features[0].identifier, "FEAT-SUBSCRIPTIONS");
        assert_eq!(report.tests[0].identifier, "tests.test_cancel");
        assert_eq!(report.uncertainties.len(), 1);
    }

    fn symbol_node(key: &str, canonical: &str, kind: SymbolKind) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::CodeSymbol,
            name: canonical.rsplit('.').next().unwrap_or(canonical).to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: "file.py".to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind: kind,
                range: SourceRange {
                    start_byte: 0,
                    end_byte: 1,
                    start_line: 1,
                    end_line: 1,
                },
                signature: None,
                structural_fingerprint: "shape".to_owned(),
                calls: Vec::new(),
            },
        }
    }

    fn intent_node(key: &str, kind: NodeKind, id: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind,
            name: id.to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: id.to_owned(),
                status: "active".to_owned(),
                body: "body".to_owned(),
                feature: None,
                source_uri: "context.yaml".to_owned(),
            },
        }
    }

    fn edge(
        source: &GraphNode,
        target: &GraphNode,
        kind: RelationKind,
        status: ClaimStatus,
    ) -> GraphEdge {
        GraphEdge {
            source: source.stable_key.clone(),
            target: target.stable_key.clone(),
            kind,
            claim_class: ClaimClass::Assertion,
            source_kind: SourceKind::Documentation,
            confidence: Confidence::CERTAIN,
            status,
            valid_from: "commit".to_owned(),
            valid_to: None,
            producer: "test".to_owned(),
            fingerprint: format!("{}:{:?}", source.stable_key, kind),
            stale_reason: (status == ClaimStatus::Stale)
                .then(|| "implementation_changed".to_owned()),
            evidence: Vec::new(),
        }
    }
}
