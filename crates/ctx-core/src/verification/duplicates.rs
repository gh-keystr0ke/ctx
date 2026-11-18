use std::collections::{BTreeMap, BTreeSet};

use crate::{
    domain::{ClaimStatus, NodeKind, RelationKind, StableKey},
    graph::{GraphNode, GraphSnapshot},
    indexing::PlannedNodeAttributes,
    repair::DuplicateDocuments,
};

use super::node_terms;

pub(crate) fn duplicate_documents(graph: &GraphSnapshot) -> Vec<DuplicateDocuments> {
    let linked = code_links(graph);
    let docs: Vec<_> = graph
        .nodes
        .values()
        .filter(|node| active_document(node))
        .map(|node| (node, node_terms(node)))
        .filter(|(_, terms)| !terms.is_empty())
        .collect();
    let link_count = |node: &GraphNode| linked.get(&node.stable_key).map_or(0, BTreeSet::len);
    let mut duplicates = Vec::new();
    for (index, (left, left_terms)) in docs.iter().enumerate() {
        for (right, right_terms) in &docs[index + 1..] {
            let smaller = left_terms.len().min(right_terms.len());
            if left.kind != right.kind
                || left_terms.intersection(right_terms).count() * 5 < smaller * 3
            {
                continue;
            }
            let (retained, superseded) = if link_count(left) > link_count(right)
                || (link_count(left) == link_count(right) && left.identifier() < right.identifier())
            {
                (*left, *right)
            } else {
                (*right, *left)
            };
            duplicates.push(DuplicateDocuments {
                retained: retained.identifier().to_owned(),
                superseded: superseded.identifier().to_owned(),
            });
        }
    }
    duplicates.sort_by(|left, right| {
        (&left.retained, &left.superseded).cmp(&(&right.retained, &right.superseded))
    });
    duplicates
}

fn active_document(node: &GraphNode) -> bool {
    matches!(
        node.kind,
        NodeKind::Requirement | NodeKind::Invariant | NodeKind::Decision
    ) && matches!(
        &node.attributes,
        PlannedNodeAttributes::Business { status, .. }
            if !matches!(status.as_str(), "deprecated" | "superseded" | "rejected" | "draft" | "proposed")
    )
}

fn code_links(graph: &GraphSnapshot) -> BTreeMap<&StableKey, BTreeSet<&StableKey>> {
    let mut links: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    for edge in &graph.edges {
        if edge.status == ClaimStatus::Active
            && matches!(
                edge.kind,
                RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
            )
            && graph
                .nodes
                .get(&edge.source)
                .is_some_and(|node| node.kind == NodeKind::CodeSymbol)
        {
            links.entry(&edge.target).or_default().insert(&edge.source);
        }
    }
    links
}
