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
            persist_edge(&transaction, repository_row, commit_row, edge)?;
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
            active_edges: count_edges(&self.connection, repository_row, "active")?,
            stale_semantic_edges: count_edges(&self.connection, repository_row, "stale")?,
        })
    }
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
            if let PlannedNodeAttributes::File { path, language } = attributes {
                files.insert(
                    path.clone(),
                    IndexedFile {
                        stable_key: StableKey::new(stable_key).map_err(domain_error)?,
                        path,
                        language,
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
            } = attributes
            else {
                continue;
            };
            if let Some(file) = files.get_mut(&file_path) {
                file.symbols.push(IndexedSymbol {
                    stable_key: StableKey::new(stable_key).map_err(domain_error)?,
                    file_path,
                    name,
                    canonical_path,
                    kind: symbol_kind,
                    range,
                    signature,
                    body_hash,
                    structural_fingerprint,
                    calls,
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
        .execute(
            "INSERT INTO commits(repository_id, oid, parent_oid, authored_at, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                repository_row,
                commit.oid.as_str(),
                commit.parent_oid.as_ref().map(CommitOid::as_str),
                commit.authored_at,
                indexed_at
            ],
        )
        .map_err(database_error)?;
    Ok(transaction.last_insert_rowid())
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
             WHERE node_id = ?2 AND valid_to IS NULL",
            params![commit_row, node_row],
        )
        .map_err(database_error)?;
    let attributes = serde_json::to_string(&node.attributes).map_err(serialization_error)?;
    transaction
        .execute(
            "INSERT INTO node_versions(
                node_id, valid_from, name, content_hash, attributes_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
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

const fn node_kind(node: &PlannedNode) -> &'static str {
    match node.kind {
        ctx_core::domain::NodeKind::File => "file",
        ctx_core::domain::NodeKind::CodeSymbol => "code_symbol",
        _ => "other",
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
