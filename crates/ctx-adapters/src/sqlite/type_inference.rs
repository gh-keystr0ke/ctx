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

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_app::ports::{GraphStore, IndexStore, RepositoryDescriptor};
    use ctx_core::{
        domain::{ClaimClass, ClaimStatus, Confidence, NodeKind, RelationKind, SourceKind},
        indexing::{IndexPlan, NodeMutationKind, PlannedNode, PlannedNodeAttributes},
        ir::{SourceRange, SymbolKind},
    };
    use tempfile::TempDir;

    #[test]
    fn persists_inference_class_type_provenance_and_evidence() {
        let (_directory, mut store, repository, commit) = setup();
        let stats = store
            .replace_type_inferences(
                &repository.id,
                &commit,
                "2026-09-04T00:00:01Z",
                "pyright",
                &[edge("line:12", "input-a")],
            )
            .expect("persist inference");
        assert_eq!(
            stats,
            TypeInferencePersistenceStats {
                created: 1,
                updated: 0,
                removed: 0,
            }
        );

        let graph = store.load_graph(&repository.id).expect("graph");
        let edge = graph.edges.first().expect("inference edge");
        assert_eq!(edge.kind, RelationKind::WritesTo);
        assert_eq!(edge.claim_class, ClaimClass::Inference);
        assert_eq!(edge.source_kind, SourceKind::TypeInference);
        assert_eq!(edge.status, ClaimStatus::Active);
        assert_eq!(edge.producer, "pyright");
        assert_eq!(edge.confidence.get(), 0.9);
        assert_eq!(edge.evidence.len(), 1);
        assert_eq!(edge.evidence[0].source_kind, SourceKind::TypeInference);
        assert_eq!(edge.evidence[0].locator, "line:12");
    }

    #[test]
    fn same_commit_refresh_replaces_evidence_without_duplicate_versions() {
        let (_directory, mut store, repository, commit) = setup();
        store
            .replace_type_inferences(
                &repository.id,
                &commit,
                "2026-09-04T00:00:01Z",
                "pyright",
                &[edge("line:12", "input-a")],
            )
            .expect("first inference");
        let stats = store
            .replace_type_inferences(
                &repository.id,
                &commit,
                "2026-09-04T00:00:02Z",
                "pyright",
                &[edge("line:24 columns:status", "input-b")],
            )
            .expect("same-commit refresh");
        assert_eq!(stats.updated, 1);
        assert_eq!(row_counts(&store), (1, 1, 1, 1));

        let graph = store.load_graph(&repository.id).expect("graph");
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].evidence.len(), 1);
        assert_eq!(graph.edges[0].evidence[0].locator, "line:24 columns:status");
        assert_eq!(graph.edges[0].evidence[0].timestamp, "2026-09-04T00:00:02Z");
    }

    #[test]
    fn same_commit_full_recompute_can_remove_the_layer_cleanly() {
        let (_directory, mut store, repository, commit) = setup();
        store
            .replace_type_inferences(
                &repository.id,
                &commit,
                "2026-09-04T00:00:01Z",
                "pyright",
                &[edge("line:12", "input-a")],
            )
            .expect("first inference");
        let stats = store
            .replace_type_inferences(
                &repository.id,
                &commit,
                "2026-09-04T00:00:02Z",
                "pyright",
                &[],
            )
            .expect("remove same-commit layer");

        assert_eq!(stats.removed, 1);
        assert_eq!(row_counts(&store), (0, 0, 0, 0));
        assert!(
            store
                .load_graph(&repository.id)
                .expect("graph")
                .edges
                .is_empty()
        );
    }

    #[test]
    fn next_commit_versions_the_inference_instead_of_overwriting_history() {
        let (_directory, mut store, repository, first) = setup();
        store
            .replace_type_inferences(
                &repository.id,
                &first,
                "2026-09-04T00:00:01Z",
                "pyright",
                &[edge("line:12", "input-a")],
            )
            .expect("first inference");
        let second = CommitMetadata {
            oid: CommitOid::new("cafebabe").expect("commit"),
            parent_oid: Some(first.oid.clone()),
            authored_at: "2026-09-05T00:00:00Z".to_owned(),
        };
        store
            .apply_index(
                &repository.id,
                &second,
                "2026-09-05T00:00:01Z",
                &IndexPlan::default(),
            )
            .expect("second indexed commit");
        store
            .replace_type_inferences(
                &repository.id,
                &second,
                "2026-09-05T00:00:02Z",
                "pyright",
                &[edge("line:24", "input-b")],
            )
            .expect("version inference");

        let counts: (i64, i64) = store
            .connection()
            .query_row(
                "SELECT COUNT(*), SUM(valid_to IS NULL) FROM edges
                 WHERE epistemic_class = 'inference'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("edge versions");
        assert_eq!(counts, (2, 1));
        let graph = store.load_graph(&repository.id).expect("graph");
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].evidence[0].locator, "line:24");
    }

    #[test]
    fn invalid_replacement_rolls_back_without_damaging_current_inferences() {
        let (_directory, mut store, repository, commit) = setup();
        store
            .replace_type_inferences(
                &repository.id,
                &commit,
                "2026-09-04T00:00:01Z",
                "pyright",
                &[edge("line:12", "input-a")],
            )
            .expect("first inference");
        let mut invalid = edge("line:99", "input-invalid");
        invalid.fingerprint = "type-inference:invalid".to_owned();
        invalid.target = StableKey::new("db:missing").expect("missing target key");
        assert!(
            store
                .replace_type_inferences(
                    &repository.id,
                    &commit,
                    "2026-09-04T00:00:02Z",
                    "pyright",
                    &[invalid],
                )
                .is_err()
        );

        assert_eq!(row_counts(&store), (1, 1, 1, 1));
        let graph = store.load_graph(&repository.id).expect("graph");
        assert_eq!(graph.edges[0].evidence[0].locator, "line:12");
    }

    fn setup() -> (TempDir, SqliteStore, RepositoryDescriptor, CommitMetadata) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut store = SqliteStore::open(&directory.path().join("ctx.db"), directory.path())
            .expect("SQLite store");
        let repository = RepositoryDescriptor {
            id: RepositoryId::new("repo:type-inference").expect("repository"),
            root_path: directory.path().display().to_string(),
            remote_url: None,
        };
        let commit = CommitMetadata {
            oid: CommitOid::new("deadbeef").expect("commit"),
            parent_oid: None,
            authored_at: "2026-09-04T00:00:00Z".to_owned(),
        };
        store
            .ensure_repository(&repository, "2026-09-04T00:00:00Z")
            .expect("repository");
        store
            .apply_index(
                &repository.id,
                &commit,
                "2026-09-04T00:00:00Z",
                &node_plan(),
            )
            .expect("indexed nodes");
        (directory, store, repository, commit)
    }

    fn node_plan() -> IndexPlan {
        let source = StableKey::new("symbol:python:service.update:Function").expect("source");
        let target = StableKey::new("db:models").expect("target");
        IndexPlan {
            nodes_to_write: vec![
                PlannedNode {
                    stable_key: source,
                    kind: NodeKind::CodeSymbol,
                    name: "update".to_owned(),
                    content_hash: "body".to_owned(),
                    attributes: PlannedNodeAttributes::Symbol {
                        file_path: "service.py".to_owned(),
                        canonical_path: "service.update".to_owned(),
                        symbol_kind: SymbolKind::Function,
                        range: SourceRange {
                            start_byte: 0,
                            end_byte: 100,
                            start_line: 1,
                            end_line: 10,
                        },
                        signature: None,
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
                    stable_key: target,
                    kind: NodeKind::DbEntity,
                    name: "models".to_owned(),
                    content_hash: "db-entity:models".to_owned(),
                    attributes: PlannedNodeAttributes::Interaction {
                        identifier: "models".to_owned(),
                    },
                    mutation: NodeMutationKind::Create,
                },
            ],
            ..IndexPlan::default()
        }
    }

    fn edge(locator: &str, input: &str) -> TypeInferenceEdge {
        TypeInferenceEdge {
            source: StableKey::new("symbol:python:service.update:Function").expect("source"),
            target: StableKey::new("db:models").expect("target"),
            relation: RelationKind::WritesTo,
            confidence: Confidence::new(0.9).expect("confidence"),
            producer: "pyright".to_owned(),
            fingerprint: "type_inference:pyright:update:writes:models".to_owned(),
            source_uri: "service.py".to_owned(),
            input_fingerprint: input.to_owned(),
            evidence_locator: locator.to_owned(),
        }
    }

    fn row_counts(store: &SqliteStore) -> (i64, i64, i64, i64) {
        store
            .connection()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM edges WHERE epistemic_class = 'inference'),
                    (SELECT COUNT(*) FROM derivations WHERE edge_id IS NOT NULL),
                    (SELECT COUNT(*) FROM sources WHERE kind = 'typeinference'),
                    (SELECT COUNT(*) FROM evidence)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("inference row counts")
    }
}
