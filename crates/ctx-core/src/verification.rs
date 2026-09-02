use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::{
    artifact::{ArtifactLink, ArtifactLinkKind, ArtifactLinkTarget, ArtifactRef},
    business::BusinessKind,
    domain::{ClaimStatus, NodeKind, RelationKind, StableKey},
    graph::{GraphNode, GraphSnapshot},
    indexing::PlannedNodeAttributes,
    knowledge::KnowledgeCandidate,
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

type NodeSet = BTreeSet<StableKey>;
type Interactions = BTreeSet<(StableKey, RelationKind)>;
type ExistingSemanticPairs = BTreeMap<StableKey, BTreeMap<RelationKind, BTreeSet<StableKey>>>;

/// Read-only indexes shared by every intent/symbol scoring pair in one
/// verification pass. Building these once keeps the scoring semantics
/// unchanged while avoiding repeated full-graph scans and tokenization.
struct VerificationIndex {
    terms_by_node: BTreeMap<StableKey, BTreeSet<String>>,
    graph: GraphIndexes,
    intent: IntentIndexes,
    artifact_symbols_by_intent: BTreeMap<String, NodeSet>,
}

/// Direct indexes derived from graph edges. Keeping this as one cohesive
/// value prevents the verification root from becoming a bag of unrelated
/// maps as new scoring signals are added.
struct GraphIndexes {
    existing_semantic_pairs: ExistingSemanticPairs,
    verified_symbols_by_intent: BTreeMap<StableKey, NodeSet>,
    linked_tests_by_intent: BTreeMap<StableKey, NodeSet>,
    active_call_neighbors: BTreeMap<StableKey, NodeSet>,
    all_call_neighbors: BTreeMap<StableKey, NodeSet>,
    interactions_by_symbol: BTreeMap<StableKey, Interactions>,
    interaction_targets_by_symbol: BTreeMap<StableKey, NodeSet>,
}

/// Context aggregated from already verified intent-to-symbol relationships.
struct IntentIndexes {
    interactions: BTreeMap<StableKey, Interactions>,
    targets: BTreeMap<StableKey, NodeSet>,
    files: BTreeMap<StableKey, BTreeSet<String>>,
}

impl VerificationIndex {
    fn new(graph: &GraphSnapshot, artifact_context: &ArtifactEvidenceContext) -> Self {
        let terms_by_node = graph
            .nodes
            .values()
            .map(|node| (node.stable_key.clone(), node_terms(node)))
            .collect();
        let graph_indexes = GraphIndexes::new(graph);
        let intent_indexes = IntentIndexes::new(graph, &graph_indexes);
        Self {
            terms_by_node,
            graph: graph_indexes,
            intent: intent_indexes,
            artifact_symbols_by_intent: artifact_symbols_by_intent(artifact_context),
        }
    }

    fn already_linked(
        &self,
        symbol: &StableKey,
        intent: &StableKey,
        relation: RelationKind,
    ) -> bool {
        self.graph
            .existing_semantic_pairs
            .get(symbol)
            .and_then(|relations| relations.get(&relation))
            .is_some_and(|targets| targets.contains(intent))
    }
}

impl GraphIndexes {
    fn new(graph: &GraphSnapshot) -> Self {
        let mut verified_symbols_by_intent: BTreeMap<StableKey, NodeSet> = BTreeMap::new();
        let mut linked_tests_by_intent: BTreeMap<StableKey, NodeSet> = BTreeMap::new();
        let mut active_call_neighbors: BTreeMap<StableKey, NodeSet> = BTreeMap::new();
        let mut all_call_neighbors: BTreeMap<StableKey, NodeSet> = BTreeMap::new();
        let mut interactions_by_symbol: BTreeMap<StableKey, Interactions> = BTreeMap::new();
        let mut interaction_targets_by_symbol: BTreeMap<StableKey, NodeSet> = BTreeMap::new();

        for edge in &graph.edges {
            if edge.kind == RelationKind::Calls {
                all_call_neighbors
                    .entry(edge.source.clone())
                    .or_default()
                    .insert(edge.target.clone());
                all_call_neighbors
                    .entry(edge.target.clone())
                    .or_default()
                    .insert(edge.source.clone());
                if edge.status == ClaimStatus::Active {
                    active_call_neighbors
                        .entry(edge.source.clone())
                        .or_default()
                        .insert(edge.target.clone());
                    active_call_neighbors
                        .entry(edge.target.clone())
                        .or_default()
                        .insert(edge.source.clone());
                }
            }
            if edge.status != ClaimStatus::Active {
                continue;
            }
            if matches!(
                edge.kind,
                RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
            ) {
                verified_symbols_by_intent
                    .entry(edge.target.clone())
                    .or_default()
                    .insert(edge.source.clone());
            }
            if edge.kind == RelationKind::CoveredBy {
                linked_tests_by_intent
                    .entry(edge.source.clone())
                    .or_default()
                    .insert(edge.target.clone());
            }
            if matches!(edge.kind, RelationKind::ReadsFrom | RelationKind::WritesTo) {
                interactions_by_symbol
                    .entry(edge.source.clone())
                    .or_default()
                    .insert((edge.target.clone(), edge.kind));
                interaction_targets_by_symbol
                    .entry(edge.source.clone())
                    .or_default()
                    .insert(edge.target.clone());
            }
        }
        Self {
            existing_semantic_pairs: existing_semantic_pairs(graph),
            verified_symbols_by_intent,
            linked_tests_by_intent,
            active_call_neighbors,
            all_call_neighbors,
            interactions_by_symbol,
            interaction_targets_by_symbol,
        }
    }
}

impl IntentIndexes {
    fn new(graph: &GraphSnapshot, indexes: &GraphIndexes) -> Self {
        let mut verified_interactions_by_intent = BTreeMap::new();
        let mut verified_interaction_targets_by_intent = BTreeMap::new();
        let mut verified_files_by_intent = BTreeMap::new();
        for (intent, verified_symbols) in &indexes.verified_symbols_by_intent {
            let mut interactions = Interactions::new();
            let mut interaction_targets = NodeSet::new();
            let mut files = BTreeSet::new();
            for symbol in verified_symbols {
                if let Some(symbol_interactions) = indexes.interactions_by_symbol.get(symbol) {
                    interactions.extend(symbol_interactions.iter().cloned());
                }
                if let Some(symbol_targets) = indexes.interaction_targets_by_symbol.get(symbol) {
                    interaction_targets.extend(symbol_targets.iter().cloned());
                }
                if let Some(file) = graph.nodes.get(symbol).and_then(symbol_file) {
                    files.insert(file.to_owned());
                }
            }
            verified_interactions_by_intent.insert(intent.clone(), interactions);
            verified_interaction_targets_by_intent.insert(intent.clone(), interaction_targets);
            verified_files_by_intent.insert(intent.clone(), files);
        }
        Self {
            interactions: verified_interactions_by_intent,
            targets: verified_interaction_targets_by_intent,
            files: verified_files_by_intent,
        }
    }
}

fn artifact_symbols_by_intent(
    artifact_context: &ArtifactEvidenceContext,
) -> BTreeMap<String, NodeSet> {
    let mut changed_symbols_by_artifact: HashMap<_, NodeSet> = HashMap::new();
    for link in &artifact_context.links {
        if link.kind == ArtifactLinkKind::ChangedSymbol
            && let ArtifactLinkTarget::CodeSymbol(symbol) = &link.target
        {
            changed_symbols_by_artifact
                .entry(link.source.clone())
                .or_default()
                .insert(symbol.clone());
        }
    }
    artifact_context
        .accepted_evidence
        .iter()
        .filter_map(|(intent, evidence)| {
            let symbols = evidence
                .iter()
                .filter_map(|item| changed_symbols_by_artifact.get(&item.identity))
                .flatten()
                .cloned()
                .collect::<NodeSet>();
            (!symbols.is_empty()).then(|| (intent.clone(), symbols))
        })
        .collect()
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
    let index = VerificationIndex::new(graph, artifact_context);
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
            if index.already_linked(&symbol.stable_key, &intent.stable_key, relation) {
                continue;
            }
            let score = score_candidate(&index, symbol, intent);
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
    index: &VerificationIndex,
    symbol: &GraphNode,
    intent: &GraphNode,
) -> ResolutionScore {
    let symbol_terms = index
        .terms_by_node
        .get(&symbol.stable_key)
        .expect("every scored symbol was indexed");
    let intent_terms = index
        .terms_by_node
        .get(&intent.stable_key)
        .expect("every scored intent was indexed");
    let overlap = symbol_terms.intersection(intent_terms).count();
    let lexical: f32 = match overlap {
        0 => 0.0,
        1 => 0.25,
        2 => 0.5,
        3 => 0.75,
        _ => 1.0,
    };
    let structural = structural_signal(index, symbol, intent);
    let test_correlation = test_signal(index, symbol, intent);
    let data_interaction = data_interaction_signal(index, symbol, intent);
    let artifact_evidence = artifact_evidence_signal(index, symbol, intent);
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
    index: &VerificationIndex,
    symbol: &GraphNode,
    intent: &GraphNode,
) -> f32 {
    if index
        .artifact_symbols_by_intent
        .get(intent.identifier())
        .is_some_and(|symbols| symbols.contains(&symbol.stable_key))
    {
        1.0
    } else {
        0.0
    }
}

fn structural_signal(index: &VerificationIndex, symbol: &GraphNode, intent: &GraphNode) -> f32 {
    let Some(verified_symbols) = index
        .graph
        .verified_symbols_by_intent
        .get(&intent.stable_key)
    else {
        return 0.0;
    };
    if index
        .graph
        .active_call_neighbors
        .get(&symbol.stable_key)
        .is_some_and(|neighbors| !neighbors.is_disjoint(verified_symbols))
    {
        return 1.0;
    }
    let file_path = symbol_file(symbol);
    let same_file = index
        .intent
        .files
        .get(&intent.stable_key)
        .zip(file_path)
        .is_some_and(|(files, candidate)| files.contains(candidate));
    if same_file { 0.6 } else { 0.0 }
}

fn test_signal(index: &VerificationIndex, symbol: &GraphNode, intent: &GraphNode) -> f32 {
    let Some(linked_tests) = index.graph.linked_tests_by_intent.get(&intent.stable_key) else {
        return 0.0;
    };
    if index
        .graph
        .all_call_neighbors
        .get(&symbol.stable_key)
        .is_some_and(|neighbors| !neighbors.is_disjoint(linked_tests))
    {
        1.0
    } else {
        0.0
    }
}

fn data_interaction_signal(
    index: &VerificationIndex,
    symbol: &GraphNode,
    intent: &GraphNode,
) -> f32 {
    let Some(candidate_interactions) = index.graph.interactions_by_symbol.get(&symbol.stable_key)
    else {
        return 0.0;
    };
    let Some(verified_interactions) = index.intent.interactions.get(&intent.stable_key) else {
        return 0.0;
    };
    if !candidate_interactions.is_disjoint(verified_interactions) {
        return 1.0;
    }
    let shares_target = index
        .graph
        .interaction_targets_by_symbol
        .get(&symbol.stable_key)
        .zip(index.intent.targets.get(&intent.stable_key))
        .is_some_and(|(candidate, verified)| !candidate.is_disjoint(verified));
    if shares_target { 0.6 } else { 0.0 }
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

fn existing_semantic_pairs(graph: &GraphSnapshot) -> ExistingSemanticPairs {
    let mut pairs: ExistingSemanticPairs = BTreeMap::new();
    for edge in &graph.edges {
        if matches!(
            edge.kind,
            RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
        ) {
            pairs
                .entry(edge.source.clone())
                .or_default()
                .entry(edge.kind)
                .or_default()
                .insert(edge.target.clone());
        }
    }
    pairs
}

fn node_terms(node: &GraphNode) -> BTreeSet<String> {
    let content = match &node.attributes {
        PlannedNodeAttributes::Business { body, .. } => body.as_str(),
        PlannedNodeAttributes::Symbol { canonical_path, .. } => canonical_path.as_str(),
        PlannedNodeAttributes::File { path, .. } => path.as_str(),
        PlannedNodeAttributes::Interaction { identifier } => identifier.as_str(),
        PlannedNodeAttributes::ApiEndpoint { endpoint } => endpoint.path.as_str(),
        PlannedNodeAttributes::ExternalCall { call } => call.url.as_str(),
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

/// Allocates stable `.context/*.yaml` document IDs for
/// `ctx verify --knowledge --auto`, the one accept path where no human types
/// an ID by hand (REQ-VERIFY-002's interactive/scripted path still always
/// takes a human-chosen ID verbatim and never derives one -- this is a
/// deliberately separate allocator for the deliberately separate flow that
/// needs it). Every ID is `<kind stem>-<prefix>-<NNN>` (e.g. `REQ-SUB-001`),
/// matching this repository's own existing convention
/// ([`BusinessKind::id_stem`]); `prefix` is the one thing a human still
/// supplies (`--id-prefix`), since a fully content-derived ID is exactly
/// what REQ-VERIFY-002 already rules out for the same underlying reason --
/// a guessed name is a silent, easy-to-miss way to collide with or shadow
/// existing product knowledge.
///
/// Numbering starts at 1 and skips any ID already in use, including one
/// this same allocator has already handed out earlier in the same run --
/// `used` is updated in place on every call, so allocating several documents
/// in one `--auto` invocation never collides with each other, not only with
/// IDs that existed before the run started.
pub struct KnowledgeIdAllocator {
    prefix: String,
    used: BTreeSet<String>,
}

impl KnowledgeIdAllocator {
    #[must_use]
    pub fn new(prefix: impl Into<String>, graph: &GraphSnapshot) -> Self {
        let used = graph
            .nodes
            .values()
            .filter_map(|node| match &node.attributes {
                PlannedNodeAttributes::Business { id, .. } => Some(id.clone()),
                PlannedNodeAttributes::Symbol { .. }
                | PlannedNodeAttributes::File { .. }
                | PlannedNodeAttributes::Interaction { .. }
                | PlannedNodeAttributes::ApiEndpoint { .. }
                | PlannedNodeAttributes::ExternalCall { .. } => None,
            })
            .collect();
        Self {
            prefix: prefix.into(),
            used,
        }
    }

    #[must_use]
    pub fn allocate(&mut self, kind: BusinessKind) -> String {
        let stem = kind.id_stem();
        let mut number = 1u32;
        loop {
            let id = format!("{stem}-{}-{number:03}", self.prefix);
            if self.used.insert(id.clone()) {
                return id;
            }
            number += 1;
        }
    }

    /// Reserves `id` without allocating it, so a later [`Self::allocate`]
    /// call skips past it. For IDs the indexed [`GraphSnapshot`] doesn't yet
    /// know about -- e.g. a `.context/*.yaml` document written since the
    /// last `ctx index` -- so callers can seed the allocator from the
    /// on-disk ground truth in addition to the graph and never hand out an
    /// ID [`crate::verification`]'s own duplicate-file check would reject.
    pub fn mark_used(&mut self, id: impl Into<String>) {
        self.used.insert(id.into());
    }
}

/// A group of still-pending [`KnowledgeCandidate`]s whose statements share
/// enough vocabulary to plausibly describe one underlying flow rather than
/// several independent ones -- e.g. a `Status`/`Data`/`Store` triad of
/// struct-comment candidates that together, not individually, describe one
/// session lifecycle. Intended to let `ctx verify --knowledge --auto` write
/// one consolidated document per cluster instead of one per candidate,
/// without ever guessing a relationship between two candidates of different
/// [`BusinessKind`]s (a Requirement and a Decision have genuinely different
/// `.context/*.yaml` shapes, so they never share a cluster). Every pending
/// candidate belongs to exactly one cluster, including a cluster of one for
/// a candidate with no close match -- callers never need a separate
/// "leftover, ungrouped" case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateCluster {
    pub kind: BusinessKind,
    /// Sorted for deterministic output; never empty.
    pub fingerprints: Vec<String>,
}

/// Groups `candidates` by pairwise statement term-overlap, transitively: if
/// A overlaps B and B overlaps C past [`possible_duplicate`]'s own
/// similarity threshold, all three land in one cluster even though A and C
/// alone might not have crossed it -- the same connected-components approach
/// a human skimming a long candidate list would use, and the same lexical
/// mechanism this module already uses for duplicate detection against the
/// active graph, reused rather than reinvented. Deterministic and total:
/// clusters and the fingerprints within each are sorted, and every input
/// fingerprint appears in exactly one output cluster.
#[must_use]
#[allow(clippy::cast_precision_loss)]
// Term-overlap counts are bounded by a statement's word count -- never
// remotely near f32's 24-bit mantissa limit.
pub fn cluster_candidates(candidates: &[KnowledgeCandidate]) -> Vec<CandidateCluster> {
    const SIMILARITY_THRESHOLD: f32 = 0.6;

    let terms: Vec<BTreeSet<String>> = candidates
        .iter()
        .map(|candidate| tokenize(&candidate.statement))
        .collect();
    let mut parent: Vec<usize> = (0..candidates.len()).collect();
    for i in 0..candidates.len() {
        if terms[i].is_empty() {
            continue;
        }
        for j in (i + 1)..candidates.len() {
            if candidates[i].kind != candidates[j].kind || terms[j].is_empty() {
                continue;
            }
            let overlap = terms[i].intersection(&terms[j]).count();
            let smaller = terms[i].len().min(terms[j].len());
            let similarity = overlap as f32 / smaller as f32;
            if similarity >= SIMILARITY_THRESHOLD {
                union(&mut parent, i, j);
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..candidates.len() {
        groups.entry(find(&mut parent, i)).or_default().push(i);
    }
    let mut clusters: Vec<CandidateCluster> = groups
        .into_values()
        .map(|indices| {
            let mut fingerprints: Vec<String> = indices
                .iter()
                .map(|&index| candidates[index].fingerprint.clone())
                .collect();
            fingerprints.sort();
            CandidateCluster {
                kind: candidates[indices[0]].kind,
                fingerprints,
            }
        })
        .collect();
    clusters.sort_by(|left, right| left.fingerprints.cmp(&right.fingerprints));
    clusters
}

fn find(parent: &mut [usize], node: usize) -> usize {
    if parent[node] != node {
        parent[node] = find(parent, parent[node]);
    }
    parent[node]
}

fn union(parent: &mut [usize], left: usize, right: usize) {
    let left_root = find(parent, left);
    let right_root = find(parent, right);
    if left_root != right_root {
        parent[left_root.max(right_root)] = left_root.min(right_root);
    }
}

fn symbol_file(node: &GraphNode) -> Option<&str> {
    match &node.attributes {
        PlannedNodeAttributes::Symbol { file_path, .. } => Some(file_path),
        _ => None,
    }
}

fn implementation_expected(node: &GraphNode) -> bool {
    match &node.attributes {
        PlannedNodeAttributes::Business {
            implementation_expected,
            ..
        } => *implementation_expected,
        _ => true,
    }
}

/// Identifiers of every active Requirement/Invariant/Decision node with no
/// active `Implements`/`Enforces`/`Satisfies` edge pointing to it (prompt3.md
/// PR-MAP-003), sorted for deterministic display. Deliberately excludes
/// Feature: every Feature document in this repository's own `.context/` and
/// its fixtures is a pure descriptive umbrella with no `implementation`/
/// `tests` of its own (the Requirements underneath it carry the actual
/// mapping) -- flagging that as unmapped would be a false positive on the
/// established convention, not a real gap. Also excludes any document
/// explicitly marked `implementation_expected: false` -- a design-spike
/// Decision that records a scope estimate or ADR with deliberately no code
/// to point at (Epic D's tracing/field-provenance spikes are the first
/// case), the same exemption as Feature but opted into per-document instead
/// of by node kind, since most Decisions do expect a mapping. Unlike a
/// repository-wide "are there any active assertions at all" check, this
/// catches the case that matters most right after `ctx verify --knowledge
/// --accept`: one freshly accepted document with no mapping, sitting
/// alongside many already-mapped ones that would otherwise hide it from a
/// coarser aggregate count.
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
            ) && implementation_expected(node)
                && !mapped.contains(&node.stable_key)
        })
        .map(|node| node.identifier().to_owned())
        .collect();
    identifiers.sort();
    identifiers
}

/// Every stale semantic relationship (`Implements`/`Enforces`/`CoveredBy`/
/// `DependsOn`/`Satisfies` with `ClaimStatus::Stale`), rendered as the exact
/// `"source -> target"` string `ctx explain` accepts as a relationship
/// query -- so a caller (`ctx status`'s "why" notice) can hand the user
/// something directly runnable instead of a bare count. Sorted for
/// deterministic display; an edge whose source or target node has since
/// been retired from the graph is silently skipped rather than rendered
/// with a missing identifier.
#[must_use]
pub fn stale_semantic_claims(graph: &GraphSnapshot) -> Vec<String> {
    let mut claims = stale_semantic_claim_details(graph)
        .into_iter()
        .map(|claim| format!("{} -> {}", claim.source.identifier, claim.target.identifier))
        .collect::<Vec<_>>();
    claims.sort();
    claims.dedup();
    claims
}

/// One stale semantic relationship, identified by the same `fingerprint` its
/// edge is stored and re-decided under -- never a position/index, matching
/// the discipline [`crate::knowledge::CandidateReviewDecision`] already
/// follows -- plus enough of its own evidence locators for a reviewer (human
/// or agent) to see where the mapping was declared.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaleClaim {
    pub fingerprint: String,
    pub relation: RelationKind,
    pub source: crate::graph::NodeSummary,
    pub target: crate::graph::NodeSummary,
    pub evidence_locators: Vec<String>,
    /// The full statement/body text of whichever side is a product-intent
    /// node -- already in memory on the graph node, so populated here
    /// directly (empty only if, unexpectedly, neither side is a Business
    /// node).
    pub intent_statement: String,
    /// The current source excerpt for whichever side is a `CodeSymbol`,
    /// read from disk -- `ctx-core` never touches the filesystem, so this
    /// is always `None` here; a caller (`ctx-cli`) fills it in before
    /// handing claims to a [`StaleClaimVerdict`]-producing agent review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_excerpt: Option<String>,
}

/// Every stale semantic relationship with enough detail to review it (`ctx
/// verify --stale`), sorted by fingerprint for deterministic display. An
/// edge whose source or target node has since been retired from the graph
/// is silently skipped, same as [`stale_semantic_claims`].
#[must_use]
pub fn stale_semantic_claim_details(graph: &GraphSnapshot) -> Vec<StaleClaim> {
    let mut claims = graph
        .edges
        .iter()
        .filter(|edge| edge.kind.is_semantic() && edge.status == ClaimStatus::Stale)
        .filter_map(|edge| {
            let source = graph.nodes.get(&edge.source)?;
            let target = graph.nodes.get(&edge.target)?;
            Some(StaleClaim {
                fingerprint: edge.fingerprint.clone(),
                relation: edge.kind,
                source: crate::graph::NodeSummary::from(source),
                target: crate::graph::NodeSummary::from(target),
                evidence_locators: edge
                    .evidence
                    .iter()
                    .map(|evidence| format!("{}#{}", evidence.source_uri, evidence.locator))
                    .collect(),
                intent_statement: intent_body(source)
                    .or_else(|| intent_body(target))
                    .unwrap_or_default(),
                symbol_excerpt: None,
            })
        })
        .collect::<Vec<_>>();
    claims.sort_by(|left, right| left.fingerprint.cmp(&right.fingerprint));
    claims.dedup_by(|left, right| left.fingerprint == right.fingerprint);
    claims
}

fn intent_body(node: &GraphNode) -> Option<String> {
    match &node.attributes {
        PlannedNodeAttributes::Business { body, .. } => Some(body.clone()),
        _ => None,
    }
}

/// One [`StaleClaim`]'s outcome from an independent agent re-review (`ctx
/// verify --stale --agent`): [`crate::knowledge::ReviewVerdict::Accept`]
/// means the agent judges the mapping still accurate given the current code
/// and is applied bindingly (the claim is reactivated); `Reject` is never
/// applied automatically -- only ever surfaced to a human as a suggestion,
/// with `reasoning` explaining why, since silently discarding a
/// hand-authored mapping is a materially different risk than confirming one
/// still holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaleClaimVerdict {
    pub fingerprint: String,
    pub verdict: crate::knowledge::ReviewVerdict,
    pub reasoning: String,
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
        | NodeKind::ApiEndpoint
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
        graph::{GraphEdge, GraphEvidence, GraphNode},
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

        let proposal = candidates
            .iter()
            .find(|item| item.target == intent.stable_key && item.source == candidate.stable_key)
            .expect("an intent with no recorded origin still gets scored like any other");
        assert!(proposal.score.artifact_evidence.abs() < f32::EPSILON);
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
    fn indexed_scoring_preserves_existing_edge_status_and_interaction_rules() {
        let intent = intent_node();
        let verified = symbol_node("verified", "subscription.cancel_access_existing");
        let mut candidate = symbol_node("candidate", "subscription.cancel_access_handler");
        let linked_test = symbol_node("linked-test", "subscription.cancel_access_test");
        let database = interaction_node("db:subscriptions", "subscriptions");
        let PlannedNodeAttributes::Symbol { file_path, .. } = &mut candidate.attributes else {
            unreachable!("candidate is a symbol")
        };
        *file_path = "handler.py".to_owned();

        let mut rejected_structural_call = edge(&candidate, &verified, RelationKind::Calls);
        rejected_structural_call.status = ClaimStatus::Rejected;
        let mut rejected_test_call = edge(&candidate, &linked_test, RelationKind::Calls);
        rejected_test_call.status = ClaimStatus::Rejected;
        let mut rejected_existing_pair = edge(&candidate, &intent, RelationKind::Implements);
        rejected_existing_pair.status = ClaimStatus::Rejected;
        let graph = GraphSnapshot {
            nodes: [
                intent.clone(),
                verified.clone(),
                candidate.clone(),
                linked_test.clone(),
                database.clone(),
            ]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect(),
            edges: vec![
                edge(&verified, &intent, RelationKind::Implements),
                edge(&intent, &linked_test, RelationKind::CoveredBy),
                rejected_structural_call,
                rejected_test_call,
                rejected_existing_pair,
                edge(&verified, &database, RelationKind::WritesTo),
                edge(&candidate, &database, RelationKind::ReadsFrom),
            ],
        };

        let index = VerificationIndex::new(&graph, &ArtifactEvidenceContext::default());

        assert!(structural_signal(&index, &candidate, &intent).abs() < f32::EPSILON);
        assert!((test_signal(&index, &candidate, &intent) - 1.0).abs() < f32::EPSILON);
        assert!((data_interaction_signal(&index, &candidate, &intent) - 0.6).abs() < f32::EPSILON);
        assert!(index.already_linked(
            &candidate.stable_key,
            &intent.stable_key,
            RelationKind::Implements
        ));
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
                visibility: crate::business::Visibility::Private,
                implementation_expected: true,
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

    #[test]
    fn stale_semantic_claims_names_only_the_stale_relationship_as_a_runnable_explain_query() {
        let intent = intent_node();
        let implementer = symbol_node("implementer", "subscription.cancel_access_handler");
        let mut stale_semantic = edge(&implementer, &intent, RelationKind::Implements);
        stale_semantic.status = ClaimStatus::Stale;
        let other = symbol_node("other", "subscription.unrelated");
        let mut stale_structural = edge(&other, &implementer, RelationKind::Calls);
        stale_structural.status = ClaimStatus::Stale;
        let graph = GraphSnapshot {
            nodes: [intent.clone(), implementer.clone(), other.clone()]
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges: vec![
                stale_semantic,
                edge(&other, &intent, RelationKind::CoveredBy),
                stale_structural,
            ],
        };

        let claims = stale_semantic_claims(&graph);

        assert_eq!(
            claims,
            vec!["subscription.cancel_access_handler -> REQ-SUB-001".to_owned()]
        );
    }

    #[test]
    fn stale_semantic_claim_details_carries_the_fingerprint_and_evidence_a_reviewer_needs() {
        let intent = intent_node();
        let implementer = symbol_node("implementer", "subscription.cancel_access_handler");
        let mut stale = edge(&implementer, &intent, RelationKind::Implements);
        stale.status = ClaimStatus::Stale;
        stale.evidence = vec![GraphEvidence {
            source_kind: SourceKind::Documentation,
            source_uri: "requirement.yaml".to_owned(),
            commit: Some("abc123".to_owned()),
            author: None,
            timestamp: "2026-08-27T00:00:00Z".to_owned(),
            locator: "implementation[0]".to_owned(),
            strength: Confidence::CERTAIN,
        }];
        let graph = GraphSnapshot {
            nodes: [intent.clone(), implementer.clone()]
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges: vec![stale.clone()],
        };

        let claims = stale_semantic_claim_details(&graph);

        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].fingerprint, stale.fingerprint);
        assert_eq!(claims[0].relation, RelationKind::Implements);
        assert_eq!(
            claims[0].source.identifier,
            "subscription.cancel_access_handler"
        );
        assert_eq!(claims[0].target.identifier, "REQ-SUB-001");
        assert_eq!(
            claims[0].evidence_locators,
            vec!["requirement.yaml#implementation[0]".to_owned()]
        );
        assert_eq!(
            claims[0].intent_statement,
            "Subscription cancel access remains available"
        );
        assert_eq!(claims[0].symbol_excerpt, None);
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
                visibility: crate::business::Visibility::Private,
                implementation_expected: true,
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

    /// A design-spike Decision that opts out with `implementation_expected:
    /// false` is exempt from the mapping check the same way a Feature is,
    /// but an ordinary unmapped Decision (the default `true`) still gets
    /// flagged -- the exemption is per-document, not per-kind.
    #[test]
    fn an_unmapped_spike_decision_is_never_flagged_but_an_ordinary_one_still_is() {
        let spike = GraphNode {
            stable_key: StableKey::new("intent:ADR-SPIKE").expect("stable key"),
            kind: NodeKind::Decision,
            name: "Trace request flow".to_owned(),
            content_hash: "spike".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: "ADR-SPIKE".to_owned(),
                status: "accepted".to_owned(),
                visibility: crate::business::Visibility::Private,
                implementation_expected: false,
                body: "Design spike, no implementation yet.".to_owned(),
                feature: None,
                source_uri: "adr-spike.yaml".to_owned(),
            },
        };
        let ordinary = GraphNode {
            stable_key: StableKey::new("intent:ADR-ORDINARY").expect("stable key"),
            kind: NodeKind::Decision,
            name: "Use local SQLite".to_owned(),
            content_hash: "ordinary".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: "ADR-ORDINARY".to_owned(),
                status: "accepted".to_owned(),
                visibility: crate::business::Visibility::Private,
                implementation_expected: true,
                body: "Store claims in local SQLite.".to_owned(),
                feature: None,
                source_uri: "adr-ordinary.yaml".to_owned(),
            },
        };
        let graph = GraphSnapshot {
            nodes: [
                (spike.stable_key.clone(), spike),
                (ordinary.stable_key.clone(), ordinary),
            ]
            .into_iter()
            .collect(),
            edges: Vec::new(),
        };

        assert_eq!(
            intents_without_mapping(&graph),
            vec!["ADR-ORDINARY".to_owned()]
        );
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

    fn knowledge_candidate(kind: BusinessKind, statement: &str) -> KnowledgeCandidate {
        use crate::knowledge::AgentProvenance;
        KnowledgeCandidate {
            fingerprint: KnowledgeCandidate::fingerprint_for(kind, statement),
            kind,
            statement: statement.to_owned(),
            evidence: Vec::new(),
            implementation_candidates: Vec::new(),
            test_candidates: Vec::new(),
            provenance: AgentProvenance {
                producer: "test".to_owned(),
                model: None,
                input_artifact_ids: Vec::new(),
                produced_at: "2026-08-23T00:00:00Z".to_owned(),
                fingerprint: "fp".to_owned(),
            },
        }
    }

    #[test]
    fn cluster_candidates_groups_overlapping_statements_of_the_same_kind() {
        let a = knowledge_candidate(
            BusinessKind::Requirement,
            "Cancellation preserves paid access until period end.",
        );
        let b = knowledge_candidate(
            BusinessKind::Requirement,
            "Cancellation must preserve paid access until the period end.",
        );
        let unrelated = knowledge_candidate(
            BusinessKind::Requirement,
            "Billing export must run nightly and email finance a CSV report.",
        );
        let clusters = cluster_candidates(&[a.clone(), b.clone(), unrelated.clone()]);

        assert_eq!(clusters.len(), 2);
        let merged = clusters
            .iter()
            .find(|cluster| cluster.fingerprints.len() == 2)
            .expect("a and b cluster together");
        assert!(merged.fingerprints.contains(&a.fingerprint));
        assert!(merged.fingerprints.contains(&b.fingerprint));
        let singleton = clusters
            .iter()
            .find(|cluster| cluster.fingerprints.len() == 1)
            .expect("the unrelated candidate gets its own cluster");
        assert_eq!(singleton.fingerprints, vec![unrelated.fingerprint]);
    }

    #[test]
    fn cluster_candidates_never_merges_across_business_kind() {
        let statement = "Cancellation preserves paid access until period end.";
        let requirement = knowledge_candidate(BusinessKind::Requirement, statement);
        let invariant = knowledge_candidate(BusinessKind::Invariant, statement);

        let clusters = cluster_candidates(&[requirement.clone(), invariant.clone()]);

        assert_eq!(clusters.len(), 2);
        assert!(
            clusters
                .iter()
                .all(|cluster| cluster.fingerprints.len() == 1)
        );
    }

    #[test]
    fn cluster_candidates_is_transitive_across_a_shared_middle_statement() {
        // `a` shares {session, status, authenticated} with `b` (3/4 terms,
        // above threshold); `b` shares {data, store, lifecycle} with `c`
        // (3/4 terms, above threshold); `a` and `c` alone share nothing.
        // All three must still land in one cluster via `b`.
        let a = knowledge_candidate(
            BusinessKind::Requirement,
            "Session status authenticated indicator.",
        );
        let b = knowledge_candidate(
            BusinessKind::Requirement,
            "Session status authenticated data store lifecycle.",
        );
        let c = knowledge_candidate(BusinessKind::Requirement, "Data store lifecycle disk.");

        let clusters = cluster_candidates(&[a.clone(), b.clone(), c.clone()]);

        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].fingerprints.len(), 3);
    }

    #[test]
    fn id_allocator_numbers_sequentially_and_never_repeats_within_a_run() {
        let graph = GraphSnapshot::default();
        let mut allocator = KnowledgeIdAllocator::new("SUB", &graph);

        assert_eq!(allocator.allocate(BusinessKind::Requirement), "REQ-SUB-001");
        assert_eq!(allocator.allocate(BusinessKind::Requirement), "REQ-SUB-002");
        // A different kind gets its own independent numbering under the
        // same prefix, since the kind stem already keeps them apart.
        assert_eq!(allocator.allocate(BusinessKind::Invariant), "INV-SUB-001");
    }

    #[test]
    fn id_allocator_skips_ids_already_used_in_the_graph() {
        let existing = GraphNode {
            stable_key: StableKey::new("intent:REQ-SUB-001").expect("stable key"),
            kind: NodeKind::Requirement,
            name: "Existing".to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: "REQ-SUB-001".to_owned(),
                status: "active".to_owned(),
                visibility: crate::business::Visibility::Private,
                implementation_expected: true,
                body: "Existing requirement.".to_owned(),
                feature: None,
                source_uri: "requirement.yaml".to_owned(),
            },
        };
        let graph = GraphSnapshot {
            nodes: [(existing.stable_key.clone(), existing)]
                .into_iter()
                .collect(),
            edges: Vec::new(),
        };
        let mut allocator = KnowledgeIdAllocator::new("SUB", &graph);

        assert_eq!(allocator.allocate(BusinessKind::Requirement), "REQ-SUB-002");
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
                visibility: crate::business::Visibility::Private,
                implementation_expected: true,
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
                orm_accesses: Vec::new(),
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
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
