use std::collections::{BTreeMap, BTreeSet};

use ctx_app::ports::{CommitMetadata, PortError, TypeInferenceStore};
use ctx_core::{
    domain::{CommitOid, RepositoryId, StableKey},
    type_inference::{TypeInferenceEdge, TypeInferencePersistenceStats},
};
use rusqlite::{OptionalExtension, Transaction, params};

use super::SqliteStore;

#[derive(Clone, Copy)]
struct CurrentInference {
    edge_row: i64,
    valid_from: i64,
}

impl TypeInferenceStore for SqliteStore {
    fn replace_type_inferences(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        inferred_at: &str,
        producer: &str,
        edges: &[TypeInferenceEdge],
    ) -> Result<TypeInferencePersistenceStats, PortError> {
        validate_edges(producer, edges)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        let commit_row = commit_row(&transaction, repository_row, &commit.oid)?;
        let current = current_inferences(&transaction, repository_row, producer)?;
        let next = edges
            .iter()
            .map(|edge| edge.fingerprint.as_str())
            .collect::<BTreeSet<_>>();
        let mut stats = TypeInferencePersistenceStats::default();

        for (fingerprint, existing) in &current {
            if next.contains(fingerprint.as_str()) {
                continue;
            }
            if existing.valid_from == commit_row {
                delete_edge(&transaction, existing.edge_row)?;
            } else {
                transaction
                    .execute(
                        "UPDATE edges SET valid_to = ?1 WHERE id = ?2",
                        params![commit_row, existing.edge_row],
                    )
                    .map_err(database_error)?;
            }
            stats.removed += 1;
        }

        for edge in edges {
            let existing = current.get(&edge.fingerprint).copied();
            if let Some(existing) = existing {
                stats.updated += 1;
                if existing.valid_from != commit_row {
                    transaction
                        .execute(
                            "UPDATE edges SET valid_to = ?1 WHERE id = ?2",
                            params![commit_row, existing.edge_row],
                        )
                        .map_err(database_error)?;
                }
            } else {
                stats.created += 1;
            }
            persist_inference(&transaction, repository_row, commit_row, inferred_at, edge)?;
        }
        transaction.commit().map_err(database_error)?;
        Ok(stats)
    }
}

fn validate_edges(producer: &str, edges: &[TypeInferenceEdge]) -> Result<(), PortError> {
    let mut fingerprints = BTreeSet::new();
    for edge in edges {
        if edge.producer != producer {
            return Err(PortError::new(format!(
                "type inference edge producer '{}' does not match layer producer '{producer}'",
                edge.producer
            )));
        }
        if !fingerprints.insert(edge.fingerprint.as_str()) {
            return Err(PortError::new(format!(
                "duplicate type inference fingerprint '{}'",
                edge.fingerprint
            )));
        }
    }
    Ok(())
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

fn commit_row(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit: &CommitOid,
) -> Result<i64, PortError> {
    transaction
        .query_row(
            "SELECT id FROM commits WHERE repository_id = ?1 AND oid = ?2",
            params![repository_row, commit.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?
        .ok_or_else(|| {
            PortError::new(format!(
                "commit '{}' has not been indexed before type inference",
                commit
            ))
        })
}

fn current_inferences(
    transaction: &Transaction<'_>,
    repository_row: i64,
    producer: &str,
) -> Result<BTreeMap<String, CurrentInference>, PortError> {
    let mut statement = transaction
        .prepare(
            "SELECT id, fingerprint, valid_from
             FROM edges
             WHERE repository_id = ?1 AND valid_to IS NULL
               AND epistemic_class = 'inference'
               AND provenance_kind = 'typeinference'
               AND producer = ?2
             ORDER BY fingerprint",
        )
        .map_err(database_error)?;
    statement
        .query_map(params![repository_row, producer], |row| {
            Ok((
                row.get::<_, String>(1)?,
                CurrentInference {
                    edge_row: row.get(0)?,
                    valid_from: row.get(2)?,
                },
            ))
        })
        .map_err(database_error)?
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(database_error)
}

fn persist_inference(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    inferred_at: &str,
    edge: &TypeInferenceEdge,
) -> Result<(), PortError> {
    let source_row = node_row(transaction, repository_row, &edge.source)?
        .ok_or_else(|| PortError::new(format!("inference source '{}' is missing", edge.source)))?;
    let target_row = node_row(transaction, repository_row, &edge.target)?
        .ok_or_else(|| PortError::new(format!("inference target '{}' is missing", edge.target)))?;
    let edge_row = transaction
        .query_row(
            "INSERT INTO edges(
                repository_id, src_node_id, dst_node_id, kind, epistemic_class,
                provenance_kind, confidence, status, valid_from, producer, fingerprint
             ) VALUES (?1, ?2, ?3, ?4, 'inference', 'typeinference', ?5, 'active', ?6, ?7, ?8)
             ON CONFLICT(repository_id, fingerprint, valid_from) DO UPDATE SET
                src_node_id = excluded.src_node_id,
                dst_node_id = excluded.dst_node_id,
                kind = excluded.kind,
                epistemic_class = 'inference',
                provenance_kind = 'typeinference',
                confidence = excluded.confidence,
                status = 'active',
                valid_to = NULL,
                producer = excluded.producer,
                stale_reason = NULL
             RETURNING id",
            params![
                repository_row,
                source_row,
                target_row,
                format!("{:?}", edge.relation).to_ascii_lowercase(),
                f64::from(edge.confidence.get()),
                commit_row,
                edge.producer,
                edge.fingerprint,
            ],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    delete_edge_inputs(transaction, edge_row)?;
    transaction
        .execute(
            "INSERT INTO derivations(edge_id, producer, source_uri, input_fingerprint)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                edge_row,
                edge.producer,
                edge.source_uri,
                edge.input_fingerprint,
            ],
        )
        .map_err(database_error)?;
    let metadata = serde_json::to_string(&serde_json::json!({
        "producer": edge.producer,
        "provenance": "type_inference",
    }))
    .map_err(serialization_error)?;
    transaction
        .execute(
            "INSERT INTO sources(
                repository_id, kind, uri, commit_id, author, timestamp,
                content_hash, metadata_json
             ) VALUES (?1, 'typeinference', ?2, ?3, NULL, ?4, ?5, ?6)",
            params![
                repository_row,
                edge.source_uri,
                commit_row,
                inferred_at,
                edge.input_fingerprint,
                metadata,
            ],
        )
        .map_err(database_error)?;
    let evidence_source = transaction.last_insert_rowid();
    transaction
        .execute(
            "INSERT INTO evidence(source_id, locator, excerpt_hash, strength, attributes_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                evidence_source,
                edge.evidence_locator,
                edge.input_fingerprint,
                f64::from(edge.confidence.get()),
                serde_json::to_string(&serde_json::json!({
                    "producer": edge.producer,
                    "provenance": "type_inference",
                }))
                .map_err(serialization_error)?,
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

fn delete_edge(transaction: &Transaction<'_>, edge_row: i64) -> Result<(), PortError> {
    delete_edge_inputs(transaction, edge_row)?;
    transaction
        .execute("DELETE FROM annotations WHERE edge_id = ?1", [edge_row])
        .map_err(database_error)?;
    transaction
        .execute("DELETE FROM edges WHERE id = ?1", [edge_row])
        .map_err(database_error)?;
    Ok(())
}

fn delete_edge_inputs(transaction: &Transaction<'_>, edge_row: i64) -> Result<(), PortError> {
    let evidence = {
        let mut statement = transaction
            .prepare(
                "SELECT ev.id, ev.source_id
                 FROM edge_evidence ee
                 JOIN evidence ev ON ev.id = ee.evidence_id
                 WHERE ee.edge_id = ?1",
            )
            .map_err(database_error)?;
        statement
            .query_map([edge_row], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(database_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?
    };
    transaction
        .execute("DELETE FROM edge_evidence WHERE edge_id = ?1", [edge_row])
        .map_err(database_error)?;
    transaction
        .execute("DELETE FROM derivations WHERE edge_id = ?1", [edge_row])
        .map_err(database_error)?;
    for (evidence_row, _) in &evidence {
        transaction
            .execute("DELETE FROM evidence WHERE id = ?1", [evidence_row])
            .map_err(database_error)?;
    }
    for source_row in evidence
        .into_iter()
        .map(|(_, source)| source)
        .collect::<BTreeSet<_>>()
    {
        transaction
            .execute(
                "DELETE FROM sources
                 WHERE id = ?1 AND NOT EXISTS (
                    SELECT 1 FROM evidence WHERE source_id = ?1
                 )",
                [source_row],
            )
            .map_err(database_error)?;
    }
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

fn database_error(error: rusqlite::Error) -> PortError {
    PortError::new(format!("SQLite type inference persistence failed: {error}"))
}

fn serialization_error(error: serde_json::Error) -> PortError {
    PortError::new(format!(
        "type inference evidence serialization failed: {error}"
    ))
}
