use std::collections::{BTreeMap, BTreeSet};

use crate::{
    domain::{ClaimStatus, RelationKind, StableKey},
    graph::{GraphNode, GraphSnapshot},
    indexing::PlannedNodeAttributes,
    ir::{DatabaseAccessKind, DecisionKind, DecisionPoint, SourceRange},
};

pub(super) struct Ranker<'a> {
    graph: &'a GraphSnapshot,
    outgoing: BTreeMap<&'a StableKey, BTreeSet<&'a StableKey>>,
    incoming: BTreeMap<&'a StableKey, BTreeSet<&'a StableKey>>,
    summaries: BTreeMap<&'a StableKey, i64>,
    entries: BTreeSet<&'a StableKey>,
}

impl<'a> Ranker<'a> {
    pub(super) fn new(graph: &'a GraphSnapshot) -> Self {
        let mut result = Self {
            graph,
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
            summaries: BTreeMap::new(),
            entries: BTreeSet::new(),
        };
        for edge in graph
            .edges
            .iter()
            .filter(|edge| edge.status == ClaimStatus::Active)
        {
            match edge.kind {
                RelationKind::Calls => {
                    result
                        .outgoing
                        .entry(&edge.source)
                        .or_default()
                        .insert(&edge.target);
                    result
                        .incoming
                        .entry(&edge.target)
                        .or_default()
                        .insert(&edge.source);
                }
                RelationKind::Exposes => {
                    result.entries.insert(&edge.source);
                }
                RelationKind::Handles => {
                    result.entries.insert(&edge.target);
                }
                _ => {}
            }
        }
        result
    }

    pub(super) fn contributions(
        &mut self,
        node: &'a GraphNode,
        point: &DecisionPoint,
        mapped: bool,
    ) -> BTreeMap<&'static str, i64> {
        let mut contributions = BTreeMap::new();
        let condition = semantic_weight(&point.condition);
        if condition > 0 {
            contributions.insert("condition", condition);
        }
        let weights: Vec<_> = point
            .regions
            .iter()
            .map(|range| effects(node, Some(*range)))
            .collect();
        let direct = weights.iter().sum::<i64>().min(12);
        if direct > 0 {
            contributions.insert("direct_effects", direct);
        }
        if weights.len() >= 2 {
            let imbalance = weights.iter().max().unwrap_or(&0) - weights.iter().min().unwrap_or(&0);
            if imbalance > 0 {
                contributions.insert("branch_asymmetry", imbalance.min(4));
            }
        }
        let calls = self.controlled_callees(&node.stable_key, &point.controlled_calls);
        let downstream = calls
            .into_iter()
            .map(|key| self.summary(key))
            .sum::<i64>()
            .min(8);
        if downstream > 0 {
            contributions.insert("downstream_calls", downstream);
        }
        let reach = self.reachable_entries(&node.stable_key);
        if reach > 0 {
            contributions.insert("entrypoint_reach", reach);
        }
        if mapped {
            contributions.insert("mapped_intent", 1);
        }
        if matches!(point.kind, DecisionKind::DefaultValue | DecisionKind::Guard) {
            contributions.insert("decision_shape", 1);
        }
        contributions
    }

    fn controlled_callees(&self, source: &StableKey, calls: &[String]) -> BTreeSet<&'a StableKey> {
        let mut result = BTreeSet::new();
        if let Some(targets) = self.outgoing.get(source) {
            for call in calls {
                let short = call.rsplit(['.', ':']).next().unwrap_or(call);
                let mut candidates = targets.iter().filter(|target| {
                    self.graph
                        .nodes
                        .get(**target)
                        .is_some_and(|node| node.identifier() == call || node.name == short)
                });
                if let Some(target) = candidates.next()
                    && candidates.next().is_none()
                {
                    result.insert(*target);
                }
            }
        }
        result
    }

    fn summary(&mut self, key: &'a StableKey) -> i64 {
        if let Some(score) = self.summaries.get(key) {
            return *score;
        }
        let mut seen = BTreeSet::new();
        let mut frontier = BTreeSet::from([key]);
        let mut weight = 0;
        for depth in 1..=3 {
            let mut next = BTreeSet::new();
            for key in frontier {
                if seen.insert(key) {
                    weight += 1;
                    if let Some(node) = self.graph.nodes.get(key) {
                        weight += effects(node, None) / depth;
                    }
                    if let Some(children) = self.outgoing.get(key) {
                        next.extend(children);
                    }
                }
            }
            if weight >= 8 {
                break;
            }
            frontier = next;
        }
        self.summaries.insert(key, weight.min(8));
        weight.min(8)
    }

    fn reachable_entries(&self, key: &StableKey) -> i64 {
        let mut seen = BTreeSet::new();
        let mut frontier = BTreeSet::from([key]);
        let mut entries = 0;
        for _ in 0..4 {
            let mut next = BTreeSet::new();
            for key in frontier {
                if seen.insert(key) {
                    entries += i64::from(self.entries.contains(key));
                    if let Some(callers) = self.incoming.get(key) {
                        next.extend(callers);
                    }
                }
            }
            if entries >= 3 {
                return 3;
            }
            frontier = next;
        }
        entries
    }
}

fn effects(node: &GraphNode, area: Option<SourceRange>) -> i64 {
    let PlannedNodeAttributes::Symbol {
        database_accesses,
        orm_accesses,
        external_calls,
        ..
    } = &node.attributes
    else {
        return 0;
    };
    let weight = |kind| {
        if kind == DatabaseAccessKind::Write {
            5
        } else {
            2
        }
    };
    let includes = |range| area.is_none_or(|region| region.encloses(range));
    database_accesses
        .iter()
        .filter(|item| includes(item.range))
        .map(|item| weight(item.kind))
        .sum::<i64>()
        + orm_accesses
            .iter()
            .filter(|item| includes(item.range))
            .map(|item| weight(item.kind))
            .sum::<i64>()
        + external_calls
            .iter()
            .filter(|item| includes(item.range))
            .map(|_| 5)
            .sum::<i64>()
}

pub(super) fn semantic_weight(condition: &str) -> i64 {
    let condition = condition.to_ascii_lowercase();
    let terms = [
        "attempt",
        "retry",
        "limit",
        "quota",
        "status",
        "state",
        "permission",
        "role",
        "expired",
        "timeout",
        "amount",
        "price",
        "paid",
    ];
    if terms.iter().any(|term| condition.contains(term)) {
        4
    } else {
        0
    }
}
