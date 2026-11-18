use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{DecisionAnchor, RankedDecision, active_intent, research::ResearchJudgment};
use crate::{domain::StableKey, graph::GraphSnapshot, indexing::PlannedNodeAttributes};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GapState {
    Explained,
    AutoResolved,
    Open,
    Asked,
    Deferred,
    Unknown,
    Dismissed,
    Reopened,
    SuspectedDivergence,
    Ambiguous,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageRecord {
    pub anchor: DecisionAnchor,
    pub state: GapState,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<DocumentEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<ResearchJudgment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_documents: Option<Vec<DocumentEvidence>>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentEvidence {
    pub id: String,
    pub fingerprint: String,
}

#[derive(Debug, Serialize)]
pub struct Gap<'a> {
    pub point: &'a RankedDecision,
    pub state: GapState,
    pub document: Option<DocumentEvidence>,
    pub record: Option<&'a CoverageRecord>,
}

#[derive(Debug, Serialize)]
pub struct GapReport<'a> {
    pub gaps: Vec<Gap<'a>>,
    pub counts: BTreeMap<GapState, usize>,
    pub stale_anchors: Vec<DecisionAnchor>,
}

#[must_use]
pub fn gap_report<'a>(
    graph: &GraphSnapshot,
    decisions: &'a [RankedDecision],
    records: &'a BTreeMap<DecisionAnchor, CoverageRecord>,
    selected_symbols: &BTreeSet<StableKey>,
) -> GapReport<'a> {
    let anchors = document_anchors(graph);
    let structural = super::structural_mappings(graph);
    let current: BTreeSet<_> = decisions.iter().map(|point| &point.anchor).collect();
    let stale_anchors: Vec<_> = anchors
        .keys()
        .chain(records.keys())
        .filter(|anchor| !current.contains(anchor) && selected_symbols.contains(&anchor.symbol))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let changed_symbols: BTreeSet<_> = stale_anchors.iter().map(|anchor| &anchor.symbol).collect();
    let mut counts = BTreeMap::new();
    let gaps = decisions
        .iter()
        .filter(|point| {
            point.ask || anchors.contains_key(&point.anchor) || records.contains_key(&point.anchor)
        })
        .map(|point| {
            let record = records.get(&point.anchor);
            let (state, document) = gap_state(
                graph,
                anchors.get(&point.anchor),
                record,
                structural.get(&point.anchor.symbol),
                &point.anchor,
                changed_symbols.contains(&point.anchor.symbol),
            );
            *counts.entry(state).or_default() += 1;
            Gap {
                point,
                state,
                document,
                record,
            }
        })
        .collect();
    GapReport {
        gaps,
        counts,
        stale_anchors,
    }
}

fn gap_state(
    graph: &GraphSnapshot,
    documents: Option<&Vec<DocumentEvidence>>,
    record: Option<&CoverageRecord>,
    mapped: Option<&BTreeSet<String>>,
    anchor: &DecisionAnchor,
    changed: bool,
) -> (GapState, Option<DocumentEvidence>) {
    match documents.map(Vec::as_slice) {
        Some([document]) => return (GapState::Explained, Some(document.clone())),
        Some([_, _, ..]) => return (GapState::Ambiguous, None),
        _ => {}
    }
    if let Some(record) = record {
        if let Some(prior) = &record.context_documents
            && *prior != mapped_documents(graph, mapped, anchor)
        {
            return (GapState::Reopened, None);
        }
        if let Some(document) = &record.document {
            let current = graph
                .nodes
                .values()
                .find(|node| node.identifier() == document.id && active_intent(node));
            if current.is_none_or(|node| node.content_hash != document.fingerprint) {
                return (GapState::Reopened, None);
            }
        }
        return (record.state, record.document.clone());
    }
    (
        if changed {
            GapState::Reopened
        } else {
            GapState::Open
        },
        None,
    )
}

fn document_anchors(graph: &GraphSnapshot) -> BTreeMap<DecisionAnchor, Vec<DocumentEvidence>> {
    let mut documents: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for node in graph.nodes.values().filter(|node| active_intent(node)) {
        if let PlannedNodeAttributes::Business { anchors, id, .. } = &node.attributes {
            for anchor in anchors.iter().collect::<BTreeSet<_>>() {
                documents
                    .entry(anchor.clone())
                    .or_default()
                    .push(DocumentEvidence {
                        id: id.clone(),
                        fingerprint: node.content_hash.clone(),
                    });
            }
        }
    }
    documents
}

#[must_use]
pub fn record_context(graph: &GraphSnapshot, anchor: &DecisionAnchor) -> Vec<DocumentEvidence> {
    let mapped = super::structural_mappings(graph);
    mapped_documents(graph, mapped.get(&anchor.symbol), anchor)
}

fn mapped_documents(
    graph: &GraphSnapshot,
    mapped: Option<&BTreeSet<String>>,
    anchor: &DecisionAnchor,
) -> Vec<DocumentEvidence> {
    let Some(mapped) = mapped else {
        return Vec::new();
    };
    graph
        .nodes
        .values()
        .filter(|node| {
            mapped.contains(node.identifier()) && active_intent(node)
                && matches!(&node.attributes, PlannedNodeAttributes::Business { anchors, .. } if anchors.is_empty() || anchors.contains(anchor))
        })
        .map(|node| DocumentEvidence {
            id: node.identifier().to_owned(),
            fingerprint: node.content_hash.clone(),
        })
        .collect()
}
