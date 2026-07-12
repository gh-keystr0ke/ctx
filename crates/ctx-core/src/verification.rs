use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    artifact::{ArtifactLink, ArtifactLinkKind, ArtifactLinkTarget, ArtifactRef},
    business::BusinessKind,
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
    /// Set when the same artifact that backs `intent`'s evidence (an
    /// accepted AI-derived candidate's `ArtifactRef`s, PR-MAP-001) also
    /// `ChangedSymbol`-links to this candidate symbol — the strongest
    /// possible signal short of an explicit mapping, since it means the
    /// exact artifact that produced the requirement also touched this code.
    pub artifact_evidence: f32,
    pub semantic_similarity: Option<f32>,
    pub total: f32,
}

/// The deterministic artifact evidence a scoring pass can draw on
/// (PR-MAP-001): every currently known artifact link, and — for intents
/// that originated from an accepted AI-derived
/// [`crate::knowledge::KnowledgeCandidate`] — the evidence artifacts that
/// backed each one, keyed by the resulting document's ID (matches
/// [`GraphNode::identifier`] for a `Business` node). A hand-authored
/// `.context/*.yaml` intent simply has no entry, so its score is unaffected.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtifactEvidenceContext {
    pub links: Vec<ArtifactLink>,
    pub accepted_evidence: BTreeMap<String, Vec<ArtifactRef>>,
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
pub fn semantic_candidates(
    graph: &GraphSnapshot,
    artifact_context: &ArtifactEvidenceContext,
) -> Vec<SemanticCandidate> {
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
            let score = score_candidate(graph, symbol, intent, artifact_context);
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
    artifact_context: &ArtifactEvidenceContext,
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
    let artifact_evidence = artifact_evidence_signal(symbol, intent, artifact_context);
    let total = lexical.mul_add(
        0.35,
        structural.mul_add(
            0.30,
            test_correlation.mul_add(
                0.15,
                data_interaction.mul_add(0.10, artifact_evidence * 0.10),
            ),
        ),
    );
    ResolutionScore {
        explicit: 0.0,
        alias: 0.0,
        lexical,
        structural,
        test_correlation,
        data_interaction,
        artifact_evidence,
        semantic_similarity: None,
        total,
    }
}

/// 1.0 when some artifact that backs `intent`'s own accepted evidence
/// (PR-MAP-001) also structurally changed `symbol` — the same artifact that
/// produced the requirement also touched this code, the strongest possible
/// non-explicit signal. 0.0 for a hand-authored intent with no accepted
/// evidence trail, never guessed at.
fn artifact_evidence_signal(
    symbol: &GraphNode,
    intent: &GraphNode,
    artifact_context: &ArtifactEvidenceContext,
) -> f32 {
    let Some(evidence) = artifact_context.accepted_evidence.get(intent.identifier()) else {
        return 0.0;
    };
    let backing_artifacts: std::collections::HashSet<_> =
        evidence.iter().map(|item| &item.identity).collect();
    let changed_by_backing_artifact = artifact_context.links.iter().any(|link| {
        link.kind == ArtifactLinkKind::ChangedSymbol
            && link.target == ArtifactLinkTarget::CodeSymbol(symbol.stable_key.clone())
            && backing_artifacts.contains(&link.source)
    });
    if changed_by_backing_artifact {
        1.0
    } else {
        0.0
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
    if score.artifact_evidence > 0.0 {
        evidence.push(format!(
            "backed by the same artifact that produced this requirement {:.2}",
            score.artifact_evidence
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
    tokenize(&format!("{} {} {content}", node.identifier(), node.name))
}

fn tokenize(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.len() >= 3)
        .map(str::to_ascii_lowercase)
        .filter(|term| !STOP_WORDS.contains(&term.as_str()))
        .collect()
}

/// The identifier of an already-active Requirement/Invariant/Decision that
/// `statement` likely restates (prompt3.md §13 MUST, "restating REQ-17 must
/// not silently become REQ-94"): plain term-overlap similarity against every
/// active node of the same kind, reusing this module's existing lexical-
/// matching approach rather than a second AI call. `None` means no existing
/// document shares enough vocabulary to be a plausible restatement -- a
/// human still makes the final call either way, this is advisory only.
#[must_use]
#[allow(clippy::cast_precision_loss)]
// Term-overlap counts are bounded by a statement's word count -- never
// remotely near f32's 24-bit mantissa limit.
pub fn possible_duplicate(
    graph: &GraphSnapshot,
    kind: BusinessKind,
    statement: &str,
) -> Option<String> {
    const SIMILARITY_THRESHOLD: f32 = 0.6;
    let candidate_terms = tokenize(statement);
    if candidate_terms.is_empty() {
        return None;
    }
    graph
        .nodes
        .values()
        .filter(|node| node.kind == kind.node_kind())
        .filter_map(|node| {
            let existing_terms = node_terms(node);
            if existing_terms.is_empty() {
                return None;
            }
            let overlap = candidate_terms.intersection(&existing_terms).count();
            let smaller = candidate_terms.len().min(existing_terms.len());
            let similarity = overlap as f32 / smaller as f32;
            (similarity >= SIMILARITY_THRESHOLD)
                .then_some((node.identifier().to_owned(), similarity))
        })
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(identifier, _)| identifier)
}

fn symbol_file(node: &GraphNode) -> Option<&str> {
    match &node.attributes {
        PlannedNodeAttributes::Symbol { file_path, .. } => Some(file_path),
        _ => None,
    }
}

/// Identifiers of every active Requirement/Invariant/Decision node with no
/// active `Implements`/`Enforces`/`Satisfies` edge pointing to it (prompt3.md
/// PR-MAP-003), sorted for deterministic display. Deliberately excludes
/// Feature: every Feature document in this repository's own `.context/` and
/// its fixtures is a pure descriptive umbrella with no `implementation`/
/// `tests` of its own (the Requirements underneath it carry the actual
/// mapping) -- flagging that as unmapped would be a false positive on the
/// established convention, not a real gap. Unlike a repository-wide "are
/// there any active assertions at all" check, this catches the case that
/// matters most right after `ctx verify --knowledge --accept`: one freshly
/// accepted document with no mapping, sitting alongside many already-mapped
/// ones that would otherwise hide it from a coarser aggregate count.
#[must_use]
pub fn intents_without_mapping(graph: &GraphSnapshot) -> Vec<String> {
    let mapped: BTreeSet<&StableKey> = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.status == ClaimStatus::Active
                && matches!(
                    edge.kind,
                    RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
                )
        })
        .map(|edge| &edge.target)
        .collect();
    let mut identifiers: Vec<String> = graph
        .nodes
        .values()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::Requirement | NodeKind::Invariant | NodeKind::Decision
            ) && !mapped.contains(&node.stable_key)
        })
        .map(|node| node.identifier().to_owned())
        .collect();
    identifiers.sort();
    identifiers
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
        artifact::{ArtifactIdentity, ArtifactKind, ArtifactProvider},
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

        let candidates = semantic_candidates(&graph, &ArtifactEvidenceContext::default());
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

    /// T7.1 (prompt3.md Phase 7): `semantic_candidates` already treats every
    /// intent node identically regardless of origin -- it only ever filters
    /// by `NodeKind`, never by how the node was created -- so an intent that
    /// reached the graph via an accepted AI-derived `KnowledgeCandidate`
    /// (Phase 6's `ctx verify --knowledge --accept`) needs no code change to
    /// get implementation-mapping candidates the same way a hand-authored
    /// `.context/*.yaml` document always has.
    #[test]
    fn an_intent_from_an_accepted_ai_derived_candidate_gets_mapping_candidates_like_any_other() {
        // Identical graph shape to `candidate_score_exposes_individual_heuristic_signals`
        // (the existing coverage for a plain, hand-authored intent): nothing
        // in `semantic_candidates` distinguishes an intent by how it reached
        // the graph, so an AI-derived, ctx-verify-accepted intent scores
        // through the exact same mechanism with no code change needed.
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

        let candidates = semantic_candidates(&graph, &ArtifactEvidenceContext::default());

        assert!(
            candidates
                .iter()
                .any(|item| item.target == intent.stable_key
                    && item.source == candidate.stable_key),
            "an intent with no recorded origin still gets scored like any other"
        );
    }

    #[test]
    fn a_symbol_changed_by_the_same_artifact_that_backs_the_requirement_scores_higher() {
        let intent = intent_node();
        let existing = symbol_node("existing", "subscription.cancel_access_existing");
        // Two otherwise structurally identical candidates -- both call the
        // already-verified implementer, both write to the same table -- so
        // every signal except artifact_evidence is equal between them.
        let backed = symbol_node("backed", "subscription.cancel_access_backed");
        let unrelated = symbol_node("unrelated", "subscription.cancel_access_unrelated");
        let database = interaction_node("db:subscriptions", "subscriptions");
        let graph = GraphSnapshot {
            nodes: [
                intent.clone(),
                existing.clone(),
                backed.clone(),
                unrelated.clone(),
                database.clone(),
            ]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect(),
            edges: vec![
                edge(&existing, &intent, RelationKind::Implements),
                edge(&existing, &database, RelationKind::WritesTo),
                edge(&backed, &existing, RelationKind::Calls),
                edge(&backed, &database, RelationKind::WritesTo),
                edge(&unrelated, &existing, RelationKind::Calls),
                edge(&unrelated, &database, RelationKind::WritesTo),
            ],
        };
        let backing_artifact = ArtifactIdentity {
            provider: ArtifactProvider::GitLab,
            kind: ArtifactKind::MergeRequest,
            external_id: "842".to_owned(),
        };
        let mut accepted_evidence = BTreeMap::new();
        accepted_evidence.insert(
            intent.identifier().to_owned(),
            vec![ArtifactRef {
                identity: backing_artifact.clone(),
                locator: "body".to_owned(),
                excerpt: "excerpt".to_owned(),
            }],
        );
        let artifact_context = ArtifactEvidenceContext {
            links: vec![ArtifactLink {
                source: backing_artifact,
                target: ArtifactLinkTarget::CodeSymbol(backed.stable_key.clone()),
                kind: ArtifactLinkKind::ChangedSymbol,
                evidence_locator: "changed_file:billing.py".to_owned(),
            }],
            accepted_evidence,
        };

        let candidates = semantic_candidates(&graph, &artifact_context);

        let backed_candidate = candidates
            .iter()
            .find(|item| item.source == backed.stable_key)
            .expect("backed candidate");
        let unrelated_candidate = candidates
            .iter()
            .find(|item| item.source == unrelated.stable_key)
            .expect("unrelated candidate");

        assert!((backed_candidate.score.artifact_evidence - 1.0).abs() < f32::EPSILON);
        assert!((unrelated_candidate.score.artifact_evidence - 0.0).abs() < f32::EPSILON);
        assert!(backed_candidate.score.total > unrelated_candidate.score.total);
        assert!(
            backed_candidate
                .evidence
                .iter()
                .any(|line| line.contains("same artifact"))
        );
    }

    #[test]
    fn intents_without_mapping_finds_only_the_unmapped_one() {
        let mapped_intent = intent_node();
        let implementer = symbol_node("implementer", "subscription.cancel_access_handler");
        let unmapped_intent = GraphNode {
            stable_key: StableKey::new("unmapped-intent").expect("stable key"),
            kind: NodeKind::Invariant,
            name: "Never delete paid history".to_owned(),
            content_hash: "unmapped".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: "INV-SUB-002".to_owned(),
                status: "active".to_owned(),
                body: "Never delete paid history".to_owned(),
                feature: None,
                source_uri: "invariant.yaml".to_owned(),
            },
        };
        let graph = GraphSnapshot {
            nodes: [
                mapped_intent.clone(),
                implementer.clone(),
                unmapped_intent.clone(),
            ]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect(),
            edges: vec![edge(&implementer, &mapped_intent, RelationKind::Implements)],
        };

        let unmapped = intents_without_mapping(&graph);

        assert_eq!(unmapped, vec!["INV-SUB-002".to_owned()]);
    }

    /// Real dogfooding catch: every Feature document in this repository's
    /// own `.context/` (and its fixtures) is a pure umbrella grouping with
    /// no `implementation`/`tests` of its own -- an unmapped Feature must
    /// never be reported as a mapping gap.
    #[test]
    fn an_unmapped_feature_is_never_flagged() {
        let feature = GraphNode {
            stable_key: StableKey::new("feature").expect("stable key"),
            kind: NodeKind::Feature,
            name: "Subscriptions".to_owned(),
            content_hash: "feature".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: "FEAT-SUBSCRIPTIONS".to_owned(),
                status: "active".to_owned(),
                body: "Users can cancel without losing already-paid entitlement.".to_owned(),
                feature: None,
                source_uri: "feature.yaml".to_owned(),
            },
        };
        let graph = GraphSnapshot {
            nodes: [(feature.stable_key.clone(), feature)]
                .into_iter()
                .collect(),
            edges: Vec::new(),
        };

        assert!(intents_without_mapping(&graph).is_empty());
    }

    #[test]
    fn possible_duplicate_finds_a_restated_requirement_by_term_overlap() {
        let existing = intent_node(); // REQ-SUB-001, "Subscription cancel access remains available"
        let graph = GraphSnapshot {
            nodes: [(existing.stable_key.clone(), existing)]
                .into_iter()
                .collect(),
            edges: Vec::new(),
        };

        let restated = possible_duplicate(
            &graph,
            BusinessKind::Requirement,
            "Subscription cancel access must remain available to the customer.",
        );
        assert_eq!(restated, Some("REQ-SUB-001".to_owned()));

        let unrelated = possible_duplicate(
            &graph,
            BusinessKind::Requirement,
            "Billing export must run nightly and email finance a CSV report.",
        );
        assert_eq!(unrelated, None);

        // Same wording, different kind: an Invariant restating a Requirement
        // is not treated as a duplicate of it.
        let different_kind = possible_duplicate(
            &graph,
            BusinessKind::Invariant,
            "Subscription cancel access must remain available to the customer.",
        );
        assert_eq!(different_kind, None);
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
