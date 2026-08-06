use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    domain::{ClaimClass, ClaimStatus, Confidence, NodeKind, RelationKind, SourceKind, StableKey},
    indexing::PlannedNodeAttributes,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub stable_key: StableKey,
    pub kind: NodeKind,
    pub name: String,
    pub content_hash: String,
    pub attributes: PlannedNodeAttributes,
}

impl GraphNode {
    pub fn identifier(&self) -> &str {
        match &self.attributes {
            PlannedNodeAttributes::File { path, .. } => path,
            PlannedNodeAttributes::Symbol { canonical_path, .. } => canonical_path,
            PlannedNodeAttributes::Interaction { identifier } => identifier,
            PlannedNodeAttributes::ApiEndpoint { endpoint } => endpoint.path.as_str(),
            PlannedNodeAttributes::ExternalCall { call } => call.url.as_str(),
            PlannedNodeAttributes::Business { id, .. } => id,
        }
    }

    pub fn is_test(&self) -> bool {
        matches!(
            &self.attributes,
            PlannedNodeAttributes::Symbol {
                symbol_kind: crate::ir::SymbolKind::Test,
                ..
            }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphEvidence {
    pub source_kind: SourceKind,
    pub source_uri: String,
    pub commit: Option<String>,
    pub author: Option<String>,
    pub timestamp: String,
    pub locator: String,
    pub strength: Confidence,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source: StableKey,
    pub target: StableKey,
    pub kind: RelationKind,
    pub claim_class: ClaimClass,
    pub source_kind: SourceKind,
    pub confidence: Confidence,
    pub status: ClaimStatus,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub producer: String,
    pub fingerprint: String,
    pub stale_reason: Option<String>,
    pub evidence: Vec<GraphEvidence>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub nodes: BTreeMap<StableKey, GraphNode>,
    pub edges: Vec<GraphEdge>,
}

impl GraphSnapshot {
    pub fn resolve(&self, query: &str) -> Vec<&GraphNode> {
        let mut exact = self
            .nodes
            .values()
            .filter(|node| {
                node.stable_key.as_str() == query
                    || node.identifier() == query
                    || node.name == query
                    || matches!(
                        &node.attributes,
                        PlannedNodeAttributes::ApiEndpoint { endpoint }
                            if endpoint.path == query
                    )
            })
            .collect::<Vec<_>>();
        exact.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
        if !exact.is_empty() {
            return exact;
        }
        let mut suffix = self
            .nodes
            .values()
            .filter(|node| {
                node.identifier()
                    .strip_suffix(query)
                    .is_some_and(|prefix| prefix.ends_with('.'))
            })
            .collect::<Vec<_>>();
        suffix.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
        suffix
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeSummary {
    pub stable_key: String,
    pub kind: NodeKind,
    pub identifier: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<crate::business::Visibility>,
}

impl From<&GraphNode> for NodeSummary {
    fn from(node: &GraphNode) -> Self {
        Self {
            stable_key: node.stable_key.to_string(),
            kind: node.kind,
            identifier: node.identifier().to_owned(),
            name: node.name.clone(),
            visibility: match &node.attributes {
                PlannedNodeAttributes::Business { visibility, .. } => Some(*visibility),
                _ => None,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymbolMatch {
    pub identifier: String,
    pub name: String,
    pub node_kind: NodeKind,
    pub symbol_kind: Option<crate::ir::SymbolKind>,
}

/// Discovery lookup for `ctx find <name>` (PR-LOOKUP-007): every distinct
/// exact/short-name match, annotated with enough to tell them apart, with no
/// ambiguity error and no merged traversal — plain discovery output.
pub fn find_symbols(query: &str, graph: &GraphSnapshot) -> Vec<SymbolMatch> {
    let mut matches = graph
        .resolve(query)
        .into_iter()
        .map(|node| SymbolMatch {
            identifier: node.identifier().to_owned(),
            name: node.name.clone(),
            node_kind: node.kind,
            symbol_kind: match &node.attributes {
                PlannedNodeAttributes::Symbol { symbol_kind, .. } => Some(*symbol_kind),
                _ => None,
            },
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.identifier.cmp(&right.identifier));
    matches
}

/// Finds requirements by stable ID and lexical content in deterministic order.
pub fn find_requirements(query: &str, graph: &GraphSnapshot) -> Vec<NodeSummary> {
    let terms = search_terms(query);
    let mut matches = graph
        .nodes
        .values()
        .filter(|node| node.kind == NodeKind::Requirement)
        .filter_map(|node| {
            let content = match &node.attributes {
                PlannedNodeAttributes::Business { body, .. } => body.as_str(),
                _ => "",
            };
            let searchable =
                search_terms(&format!("{} {} {content}", node.identifier(), node.name));
            let score = terms.intersection(&searchable).count();
            (score > 0 || node.identifier() == query).then_some((score, NodeSummary::from(node)))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.identifier.cmp(&right.1.identifier))
    });
    matches.into_iter().map(|(_, summary)| summary).collect()
}

fn search_terms(value: &str) -> std::collections::BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}
