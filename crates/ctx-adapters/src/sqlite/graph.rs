use std::collections::BTreeMap;

use ctx_app::ports::{GraphStore, PortError};
use ctx_core::{
    domain::{
        ClaimClass, ClaimStatus, Confidence, NodeKind, RelationKind, RepositoryId, SourceKind,
        StableKey,
    },
    graph::{GraphEdge, GraphEvidence, GraphNode, GraphSnapshot},
};

use super::SqliteStore;

impl GraphStore for SqliteStore {
    fn load_graph(&self, repository: &RepositoryId) -> Result<GraphSnapshot, PortError> {
        let repository_row = self
            .connection
            .query_row(
                "SELECT id FROM repositories WHERE stable_id = ?1",
                [repository.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(database_error)?;
        let nodes = load_nodes(&self.connection, repository_row)?;
        let evidence = load_evidence(&self.connection, repository_row)?;
        let edges = load_edges(&self.connection, repository_row, evidence)?;
        Ok(GraphSnapshot { nodes, edges })
    }
}

fn load_nodes(
    connection: &rusqlite::Connection,
    repository_row: i64,
) -> Result<BTreeMap<StableKey, GraphNode>, PortError> {
    let mut statement = connection
        .prepare(
            "SELECT n.stable_key, n.kind, nv.name, nv.content_hash, nv.attributes_json
             FROM nodes n
             JOIN node_versions nv ON nv.node_id = n.id AND nv.valid_to IS NULL
             WHERE n.repository_id = ?1 AND n.retired_commit IS NULL
             ORDER BY n.stable_key",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([repository_row], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(database_error)?;
    let mut nodes = BTreeMap::new();
    for row in rows {
        let (stable_key, kind, name, content_hash, attributes) = row.map_err(database_error)?;
        let stable_key = StableKey::new(stable_key).map_err(domain_error)?;
        nodes.insert(
            stable_key.clone(),
            GraphNode {
                stable_key,
                kind: parse_node_kind(&kind)?,
                name,
                content_hash,
                attributes: serde_json::from_str(&attributes).map_err(serialization_error)?,
            },
        );
    }
    Ok(nodes)
}

fn load_evidence(
    connection: &rusqlite::Connection,
    repository_row: i64,
) -> Result<BTreeMap<i64, Vec<GraphEvidence>>, PortError> {
    let mut statement = connection
        .prepare(
            "SELECT ee.edge_id, s.kind, s.uri, c.oid, s.author, s.timestamp,
                    ev.locator, ev.strength
             FROM edge_evidence ee
             JOIN evidence ev ON ev.id = ee.evidence_id
             JOIN sources s ON s.id = ev.source_id
             LEFT JOIN commits c ON c.id = s.commit_id
             WHERE s.repository_id = ?1
             ORDER BY ee.edge_id, ev.id",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([repository_row], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, f64>(7)?,
            ))
        })
        .map_err(database_error)?;
    let mut evidence = BTreeMap::new();
    for row in rows {
        let (edge_row, kind, uri, commit, author, timestamp, locator, strength) =
            row.map_err(database_error)?;
        evidence
            .entry(edge_row)
            .or_insert_with(Vec::new)
            .push(GraphEvidence {
                source_kind: parse_source_kind(&kind)?,
                source_uri: uri,
                commit,
                author,
                timestamp,
                locator,
                strength: confidence(strength)?,
            });
    }
    Ok(evidence)
}

fn load_edges(
    connection: &rusqlite::Connection,
    repository_row: i64,
    mut evidence: BTreeMap<i64, Vec<GraphEvidence>>,
) -> Result<Vec<GraphEdge>, PortError> {
    let mut statement = connection
        .prepare(
            "SELECT e.id, src.stable_key, dst.stable_key, e.kind, e.epistemic_class,
                    e.provenance_kind, e.confidence, e.status, from_commit.oid,
                    to_commit.oid, e.producer, e.fingerprint, e.stale_reason
             FROM edges e
             JOIN nodes src ON src.id = e.src_node_id
             JOIN nodes dst ON dst.id = e.dst_node_id
             JOIN commits from_commit ON from_commit.id = e.valid_from
             LEFT JOIN commits to_commit ON to_commit.id = e.valid_to
             WHERE e.repository_id = ?1 AND e.valid_to IS NULL
             ORDER BY e.fingerprint",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([repository_row], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, Option<String>>(12)?,
            ))
        })
        .map_err(database_error)?;
    let mut edges = Vec::new();
    for row in rows {
        let (
            edge_row,
            source,
            target,
            kind,
            claim_class,
            source_kind,
            score,
            status,
            valid_from,
            valid_to,
            producer,
            fingerprint,
            stale_reason,
        ) = row.map_err(database_error)?;
        edges.push(GraphEdge {
            source: StableKey::new(source).map_err(domain_error)?,
            target: StableKey::new(target).map_err(domain_error)?,
            kind: parse_relation_kind(&kind)?,
            claim_class: parse_claim_class(&claim_class)?,
            source_kind: parse_source_kind(&source_kind)?,
            confidence: confidence(score)?,
            status: parse_claim_status(&status)?,
            valid_from,
            valid_to,
            producer,
            fingerprint,
            stale_reason,
            evidence: evidence.remove(&edge_row).unwrap_or_default(),
        });
    }
    Ok(edges)
}

fn parse_node_kind(value: &str) -> Result<NodeKind, PortError> {
    match value {
        "feature" => Ok(NodeKind::Feature),
        "requirement" => Ok(NodeKind::Requirement),
        "invariant" => Ok(NodeKind::Invariant),
        "decision" => Ok(NodeKind::Decision),
        "domain_concept" => Ok(NodeKind::DomainConcept),
        "external_system" => Ok(NodeKind::ExternalSystem),
        "file" => Ok(NodeKind::File),
        "code_symbol" => Ok(NodeKind::CodeSymbol),
        "endpoint" => Ok(NodeKind::Endpoint),
        "db_entity" => Ok(NodeKind::DbEntity),
        "event" => Ok(NodeKind::Event),
        _ => Err(invalid_enum("node kind", value)),
    }
}

fn parse_relation_kind(value: &str) -> Result<RelationKind, PortError> {
    match value {
        "contains" => Ok(RelationKind::Contains),
        "calls" => Ok(RelationKind::Calls),
        "references" => Ok(RelationKind::References),
        "readsfrom" | "reads_from" => Ok(RelationKind::ReadsFrom),
        "writesto" | "writes_to" => Ok(RelationKind::WritesTo),
        "emits" => Ok(RelationKind::Emits),
        "handles" => Ok(RelationKind::Handles),
        "implements" => Ok(RelationKind::Implements),
        "enforces" => Ok(RelationKind::Enforces),
        "coveredby" | "covered_by" => Ok(RelationKind::CoveredBy),
        "dependson" | "depends_on" => Ok(RelationKind::DependsOn),
        "satisfies" => Ok(RelationKind::Satisfies),
        _ => Err(invalid_enum("relation kind", value)),
    }
}

fn parse_claim_class(value: &str) -> Result<ClaimClass, PortError> {
    match value {
        "fact" => Ok(ClaimClass::Fact),
        "assertion" => Ok(ClaimClass::Assertion),
        "inference" => Ok(ClaimClass::Inference),
        _ => Err(invalid_enum("claim class", value)),
    }
}

fn parse_source_kind(value: &str) -> Result<SourceKind, PortError> {
    match value {
        "staticanalysis" | "static_analysis" => Ok(SourceKind::StaticAnalysis),
        "human" => Ok(SourceKind::Human),
        "documentation" => Ok(SourceKind::Documentation),
        "llminference" | "llm_inference" => Ok(SourceKind::LlmInference),
        "runtime" => Ok(SourceKind::Runtime),
        "externalsystem" | "external_system" => Ok(SourceKind::ExternalSystem),
        _ => Err(invalid_enum("source kind", value)),
    }
}

fn parse_claim_status(value: &str) -> Result<ClaimStatus, PortError> {
    match value {
        "active" => Ok(ClaimStatus::Active),
        "stale" => Ok(ClaimStatus::Stale),
        "rejected" => Ok(ClaimStatus::Rejected),
        _ => Err(invalid_enum("claim status", value)),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn confidence(value: f64) -> Result<Confidence, PortError> {
    Confidence::new(value as f32)
        .map_err(|error| PortError::new(format!("stored confidence is invalid: {error}")))
}

fn invalid_enum(kind: &str, value: &str) -> PortError {
    PortError::new(format!("stored {kind} '{value}' is invalid"))
}

#[allow(clippy::needless_pass_by_value)]
fn database_error(error: rusqlite::Error) -> PortError {
    PortError::new(format!("SQLite graph query failed: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn serialization_error(error: serde_json::Error) -> PortError {
    PortError::new(format!("stored graph data is invalid: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn domain_error(error: ctx_core::domain::InvalidIdentifier) -> PortError {
    PortError::new(format!("stored graph identity is invalid: {error}"))
}
