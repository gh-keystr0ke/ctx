use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    graph::GraphSnapshot,
    indexing::PlannedNodeAttributes,
    ir::{DecisionKind, DecisionPoint, SymbolKind},
    review::ReviewInput,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NewDecision {
    pub file: String,
    pub symbol: String,
    pub line: usize,
    pub kind: DecisionKind,
    pub condition: String,
}

pub(crate) fn new_decisions(graph: &GraphSnapshot, input: &ReviewInput) -> Vec<NewDecision> {
    if !input.verbose {
        return Vec::new();
    }
    let mut previous: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for file in input.before.values() {
        for symbol in &file.symbols {
            for point in &symbol.decision_points {
                *previous
                    .entry((&symbol.canonical_path, &point.fingerprint))
                    .or_default() += 1;
            }
        }
    }
    let anchored = super::confirmed_anchors(graph);
    let known: BTreeSet<_> = anchored
        .iter()
        .filter_map(|anchor| {
            let symbol = graph.nodes.get(&anchor.symbol)?;
            Some((
                symbol.identifier(),
                anchor.fingerprint.as_str(),
                anchor.ordinal,
            ))
        })
        .collect();
    let mut result = Vec::new();
    for file in input.after.values() {
        for symbol in file
            .symbols
            .iter()
            .filter(|symbol| symbol.kind != SymbolKind::Test)
        {
            let mut ordinals = BTreeMap::<&str, usize>::new();
            for point in &symbol.decision_points {
                let ordinal = ordinals.entry(&point.fingerprint).or_default();
                *ordinal += 1;
                let key = (symbol.canonical_path.as_str(), point.fingerprint.as_str());
                if *ordinal > previous.get(&key).copied().unwrap_or_default()
                    && !known.contains(&(key.0, key.1, *ordinal))
                    && relevant(point, symbol)
                {
                    result.push(NewDecision {
                        file: file.path.clone(),
                        symbol: symbol.canonical_path.clone(),
                        line: point.range.start_line,
                        kind: point.kind,
                        condition: point.condition.clone(),
                    });
                }
            }
        }
    }
    result
}

fn relevant(point: &DecisionPoint, symbol: &crate::ir::SymbolDefinition) -> bool {
    !point.routine
        && (super::ranking::semantic_weight(&point.condition) >= 4
            || point.regions.iter().any(|range| {
                symbol
                    .database_accesses
                    .iter()
                    .any(|access| range.encloses(access.range))
                    || symbol
                        .external_calls
                        .iter()
                        .any(|call| range.encloses(call.range))
            }))
}

pub(crate) fn explanation_document_keys(
    graph: &GraphSnapshot,
    reachable: &BTreeMap<crate::domain::StableKey, usize>,
) -> Vec<crate::domain::StableKey> {
    graph
        .nodes
        .values()
        .filter(|node| super::active_intent(node))
        .filter_map(|node| {
            let PlannedNodeAttributes::Business { anchors, .. } = &node.attributes else {
                return None;
            };
            anchors
                .iter()
                .any(|anchor| {
                    reachable
                        .get(&anchor.symbol)
                        .is_some_and(|distance| *distance <= 2)
                        && super::is_current_anchor(graph, anchor)
                })
                .then(|| node.stable_key.clone())
        })
        .collect()
}
