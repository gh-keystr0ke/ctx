use std::collections::{BTreeSet, VecDeque};

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
    pub data_contracts: Vec<NodeSummary>,
    pub implementation: Vec<NodeSummary>,
    pub tests: Vec<NodeSummary>,
    pub uncertainties: Vec<ImpactUncertainty>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ImpactError {
    #[error("no indexed file, symbol, or product context matches '{0}'")]
    NotFound(String),
}

/// Compiles a bounded intent-to-implementation impact view for every distinct
/// symbol/node a short or ambiguous query resolves to.
///
/// Several exact matches (for example a short name shared by symbols in
/// unrelated namespaces) are not an error: each distinct match gets its own
/// independently computed [`ImpactReport`], equivalent to calling this
/// function once per fully-qualified match and aggregating the results. This
/// keeps unrelated graph neighborhoods from leaking into one another.
///
/// The policy expands file containment, calls, and statically proven data or
/// external interactions only one hop, then follows at most three semantic
/// hops. An inference can never lead to another inference.
///
/// # Errors
///
/// Returns [`ImpactError`] when the query resolves to nothing.
pub fn analyze_impact(
    query: &str,
    graph: &GraphSnapshot,
) -> Result<Vec<ImpactReport>, ImpactError> {
    let (seed_query, column) = resolve_table_column_seed(query, graph);
    let groups = resolve_groups(&seed_query, graph)?;
    Ok(groups
        .into_iter()
        .map(|seeds| analyze_impact_for_seeds(query, &seeds, column, graph))
        .collect())
}

fn analyze_impact_for_seeds(
    query: &str,
    seeds: &[&crate::graph::GraphNode],
    column: Option<&str>,
    graph: &GraphSnapshot,
) -> ImpactReport {
    let seed_keys = seeds
        .iter()
        .map(|node| node.stable_key.clone())
        .collect::<BTreeSet<_>>();
    let mut selected = seed_keys.clone();
    expand_structural_seed_neighborhood(graph, &seed_keys, &mut selected);
    let mut uncertainties = Vec::new();
    expand_semantics(graph, &seed_keys, &mut selected, &mut uncertainties);

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
            NodeKind::DbEntity
            | NodeKind::ExternalSystem
            | NodeKind::Endpoint
            | NodeKind::Event => {
                report.data_contracts.push(summary);
            }
            NodeKind::CodeSymbol if node.is_test() => report.tests.push(summary),
            NodeKind::CodeSymbol | NodeKind::File => report.implementation.push(summary),
            NodeKind::DomainConcept => {}
        }
    }
    if let Some(column) = column {
        focus_on_column(graph, &seed_keys, column, &mut report);
    }
    sort_report(&mut report);
    report
}

/// Recognizes a `table.column` seed (for example `subscriptions.paid_until`)
/// so `ctx impact` can answer a column-level question without a dedicated
/// column graph node: the seed still resolves to the table's `DbEntity` (the
/// same node a bare table query resolves to), and the recognized column name
/// is used afterward to narrow `implementation` to the specific
/// readers/writers whose evidence names that column. An exact match on the
/// literal query (a real node identifier that happens to contain a dot) is
/// always preferred, and `table` must resolve to exactly one `DbEntity` —
/// anything else falls back to resolving `query` unchanged, which reports
/// "not found" exactly as it did before this recognizer existed.
fn resolve_table_column_seed<'a>(
    query: &'a str,
    graph: &GraphSnapshot,
) -> (String, Option<&'a str>) {
    if !graph.resolve(query).is_empty() {
        return (query.to_owned(), None);
    }
    let Some((table, column)) = query.rsplit_once('.') else {
        return (query.to_owned(), None);
    };
    let matches = graph.resolve(table);
    if let [node] = matches.as_slice()
        && node.kind == NodeKind::DbEntity
    {
        (table.to_owned(), Some(column))
    } else {
        (query.to_owned(), None)
    }
}

/// Narrows `report.implementation` to the code that a `table.column` seed's
/// evidence actually names as reading/writing/declaring that column. When no
/// evidence anywhere mentions the column, the table-level impact is left
/// intact and an uncertainty explains why — a possible typo or a column this
/// codebase's static recognizers cannot see should never silently look
/// identical to "this column has no readers".
fn focus_on_column(
    graph: &GraphSnapshot,
    seed_keys: &BTreeSet<StableKey>,
    column: &str,
    report: &mut ImpactReport,
) {
    let column_readers = graph
        .edges
        .iter()
        .filter(|edge| {
            seed_keys.contains(&edge.target)
                && matches!(
                    edge.kind,
                    RelationKind::ReadsFrom | RelationKind::WritesTo | RelationKind::DefinesSchema
                )
                && edge
                    .evidence
                    .iter()
                    .any(|evidence| evidence_columns(&evidence.locator).contains(column))
        })
        .map(|edge| edge.source.clone())
        .collect::<BTreeSet<_>>();
    if column_readers.is_empty() {
        report.uncertainties.push(ImpactUncertainty {
            relationship: format!("{}.{column}", report.query.rsplit_once('.').map_or("", |(t, _)| t)),
            reason: format!(
                "column '{column}' was not found in any known schema declaration or static access evidence for this table; showing table-level impact instead"
            ),
            confidence: 0.0,
        });
        return;
    }
    let readers = column_readers
        .iter()
        .filter_map(|key| graph.nodes.get(key))
        .map(|node| node.identifier().to_owned())
        .collect::<BTreeSet<_>>();
    report
        .implementation
        .retain(|summary| readers.contains(&summary.identifier));
}

fn evidence_columns(locator: &str) -> BTreeSet<&str> {
    locator
        .split("columns:")
        .nth(1)
        .into_iter()
        .flat_map(|rest| rest.split(','))
        .map(str::trim)
        .filter(|column| !column.is_empty())
        .collect()
}

/// Resolves a query to its distinct matches, grouped by identifier: nodes
/// that legitimately share one identifier (the same symbol resolved through
/// different lookup paths) stay one seed group and analyze together, while
/// genuinely distinct matches (a short name shared across namespaces) each
/// become their own independent group. Groups are ordered by identifier for
/// deterministic output.
fn resolve_groups<'a>(
    query: &str,
    graph: &'a GraphSnapshot,
) -> Result<Vec<Vec<&'a crate::graph::GraphNode>>, ImpactError> {
    let nodes = graph.resolve(query);
    if nodes.is_empty() {
        return Err(ImpactError::NotFound(query.to_owned()));
    }
    let mut grouped = std::collections::BTreeMap::<&str, Vec<&crate::graph::GraphNode>>::new();
    for node in nodes {
        grouped.entry(node.identifier()).or_default().push(node);
    }
    Ok(grouped.into_values().collect())
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
        if !touches_seed
            || !matches!(
                edge.kind,
                RelationKind::Contains
                    | RelationKind::Calls
                    | RelationKind::References
                    | RelationKind::ReadsFrom
                    | RelationKind::WritesTo
                    | RelationKind::DefinesSchema
                    | RelationKind::Emits
                    | RelationKind::Handles
            )
        {
            continue;
        }
        selected.insert(edge.source.clone());
        selected.insert(edge.target.clone());
    }
}

fn expand_semantics(
    graph: &GraphSnapshot,
    seed_keys: &BTreeSet<StableKey>,
    selected: &mut BTreeSet<StableKey>,
    uncertainties: &mut Vec<ImpactUncertainty>,
) {
    let roots = selected.clone();
    let mut queue = roots
        .iter()
        .cloned()
        .map(|key| {
            let is_seed = seed_keys.contains(&key);
            (key, 0_usize, false, is_seed)
        })
        .collect::<VecDeque<_>>();
    let mut visited = roots
        .into_iter()
        .map(|key| (key, false))
        .collect::<BTreeSet<_>>();

    while let Some((key, distance, inferred, is_seed)) = queue.pop_front() {
        if distance >= 3 || !can_expand_semantics(graph, &key, distance, is_seed) {
            continue;
        }
        for edge in graph
            .edges
            .iter()
            .filter(|edge| edge.kind.is_semantic() && touches(edge, &key))
        {
            if edge.status == ClaimStatus::Rejected {
                continue;
            }
            let candidate = if edge.source == key {
                edge.target.clone()
            } else {
                edge.source.clone()
            };
            if edge.status == ClaimStatus::Stale {
                uncertainties.push(uncertainty(edge, "stale relationship"));
                selected.insert(candidate);
                continue;
            }
            let next_inferred = inferred || edge.claim_class == ClaimClass::Inference;
            if edge.claim_class == ClaimClass::Inference {
                uncertainties.push(uncertainty(edge, "inferred relationship"));
                if inferred || edge.confidence.get() < 0.65 {
                    continue;
                }
            }
            selected.insert(candidate.clone());
            if visited.insert((candidate.clone(), next_inferred)) {
                queue.push_back((candidate, distance + 1, next_inferred, false));
            }
        }
    }
}

/// Governs whether `key`'s own semantic edges may be followed.
///
/// A node reached only through the seed's one-hop structural neighborhood
/// (a caller/callee, added by [`expand_structural_seed_neighborhood`]) may
/// still expose its *own* direct claims, matching `ctx-core`'s traversal
/// policy that direct seeds and their one-hop callers/callees may discover
/// product intent. A test is the one exception: tests are a common fan-in
/// point (many requirements can share one end-to-end test), so a test must
/// never bridge into unrelated intent unless it is itself the explicit seed
/// being queried.
fn can_expand_semantics(
    graph: &GraphSnapshot,
    key: &StableKey,
    distance: usize,
    is_seed: bool,
) -> bool {
    let Some(node) = graph.nodes.get(key) else {
        return false;
    };
    if node.is_test() && !is_seed {
        return false;
    }
    if distance == 0 {
        return true;
    }
    matches!(
        node.kind,
        NodeKind::Requirement | NodeKind::Invariant | NodeKind::Decision | NodeKind::DomainConcept
    )
}

fn touches(edge: &GraphEdge, key: &StableKey) -> bool {
    edge.source == *key || edge.target == *key
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
        &mut report.data_contracts,
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
        graph::{GraphEdge, GraphEvidence, GraphNode},
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
            .expect("impact report")
            .remove(0);

        assert_eq!(report.requirements[0].identifier, "REQ-SUB-014");
        assert_eq!(report.invariants[0].identifier, "INV-SUB-003");
        assert_eq!(report.features[0].identifier, "FEAT-SUBSCRIPTIONS");
        assert_eq!(report.tests[0].identifier, "tests.test_cancel");
        assert_eq!(report.uncertainties.len(), 1);
    }

    #[test]
    fn shared_tests_do_not_bridge_unrelated_product_intent() {
        let code = symbol_node("code", "review.build", SymbolKind::Function);
        let shared_test = symbol_node("test", "tests.complete_journey", SymbolKind::Test);
        let requirement = intent_node("req", NodeKind::Requirement, "REQ-REVIEW-001");
        let feature = intent_node("feature", NodeKind::Feature, "FEAT-REVIEW");
        let unrelated = intent_node("other", NodeKind::Requirement, "REQ-INDEX-001");
        let unrelated_feature = intent_node("other-feature", NodeKind::Feature, "FEAT-INDEX");
        let nodes = [
            code.clone(),
            shared_test.clone(),
            requirement.clone(),
            feature.clone(),
            unrelated.clone(),
            unrelated_feature.clone(),
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
                &requirement,
                &shared_test,
                RelationKind::CoveredBy,
                ClaimStatus::Active,
            ),
            edge(
                &unrelated,
                &shared_test,
                RelationKind::CoveredBy,
                ClaimStatus::Active,
            ),
            edge(
                &requirement,
                &feature,
                RelationKind::DependsOn,
                ClaimStatus::Active,
            ),
            edge(
                &unrelated,
                &unrelated_feature,
                RelationKind::DependsOn,
                ClaimStatus::Active,
            ),
        ];

        let report = analyze_impact("review.build", &GraphSnapshot { nodes, edges })
            .expect("impact report")
            .remove(0);

        assert_eq!(
            report
                .requirements
                .iter()
                .map(|node| node.identifier.as_str())
                .collect::<Vec<_>>(),
            vec!["REQ-REVIEW-001"]
        );
        assert_eq!(report.features[0].identifier, "FEAT-REVIEW");
        assert_eq!(report.tests[0].identifier, "tests.complete_journey");
    }

    /// A shared test that structurally *calls* the seed (not merely reached
    /// through a covering requirement's semantic edge) is exactly the
    /// caller/callee case the seed's one-hop structural neighborhood is
    /// meant to surface. It must still not become a free root for its own
    /// unrelated coverage: `expand_structural_seed_neighborhood` pulls such
    /// a test into `selected` at "distance zero" alongside the true seeds,
    /// and without the `is_seed` guard in `can_expand_semantics` that zero
    /// distance alone used to be enough to let it fan out into whatever else
    /// it covers.
    #[test]
    fn shared_test_reached_through_the_seeds_call_graph_does_not_bridge_unrelated_intent() {
        let code = symbol_node("code", "billing.subscription.cancel", SymbolKind::Method);
        let shared_test = symbol_node("test", "tests.workflow", SymbolKind::Test);
        let requirement = intent_node("req", NodeKind::Requirement, "REQ-SUB-014");
        let unrelated = intent_node("other", NodeKind::Requirement, "REQ-REFUND-001");
        let nodes = [
            code.clone(),
            shared_test.clone(),
            requirement.clone(),
            unrelated.clone(),
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
            classified_edge(
                &shared_test,
                &code,
                RelationKind::Calls,
                ClaimClass::Fact,
                ClaimStatus::Active,
            ),
            edge(
                &requirement,
                &shared_test,
                RelationKind::CoveredBy,
                ClaimStatus::Active,
            ),
            edge(
                &unrelated,
                &shared_test,
                RelationKind::CoveredBy,
                ClaimStatus::Active,
            ),
        ];

        let report = analyze_impact(
            "billing.subscription.cancel",
            &GraphSnapshot { nodes, edges },
        )
        .expect("impact report")
        .remove(0);

        assert_eq!(
            report
                .requirements
                .iter()
                .map(|node| node.identifier.as_str())
                .collect::<Vec<_>>(),
            vec!["REQ-SUB-014"]
        );
        assert_eq!(report.tests[0].identifier, "tests.workflow");
    }

    #[test]
    fn semantic_expansion_stops_after_three_hops() {
        let code = symbol_node("code", "billing.cancel", SymbolKind::Method);
        let requirement = intent_node("req", NodeKind::Requirement, "REQ-SUB-014");
        let decision = intent_node("decision", NodeKind::Decision, "ADR-SUB-001");
        let invariant = intent_node("invariant", NodeKind::Invariant, "INV-SUB-003");
        let too_distant = intent_node("feature", NodeKind::Feature, "FEAT-SUBSCRIPTIONS");
        let nodes = [
            code.clone(),
            requirement.clone(),
            decision.clone(),
            invariant.clone(),
            too_distant.clone(),
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
                &requirement,
                &decision,
                RelationKind::DependsOn,
                ClaimStatus::Active,
            ),
            edge(
                &decision,
                &invariant,
                RelationKind::DependsOn,
                ClaimStatus::Active,
            ),
            edge(
                &invariant,
                &too_distant,
                RelationKind::DependsOn,
                ClaimStatus::Active,
            ),
        ];

        let report = analyze_impact("billing.cancel", &GraphSnapshot { nodes, edges })
            .expect("impact report")
            .remove(0);

        assert_eq!(report.requirements[0].identifier, "REQ-SUB-014");
        assert_eq!(report.decisions[0].identifier, "ADR-SUB-001");
        assert_eq!(report.invariants[0].identifier, "INV-SUB-003");
        assert!(report.features.is_empty());
    }

    #[test]
    fn rejected_relationships_are_excluded() {
        let code = symbol_node("code", "billing.cancel", SymbolKind::Method);
        let requirement = intent_node("req", NodeKind::Requirement, "REQ-SUB-014");
        let nodes = [code.clone(), requirement.clone()]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect::<BTreeMap<_, _>>();
        let edges = vec![edge(
            &code,
            &requirement,
            RelationKind::Implements,
            ClaimStatus::Rejected,
        )];

        let report = analyze_impact("billing.cancel", &GraphSnapshot { nodes, edges })
            .expect("impact report")
            .remove(0);

        assert!(report.requirements.is_empty());
        assert!(report.uncertainties.is_empty());
    }

    #[test]
    fn includes_a_direct_database_contract_without_expanding_the_whole_data_graph() {
        let code = symbol_node("code", "billing.persist", SymbolKind::Function);
        let other = symbol_node("other", "reporting.read", SymbolKind::Function);
        let database = interaction_node("db:subscriptions", "subscriptions", NodeKind::DbEntity);
        let nodes = [code.clone(), other.clone(), database.clone()]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect::<BTreeMap<_, _>>();
        let edges = vec![
            classified_edge(
                &code,
                &database,
                RelationKind::WritesTo,
                ClaimClass::Fact,
                ClaimStatus::Active,
            ),
            classified_edge(
                &other,
                &database,
                RelationKind::ReadsFrom,
                ClaimClass::Fact,
                ClaimStatus::Active,
            ),
        ];

        let report = analyze_impact("billing.persist", &GraphSnapshot { nodes, edges })
            .expect("database impact")
            .remove(0);

        assert_eq!(report.data_contracts[0].identifier, "subscriptions");
        assert!(
            !report
                .implementation
                .iter()
                .any(|node| node.identifier == "reporting.read")
        );
    }

    #[test]
    fn table_dot_column_seed_resolves_to_the_table_and_narrows_to_column_readers() {
        let writer = symbol_node("writer", "billing.cancel", SymbolKind::Method);
        let other_writer = symbol_node("other", "billing.rename", SymbolKind::Method);
        let database = interaction_node("db:subscriptions", "subscriptions", NodeKind::DbEntity);
        let nodes = [writer.clone(), other_writer.clone(), database.clone()]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect::<BTreeMap<_, _>>();
        let mut writes_paid_until = classified_edge(
            &writer,
            &database,
            RelationKind::WritesTo,
            ClaimClass::Fact,
            ClaimStatus::Active,
        );
        writes_paid_until.evidence.push(GraphEvidence {
            source_kind: SourceKind::StaticAnalysis,
            source_uri: "billing.py".to_owned(),
            commit: None,
            author: None,
            timestamp: "now".to_owned(),
            locator: "lines:1 columns:paid_until,status".to_owned(),
            strength: Confidence::CERTAIN,
        });
        let mut writes_name_only = classified_edge(
            &other_writer,
            &database,
            RelationKind::WritesTo,
            ClaimClass::Fact,
            ClaimStatus::Active,
        );
        writes_name_only.evidence.push(GraphEvidence {
            source_kind: SourceKind::StaticAnalysis,
            source_uri: "billing.py".to_owned(),
            commit: None,
            author: None,
            timestamp: "now".to_owned(),
            locator: "lines:2 columns:name".to_owned(),
            strength: Confidence::CERTAIN,
        });
        let edges = vec![writes_paid_until, writes_name_only];

        let report = analyze_impact("subscriptions.paid_until", &GraphSnapshot { nodes, edges })
            .expect("column impact")
            .remove(0);

        assert_eq!(report.selected[0].identifier, "subscriptions");
        assert_eq!(report.data_contracts[0].identifier, "subscriptions");
        assert!(
            report
                .implementation
                .iter()
                .any(|node| node.identifier == "billing.cancel")
        );
        assert!(
            !report
                .implementation
                .iter()
                .any(|node| node.identifier == "billing.rename")
        );
        assert!(report.uncertainties.is_empty());
    }

    #[test]
    fn unknown_column_falls_back_to_table_level_impact_with_an_uncertainty() {
        let writer = symbol_node("writer", "billing.cancel", SymbolKind::Method);
        let database = interaction_node("db:subscriptions", "subscriptions", NodeKind::DbEntity);
        let nodes = [writer.clone(), database.clone()]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect::<BTreeMap<_, _>>();
        let edge = classified_edge(
            &writer,
            &database,
            RelationKind::WritesTo,
            ClaimClass::Fact,
            ClaimStatus::Active,
        );

        let report = analyze_impact(
            "subscriptions.nonexistent_column",
            &GraphSnapshot {
                nodes,
                edges: vec![edge],
            },
        )
        .expect("table-level fallback")
        .remove(0);

        assert_eq!(report.selected[0].identifier, "subscriptions");
        assert!(
            report
                .implementation
                .iter()
                .any(|node| node.identifier == "billing.cancel")
        );
        assert_eq!(report.uncertainties.len(), 1);
        assert!(
            report.uncertainties[0]
                .reason
                .contains("nonexistent_column")
        );
    }

    #[test]
    fn does_not_chain_one_inference_through_another() {
        let code = symbol_node("code", "billing.cancel", SymbolKind::Method);
        let requirement = intent_node("req", NodeKind::Requirement, "REQ-SUB-014");
        let feature = intent_node("feature", NodeKind::Feature, "FEAT-SUBSCRIPTIONS");
        let nodes = [code.clone(), requirement.clone(), feature.clone()]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect::<BTreeMap<_, _>>();
        let edges = vec![
            classified_edge(
                &code,
                &requirement,
                RelationKind::Implements,
                ClaimClass::Inference,
                ClaimStatus::Active,
            ),
            classified_edge(
                &requirement,
                &feature,
                RelationKind::DependsOn,
                ClaimClass::Inference,
                ClaimStatus::Active,
            ),
        ];

        let report = analyze_impact("billing.cancel", &GraphSnapshot { nodes, edges })
            .expect("impact report")
            .remove(0);

        assert_eq!(report.requirements[0].identifier, "REQ-SUB-014");
        assert!(report.features.is_empty());
        assert_eq!(report.uncertainties.len(), 2);
    }

    /// Mirrors prompt3.md's `Replication` example (PR-LOOKUP-002/003, FR-04):
    /// several distinct namespaces sharing one short name is not an error,
    /// and each match's impact is computed independently rather than pooled
    /// into one merged neighborhood.
    #[test]
    fn multiple_short_name_matches_produce_independent_reports() {
        let manager = symbol_node(
            "manager",
            "internal.logic.manager.Replication",
            SymbolKind::Struct,
        );
        let storage = symbol_node(
            "storage",
            "storage.replication.Replication",
            SymbolKind::Struct,
        );
        let manager_requirement =
            intent_node("manager-req", NodeKind::Requirement, "REQ-MANAGER-001");
        let storage_requirement =
            intent_node("storage-req", NodeKind::Requirement, "REQ-STORAGE-001");
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
            edge(
                &manager,
                &manager_requirement,
                RelationKind::Implements,
                ClaimStatus::Active,
            ),
            edge(
                &storage,
                &storage_requirement,
                RelationKind::Implements,
                ClaimStatus::Active,
            ),
        ];

        let mut reports = analyze_impact("Replication", &GraphSnapshot { nodes, edges })
            .expect("independent impact reports per match");

        assert_eq!(reports.len(), 2);
        reports.sort_by(|left, right| {
            left.selected[0]
                .identifier
                .cmp(&right.selected[0].identifier)
        });
        assert_eq!(
            reports[0].selected[0].identifier,
            "internal.logic.manager.Replication"
        );
        assert_eq!(reports[0].requirements[0].identifier, "REQ-MANAGER-001");
        assert!(
            reports[0]
                .requirements
                .iter()
                .all(|node| node.identifier != "REQ-STORAGE-001")
        );
        assert_eq!(
            reports[1].selected[0].identifier,
            "storage.replication.Replication"
        );
        assert_eq!(reports[1].requirements[0].identifier, "REQ-STORAGE-001");
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
                database_accesses: Vec::new(),
                schema_tables: Vec::new(),
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
                visibility: crate::business::Visibility::Private,
                body: "body".to_owned(),
                feature: None,
                source_uri: "context.yaml".to_owned(),
            },
        }
    }

    fn interaction_node(key: &str, identifier: &str, kind: NodeKind) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("interaction key"),
            kind,
            name: identifier.to_owned(),
            content_hash: identifier.to_owned(),
            attributes: PlannedNodeAttributes::Interaction {
                identifier: identifier.to_owned(),
            },
        }
    }

    fn edge(
        source: &GraphNode,
        target: &GraphNode,
        kind: RelationKind,
        status: ClaimStatus,
    ) -> GraphEdge {
        classified_edge(source, target, kind, ClaimClass::Assertion, status)
    }

    fn classified_edge(
        source: &GraphNode,
        target: &GraphNode,
        kind: RelationKind,
        claim_class: ClaimClass,
        status: ClaimStatus,
    ) -> GraphEdge {
        GraphEdge {
            source: source.stable_key.clone(),
            target: target.stable_key.clone(),
            kind,
            claim_class,
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
