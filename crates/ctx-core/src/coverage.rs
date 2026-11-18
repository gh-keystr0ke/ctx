//! Structural mappings and ranked behavioral points in an indexed graph.

mod new_decisions;
mod ranking;

pub use new_decisions::NewDecision;
pub(crate) use new_decisions::{explanation_document_keys, new_decisions};
pub mod research;
mod state;

pub use state::{
    CoverageRecord, DocumentEvidence, Gap, GapReport, GapState, gap_report, record_context,
};

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    domain::{ClaimClass, ClaimStatus, NodeKind, RelationKind, StableKey},
    graph::{GraphNode, GraphSnapshot},
    indexing::PlannedNodeAttributes,
    ir::{DecisionKind, SourceRange},
};

use ranking::Ranker;

pub const RANKING_VERSION: &str = RankingVersion::Current.name();

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum RankingVersion {
    Initial,
    Current,
}

impl RankingVersion {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Initial => "v0",
            Self::Current => "v1",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAnchor {
    pub symbol: StableKey,
    pub fingerprint: String,
    pub ordinal: usize,
}

impl std::fmt::Display for DecisionAnchor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}#{}#{}",
            self.symbol.as_str(),
            self.fingerprint,
            self.ordinal
        )
    }
}

#[derive(Debug, Serialize)]
pub struct RankedDecision {
    pub anchor: DecisionAnchor,
    pub file: String,
    pub symbol: String,
    pub kind: DecisionKind,
    pub range: SourceRange,
    pub condition: String,
    pub score: i64,
    pub ask: bool,
    pub contributions: BTreeMap<&'static str, i64>,
    pub effects: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Coverage {
    pub ranking_version: &'static str,
    pub mapped_symbols: usize,
    pub analyzed_symbols: usize,
    pub structural_percent: Option<f64>,
    pub mapped_intents: BTreeSet<String>,
    pub decisions: Vec<RankedDecision>,
}

#[must_use]
pub fn analyze_coverage(graph: &GraphSnapshot, selected_symbols: &BTreeSet<StableKey>) -> Coverage {
    analyze_with_ranking(graph, selected_symbols, RankingVersion::Current)
}

#[must_use]
pub fn analyze_with_ranking(
    graph: &GraphSnapshot,
    selected_symbols: &BTreeSet<StableKey>,
    version: RankingVersion,
) -> Coverage {
    let mappings = structural_mappings(graph);
    let mut ranker = Ranker::new(graph);
    let mut mapped_symbols = 0;
    let mut analyzed_symbols = 0;
    let mut mapped_intents = BTreeSet::new();
    let mut decisions = Vec::new();
    for symbol in selected_symbols
        .iter()
        .filter_map(|key| graph.nodes.get(key))
    {
        let PlannedNodeAttributes::Symbol {
            decision_points,
            file_path,
            ..
        } = &symbol.attributes
        else {
            continue;
        };
        if symbol.is_test() {
            continue;
        }
        analyzed_symbols += 1;
        let mapped = mappings.get(&symbol.stable_key);
        if let Some(intents) = mapped {
            mapped_symbols += 1;
            mapped_intents.extend(intents.iter().cloned());
        }
        let mut ordinals = BTreeMap::new();
        for point in decision_points {
            let ordinal = ordinals.entry(&point.fingerprint).or_insert(0);
            *ordinal += 1;
            let contributions = ranker.contributions(symbol, point, mapped.is_some(), version);
            let score = contributions.values().sum();
            decisions.push(RankedDecision {
                anchor: DecisionAnchor {
                    symbol: symbol.stable_key.clone(),
                    fingerprint: point.fingerprint.clone(),
                    ordinal: *ordinal,
                },
                file: file_path.clone(),
                symbol: symbol.identifier().to_owned(),
                kind: point.kind,
                range: point.range,
                condition: point.condition.clone(),
                score,
                ask: !(point.routine
                    || version == RankingVersion::Current && point.routine_extended)
                    && score >= 4,
                contributions,
                effects: ranking::evidence(symbol, point, version),
            });
        }
    }
    decisions.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.anchor.cmp(&right.anchor))
    });
    Coverage {
        ranking_version: version.name(),
        mapped_symbols,
        analyzed_symbols,
        structural_percent: percentage(mapped_symbols, analyzed_symbols),
        mapped_intents,
        decisions,
    }
}

#[allow(clippy::cast_precision_loss)]
fn percentage(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator != 0).then(|| 100.0 * numerator as f64 / denominator as f64)
}

#[must_use]
pub fn structural_mappings(graph: &GraphSnapshot) -> BTreeMap<StableKey, BTreeSet<String>> {
    let mut mappings: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    for edge in &graph.edges {
        if edge.status == ClaimStatus::Active
            && edge.claim_class == ClaimClass::Assertion
            && matches!(
                edge.kind,
                RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
            )
            && let Some(target) = graph
                .nodes
                .get(&edge.target)
                .filter(|node| active_intent(node))
        {
            mappings
                .entry(edge.source.clone())
                .or_default()
                .insert(target.identifier().to_owned());
        }
    }
    mappings
}

fn active_intent(node: &GraphNode) -> bool {
    matches!(
        node.kind,
        NodeKind::Requirement | NodeKind::Invariant | NodeKind::Decision
    ) && matches!(&node.attributes, PlannedNodeAttributes::Business { status, .. }
            if matches!(status.as_str(), "active" | "accepted" | "approved"))
}

pub(crate) fn confirmed_anchors(graph: &GraphSnapshot) -> BTreeSet<DecisionAnchor> {
    graph
        .nodes
        .values()
        .filter(|node| active_intent(node))
        .flat_map(|node| match &node.attributes {
            PlannedNodeAttributes::Business { anchors, .. } => anchors.as_slice(),
            _ => &[],
        })
        .filter(|anchor| is_current_anchor(graph, anchor))
        .cloned()
        .collect()
}

fn is_current_anchor(graph: &GraphSnapshot, anchor: &DecisionAnchor) -> bool {
    if anchor.ordinal == 0 {
        return false;
    }
    graph.nodes.get(&anchor.symbol).is_some_and(|node| {
        if let PlannedNodeAttributes::Symbol {
            decision_points, ..
        } = &node.attributes
        {
            decision_points
                .iter()
                .filter(|point| point.fingerprint == anchor.fingerprint)
                .nth(anchor.ordinal - 1)
                .is_some()
        } else {
            false
        }
    })
}
