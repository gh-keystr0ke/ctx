use std::collections::BTreeMap;

use ctx_app::ports::{
    CommitMetadata, IndexStore, PortError, RepositoryDescriptor, RepositoryStatus,
};
use ctx_core::{
    domain::{CommitOid, RepositoryId, StableKey},
    indexing::{
        IndexPlan, IndexedFile, IndexedSymbol, PlannedEdge, PlannedNode, PlannedNodeAttributes,
        RepositorySnapshot,
    },
};
use rusqlite::{OptionalExtension, Transaction, params};

use super::SqliteStore;

impl IndexStore for SqliteStore {
    fn ensure_repository(
        &mut self,
        repository: &RepositoryDescriptor,
        created_at: &str,
    ) -> Result<(), PortError> {
        self.connection
            .execute(
                "INSERT INTO repositories(stable_id, root_path, remote_url, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(stable_id) DO UPDATE SET
                    root_path = excluded.root_path,
                    remote_url = excluded.remote_url",
                params![
                    repository.id.as_str(),
                    repository.root_path,
                    repository.remote_url,
                    created_at
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    fn latest_commit(
        &self,
        repository: &RepositoryId,
    ) -> Result<Option<CommitMetadata>, PortError> {
        self.connection
            .query_row(
                "SELECT c.oid, c.parent_oid, c.authored_at
                 FROM commits c
                 JOIN repositories r ON r.id = c.repository_id
                 WHERE r.stable_id = ?1
                 ORDER BY c.id DESC LIMIT 1",
                [repository.as_str()],
                |row| {
                    let oid: String = row.get(0)?;
                    let parent_oid: Option<String> = row.get(1)?;
                    let authored_at: String = row.get(2)?;
                    Ok((oid, parent_oid, authored_at))
                },
            )
            .optional()
            .map_err(database_error)?
            .map(|(oid, parent_oid, authored_at)| {
                Ok(CommitMetadata {
                    oid: CommitOid::new(oid).map_err(domain_error)?,
                    parent_oid: parent_oid
                        .map(CommitOid::new)
                        .transpose()
                        .map_err(domain_error)?,
                    authored_at,
                })
            })
            .transpose()
    }

    fn load_snapshot(&self, repository: &RepositoryId) -> Result<RepositorySnapshot, PortError> {
        let mut files = self.load_current_files(repository)?;
        self.load_current_symbols(repository, &mut files)?;
        Ok(RepositorySnapshot { files })
    }

    fn apply_index(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        indexed_at: &str,
        plan: &IndexPlan,
    ) -> Result<(), PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        let commit_row = insert_commit(&transaction, repository_row, commit, indexed_at)?;
        close_derived_edges(&transaction, repository_row, commit_row, plan)?;
        retire_nodes(&transaction, repository_row, commit_row, plan)?;
        for node in &plan.nodes_to_write {
            persist_node(&transaction, repository_row, commit_row, node)?;
        }
        mark_semantic_edges_stale(&transaction, repository_row, plan)?;
        for edge in &plan.edges_to_create {
            persist_edge(&transaction, repository_row, commit_row, indexed_at, edge)?;
        }
        transaction.commit().map_err(database_error)
    }

    fn status(&self, repository: &RepositoryId) -> Result<RepositoryStatus, PortError> {
        let latest = self.latest_commit(repository)?;
        let repository_row = self
            .connection
            .query_row(
                "SELECT id FROM repositories WHERE stable_id = ?1",
                [repository.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(database_error)?;
        let Some(repository_row) = repository_row else {
            return Ok(RepositoryStatus::default());
        };
        Ok(RepositoryStatus {
            last_indexed_commit: latest.map(|commit| commit.oid),
            files: count_current_nodes(&self.connection, repository_row, "file")?,
            symbols: count_current_nodes(&self.connection, repository_row, "code_symbol")?,
            db_entities: count_current_nodes(&self.connection, repository_row, "db_entity")?,
            features: count_current_nodes(&self.connection, repository_row, "feature")?,
            requirements: count_current_nodes(&self.connection, repository_row, "requirement")?,
            invariants: count_current_nodes(&self.connection, repository_row, "invariant")?,
            decisions: count_current_nodes(&self.connection, repository_row, "decision")?,
            public_documents: count_public_documents(&self.connection, repository_row)?,
            active_edges: count_edges(&self.connection, repository_row, "active")?,
            structural_facts: count_current_edges_by_class(
                &self.connection,
                repository_row,
                "fact",
                "active",
            )?,
            active_assertions: count_current_edges_by_class(
                &self.connection,
                repository_row,
                "assertion",
                "active",
            )?,
            active_inferences: count_current_edges_by_class(
                &self.connection,
                repository_row,
                "inference",
                "active",
            )?,
            stale_semantic_edges: count_edges(&self.connection, repository_row, "stale")?,
            rejected_semantic_edges: count_edges(&self.connection, repository_row, "rejected")?,
        })
    }
}

fn count_public_documents(
    connection: &rusqlite::Connection,
    repository_row: i64,
) -> Result<usize, PortError> {
    connection
        .query_row(
            "SELECT COUNT(*)
             FROM nodes n
             JOIN node_versions nv ON nv.node_id = n.id AND nv.valid_to IS NULL
             WHERE n.repository_id = ?1 AND n.retired_commit IS NULL
               AND n.kind IN ('feature', 'requirement', 'invariant', 'decision')
               AND nv.visibility = 'public'",
            [repository_row],
            |row| row.get(0),
        )
        .map_err(database_error)
}

impl SqliteStore {
    fn load_current_files(
        &self,
        repository: &RepositoryId,
    ) -> Result<BTreeMap<String, IndexedFile>, PortError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT n.stable_key, nv.content_hash, nv.attributes_json
                 FROM nodes n
                 JOIN node_versions nv ON nv.node_id = n.id AND nv.valid_to IS NULL
                 JOIN repositories r ON r.id = n.repository_id
                 WHERE r.stable_id = ?1 AND n.kind = 'file' AND n.retired_commit IS NULL
                 ORDER BY n.stable_key",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([repository.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(database_error)?;
        let mut files = BTreeMap::new();
        for row in rows {
            let (stable_key, content_hash, attributes) = row.map_err(database_error)?;
            let attributes: PlannedNodeAttributes =
                serde_json::from_str(&attributes).map_err(serialization_error)?;
            if let PlannedNodeAttributes::File {
                path,
                language,
                analysis_version,
            } = attributes
            {
                files.insert(
                    path.clone(),
                    IndexedFile {
                        stable_key: StableKey::new(stable_key).map_err(domain_error)?,
                        path,
                        language,
                        analysis_version,
                        content_hash,
                        symbols: Vec::new(),
                    },
                );
            }
        }
        Ok(files)
    }

    fn load_current_symbols(
        &self,
        repository: &RepositoryId,
        files: &mut BTreeMap<String, IndexedFile>,
    ) -> Result<(), PortError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT n.stable_key, nv.name, nv.content_hash, nv.attributes_json
                 FROM nodes n
                 JOIN node_versions nv ON nv.node_id = n.id AND nv.valid_to IS NULL
                 JOIN repositories r ON r.id = n.repository_id
                 WHERE r.stable_id = ?1 AND n.kind = 'code_symbol'
                   AND n.retired_commit IS NULL
                 ORDER BY n.stable_key",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([repository.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (stable_key, name, body_hash, attributes) = row.map_err(database_error)?;
            let attributes: PlannedNodeAttributes =
                serde_json::from_str(&attributes).map_err(serialization_error)?;
            let PlannedNodeAttributes::Symbol {
                file_path,
                canonical_path,
                symbol_kind,
                range,
                signature,
                structural_fingerprint,
                calls,
                database_accesses,
                orm_accesses,
                schema_tables,
                api_endpoints,
                external_calls,
            } = attributes
            else {
                continue;
            };
            if let Some(file) = files.get_mut(&file_path) {
                file.symbols.push(IndexedSymbol {
                    stable_key: StableKey::new(stable_key).map_err(domain_error)?,
                    language: file.language.clone(),
                    file_path,
                    name,
                    canonical_path,
                    kind: symbol_kind,
                    range,
                    signature,
                    body_hash,
                    structural_fingerprint,
                    calls,
                    database_accesses,
                    orm_accesses,
                    schema_tables,
                    api_endpoints,
                    external_calls,
                });
            }
        }
        Ok(())
    }
}

fn repository_row(
    transaction: &Transaction<'_>,
    repository: &RepositoryId,
) -> Result<i64, PortError> {
    transaction
        .query_row(
            "SELECT id FROM repositories WHERE stable_id = ?1",
            [repository.as_str()],
            |row| row.get(0),
        )
        .map_err(database_error)
}

fn insert_commit(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit: &CommitMetadata,
    indexed_at: &str,
) -> Result<i64, PortError> {
    transaction
        .query_row(
            "INSERT INTO commits(repository_id, oid, parent_oid, authored_at, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(repository_id, oid) DO UPDATE SET
                parent_oid = excluded.parent_oid,
                authored_at = excluded.authored_at,
                indexed_at = excluded.indexed_at
             RETURNING id",
            params![
                repository_row,
                commit.oid.as_str(),
                commit.parent_oid.as_ref().map(CommitOid::as_str),
                commit.authored_at,
                indexed_at
            ],
            |row| row.get(0),
        )
        .map_err(database_error)
}

fn close_derived_edges(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    plan: &IndexPlan,
) -> Result<(), PortError> {
    for source_uri in &plan.structural_sources_to_close {
        transaction
            .execute(
                "UPDATE edges SET valid_to = ?1
                 WHERE repository_id = ?2 AND valid_to IS NULL
                   AND epistemic_class = 'fact'
                   AND id IN (SELECT edge_id FROM derivations WHERE source_uri = ?3)",
                params![commit_row, repository_row, source_uri],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

fn retire_nodes(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    plan: &IndexPlan,
) -> Result<(), PortError> {
    for stable_key in &plan.nodes_to_retire {
        let node_row = node_row(transaction, repository_row, stable_key)?;
        let Some(node_row) = node_row else {
            continue;
        };
        transaction
            .execute(
                "UPDATE node_versions SET valid_to = ?1
                 WHERE node_id = ?2 AND valid_to IS NULL",
                params![commit_row, node_row],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "UPDATE nodes SET retired_commit = ?1 WHERE id = ?2",
                params![commit_row, node_row],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "UPDATE edges SET valid_to = ?1
                 WHERE valid_to IS NULL AND epistemic_class = 'fact'
                   AND (src_node_id = ?2 OR dst_node_id = ?2)",
                params![commit_row, node_row],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

fn persist_node(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    node: &PlannedNode,
) -> Result<(), PortError> {
    transaction
        .execute(
            "INSERT INTO nodes(repository_id, kind, stable_key, created_commit)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repository_id, kind, stable_key)
             DO UPDATE SET retired_commit = NULL",
            params![
                repository_row,
                node_kind(node),
                node.stable_key.as_str(),
                commit_row
            ],
        )
        .map_err(database_error)?;
    let node_row = node_row(transaction, repository_row, &node.stable_key)?
        .ok_or_else(|| PortError::new("persisted node could not be read back"))?;
    transaction
        .execute(
            "UPDATE node_versions SET valid_to = ?1
             WHERE node_id = ?2 AND valid_to IS NULL AND valid_from != ?1",
            params![commit_row, node_row],
        )
        .map_err(database_error)?;
    let attributes = serde_json::to_string(&node.attributes).map_err(serialization_error)?;
    transaction
        .execute(
            "INSERT INTO node_versions(
                node_id, valid_from, name, content_hash, attributes_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id, valid_from) DO UPDATE SET
                valid_to = NULL,
                name = excluded.name,
                content_hash = excluded.content_hash,
                attributes_json = excluded.attributes_json",
            params![
                node_row,
                commit_row,
                node.name,
                node.content_hash,
                attributes
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

fn mark_semantic_edges_stale(
    transaction: &Transaction<'_>,
    repository_row: i64,
    plan: &IndexPlan,
) -> Result<(), PortError> {
    for stable_key in &plan.semantic_sources_to_mark_stale {
        let Some(node_row) = node_row(transaction, repository_row, stable_key)? else {
            continue;
        };
        transaction
            .execute(
                "UPDATE edges SET status = 'stale', stale_reason = 'implementation_changed'
                 WHERE repository_id = ?1 AND valid_to IS NULL
                   AND epistemic_class != 'fact'
                   AND (src_node_id = ?2 OR dst_node_id = ?2)",
                params![repository_row, node_row],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

fn persist_edge(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    indexed_at: &str,
    edge: &PlannedEdge,
) -> Result<(), PortError> {
    let source_row = node_row(transaction, repository_row, &edge.source)?
        .ok_or_else(|| PortError::new(format!("edge source '{}' is missing", edge.source)))?;
    let target_row = node_row(transaction, repository_row, &edge.target)?
        .ok_or_else(|| PortError::new(format!("edge target '{}' is missing", edge.target)))?;
    transaction
        .execute(
            "INSERT INTO edges(
                repository_id, src_node_id, dst_node_id, kind, epistemic_class,
                provenance_kind, confidence, status, valid_from, producer, fingerprint
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                repository_row,
                source_row,
                target_row,
                relation_kind(edge),
                claim_class(edge),
                source_kind(edge),
                f64::from(edge.confidence.get()),
                claim_status(edge),
                commit_row,
                edge.producer,
                edge.fingerprint
            ],
        )
        .map_err(database_error)?;
    let edge_row = transaction.last_insert_rowid();
    transaction
        .execute(
            "INSERT INTO derivations(edge_id, producer, source_uri, input_fingerprint)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                edge_row,
                edge.producer,
                edge.source_uri,
                edge.input_fingerprint
            ],
        )
        .map_err(database_error)?;
    if let Some(locator) = &edge.evidence_locator {
        persist_static_evidence(
            transaction,
            repository_row,
            commit_row,
            indexed_at,
            edge_row,
            edge,
            locator,
        )?;
    }
    Ok(())
}

fn persist_static_evidence(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    indexed_at: &str,
    edge_row: i64,
    edge: &PlannedEdge,
    locator: &str,
) -> Result<(), PortError> {
    transaction
        .execute(
            "INSERT INTO sources(
                repository_id, kind, uri, commit_id, author, timestamp,
                content_hash, metadata_json
             ) VALUES (?1, 'staticanalysis', ?2, ?3, NULL, ?4, ?5, ?6)",
            params![
                repository_row,
                edge.source_uri,
                commit_row,
                indexed_at,
                edge.input_fingerprint,
                format!(r#"{{"producer":"{}"}}"#, edge.producer)
            ],
        )
        .map_err(database_error)?;
    let source_row = transaction.last_insert_rowid();
    transaction
        .execute(
            "INSERT INTO evidence(
                source_id, locator, excerpt_hash, strength, attributes_json
             ) VALUES (?1, ?2, ?3, ?4, '{}')",
            params![
                source_row,
                locator,
                edge.input_fingerprint,
                f64::from(edge.confidence.get())
            ],
        )
        .map_err(database_error)?;
    let evidence_row = transaction.last_insert_rowid();
    transaction
        .execute(
            "INSERT INTO edge_evidence(edge_id, evidence_id) VALUES (?1, ?2)",
            params![edge_row, evidence_row],
        )
        .map_err(database_error)?;
    Ok(())
}

fn node_row(
    transaction: &Transaction<'_>,
    repository_row: i64,
    stable_key: &StableKey,
) -> Result<Option<i64>, PortError> {
    transaction
        .query_row(
            "SELECT id FROM nodes WHERE repository_id = ?1 AND stable_key = ?2",
            params![repository_row, stable_key.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)
}

fn count_current_nodes(
    connection: &rusqlite::Connection,
    repository_row: i64,
    kind: &str,
) -> Result<usize, PortError> {
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM nodes
             WHERE repository_id = ?1 AND kind = ?2 AND retired_commit IS NULL",
            params![repository_row, kind],
            |row| row.get::<_, i64>(0),
        )
        .map_err(database_error)?;
    usize::try_from(count).map_err(|error| PortError::new(error.to_string()))
}

fn count_edges(
    connection: &rusqlite::Connection,
    repository_row: i64,
    status: &str,
) -> Result<usize, PortError> {
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM edges
             WHERE repository_id = ?1 AND status = ?2 AND valid_to IS NULL",
            params![repository_row, status],
            |row| row.get::<_, i64>(0),
        )
        .map_err(database_error)?;
    usize::try_from(count).map_err(|error| PortError::new(error.to_string()))
}

fn count_current_edges_by_class(
    connection: &rusqlite::Connection,
    repository_row: i64,
    epistemic_class: &str,
    status: &str,
) -> Result<usize, PortError> {
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM edges
             WHERE repository_id = ?1 AND epistemic_class = ?2
               AND status = ?3 AND valid_to IS NULL",
            params![repository_row, epistemic_class, status],
            |row| row.get::<_, i64>(0),
        )
        .map_err(database_error)?;
    usize::try_from(count).map_err(|error| PortError::new(error.to_string()))
}

const fn node_kind(node: &PlannedNode) -> &'static str {
    match node.kind {
        ctx_core::domain::NodeKind::Feature => "feature",
        ctx_core::domain::NodeKind::Requirement => "requirement",
        ctx_core::domain::NodeKind::Invariant => "invariant",
        ctx_core::domain::NodeKind::Decision => "decision",
        ctx_core::domain::NodeKind::DomainConcept => "domain_concept",
        ctx_core::domain::NodeKind::ExternalSystem => "external_system",
        ctx_core::domain::NodeKind::File => "file",
        ctx_core::domain::NodeKind::CodeSymbol => "code_symbol",
        ctx_core::domain::NodeKind::Endpoint => "endpoint",
        ctx_core::domain::NodeKind::ApiEndpoint => "api_endpoint",
        ctx_core::domain::NodeKind::DbEntity => "db_entity",
        ctx_core::domain::NodeKind::Event => "event",
    }
}

fn relation_kind(edge: &PlannedEdge) -> String {
    format!("{:?}", edge.kind).to_ascii_lowercase()
}

fn claim_class(edge: &PlannedEdge) -> String {
    format!("{:?}", edge.claim_class).to_ascii_lowercase()
}

fn source_kind(edge: &PlannedEdge) -> String {
    format!("{:?}", edge.source_kind).to_ascii_lowercase()
}

fn claim_status(edge: &PlannedEdge) -> String {
    format!("{:?}", edge.status).to_ascii_lowercase()
}

#[allow(clippy::needless_pass_by_value)]
fn database_error(error: rusqlite::Error) -> PortError {
    PortError::new(format!("SQLite operation failed: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn serialization_error(error: serde_json::Error) -> PortError {
    PortError::new(format!("stored node data is invalid: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn domain_error(error: ctx_core::domain::InvalidIdentifier) -> PortError {
    PortError::new(format!("stored identifier is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use ctx_app::ports::{CommitMetadata, GraphStore, IndexStore, RepositoryDescriptor};
    use ctx_core::{
        domain::{
            ClaimClass, ClaimStatus, CommitOid, Confidence, NodeKind, RelationKind, RepositoryId,
            SourceKind, StableKey,
        },
        indexing::{IndexPlan, NodeMutationKind, PlannedEdge, PlannedNode, PlannedNodeAttributes},
        ir::{SourceRange, SymbolKind},
    };
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn same_commit_reanalysis_replaces_derived_node_version() {
        let directory = tempdir().expect("temporary directory");
        let mut store = SqliteStore::open(&directory.path().join("ctx.db"), directory.path())
            .expect("SQLite store");
        let repository = RepositoryDescriptor {
            id: RepositoryId::new("repo:test").expect("repository ID"),
            root_path: "/repo".to_owned(),
            remote_url: None,
        };
        let commit = CommitMetadata {
            oid: CommitOid::new("aaaaaaaa").expect("commit OID"),
            parent_oid: None,
            authored_at: "2026-08-17T00:00:00Z".to_owned(),
        };
        store
            .ensure_repository(&repository, "2026-08-17T00:00:01Z")
            .expect("repository");
        store
            .apply_index(
                &repository.id,
                &commit,
                "2026-08-17T00:00:01Z",
                &file_plan("old", "rust-tree-sitter-v1", NodeMutationKind::Create),
            )
            .expect("first analysis");
        store
            .apply_index(
                &repository.id,
                &commit,
                "2026-08-17T00:00:02Z",
                &file_plan("new", "rust-tree-sitter-v2", NodeMutationKind::Version),
            )
            .expect("same-commit reanalysis");

        let state: (i64, i64, String, String) = store
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM commits),
                    (SELECT COUNT(*) FROM node_versions),
                    nv.content_hash,
                    json_extract(nv.attributes_json, '$.analysis_version')
                 FROM node_versions nv WHERE nv.valid_to IS NULL",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("current reanalysis state");

        assert_eq!(
            state,
            (1, 1, "new".to_owned(), "rust-tree-sitter-v2".to_owned())
        );
    }

    #[test]
    fn database_facts_persist_with_static_evidence_and_status_counts() {
        let directory = tempdir().expect("temporary directory");
        let mut store = SqliteStore::open(&directory.path().join("ctx.db"), directory.path())
            .expect("SQLite store");
        let repository = RepositoryDescriptor {
            id: RepositoryId::new("repo:data").expect("repository ID"),
            root_path: "/repo".to_owned(),
            remote_url: None,
        };
        let commit = CommitMetadata {
            oid: CommitOid::new("bbbbbbbb").expect("commit OID"),
            parent_oid: None,
            authored_at: "2026-08-17T00:00:00Z".to_owned(),
        };
        store
            .ensure_repository(&repository, "2026-08-17T00:00:01Z")
            .expect("repository");
        store
            .apply_index(
                &repository.id,
                &commit,
                "2026-08-17T00:00:01Z",
                &database_plan(),
            )
            .expect("database fact");

        let graph = store.load_graph(&repository.id).expect("graph");
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == RelationKind::WritesTo)
            .expect("write fact");
        assert_eq!(edge.claim_class, ClaimClass::Fact);
        assert_eq!(edge.source_kind, SourceKind::StaticAnalysis);
        assert_eq!(edge.evidence.len(), 1);
        assert_eq!(edge.evidence[0].locator, "lines:12");
        assert_eq!(store.status(&repository.id).expect("status").db_entities, 1);
    }

    fn file_plan(
        content_hash: &str,
        analysis_version: &str,
        mutation: NodeMutationKind,
    ) -> IndexPlan {
        IndexPlan {
            nodes_to_write: vec![PlannedNode {
                stable_key: StableKey::new("file:src/lib.rs").expect("file key"),
                kind: NodeKind::File,
                name: "src/lib.rs".to_owned(),
                content_hash: content_hash.to_owned(),
                attributes: PlannedNodeAttributes::File {
                    path: "src/lib.rs".to_owned(),
                    language: "rust".to_owned(),
                    analysis_version: analysis_version.to_owned(),
                },
                mutation,
            }],
            ..IndexPlan::default()
        }
    }

    fn database_plan() -> IndexPlan {
        let symbol = StableKey::new("symbol:python:billing.persist:Function").expect("symbol key");
        let database = StableKey::new("db:subscriptions").expect("database key");
        IndexPlan {
            nodes_to_write: vec![
                PlannedNode {
                    stable_key: symbol.clone(),
                    kind: NodeKind::CodeSymbol,
                    name: "persist".to_owned(),
                    content_hash: "body".to_owned(),
                    attributes: PlannedNodeAttributes::Symbol {
                        file_path: "src/billing.py".to_owned(),
                        canonical_path: "billing.persist".to_owned(),
                        symbol_kind: SymbolKind::Function,
                        range: SourceRange {
                            start_byte: 0,
                            end_byte: 20,
                            start_line: 10,
                            end_line: 14,
                        },
                        signature: Some("()".to_owned()),
                        structural_fingerprint: "shape".to_owned(),
                        calls: Vec::new(),
                        database_accesses: Vec::new(),
                        orm_accesses: Vec::new(),
                        schema_tables: Vec::new(),
                        api_endpoints: Vec::new(),
                        external_calls: Vec::new(),
                    },
                    mutation: NodeMutationKind::Create,
                },
                PlannedNode {
                    stable_key: database.clone(),
                    kind: NodeKind::DbEntity,
                    name: "subscriptions".to_owned(),
                    content_hash: "db-entity:subscriptions".to_owned(),
                    attributes: PlannedNodeAttributes::Interaction {
                        identifier: "subscriptions".to_owned(),
                    },
                    mutation: NodeMutationKind::Create,
                },
            ],
            edges_to_create: vec![PlannedEdge {
                source: symbol,
                target: database,
                kind: RelationKind::WritesTo,
                claim_class: ClaimClass::Fact,
                source_kind: SourceKind::StaticAnalysis,
                confidence: Confidence::CERTAIN,
                status: ClaimStatus::Active,
                producer: "python_tree_sitter".to_owned(),
                fingerprint: "write:subscriptions".to_owned(),
                source_uri: "src/billing.py".to_owned(),
                input_fingerprint: "sql-hash".to_owned(),
                evidence_locator: Some("lines:12".to_owned()),
            }],
            ..IndexPlan::default()
        }
    }
}
