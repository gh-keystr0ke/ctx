use ctx_app::ports::{CommitMetadata, PortError, StaleClaimStore, VerificationStore};
use ctx_core::{
    domain::{CommitOid, RelationKind, RepositoryId, StableKey},
    verification::{SemanticCandidate, VerificationDecision},
};
use rusqlite::{OptionalExtension, Transaction, params};

use super::SqliteStore;

impl StaleClaimStore for SqliteStore {
    fn reactivate_stale_claim(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        fingerprint: &str,
        reviewer: &str,
        reasoning: &str,
        timestamp: &str,
    ) -> Result<bool, PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        // Confirms the caller is operating at a real indexed commit rather
        // than a stray one; the row id itself isn't otherwise needed since
        // reactivation flips status in place instead of versioning the edge.
        let _ = commit_row(&transaction, repository_row, &commit.oid)?;
        let edge_row: Option<i64> = transaction
            .query_row(
                "SELECT id FROM edges
                 WHERE repository_id = ?1 AND fingerprint = ?2
                   AND valid_to IS NULL AND status = 'stale'",
                params![repository_row, fingerprint],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        let Some(edge_row) = edge_row else {
            transaction.commit().map_err(database_error)?;
            return Ok(false);
        };
        transaction
            .execute(
                "UPDATE edges SET status = 'active', stale_reason = NULL WHERE id = ?1",
                params![edge_row],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO annotations(edge_id, action, author, comment, created_at)
                 VALUES (?1, 'confirm', ?2, ?3, ?4)",
                params![
                    edge_row,
                    reviewer,
                    format!("stale claim reactivated: {reasoning}"),
                    timestamp
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(true)
    }
}

impl VerificationStore for SqliteStore {
    fn record_verification(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        candidate: &SemanticCandidate,
        decision: VerificationDecision,
        author: &str,
        timestamp: &str,
    ) -> Result<(), PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        let commit_row = commit_row(&transaction, repository_row, &commit.oid)?;
        let inference_edge = persist_inference(
            &transaction,
            repository_row,
            commit_row,
            candidate,
            decision,
            timestamp,
        )?;
        transaction
            .execute(
                "INSERT INTO annotations(edge_id, action, author, comment, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    inference_edge,
                    decision_name(decision),
                    author,
                    format!("candidate {}", candidate.fingerprint),
                    timestamp
                ],
            )
            .map_err(database_error)?;
        if decision == VerificationDecision::Accept {
            persist_human_assertion(
                &transaction,
                repository_row,
                commit_row,
                candidate,
                inference_edge,
                author,
                timestamp,
            )?;
        }
        transaction.commit().map_err(database_error)
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_inference(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    candidate: &SemanticCandidate,
    decision: VerificationDecision,
    timestamp: &str,
) -> Result<i64, PortError> {
    let serialized = serde_json::to_string(candidate).map_err(serialization_error)?;
    let content_hash = blake3::hash(serialized.as_bytes()).to_hex().to_string();
    transaction
        .execute(
            "INSERT INTO sources(
                repository_id, kind, uri, commit_id, timestamp, content_hash, metadata_json
             ) VALUES (?1, 'staticanalysis', ?2, ?3, ?4, ?5, ?6)",
            params![
                repository_row,
                format!("ctx://heuristic/{}", candidate.fingerprint),
                commit_row,
                timestamp,
                content_hash,
                serialized
            ],
        )
        .map_err(database_error)?;
    let source_record = transaction.last_insert_rowid();
    let source_node = node_row(transaction, repository_row, &candidate.source)?;
    let target_node = node_row(transaction, repository_row, &candidate.target)?;
    let edge_fingerprint = format!("inference:{}", candidate.fingerprint);
    transaction
        .execute(
            "INSERT INTO edges(
                repository_id, src_node_id, dst_node_id, kind, epistemic_class,
                provenance_kind, confidence, status, valid_from, valid_to,
                producer, fingerprint, stale_reason
             ) VALUES (?1, ?2, ?3, ?4, 'inference', 'staticanalysis', ?5, ?6,
                       ?7, ?8, 'heuristic_semantic_resolver', ?9, ?10)",
            params![
                repository_row,
                source_node,
                target_node,
                relation_name(candidate.relation),
                f64::from(candidate.score.total),
                if decision == VerificationDecision::Reject {
                    "rejected"
                } else {
                    "active"
                },
                commit_row,
                (decision == VerificationDecision::Accept).then_some(commit_row),
                edge_fingerprint,
                (decision == VerificationDecision::Accept)
                    .then_some("superseded_by_human_assertion")
            ],
        )
        .map_err(database_error)?;
    let edge_row = transaction.last_insert_rowid();
    for (index, item) in candidate.evidence.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO evidence(
                    source_id, locator, excerpt_hash, strength, attributes_json
                 ) VALUES (?1, ?2, ?3, ?4, '{}')",
                params![
                    source_record,
                    format!("signal[{index}]"),
                    blake3::hash(item.as_bytes()).to_hex().to_string(),
                    f64::from(candidate.score.total)
                ],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO edge_evidence(edge_id, evidence_id) VALUES (?1, ?2)",
                params![edge_row, transaction.last_insert_rowid()],
            )
            .map_err(database_error)?;
    }
    transaction
        .execute(
            "INSERT INTO derivations(edge_id, producer, source_uri, input_fingerprint)
             VALUES (?1, 'heuristic_semantic_resolver', ?2, ?3)",
            params![
                edge_row,
                format!("ctx://heuristic/{}", candidate.fingerprint),
                content_hash
            ],
        )
        .map_err(database_error)?;
    Ok(edge_row)
}

#[allow(clippy::too_many_arguments)]
fn persist_human_assertion(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    candidate: &SemanticCandidate,
    inference_edge: i64,
    author: &str,
    timestamp: &str,
) -> Result<(), PortError> {
    let metadata = serde_json::json!({
        "candidate": candidate.fingerprint,
        "based_on_inference_edge": inference_edge
    })
    .to_string();
    let content_hash = blake3::hash(metadata.as_bytes()).to_hex().to_string();
    transaction
        .execute(
            "INSERT INTO sources(
                repository_id, kind, uri, commit_id, author, timestamp,
                content_hash, metadata_json
             ) VALUES (?1, 'human', ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                repository_row,
                format!("ctx://verification/{}", candidate.fingerprint),
                commit_row,
                author,
                timestamp,
                content_hash,
                metadata
            ],
        )
        .map_err(database_error)?;
    let source_record = transaction.last_insert_rowid();
    transaction
        .execute(
            "INSERT INTO evidence(source_id, locator, excerpt_hash, strength, attributes_json)
             VALUES (?1, ?2, ?3, 1.0, ?4)",
            params![
                source_record,
                candidate.fingerprint,
                blake3::hash(candidate.fingerprint.as_bytes())
                    .to_hex()
                    .to_string(),
                serde_json::json!({"based_on_inference_edge": inference_edge}).to_string()
            ],
        )
        .map_err(database_error)?;
    let evidence_row = transaction.last_insert_rowid();
    let source_node = node_row(transaction, repository_row, &candidate.source)?;
    let target_node = node_row(transaction, repository_row, &candidate.target)?;
    transaction
        .execute(
            "INSERT INTO edges(
                repository_id, src_node_id, dst_node_id, kind, epistemic_class,
                provenance_kind, confidence, status, valid_from, producer, fingerprint
             ) VALUES (?1, ?2, ?3, ?4, 'assertion', 'human', 1.0, 'active',
                       ?5, 'human_verification', ?6)",
            params![
                repository_row,
                source_node,
                target_node,
                relation_name(candidate.relation),
                commit_row,
                format!(
                    "human:{}:{:?}:{}",
                    candidate.source, candidate.relation, candidate.target
                )
            ],
        )
        .map_err(database_error)?;
    transaction
        .execute(
            "INSERT INTO edge_evidence(edge_id, evidence_id) VALUES (?1, ?2)",
            params![transaction.last_insert_rowid(), evidence_row],
        )
        .map_err(database_error)?;
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
        .map_err(database_error)
}

fn node_row(
    transaction: &Transaction<'_>,
    repository_row: i64,
    stable_key: &StableKey,
) -> Result<i64, PortError> {
    transaction
        .query_row(
            "SELECT id FROM nodes WHERE repository_id = ?1 AND stable_key = ?2",
            params![repository_row, stable_key.as_str()],
            |row| row.get(0),
        )
        .map_err(database_error)
}

const fn relation_name(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Implements => "implements",
        RelationKind::Enforces => "enforces",
        RelationKind::Satisfies => "satisfies",
        RelationKind::CoveredBy => "coveredby",
        RelationKind::DependsOn => "dependson",
        RelationKind::Contains => "contains",
        RelationKind::Calls => "calls",
        RelationKind::References => "references",
        RelationKind::ReadsFrom => "readsfrom",
        RelationKind::WritesTo => "writesto",
        RelationKind::DefinesSchema => "definesschema",
        RelationKind::Exposes => "exposes",
        RelationKind::CallsExternal => "callsexternal",
        RelationKind::Emits => "emits",
        RelationKind::Handles => "handles",
    }
}

const fn decision_name(decision: VerificationDecision) -> &'static str {
    match decision {
        VerificationDecision::Accept => "confirm",
        VerificationDecision::Reject => "reject",
    }
}

#[allow(clippy::needless_pass_by_value)]
fn database_error(error: rusqlite::Error) -> PortError {
    PortError::new(format!("SQLite verification operation failed: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn serialization_error(error: serde_json::Error) -> PortError {
    PortError::new(format!(
        "verification candidate could not be serialized: {error}"
    ))
}

#[cfg(test)]
mod stale_claim_tests {
    use ctx_app::ports::{BusinessContextStore, GraphStore, IndexStore, RepositoryDescriptor};
    use ctx_core::{
        business::{BusinessDocument, BusinessKind, ExplicitSymbolLink, Visibility},
        domain::{ClaimStatus, NodeKind},
        indexing::{IndexPlan, NodeMutationKind, PlannedNode, PlannedNodeAttributes},
        ir::{SourceRange, SymbolKind},
    };
    use tempfile::tempdir;

    use super::*;

    /// The returned `TempDir` must stay alive as long as the store is used
    /// -- dropping it removes the backing database file.
    fn store_with_a_stale_implements_claim() -> (
        tempfile::TempDir,
        SqliteStore,
        RepositoryId,
        CommitMetadata,
        String,
    ) {
        let directory = tempdir().expect("temporary directory");
        let mut store = SqliteStore::open(&directory.path().join("ctx.db"), directory.path())
            .expect("database");
        let repository = RepositoryDescriptor {
            id: RepositoryId::new("repo:stale-claim").expect("repository ID"),
            root_path: "/repo".to_owned(),
            remote_url: None,
        };
        let commit = CommitMetadata {
            oid: CommitOid::new("cccccccc").expect("commit"),
            parent_oid: None,
            authored_at: "2026-08-27T00:00:00Z".to_owned(),
        };
        store
            .ensure_repository(&repository, "2026-08-27T00:00:00Z")
            .expect("repository");
        let symbol_key =
            StableKey::new("symbol:python:billing.cancel:Function").expect("symbol stable key");
        let plan = IndexPlan {
            nodes_to_write: vec![PlannedNode {
                stable_key: symbol_key.clone(),
                kind: NodeKind::CodeSymbol,
                name: "cancel".to_owned(),
                content_hash: "body".to_owned(),
                attributes: PlannedNodeAttributes::Symbol {
                    file_path: "billing.py".to_owned(),
                    canonical_path: "billing.cancel".to_owned(),
                    symbol_kind: SymbolKind::Function,
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
                mutation: NodeMutationKind::Create,
            }],
            ..IndexPlan::default()
        };
        store
            .apply_index(&repository.id, &commit, "2026-08-27T00:00:00Z", &plan)
            .expect("index");
        let document = BusinessDocument {
            id: "REQ-SUB-014".to_owned(),
            kind: BusinessKind::Requirement,
            title: "Keep access".to_owned(),
            body: "Keep access until paid_until".to_owned(),
            status: "active".to_owned(),
            implementation_expected: true,
            visibility: Visibility::Public,
            feature: None,
            implementation: vec![ExplicitSymbolLink {
                symbol: "billing.cancel".to_owned(),
                locator: "implementation[0]".to_owned(),
            }],
            tests: Vec::new(),
            source_uri: ".context/requirements/cancel.yaml".to_owned(),
            content_hash: "document".to_owned(),
        };
        store
            .sync_context(&repository.id, &commit, "2026-08-27T00:00:00Z", &[document])
            .expect("context");

        // Marks the just-created Implements edge stale the same way real
        // reindexing would once `billing.cancel`'s shape changed -- this
        // field is exactly what the real planner populates; setting it
        // directly here exercises the same `mark_semantic_edges_stale` path
        // without needing a second full reanalysis.
        store
            .apply_index(
                &repository.id,
                &commit,
                "2026-08-27T00:00:01Z",
                &IndexPlan {
                    semantic_sources_to_mark_stale: vec![symbol_key],
                    ..IndexPlan::default()
                },
            )
            .expect("mark stale");

        let fingerprint: String = store
            .connection()
            .query_row(
                "SELECT fingerprint FROM edges WHERE kind = 'implements' AND status = 'stale'",
                [],
                |row| row.get(0),
            )
            .expect("stale edge fingerprint");

        (directory, store, repository.id, commit, fingerprint)
    }

    #[test]
    fn reactivates_the_specific_stale_edge_and_records_an_audit_annotation() {
        let (_directory, mut store, repository, commit, fingerprint) =
            store_with_a_stale_implements_claim();

        let reactivated = store
            .reactivate_stale_claim(
                &repository,
                &commit,
                &fingerprint,
                "claude-code",
                "billing.cancel still implements REQ-SUB-014 as of the current code",
                "2026-08-27T00:00:02Z",
            )
            .expect("reactivation");

        assert!(reactivated);
        let graph = store.load_graph(&repository).expect("graph");
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.fingerprint == fingerprint)
            .expect("reactivated edge");
        assert_eq!(edge.status, ClaimStatus::Active);
        assert_eq!(edge.stale_reason, None);
        let annotation: (String, String, String) = store
            .connection()
            .query_row(
                "SELECT action, author, comment FROM annotations WHERE edge_id = (
                    SELECT id FROM edges WHERE fingerprint = ?1
                )",
                [&fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("audit annotation");
        assert_eq!(annotation.0, "confirm");
        assert_eq!(annotation.1, "claude-code");
        assert!(annotation.2.contains("still implements REQ-SUB-014"));
    }

    #[test]
    fn reactivating_an_unknown_fingerprint_is_reported_honestly_rather_than_erroring() {
        let (_directory, mut store, repository, commit, _fingerprint) =
            store_with_a_stale_implements_claim();

        let reactivated = store
            .reactivate_stale_claim(
                &repository,
                &commit,
                "explicit:does-not-exist",
                "claude-code",
                "reasoning",
                "2026-08-27T00:00:02Z",
            )
            .expect("reactivation call");

        assert!(!reactivated);
    }
}
