use std::collections::{BTreeMap, BTreeSet, HashMap};

use ctx_core::{
    artifact::{
        Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkTarget,
        ArtifactProvider,
    },
    domain::{ClaimClass, ClaimStatus, NodeKind, RelationKind, SourceKind, StableKey},
    explain::KnowledgeProvenance,
    graph::{GraphEdge, GraphEvidence, GraphNode, GraphSnapshot, NodeSummary},
    indexing::PlannedNodeAttributes,
    neighborhood::{LinkedArtifact, artifact_history},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ports::{
        ArtifactLinkStore, ArtifactRepository, GitRepository, GraphStore, KnowledgeCandidateStore,
        PortError, RepositoryStatus, SourceScope,
    },
    status::{IndexState, StatusError, StatusHealth, StatusService},
};

const CATALOG_KINDS: [NodeKind; 7] = [
    NodeKind::Feature,
    NodeKind::Requirement,
    NodeKind::Invariant,
    NodeKind::Decision,
    NodeKind::DomainConcept,
    NodeKind::Event,
    NodeKind::ExternalSystem,
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportData {
    pub meta: ReportMeta,
    pub catalogs: Vec<ReportCatalog>,
    pub tree: ReportTree,
    pub details: BTreeMap<StableKey, ReportDetail>,
    pub search_index: Vec<SearchEntry>,
    pub dashboard_graph: ReportGraph,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportMeta {
    pub source_commit: String,
    pub remote_url: Option<String>,
    pub index_state: IndexState,
    pub health: StatusHealth,
    pub source_scope: SourceScope,
    pub knowledge: RepositoryStatus,
    pub notices: Vec<String>,
    pub suggested_actions: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportCatalog {
    pub kind: NodeKind,
    pub nodes: Vec<NodeSummary>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportTree {
    pub directories: Vec<ReportDirectory>,
    pub files: Vec<ReportFile>,
    pub unattached_symbols: Vec<ReportSymbol>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportDirectory {
    pub name: String,
    pub path: String,
    pub directories: Vec<Self>,
    pub files: Vec<ReportFile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportFile {
    pub node: NodeSummary,
    pub symbols: Vec<ReportSymbol>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportSymbol {
    pub node: NodeSummary,
    pub symbol_kind: ctx_core::ir::SymbolKind,
    pub children: Vec<Self>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportDetail {
    pub node: GraphNode,
    pub relations: Vec<ReportRelation>,
    pub knowledge_provenance: Option<KnowledgeProvenance>,
    pub provenance_artifacts: Vec<Artifact>,
    pub artifact_history: Vec<LinkedArtifact>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportRelation {
    pub source: NodeSummary,
    pub target: NodeSummary,
    pub kind: RelationKind,
    pub claim_class: ClaimClass,
    pub status: ClaimStatus,
    pub confidence: f32,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub provenance: SourceKind,
    pub producer: String,
    pub fingerprint: String,
    pub stale_reason: Option<String>,
    pub evidence: Vec<GraphEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchEntry {
    pub stable_key: StableKey,
    pub kind: NodeKind,
    pub name: String,
    pub identifier: String,
    pub file_path: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportGraph {
    pub nodes: Vec<NodeSummary>,
    pub edges: Vec<ReportGraphEdge>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportGraphEdge {
    pub source: StableKey,
    pub target: StableKey,
    pub kind: RelationKind,
    pub claim_class: ClaimClass,
    pub status: ClaimStatus,
}

#[derive(Debug, Error)]
pub enum ReportError {
    #[error(transparent)]
    Status(#[from] StatusError),
    #[error("report data could not be loaded: {0}")]
    Store(PortError),
    #[error(
        "ctx report requires an index at HEAD {head}; the current index is {indexed}. Run 'ctx index' first"
    )]
    RequiresCurrentIndex { head: String, indexed: String },
}

pub struct ReportService<'a, G, S> {
    git: &'a G,
    store: &'a S,
}

impl<'a, G, S> ReportService<'a, G, S>
where
    G: GitRepository,
    S: GraphStore
        + ArtifactRepository
        + ArtifactLinkStore
        + KnowledgeCandidateStore
        + crate::ports::IndexStore,
{
    pub const fn new(git: &'a G, store: &'a S) -> Self {
        Self { git, store }
    }

    /// Builds the format-neutral, deterministic projection consumed by every
    /// static report renderer.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError`] when the index does not describe `HEAD` or any
    /// required Git, graph, artifact, or provenance data cannot be loaded.
    pub fn build(&self) -> Result<ReportData, ReportError> {
        let status = StatusService::new(self.git, self.store).inspect()?;
        if status.index_state != IndexState::Current {
            return Err(ReportError::RequiresCurrentIndex {
                head: status.head_commit.to_string(),
                indexed: status
                    .knowledge
                    .last_indexed_commit
                    .as_ref()
                    .map_or_else(|| "not indexed".to_owned(), ToString::to_string),
            });
        }
        let repository = self.git.descriptor().map_err(ReportError::Store)?;
        let graph = self
            .store
            .load_graph(&repository.id)
            .map_err(ReportError::Store)?;
        let artifacts = self
            .store
            .list_artifacts(&repository.id)
            .map_err(ReportError::Store)?;
        let links = self
            .store
            .list_links(&repository.id)
            .map_err(ReportError::Store)?;
        let mut accepted = BTreeMap::new();
        for node in graph.nodes.values().filter(|node| {
            matches!(
                node.kind,
                NodeKind::Feature
                    | NodeKind::Requirement
                    | NodeKind::Invariant
                    | NodeKind::Decision
            )
        }) {
            if let Some(record) = self
                .store
                .accepted_record_for_document(&repository.id, node.identifier())
                .map_err(ReportError::Store)?
            {
                accepted.insert(node.stable_key.clone(), record);
            }
        }
        Ok(build_report_data(
            status,
            repository.remote_url,
            &graph,
            &artifacts,
            &links,
            &accepted,
        ))
    }
}

fn build_report_data(
    status: crate::status::StatusReport,
    remote_url: Option<String>,
    graph: &GraphSnapshot,
    artifacts: &[Artifact],
    links: &[ArtifactLink],
    accepted: &BTreeMap<StableKey, ctx_core::knowledge::AcceptedKnowledgeRecord>,
) -> ReportData {
    let catalogs = build_catalogs(graph);
    let tree = build_tree(graph);
    let search_index = build_search_index(graph);
    let dashboard_graph = build_dashboard_graph(graph);
    let histories = artifact_histories(links, artifacts);
    let artifact_by_id = artifacts
        .iter()
        .map(|artifact| (artifact_id(&artifact.identity), artifact))
        .collect::<HashMap<_, _>>();
    let mut details = graph
        .nodes
        .values()
        .filter(|node| is_detail_kind(node.kind))
        .map(|node| {
            let record = accepted.get(&node.stable_key);
            let knowledge_provenance = record.map(|record| KnowledgeProvenance {
                derived_from: record.candidate.provenance.input_artifact_ids.clone(),
                agent_producer: record.candidate.provenance.producer.clone(),
                agent_model: record.candidate.provenance.model.clone(),
                decided_by: record.decided_by.clone(),
                decided_at: record.decided_at.clone(),
                decision_method: record.decision_method,
            });
            let mut provenance_artifacts = record.map_or_else(Vec::new, |record| {
                record
                    .candidate
                    .provenance
                    .input_artifact_ids
                    .iter()
                    .filter_map(|id| artifact_by_id.get(id).copied().cloned())
                    .collect()
            });
            provenance_artifacts.sort_by(|left, right| {
                artifact_id(&left.identity).cmp(&artifact_id(&right.identity))
            });
            provenance_artifacts.dedup_by(|left, right| left.identity == right.identity);
            let artifact_history = histories.get(&node.stable_key).cloned().unwrap_or_default();
            (
                node.stable_key.clone(),
                ReportDetail {
                    node: node.clone(),
                    relations: Vec::new(),
                    knowledge_provenance,
                    provenance_artifacts,
                    artifact_history,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for edge in &graph.edges {
        let Some(relation) = report_relation(edge, graph) else {
            continue;
        };
        if let Some(detail) = details.get_mut(&edge.source) {
            detail.relations.push(relation.clone());
        }
        if edge.target != edge.source
            && let Some(detail) = details.get_mut(&edge.target)
        {
            detail.relations.push(relation);
        }
    }
    for detail in details.values_mut() {
        detail.relations.sort_by(relation_order);
    }
    ReportData {
        meta: ReportMeta {
            source_commit: status.head_commit.to_string(),
            remote_url,
            index_state: status.index_state,
            health: status.health,
            source_scope: status.source_scope,
            knowledge: status.knowledge,
            notices: status.notices,
            suggested_actions: status.suggested_actions,
        },
        catalogs,
        tree,
        details,
        search_index,
        dashboard_graph,
    }
}

fn build_catalogs(graph: &GraphSnapshot) -> Vec<ReportCatalog> {
    CATALOG_KINDS
        .into_iter()
        .map(|kind| ReportCatalog {
            kind,
            nodes: graph
                .nodes
                .values()
                .filter(|node| node.kind == kind)
                .map(NodeSummary::from)
                .collect(),
        })
        .collect()
}

fn build_search_index(graph: &GraphSnapshot) -> Vec<SearchEntry> {
    graph
        .nodes
        .values()
        .map(|node| SearchEntry {
            stable_key: node.stable_key.clone(),
            kind: node.kind,
            name: node.name.clone(),
            identifier: node.identifier().to_owned(),
            file_path: match &node.attributes {
                PlannedNodeAttributes::File { path, .. } => Some(path.clone()),
                PlannedNodeAttributes::Symbol { file_path, .. } => Some(file_path.clone()),
                _ => None,
            },
        })
        .collect()
}

fn build_dashboard_graph(graph: &GraphSnapshot) -> ReportGraph {
    let selected = graph
        .nodes
        .values()
        .filter(|node| is_catalog_kind(node.kind))
        .map(|node| (node.stable_key.clone(), NodeSummary::from(node)))
        .collect::<BTreeMap<_, _>>();
    let mut edges = graph
        .edges
        .iter()
        .filter(|edge| selected.contains_key(&edge.source) && selected.contains_key(&edge.target))
        .map(|edge| ReportGraphEdge {
            source: edge.source.clone(),
            target: edge.target.clone(),
            kind: edge.kind,
            claim_class: edge.claim_class,
            status: edge.status,
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then(left.target.cmp(&right.target))
            .then(format!("{:?}", left.kind).cmp(&format!("{:?}", right.kind)))
            .then(format!("{:?}", left.claim_class).cmp(&format!("{:?}", right.claim_class)))
    });
    ReportGraph {
        nodes: selected.into_values().collect(),
        edges,
    }
}

fn report_relation(edge: &GraphEdge, graph: &GraphSnapshot) -> Option<ReportRelation> {
    Some(ReportRelation {
        source: NodeSummary::from(graph.nodes.get(&edge.source)?),
        target: NodeSummary::from(graph.nodes.get(&edge.target)?),
        kind: edge.kind,
        claim_class: edge.claim_class,
        status: edge.status,
        confidence: edge.confidence.get(),
        valid_from: edge.valid_from.clone(),
        valid_to: edge.valid_to.clone(),
        provenance: edge.source_kind,
        producer: edge.producer.clone(),
        fingerprint: edge.fingerprint.clone(),
        stale_reason: edge.stale_reason.clone(),
        evidence: edge.evidence.clone(),
    })
}

fn relation_order(left: &ReportRelation, right: &ReportRelation) -> std::cmp::Ordering {
    left.source
        .stable_key
        .cmp(&right.source.stable_key)
        .then(left.target.stable_key.cmp(&right.target.stable_key))
        .then(format!("{:?}", left.kind).cmp(&format!("{:?}", right.kind)))
        .then(left.fingerprint.cmp(&right.fingerprint))
}

fn artifact_histories(
    links: &[ArtifactLink],
    artifacts: &[Artifact],
) -> BTreeMap<StableKey, Vec<LinkedArtifact>> {
    links
        .iter()
        .filter_map(|link| match &link.target {
            ArtifactLinkTarget::CodeSymbol(symbol) => Some(symbol.clone()),
            ArtifactLinkTarget::Artifact(_) => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|symbol| {
            let history = artifact_history(&symbol, links, artifacts);
            (symbol, history)
        })
        .collect()
}

fn artifact_id(identity: &ArtifactIdentity) -> String {
    format!(
        "{}:{}:{}",
        provider_name(identity.provider),
        artifact_kind_name(identity.kind),
        identity.external_id
    )
}

const fn provider_name(provider: ArtifactProvider) -> &'static str {
    match provider {
        ArtifactProvider::Git => "git",
        ArtifactProvider::GitLab => "gitlab",
        ArtifactProvider::GitHub => "github",
        ArtifactProvider::Jira => "jira",
        ArtifactProvider::Code => "code",
    }
}

const fn artifact_kind_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Commit => "commit",
        ArtifactKind::Branch => "branch",
        ArtifactKind::Issue => "issue",
        ArtifactKind::MergeRequest => "merge_request",
        ArtifactKind::PullRequest => "pull_request",
        ArtifactKind::Comment => "comment",
        ArtifactKind::ReviewComment => "review_comment",
        ArtifactKind::CodeComment => "code_comment",
        ArtifactKind::Docstring => "docstring",
        ArtifactKind::Documentation => "documentation",
    }
}

fn is_catalog_kind(kind: NodeKind) -> bool {
    CATALOG_KINDS.contains(&kind)
}

fn is_detail_kind(kind: NodeKind) -> bool {
    is_catalog_kind(kind) || kind == NodeKind::CodeSymbol
}

#[derive(Default)]
struct DirectoryBuilder {
    name: String,
    path: String,
    directories: BTreeMap<String, Self>,
    files: Vec<ReportFile>,
}

fn build_tree(graph: &GraphSnapshot) -> ReportTree {
    let file_nodes = graph
        .nodes
        .values()
        .filter(|node| node.kind == NodeKind::File)
        .map(|node| (node.stable_key.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let mut symbols_by_file = BTreeMap::<StableKey, Vec<&GraphNode>>::new();
    let mut attached = BTreeSet::new();
    for edge in &graph.edges {
        if edge.kind != RelationKind::Contains || edge.status != ClaimStatus::Active {
            continue;
        }
        let Some(file) = file_nodes.get(&edge.source) else {
            continue;
        };
        let Some(symbol) = graph.nodes.get(&edge.target) else {
            continue;
        };
        if symbol.kind != NodeKind::CodeSymbol {
            continue;
        }
        symbols_by_file
            .entry(file.stable_key.clone())
            .or_default()
            .push(symbol);
        attached.insert(symbol.stable_key.clone());
    }
    let mut root = DirectoryBuilder::default();
    for file in file_nodes.values() {
        let symbols = symbols_by_file.remove(&file.stable_key).unwrap_or_default();
        root.insert_file(ReportFile {
            node: NodeSummary::from(*file),
            symbols: nest_symbols(&symbols),
        });
    }
    let mut tree = root.finish();
    tree.unattached_symbols = graph
        .nodes
        .values()
        .filter(|node| node.kind == NodeKind::CodeSymbol && !attached.contains(&node.stable_key))
        .filter_map(report_symbol)
        .collect();
    tree
}

impl DirectoryBuilder {
    fn insert_file(&mut self, file: ReportFile) {
        let path = file.node.identifier.clone();
        let mut parts = path.split('/').collect::<Vec<_>>();
        parts.pop();
        let mut directory = self;
        let mut current = String::new();
        for part in parts {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(part);
            directory = directory
                .directories
                .entry(part.to_owned())
                .or_insert_with(|| Self {
                    name: part.to_owned(),
                    path: current.clone(),
                    ..Self::default()
                });
        }
        directory.files.push(file);
    }

    fn finish(mut self) -> ReportTree {
        self.files
            .sort_by(|left, right| left.node.identifier.cmp(&right.node.identifier));
        ReportTree {
            directories: self
                .directories
                .into_values()
                .map(Self::finish_directory)
                .collect(),
            files: self.files,
            unattached_symbols: Vec::new(),
        }
    }

    fn finish_directory(mut self) -> ReportDirectory {
        self.files
            .sort_by(|left, right| left.node.identifier.cmp(&right.node.identifier));
        ReportDirectory {
            name: self.name,
            path: self.path,
            directories: self
                .directories
                .into_values()
                .map(Self::finish_directory)
                .collect(),
            files: self.files,
        }
    }
}

fn nest_symbols(symbols: &[&GraphNode]) -> Vec<ReportSymbol> {
    let mut parent_by_child = BTreeMap::<StableKey, StableKey>::new();
    for child in symbols {
        let child_path = child.identifier();
        let candidates = symbols
            .iter()
            .copied()
            .filter(|parent| parent.stable_key != child.stable_key)
            .filter(|parent| {
                child_path
                    .strip_prefix(parent.identifier())
                    .is_some_and(|suffix| suffix.starts_with('.'))
            })
            .collect::<Vec<_>>();
        let longest = candidates
            .iter()
            .map(|candidate| candidate.identifier().len())
            .max();
        let Some(longest) = longest else {
            continue;
        };
        let winners = candidates
            .into_iter()
            .filter(|candidate| candidate.identifier().len() == longest)
            .collect::<Vec<_>>();
        if let [parent] = winners.as_slice() {
            parent_by_child.insert(child.stable_key.clone(), parent.stable_key.clone());
        }
    }
    let mut children = BTreeMap::<StableKey, Vec<&GraphNode>>::new();
    let mut roots = Vec::new();
    for symbol in symbols {
        if let Some(parent) = parent_by_child.get(&symbol.stable_key) {
            children.entry(parent.clone()).or_default().push(*symbol);
        } else {
            roots.push(*symbol);
        }
    }
    roots.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    roots
        .into_iter()
        .filter_map(|node| nested_symbol(node, &mut children))
        .collect()
}

fn nested_symbol(
    node: &GraphNode,
    children: &mut BTreeMap<StableKey, Vec<&GraphNode>>,
) -> Option<ReportSymbol> {
    let mut nested = children.remove(&node.stable_key).unwrap_or_default();
    nested.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    let mut result = report_symbol(node)?;
    result.children = nested
        .into_iter()
        .filter_map(|child| nested_symbol(child, children))
        .collect();
    Some(result)
}

fn report_symbol(node: &GraphNode) -> Option<ReportSymbol> {
    let PlannedNodeAttributes::Symbol { symbol_kind, .. } = &node.attributes else {
        return None;
    };
    Some(ReportSymbol {
        node: NodeSummary::from(node),
        symbol_kind: *symbol_kind,
        children: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use ctx_core::ir::{SourceRange, SymbolKind};

    use super::*;

    #[test]
    fn catalogs_follow_the_explicit_product_order_and_exclude_leaf_kinds() {
        let graph = snapshot(vec![
            interaction("requirement", NodeKind::Requirement, "REQ-1"),
            interaction("database", NodeKind::DbEntity, "subscriptions"),
            interaction("feature", NodeKind::Feature, "FEAT-1"),
        ]);

        let catalogs = build_catalogs(&graph);

        assert_eq!(catalogs.len(), 7);
        assert_eq!(catalogs[0].kind, NodeKind::Feature);
        assert_eq!(catalogs[1].kind, NodeKind::Requirement);
        assert_eq!(catalogs[0].nodes[0].identifier, "FEAT-1");
        assert!(
            catalogs
                .iter()
                .all(|catalog| catalog.kind != NodeKind::DbEntity)
        );
    }

    #[test]
    fn tree_nests_only_unique_segment_prefixes_and_keeps_free_functions_flat() {
        let owner = symbol("owner", "pkg.Service", SymbolKind::Class, "src/service.py");
        let method = symbol(
            "method",
            "pkg.Service.run",
            SymbolKind::Method,
            "src/service.py",
        );
        let similarly_named = symbol(
            "similar",
            "pkg.ServiceWorker.run",
            SymbolKind::Method,
            "src/service.py",
        );
        let function = symbol(
            "function",
            "pkg.run",
            SymbolKind::Function,
            "src/service.py",
        );

        let nested = nest_symbols(&[&owner, &method, &similarly_named, &function]);

        assert_eq!(nested.len(), 3);
        let service = nested
            .iter()
            .find(|entry| entry.node.identifier == "pkg.Service")
            .expect("owner");
        assert_eq!(service.children.len(), 1);
        assert_eq!(service.children[0].node.identifier, "pkg.Service.run");
        assert!(
            nested
                .iter()
                .any(|entry| entry.node.identifier == "pkg.ServiceWorker.run")
        );
        assert!(
            nested
                .iter()
                .any(|entry| entry.node.identifier == "pkg.run")
        );
    }

    #[test]
    fn ambiguous_collision_suffixed_owners_do_not_claim_a_child() {
        let first = symbol(
            "owner#1",
            "pkg.Service",
            SymbolKind::Class,
            "src/service.py",
        );
        let second = symbol(
            "owner#2",
            "pkg.Service",
            SymbolKind::Class,
            "src/service.py",
        );
        let method = symbol(
            "method",
            "pkg.Service.run",
            SymbolKind::Method,
            "src/service.py",
        );

        let nested = nest_symbols(&[&first, &second, &method]);

        assert_eq!(nested.len(), 3);
        assert!(nested.iter().all(|entry| entry.children.is_empty()));
    }

    fn snapshot(nodes: Vec<GraphNode>) -> GraphSnapshot {
        GraphSnapshot {
            nodes: nodes
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges: Vec::new(),
        }
    }

    fn interaction(key: &str, kind: NodeKind, identifier: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("key"),
            kind,
            name: identifier.to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Interaction {
                identifier: identifier.to_owned(),
            },
        }
    }

    fn symbol(key: &str, canonical: &str, kind: SymbolKind, file_path: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("key"),
            kind: NodeKind::CodeSymbol,
            name: canonical.rsplit('.').next().unwrap_or(canonical).to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: file_path.to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind: kind,
                range: SourceRange::default(),
                signature: None,
                structural_fingerprint: "shape".to_owned(),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                orm_accesses: Vec::new(),
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            },
        }
    }
}
