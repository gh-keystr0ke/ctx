use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    domain::{ClaimClass, ClaimStatus, NodeKind, RelationKind, StableKey},
    graph::{GraphEdge, GraphNode, GraphSnapshot},
    indexing::PlannedNodeAttributes,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextRequest {
    pub task: String,
    pub files: Vec<String>,
    pub symbols: Vec<String>,
    pub token_budget: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPriority {
    Invariant,
    Requirement,
    Feature,
    Decision,
    DirectImplementation,
    Test,
    DataContract,
    AdjacentImplementation,
    LowConfidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    pub priority: ContextPriority,
    pub kind: NodeKind,
    pub identifier: String,
    pub title: String,
    pub content: String,
    pub estimated_tokens: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextEvidence {
    pub claim: String,
    pub claim_class: ClaimClass,
    pub status: ClaimStatus,
    pub confidence: f32,
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextUncertainty {
    pub relationship: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextPack {
    pub task: String,
    pub token_budget: usize,
    pub estimated_tokens: usize,
    pub truncated: bool,
    pub seeds: Vec<String>,
    pub items: Vec<ContextItem>,
    pub evidence: Vec<ContextEvidence>,
    pub uncertainties: Vec<ContextUncertainty>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ContextCompileError {
    #[error("token budget must be greater than zero")]
    EmptyBudget,
    #[error("no indexed context matches the task or supplied seeds")]
    NoSeeds,
}

#[derive(Clone, Debug)]
struct Candidate {
    key: StableKey,
    distance: usize,
    direct: bool,
    score: usize,
}

/// Compiles a bounded task-oriented context pack using typed graph traversal.
///
/// # Errors
///
/// Returns [`ContextCompileError`] when the budget is empty, an explicit seed
/// is ambiguous, or no deterministic/lexical seed can be found.
pub fn compile_context_pack(
    graph: &GraphSnapshot,
    request: &ContextRequest,
) -> Result<ContextPack, ContextCompileError> {
    if request.token_budget == 0 {
        return Err(ContextCompileError::EmptyBudget);
    }
    let task_terms = terms(&request.task);
    let seed_keys = detect_seeds(graph, request, &task_terms);
    if seed_keys.is_empty() {
        return Err(ContextCompileError::NoSeeds);
    }
    let candidates = expand_candidates(graph, &seed_keys, &task_terms);
    let task_tokens = estimate_tokens(&request.task).max(1);
    let mut used = task_tokens.min(request.token_budget);
    let evidence_reserve = if request.token_budget >= 100 {
        request.token_budget / 5
    } else {
        0
    };
    let item_limit = request.token_budget.saturating_sub(evidence_reserve);
    let mut items = Vec::new();
    let mut selected = BTreeSet::new();
    let mut truncated = false;
    for candidate in candidates {
        let Some(node) = graph.nodes.get(&candidate.key) else {
            continue;
        };
        let remaining = item_limit.saturating_sub(used);
        if remaining == 0 {
            truncated = true;
            break;
        }
        let mut item = context_item(node, &seed_keys, candidate.distance);
        if item.estimated_tokens > remaining {
            if items.is_empty() || matches!(item.priority, ContextPriority::Invariant) {
                item.content = truncate_to_tokens(&item.content, remaining.saturating_sub(4));
                item.estimated_tokens = estimate_item_tokens(&item);
            }
            if item.estimated_tokens > remaining || item.content.is_empty() {
                truncated = true;
                continue;
            }
            truncated = true;
        }
        used += item.estimated_tokens;
        selected.insert(candidate.key);
        items.push(item);
    }
    let (all_evidence, all_uncertainties) = compile_evidence(graph, &selected);
    let mut evidence = Vec::new();
    let mut uncertainties = Vec::new();
    for item in all_evidence {
        let cost = estimate_evidence_tokens(&item);
        if used + cost <= request.token_budget {
            used += cost;
            evidence.push(item);
        } else {
            truncated = true;
        }
    }
    for item in all_uncertainties {
        let cost = estimate_tokens(&format!("{} {}", item.relationship, item.reason));
        if used + cost <= request.token_budget {
            used += cost;
            uncertainties.push(item);
        } else {
            truncated = true;
        }
    }
    let seeds = seed_keys
        .iter()
        .filter_map(|key| graph.nodes.get(key))
        .map(|node| node.identifier().to_owned())
        .collect();
    Ok(ContextPack {
        task: request.task.clone(),
        token_budget: request.token_budget,
        estimated_tokens: used,
        truncated,
        seeds,
        items,
        evidence,
        uncertainties,
    })
}

fn detect_seeds(
    graph: &GraphSnapshot,
    request: &ContextRequest,
    task_terms: &BTreeSet<String>,
) -> BTreeSet<StableKey> {
    let mut seeds = BTreeSet::new();
    for query in request.files.iter().chain(&request.symbols) {
        // Several exact matches for one explicit seed (a short name shared
        // across namespaces) are not an error: each becomes its own explicit
        // root in the same pack rather than being rejected or collapsed to
        // one arbitrary pick (PR-CONTEXT-001).
        for node in graph.resolve(query) {
            seeds.insert(node.stable_key.clone());
        }
    }
    // A resolved explicit seed is a scope boundary, not merely a hint. Its
    // graph neighborhood supplies the related context; adding independent
    // lexical roots can spend the budget on unrelated task-word matches and
    // push direct contracts out of the pack.
    if !seeds.is_empty() {
        return seeds;
    }
    // Tests are excluded from lexical auto-seeding: a task description
    // incidentally overlapping with the identifiers a test happens to call
    // (a common occurrence for a shared end-to-end test) is weak evidence,
    // and unlike an explicitly named seed, a lexically guessed test would
    // still be granted root rights to expose everything it covers. A
    // relevant test remains reachable through its covering
    // requirement/invariant or through an explicit seed's own call graph.
    let mut lexical = graph
        .nodes
        .values()
        .filter(|node| !node.is_test())
        .filter_map(|node| {
            let score = lexical_score(node, task_terms);
            (score > 0).then_some((score, node.stable_key.clone()))
        })
        .collect::<Vec<_>>();
    lexical.sort_by(|left, right| right.cmp(left));
    for (_, key) in lexical.into_iter().take(5) {
        seeds.insert(key);
    }
    seeds
}

fn expand_candidates(
    graph: &GraphSnapshot,
    seeds: &BTreeSet<StableKey>,
    task_terms: &BTreeSet<String>,
) -> Vec<Candidate> {
    let mut distances = seeds
        .iter()
        .cloned()
        .map(|key| (key, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut queue = seeds
        .iter()
        .cloned()
        .map(|key| (key, 0_usize, false, true))
        .collect::<VecDeque<_>>();
    let mut visited = seeds
        .iter()
        .cloned()
        .map(|key| (key, false, true))
        .collect::<BTreeSet<_>>();
    while let Some((key, distance, inferred, semantic_root)) = queue.pop_front() {
        if distance >= 3 {
            continue;
        }
        for edge in graph.edges.iter().filter(|edge| touches(edge, &key)) {
            let Some(node) = graph.nodes.get(&key) else {
                continue;
            };
            if !traversable(edge, node.kind, distance, inferred, semantic_root) {
                continue;
            }
            let next = if edge.source == key {
                edge.target.clone()
            } else {
                edge.source.clone()
            };
            let next_distance = distance + 1;
            distances
                .entry(next.clone())
                .and_modify(|known| *known = (*known).min(next_distance))
                .or_insert(next_distance);
            let next_inferred = inferred || edge.claim_class == ClaimClass::Inference;
            // A structural (Contains/Calls) hop normally grants the reached
            // node root rights to discover its own product intent, matching
            // the policy that a seed's one-hop callers/callees may surface
            // their own claims. A test is the one exception: tests are a
            // common fan-in point (several requirements can share one
            // end-to-end test), so reaching a test through the seed's own
            // call graph must not license that test to bridge into whatever
            // *else* it happens to cover.
            let next_is_test = graph.nodes.get(&next).is_some_and(GraphNode::is_test);
            let next_semantic_root = !edge.kind.is_semantic() && !next_is_test;
            if edge.status == ClaimStatus::Active
                && visited.insert((next.clone(), next_inferred, next_semantic_root))
            {
                queue.push_back((next, next_distance, next_inferred, next_semantic_root));
            }
        }
    }
    let mut candidates = distances
        .into_iter()
        .filter_map(|(key, distance)| {
            let node = graph.nodes.get(&key)?;
            Some(Candidate {
                direct: seeds.contains(&key),
                score: candidate_score(node, task_terms, distance, seeds.contains(&key)),
                key,
                distance,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left_node = &graph.nodes[&left.key];
        let right_node = &graph.nodes[&right.key];
        priority(left_node, left.direct, left.distance)
            .cmp(&priority(right_node, right.direct, right.distance))
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.key.cmp(&right.key))
    });
    candidates
}

fn traversable(
    edge: &GraphEdge,
    node_kind: NodeKind,
    distance: usize,
    inferred: bool,
    semantic_root: bool,
) -> bool {
    if edge.status == ClaimStatus::Rejected {
        return false;
    }
    if edge.claim_class == ClaimClass::Inference && (inferred || edge.confidence.get() < 0.65) {
        return false;
    }
    if edge.kind.is_semantic() {
        return semantic_root
            || matches!(
                node_kind,
                NodeKind::Requirement
                    | NodeKind::Invariant
                    | NodeKind::Decision
                    | NodeKind::DomainConcept
            );
    }
    distance == 0
        && matches!(
            edge.kind,
            RelationKind::Contains
                | RelationKind::Calls
                | RelationKind::References
                | RelationKind::ReadsFrom
                | RelationKind::WritesTo
                | RelationKind::DefinesSchema
                | RelationKind::Exposes
                | RelationKind::CallsExternal
                | RelationKind::Emits
                | RelationKind::Handles
        )
}

fn touches(edge: &GraphEdge, key: &StableKey) -> bool {
    edge.source == *key || edge.target == *key
}

fn lexical_score(node: &GraphNode, task_terms: &BTreeSet<String>) -> usize {
    let searchable = format!("{} {} {}", node.identifier(), node.name, node_content(node));
    let node_terms = terms(&searchable);
    task_terms.intersection(&node_terms).count()
}

fn candidate_score(
    node: &GraphNode,
    task_terms: &BTreeSet<String>,
    distance: usize,
    direct: bool,
) -> usize {
    usize::from(direct) * 1_000 + lexical_score(node, task_terms) * 100 + (3 - distance) * 10
}

fn context_item(node: &GraphNode, seeds: &BTreeSet<StableKey>, distance: usize) -> ContextItem {
    let mut item = ContextItem {
        priority: priority(node, seeds.contains(&node.stable_key), distance),
        kind: node.kind,
        identifier: node.identifier().to_owned(),
        title: node.name.trim().to_owned(),
        content: node_content(node),
        estimated_tokens: 0,
    };
    item.estimated_tokens = estimate_item_tokens(&item);
    item
}

fn priority(node: &GraphNode, direct: bool, distance: usize) -> ContextPriority {
    match node.kind {
        NodeKind::Invariant => ContextPriority::Invariant,
        NodeKind::Requirement => ContextPriority::Requirement,
        NodeKind::Feature => ContextPriority::Feature,
        NodeKind::Decision => ContextPriority::Decision,
        NodeKind::CodeSymbol if node.is_test() => ContextPriority::Test,
        NodeKind::DbEntity
        | NodeKind::ExternalSystem
        | NodeKind::Endpoint
        | NodeKind::ApiEndpoint
        | NodeKind::Event => ContextPriority::DataContract,
        NodeKind::CodeSymbol | NodeKind::File if direct || distance <= 1 => {
            ContextPriority::DirectImplementation
        }
        NodeKind::CodeSymbol | NodeKind::File => ContextPriority::AdjacentImplementation,
        NodeKind::DomainConcept => ContextPriority::LowConfidence,
    }
}

fn node_content(node: &GraphNode) -> String {
    match &node.attributes {
        PlannedNodeAttributes::Business { body, status, .. } => {
            format!("Status: {status}\n{}", body.trim())
        }
        PlannedNodeAttributes::Symbol {
            file_path,
            range,
            signature,
            calls,
            database_accesses,
            schema_tables,
            api_endpoints,
            external_calls,
            ..
        } => {
            let reads = database_entities(database_accesses, crate::ir::DatabaseAccessKind::Read);
            let writes = database_entities(database_accesses, crate::ir::DatabaseAccessKind::Write);
            let schema_line = if schema_tables.is_empty() {
                String::new()
            } else {
                format!("\nDefines schema: {}", render_schema(schema_tables))
            };
            let api_line = if api_endpoints.is_empty() {
                String::new()
            } else {
                format!(
                    "\nExposes: {}",
                    api_endpoints
                        .iter()
                        .map(|endpoint| format!("{} {}", endpoint.method.as_str(), endpoint.path))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let external_line = if external_calls.is_empty() {
                String::new()
            } else {
                format!(
                    "\nCalls external: {}",
                    external_calls
                        .iter()
                        .map(|call| format!("{} {}", call.method.as_str(), call.url))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!(
                "{}:{}-{}\nSignature: {}\nCalls: {}\nDB reads: {}\nDB writes: {}{}{}{}",
                file_path,
                range.start_line,
                range.end_line,
                signature.as_deref().unwrap_or("unknown"),
                if calls.is_empty() {
                    "none".to_owned()
                } else {
                    calls.join(", ")
                },
                render_entities(&reads),
                render_entities(&writes),
                schema_line,
                api_line,
                external_line,
            )
        }
        PlannedNodeAttributes::File { path, language, .. } => {
            format!("{language} source file: {path}")
        }
        PlannedNodeAttributes::Interaction { identifier } => {
            format!("Statically discovered data or external contract: {identifier}")
        }
        PlannedNodeAttributes::ApiEndpoint { endpoint } => format!(
            "{} {}\nFramework: {}\nParameters: {}\nReturns: {}",
            endpoint.method.as_str(),
            endpoint.path,
            endpoint.framework,
            endpoint
                .params
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            endpoint.return_type.as_deref().unwrap_or("unknown")
        ),
        PlannedNodeAttributes::ExternalCall { call } => {
            format!("{} {}", call.method.as_str(), call.url)
        }
    }
}

fn database_entities(
    accesses: &[crate::ir::DatabaseAccess],
    kind: crate::ir::DatabaseAccessKind,
) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut entities = BTreeMap::<&str, BTreeSet<&str>>::new();
    for access in accesses.iter().filter(|access| access.kind == kind) {
        let columns = entities.entry(access.entity.as_str()).or_default();
        columns.extend(access.columns.iter().map(String::as_str));
    }
    entities
}

fn render_entities(entities: &BTreeMap<&str, BTreeSet<&str>>) -> String {
    if entities.is_empty() {
        return "none".to_owned();
    }
    entities
        .iter()
        .map(|(entity, columns)| {
            if columns.is_empty() {
                (*entity).to_owned()
            } else {
                format!(
                    "{entity}({})",
                    columns.iter().copied().collect::<Vec<_>>().join(", ")
                )
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_schema(tables: &[crate::ir::SchemaTableDefinition]) -> String {
    tables
        .iter()
        .map(|table| {
            let mut parts = Vec::new();
            if !table.columns.is_empty() {
                let columns = table
                    .columns
                    .iter()
                    .map(render_schema_column)
                    .collect::<Vec<_>>()
                    .join(", ");
                parts.push(format!("({columns})"));
            }
            if !table.dropped_columns.is_empty() {
                parts.push(format!("drops: {}", table.dropped_columns.join(", ")));
            }
            if !table.renamed_columns.is_empty() {
                let renames = table
                    .renamed_columns
                    .iter()
                    .map(|rename| format!("{}->{}", rename.previous_name, rename.new_name))
                    .collect::<Vec<_>>()
                    .join(", ");
                parts.push(format!("renames: {renames}"));
            }
            if table.table_dropped {
                parts.push("table dropped".to_owned());
            }
            if let Some(previous) = &table.renamed_from {
                parts.push(format!("renamed from {previous}"));
            }
            if parts.is_empty() {
                table.entity.clone()
            } else {
                format!("{} {}", table.entity, parts.join(" "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn render_schema_column(column: &crate::ir::SchemaColumn) -> String {
    let mut markers = Vec::new();
    if column.primary_key {
        markers.push("PK".to_owned());
    }
    if column.unique {
        markers.push("UNIQUE".to_owned());
    }
    match column.nullable {
        Some(false) => markers.push("NOT NULL".to_owned()),
        Some(true) => markers.push("NULL".to_owned()),
        None => {}
    }
    if let Some(foreign_key) = &column.foreign_key {
        markers.push(format!(
            "FK->{}{}",
            foreign_key.table,
            foreign_key
                .column
                .as_ref()
                .map_or_else(String::new, |name| format!(".{name}"))
        ));
    }
    if let Some(default) = &column.default {
        markers.push(format!("DEFAULT {default}"));
    }
    let suffix = if markers.is_empty() {
        String::new()
    } else {
        format!(" {}", markers.join(" "))
    };
    format!("{} {}{suffix}", column.name, column.data_type)
}

fn compile_evidence(
    graph: &GraphSnapshot,
    selected: &BTreeSet<StableKey>,
) -> (Vec<ContextEvidence>, Vec<ContextUncertainty>) {
    let mut evidence = Vec::new();
    let mut uncertainties = Vec::new();
    let mut relevant_edges = graph
        .edges
        .iter()
        .filter(|edge| {
            selected.contains(&edge.source)
                && selected.contains(&edge.target)
                && (edge.kind.is_semantic() || !edge.evidence.is_empty())
        })
        .collect::<Vec<_>>();
    relevant_edges.sort_by(|left, right| {
        evidence_priority(left.kind)
            .cmp(&evidence_priority(right.kind))
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
    });
    for edge in relevant_edges {
        let claim = claim_text(edge, graph);
        evidence.push(ContextEvidence {
            claim: claim.clone(),
            claim_class: edge.claim_class,
            status: edge.status,
            confidence: edge.confidence.get(),
            sources: edge
                .evidence
                .iter()
                .map(|item| format!("{}#{}", item.source_uri, item.locator))
                .collect(),
        });
        if edge.status != ClaimStatus::Active || edge.claim_class == ClaimClass::Inference {
            uncertainties.push(ContextUncertainty {
                relationship: claim,
                reason: edge.stale_reason.clone().unwrap_or_else(|| {
                    if edge.status == ClaimStatus::Active {
                        "inferred relationship".to_owned()
                    } else {
                        "stale relationship".to_owned()
                    }
                }),
            });
        }
    }
    uncertainties.sort_by(|left, right| left.relationship.cmp(&right.relationship));
    (evidence, uncertainties)
}

const fn evidence_priority(kind: RelationKind) -> usize {
    match kind {
        RelationKind::Enforces => 0,
        RelationKind::Implements => 1,
        RelationKind::CoveredBy => 2,
        RelationKind::Satisfies => 3,
        RelationKind::DependsOn => 4,
        RelationKind::Contains
        | RelationKind::Calls
        | RelationKind::References
        | RelationKind::ReadsFrom
        | RelationKind::WritesTo
        | RelationKind::DefinesSchema
        | RelationKind::Exposes
        | RelationKind::CallsExternal
        | RelationKind::Emits
        | RelationKind::Handles => 5,
    }
}

fn claim_text(edge: &GraphEdge, graph: &GraphSnapshot) -> String {
    let source = graph.nodes.get(&edge.source).map_or_else(
        || edge.source.to_string(),
        |node| node.identifier().to_owned(),
    );
    let target = graph.nodes.get(&edge.target).map_or_else(
        || edge.target.to_string(),
        |node| node.identifier().to_owned(),
    );
    format!("{source} {:?} {target}", edge.kind)
}

fn terms(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '-'
        })
        .filter(|term| term.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn estimate_item_tokens(item: &ContextItem) -> usize {
    estimate_tokens(&format!(
        "{} {} {}",
        item.identifier, item.title, item.content
    ))
}

fn estimate_evidence_tokens(evidence: &ContextEvidence) -> usize {
    estimate_tokens(&format!(
        "{} {:?} {:?} {}",
        evidence.claim,
        evidence.claim_class,
        evidence.status,
        evidence.sources.join(" ")
    ))
}

pub(crate) fn estimate_tokens(value: &str) -> usize {
    value.chars().count().div_ceil(4)
}

pub(crate) fn truncate_to_tokens(value: &str, tokens: usize) -> String {
    let character_limit = tokens.saturating_mul(4);
    let mut truncated = value.chars().take(character_limit).collect::<String>();
    if truncated.chars().count() < value.chars().count() {
        truncated.push('…');
    }
    truncated
}

#[cfg(test)]
mod tests {
    use crate::{
        domain::{Confidence, SourceKind},
        graph::{GraphEdge, GraphNode},
        ir::{SourceRange, SymbolKind},
    };

    use super::*;

    #[test]
    fn preserves_invariants_first_and_respects_the_budget() {
        let code = symbol_node("code", "billing.cancel");
        let invariant = intent_node("invariant", NodeKind::Invariant, "INV-SUB-003");
        let graph = graph_with(
            &[code.clone(), invariant.clone()],
            vec![edge(
                &code,
                &invariant,
                RelationKind::Enforces,
                ClaimClass::Assertion,
            )],
        );
        let request = ContextRequest {
            task: "fix cancellation paid access".to_owned(),
            files: Vec::new(),
            symbols: vec!["billing.cancel".to_owned()],
            token_budget: 45,
        };

        let pack = compile_context_pack(&graph, &request).expect("context pack");

        assert_eq!(pack.items[0].priority, ContextPriority::Invariant);
        assert!(pack.estimated_tokens <= pack.token_budget);
    }

    #[test]
    fn never_chains_one_inference_through_another() {
        let code = symbol_node("code", "billing.cancel");
        let requirement = intent_node("requirement", NodeKind::Requirement, "REQ-SUB-014");
        let feature = intent_node("feature", NodeKind::Feature, "FEAT-SUBSCRIPTIONS");
        let graph = graph_with(
            &[code.clone(), requirement.clone(), feature.clone()],
            vec![
                edge(
                    &code,
                    &requirement,
                    RelationKind::Implements,
                    ClaimClass::Inference,
                ),
                edge(
                    &requirement,
                    &feature,
                    RelationKind::DependsOn,
                    ClaimClass::Inference,
                ),
            ],
        );
        let request = ContextRequest {
            task: "unrelated wording".to_owned(),
            files: Vec::new(),
            symbols: vec!["billing.cancel".to_owned()],
            token_budget: 1_000,
        };

        let pack = compile_context_pack(&graph, &request).expect("context pack");

        assert!(
            pack.items
                .iter()
                .any(|item| item.identifier == "REQ-SUB-014")
        );
        assert!(
            !pack
                .items
                .iter()
                .any(|item| item.identifier == "FEAT-SUBSCRIPTIONS")
        );
    }

    #[test]
    fn includes_direct_database_contracts_as_prioritized_context() {
        let code = symbol_node("code", "billing.persist");
        let database = interaction_node("db:subscriptions", "subscriptions", NodeKind::DbEntity);
        let graph = graph_with(
            &[code.clone(), database.clone()],
            vec![edge(
                &code,
                &database,
                RelationKind::WritesTo,
                ClaimClass::Fact,
            )],
        );
        let request = ContextRequest {
            task: "change subscription persistence".to_owned(),
            files: Vec::new(),
            symbols: vec!["billing.persist".to_owned()],
            token_budget: 500,
        };

        let pack = compile_context_pack(&graph, &request).expect("database context");
        let data = pack
            .items
            .iter()
            .find(|item| item.identifier == "subscriptions")
            .expect("database contract");

        assert_eq!(data.priority, ContextPriority::DataContract);
        assert!(pack.estimated_tokens <= pack.token_budget);
    }

    #[test]
    fn shared_tests_do_not_bridge_unrelated_product_intent() {
        let code = symbol_node("code", "review.build");
        let shared_test = symbol_node_with_kind("test", "tests.complete_journey", SymbolKind::Test);
        let requirement = intent_node("requirement", NodeKind::Requirement, "REQ-REVIEW-001");
        let feature = intent_node("feature", NodeKind::Feature, "FEAT-REVIEW");
        let unrelated = intent_node("unrelated", NodeKind::Requirement, "REQ-INDEX-001");
        let unrelated_feature = intent_node("unrelated-feature", NodeKind::Feature, "FEAT-INDEX");
        let graph = graph_with(
            &[
                code.clone(),
                shared_test.clone(),
                requirement.clone(),
                feature.clone(),
                unrelated.clone(),
                unrelated_feature.clone(),
            ],
            vec![
                edge(
                    &code,
                    &requirement,
                    RelationKind::Implements,
                    ClaimClass::Assertion,
                ),
                edge(
                    &requirement,
                    &shared_test,
                    RelationKind::CoveredBy,
                    ClaimClass::Assertion,
                ),
                edge(
                    &unrelated,
                    &shared_test,
                    RelationKind::CoveredBy,
                    ClaimClass::Assertion,
                ),
                edge(
                    &requirement,
                    &feature,
                    RelationKind::DependsOn,
                    ClaimClass::Assertion,
                ),
                edge(
                    &unrelated,
                    &unrelated_feature,
                    RelationKind::DependsOn,
                    ClaimClass::Assertion,
                ),
            ],
        );
        let request = ContextRequest {
            task: "zzznomatch".to_owned(),
            files: Vec::new(),
            symbols: vec!["review.build".to_owned()],
            token_budget: 1_000,
        };

        let pack = compile_context_pack(&graph, &request).expect("context pack");
        let identifiers = pack
            .items
            .iter()
            .map(|item| item.identifier.as_str())
            .collect::<BTreeSet<_>>();

        assert!(identifiers.contains("REQ-REVIEW-001"));
        assert!(identifiers.contains("FEAT-REVIEW"));
        assert!(identifiers.contains("tests.complete_journey"));
        assert!(!identifiers.contains("REQ-INDEX-001"));
        assert!(!identifiers.contains("FEAT-INDEX"));
    }

    /// A shared test that structurally *calls* the seed is a legitimate
    /// one-hop callee neighbor, which the traversal is meant to expose. It
    /// must not, in turn, bridge into whatever *else* that test covers: only
    /// the seed itself gets unconditional root rights, not every node
    /// discovered through the seed's one-hop call graph.
    #[test]
    fn shared_test_reached_through_the_seeds_call_graph_does_not_bridge_unrelated_intent() {
        let code = symbol_node("code", "billing.subscription.cancel");
        let shared_test = symbol_node_with_kind("test", "tests.workflow", SymbolKind::Test);
        let requirement = intent_node("requirement", NodeKind::Requirement, "REQ-SUB-014");
        let unrelated = intent_node("unrelated", NodeKind::Requirement, "REQ-REFUND-001");
        let graph = graph_with(
            &[
                code.clone(),
                shared_test.clone(),
                requirement.clone(),
                unrelated.clone(),
            ],
            vec![
                edge(
                    &code,
                    &requirement,
                    RelationKind::Implements,
                    ClaimClass::Assertion,
                ),
                edge(&shared_test, &code, RelationKind::Calls, ClaimClass::Fact),
                edge(
                    &requirement,
                    &shared_test,
                    RelationKind::CoveredBy,
                    ClaimClass::Assertion,
                ),
                edge(
                    &unrelated,
                    &shared_test,
                    RelationKind::CoveredBy,
                    ClaimClass::Assertion,
                ),
            ],
        );
        let request = ContextRequest {
            task: "zzznomatch".to_owned(),
            files: Vec::new(),
            symbols: vec!["billing.subscription.cancel".to_owned()],
            token_budget: 1_000,
        };

        let pack = compile_context_pack(&graph, &request).expect("context pack");
        let identifiers = pack
            .items
            .iter()
            .map(|item| item.identifier.as_str())
            .collect::<BTreeSet<_>>();

        assert!(identifiers.contains("REQ-SUB-014"));
        assert!(identifiers.contains("tests.workflow"));
        assert!(!identifiers.contains("REQ-REFUND-001"));
    }

    /// A task description that happens to lexically overlap with the
    /// identifiers a shared test calls (a common occurrence for an
    /// end-to-end test) must not auto-seed that test: an auto-seeded node
    /// gets full root rights and would otherwise expose everything else the
    /// test covers.
    #[test]
    fn lexical_seed_detection_never_auto_seeds_a_test() {
        let code = symbol_node("code", "billing.subscription.cancel");
        let shared_test = symbol_node_with_kind("test", "tests.workflow", SymbolKind::Test);
        let unrelated = intent_node("unrelated", NodeKind::Requirement, "REQ-REFUND-001");
        let graph = graph_with(
            &[code.clone(), shared_test.clone(), unrelated.clone()],
            vec![edge(
                &unrelated,
                &shared_test,
                RelationKind::CoveredBy,
                ClaimClass::Assertion,
            )],
        );
        let request = ContextRequest {
            task: "workflow".to_owned(),
            files: Vec::new(),
            symbols: vec!["billing.subscription.cancel".to_owned()],
            token_budget: 1_000,
        };

        let pack = compile_context_pack(&graph, &request).expect("context pack");
        let identifiers = pack
            .items
            .iter()
            .map(|item| item.identifier.as_str())
            .collect::<BTreeSet<_>>();

        assert!(!identifiers.contains("tests.workflow"));
        assert!(!identifiers.contains("REQ-REFUND-001"));
    }

    #[test]
    fn explicit_seed_prevents_unrelated_lexical_roots() {
        let explicit = symbol_node("explicit", "billing.subscription.cancel");
        let lexical = symbol_node("lexical", "database.write.change.safely");
        let database = interaction_node("db:subscriptions", "subscriptions", NodeKind::DbEntity);
        let graph = graph_with(
            &[explicit.clone(), lexical, database.clone()],
            vec![edge(
                &explicit,
                &database,
                RelationKind::WritesTo,
                ClaimClass::Fact,
            )],
        );
        let request = ContextRequest {
            task: "change database write safely".to_owned(),
            files: Vec::new(),
            symbols: vec!["billing.subscription.cancel".to_owned()],
            token_budget: 500,
        };

        let pack = compile_context_pack(&graph, &request).expect("bounded explicit context");
        let identifiers = pack
            .items
            .iter()
            .map(|item| item.identifier.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(pack.seeds, ["billing.subscription.cancel"]);
        assert!(identifiers.contains("subscriptions"));
        assert!(!identifiers.contains("database.write.change.safely"));
    }

    fn graph_with(nodes: &[GraphNode], edges: Vec<GraphEdge>) -> GraphSnapshot {
        GraphSnapshot {
            nodes: nodes
                .iter()
                .cloned()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges,
        }
    }

    fn symbol_node(key: &str, canonical: &str) -> GraphNode {
        symbol_node_with_kind(key, canonical, SymbolKind::Method)
    }

    fn symbol_node_with_kind(key: &str, canonical: &str, symbol_kind: SymbolKind) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::CodeSymbol,
            name: "cancel".to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: "billing.py".to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind,
                range: SourceRange {
                    start_byte: 0,
                    end_byte: 10,
                    start_line: 1,
                    end_line: 2,
                },
                signature: Some("()".to_owned()),
                structural_fingerprint: "shape".to_owned(),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
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
                body: "Paid access remains active until paid_until.".to_owned(),
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
        claim_class: ClaimClass,
    ) -> GraphEdge {
        GraphEdge {
            source: source.stable_key.clone(),
            target: target.stable_key.clone(),
            kind,
            claim_class,
            source_kind: SourceKind::Documentation,
            confidence: Confidence::new(0.9).expect("confidence"),
            status: ClaimStatus::Active,
            valid_from: "commit".to_owned(),
            valid_to: None,
            producer: "test".to_owned(),
            fingerprint: format!("{kind:?}:{}", target.stable_key),
            stale_reason: None,
            evidence: Vec::new(),
        }
    }
}
