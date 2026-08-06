use ctx_app::ports::{CommitMetadata, PortError, VerificationStore};
use ctx_core::{
    domain::{CommitOid, RelationKind, RepositoryId, StableKey},
    verification::{SemanticCandidate, VerificationDecision},
};
use rusqlite::{Transaction, params};

use super::SqliteStore;

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
