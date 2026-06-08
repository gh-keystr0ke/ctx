use std::collections::{BTreeMap, BTreeSet};

use ctx_app::ports::{BusinessContextStore, CommitMetadata, PortError};
use ctx_core::{
    business::{BusinessDocument, BusinessKind, ContextImportStats, ExplicitSymbolLink},
    domain::{CommitOid, RelationKind, RepositoryId, StableKey},
    indexing::PlannedNodeAttributes,
};
use rusqlite::{OptionalExtension, Transaction, params};

use super::SqliteStore;

#[derive(Clone, Debug)]
struct CurrentBusinessNode {
    content_hash: String,
    source_uri: String,
}

impl BusinessContextStore for SqliteStore {
    fn sync_context(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        indexed_at: &str,
        documents: &[BusinessDocument],
    ) -> Result<ContextImportStats, PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        let commit_row = commit_row(&transaction, repository_row, &commit.oid)?;
        let current = current_business_nodes(&transaction, repository_row)?;
        let desired = documents
            .iter()
            .map(BusinessDocument::stable_key)
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(domain_error)?;
        let mut stats = ContextImportStats::default();

        retire_removed_documents(
            &transaction,
            repository_row,
            commit_row,
            &current,
            &desired,
            &mut stats,
        )?;
        for document in documents {
            let stable_key = document.stable_key().map_err(domain_error)?;
            let previous = current.get(&stable_key);
            if previous.is_some_and(|node| node.content_hash == document.content_hash) {
                continue;
            }
            if let Some(node) = previous {
                close_document_edges(&transaction, repository_row, commit_row, &node.source_uri)?;
                stats.documents_versioned += 1;
            } else {
                stats.documents_created += 1;
            }
            persist_document(
                &transaction,
                repository_row,
                commit_row,
                document,
                &stable_key,
            )?;
        }

        let symbols = current_symbol_keys(&transaction, repository_row)?;
        let intents = current_intent_keys(&transaction, repository_row)?;
        for document in documents {
            persist_document_claims(
                &transaction,
                repository_row,
                commit_row,
                indexed_at,
                document,
                &symbols,
                &intents,
                &mut stats,
            )?;
        }
        stats.unresolved_symbols.sort();
        stats.unresolved_symbols.dedup();
        transaction.commit().map_err(database_error)?;
        Ok(stats)
    }
}

fn retire_removed_documents(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    current: &BTreeMap<StableKey, CurrentBusinessNode>,
    desired: &BTreeSet<StableKey>,
    stats: &mut ContextImportStats,
) -> Result<(), PortError> {
    for (stable_key, node) in current {
        if desired.contains(stable_key) {
            continue;
        }
        let node_row = required_node_row(transaction, repository_row, stable_key)?;
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
        close_document_edges(transaction, repository_row, commit_row, &node.source_uri)?;
        stats.documents_retired += 1;
    }
    Ok(())
}

fn persist_document(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    document: &BusinessDocument,
    stable_key: &StableKey,
) -> Result<(), PortError> {
    transaction
        .execute(
            "INSERT INTO nodes(repository_id, kind, stable_key, created_commit)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repository_id, kind, stable_key)
             DO UPDATE SET retired_commit = NULL",
            params![
                repository_row,
                business_kind(document.kind),
                stable_key.as_str(),
                commit_row
            ],
        )
        .map_err(database_error)?;
    let node_row = required_node_row(transaction, repository_row, stable_key)?;
    let attributes = serde_json::to_string(&PlannedNodeAttributes::Business {
        id: document.id.clone(),
        status: document.status.clone(),
        body: document.body.clone(),
        feature: document.feature.clone(),
        source_uri: document.source_uri.clone(),
    })
    .map_err(serialization_error)?;
    let current_from = transaction
        .query_row(
            "SELECT valid_from FROM node_versions
             WHERE node_id = ?1 AND valid_to IS NULL",
            [node_row],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(database_error)?;
    if current_from == Some(commit_row) {
        transaction
            .execute(
                "UPDATE node_versions
                 SET name = ?1, content_hash = ?2, attributes_json = ?3
                 WHERE node_id = ?4 AND valid_from = ?5",
                params![
                    document.title,
                    document.content_hash,
                    attributes,
                    node_row,
                    commit_row
                ],
            )
            .map_err(database_error)?;
        return Ok(());
    }
    transaction
        .execute(
            "UPDATE node_versions SET valid_to = ?1
             WHERE node_id = ?2 AND valid_to IS NULL",
            params![commit_row, node_row],
        )
        .map_err(database_error)?;
    transaction
        .execute(
            "INSERT INTO node_versions(node_id, valid_from, name, content_hash, attributes_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                node_row,
                commit_row,
                document.title,
                document.content_hash,
                attributes
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn persist_document_claims(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    indexed_at: &str,
    document: &BusinessDocument,
    symbols: &BTreeMap<String, Vec<StableKey>>,
    intents: &BTreeMap<String, StableKey>,
    stats: &mut ContextImportStats,
) -> Result<(), PortError> {
    let intent_key = document.stable_key().map_err(domain_error)?;
    for link in &document.implementation {
        persist_symbol_claim(
            transaction,
            repository_row,
            commit_row,
            indexed_at,
            document,
            link,
            symbols,
            &intent_key,
            document.kind.implementation_relation(),
            false,
            stats,
        )?;
    }
    for link in &document.tests {
        persist_symbol_claim(
            transaction,
            repository_row,
            commit_row,
            indexed_at,
            document,
            link,
            symbols,
            &intent_key,
            RelationKind::CoveredBy,
            true,
            stats,
        )?;
    }
    if let Some(feature_id) = &document.feature {
        if let Some(feature_key) = intents.get(feature_id) {
            create_claim_if_missing(
                transaction,
                repository_row,
                commit_row,
                indexed_at,
                document,
                &intent_key,
                feature_key,
                RelationKind::DependsOn,
                "feature",
                feature_id,
                stats,
            )?;
        } else {
            stats
                .unresolved_symbols
                .push(format!("feature:{feature_id}"));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn persist_symbol_claim(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    indexed_at: &str,
    document: &BusinessDocument,
    link: &ExplicitSymbolLink,
    symbols: &BTreeMap<String, Vec<StableKey>>,
    intent_key: &StableKey,
    relation: RelationKind,
    intent_is_source: bool,
    stats: &mut ContextImportStats,
) -> Result<(), PortError> {
    let Some(candidates) = symbols.get(&link.symbol) else {
        stats.unresolved_symbols.push(link.symbol.clone());
        return Ok(());
    };
    let [symbol_key] = candidates.as_slice() else {
        stats
            .unresolved_symbols
            .push(format!("ambiguous:{}", link.symbol));
        return Ok(());
    };
    let (source, target) = if intent_is_source {
        (intent_key, symbol_key)
    } else {
        (symbol_key, intent_key)
    };
    create_claim_if_missing(
        transaction,
        repository_row,
        commit_row,
        indexed_at,
        document,
        source,
        target,
        relation,
        &link.locator,
        &link.symbol,
        stats,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_claim_if_missing(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    indexed_at: &str,
    document: &BusinessDocument,
    source: &StableKey,
    target: &StableKey,
    relation: RelationKind,
    locator: &str,
    excerpt: &str,
    stats: &mut ContextImportStats,
) -> Result<(), PortError> {
    let fingerprint = format!(
        "explicit:{}:{relation:?}:{}:{}:{locator}",
        source.as_str(),
        target.as_str(),
        document.source_uri
    );
    let exists = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM edges
                WHERE repository_id = ?1 AND fingerprint = ?2 AND valid_to IS NULL
             )",
            params![repository_row, fingerprint],
            |row| row.get::<_, bool>(0),
        )
        .map_err(database_error)?;
    if exists {
        return Ok(());
    }
    let source_row = required_node_row(transaction, repository_row, source)?;
    let target_row = required_node_row(transaction, repository_row, target)?;
    let source_record = ensure_source(
        transaction,
        repository_row,
        commit_row,
        indexed_at,
        document,
    )?;
    transaction
        .execute(
            "INSERT INTO evidence(source_id, locator, excerpt_hash, strength, attributes_json)
             VALUES (?1, ?2, ?3, 1.0, '{}')",
            params![
                source_record,
                locator,
                blake3::hash(excerpt.as_bytes()).to_hex().to_string()
            ],
        )
        .map_err(database_error)?;
    let evidence_row = transaction.last_insert_rowid();
    transaction
        .execute(
            "INSERT INTO edges(
                repository_id, src_node_id, dst_node_id, kind, epistemic_class,
                provenance_kind, confidence, status, valid_from, producer, fingerprint
             ) VALUES (?1, ?2, ?3, ?4, 'assertion', 'documentation', 1.0,
                       'active', ?5, 'business_context_explicit', ?6)",
            params![
                repository_row,
                source_row,
                target_row,
                relation_kind(relation),
                commit_row,
                fingerprint
            ],
        )
        .map_err(database_error)?;
    let edge_row = transaction.last_insert_rowid();
    transaction
        .execute(
            "INSERT INTO edge_evidence(edge_id, evidence_id) VALUES (?1, ?2)",
            params![edge_row, evidence_row],
        )
        .map_err(database_error)?;
    stats.explicit_links_created += 1;
    Ok(())
}

fn ensure_source(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    indexed_at: &str,
    document: &BusinessDocument,
) -> Result<i64, PortError> {
    let existing = transaction
        .query_row(
            "SELECT id FROM sources
             WHERE repository_id = ?1 AND uri = ?2 AND content_hash = ?3
             ORDER BY id DESC LIMIT 1",
            params![repository_row, document.source_uri, document.content_hash],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(database_error)?;
    if let Some(source_row) = existing {
        return Ok(source_row);
    }
    let metadata = serde_json::json!({"document_id": document.id}).to_string();
    transaction
        .execute(
            "INSERT INTO sources(
                repository_id, kind, uri, commit_id, timestamp, content_hash, metadata_json
             ) VALUES (?1, 'documentation', ?2, ?3, ?4, ?5, ?6)",
            params![
                repository_row,
                document.source_uri,
                commit_row,
                indexed_at,
                document.content_hash,
                metadata
            ],
        )
        .map_err(database_error)?;
    Ok(transaction.last_insert_rowid())
}

fn close_document_edges(
    transaction: &Transaction<'_>,
    repository_row: i64,
    commit_row: i64,
    source_uri: &str,
) -> Result<(), PortError> {
    let mut statement = transaction
        .prepare(
            "SELECT DISTINCT e.id, e.valid_from
             FROM edges e
             JOIN edge_evidence ee ON ee.edge_id = e.id
             JOIN evidence ev ON ev.id = ee.evidence_id
             JOIN sources s ON s.id = ev.source_id
             WHERE e.repository_id = ?1 AND e.valid_to IS NULL AND s.uri = ?2",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map(params![repository_row, source_uri], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    drop(statement);
    for (edge_row, valid_from) in rows {
        if valid_from == commit_row {
            transaction
                .execute("DELETE FROM edge_evidence WHERE edge_id = ?1", [edge_row])
                .map_err(database_error)?;
            transaction
                .execute("DELETE FROM edges WHERE id = ?1", [edge_row])
                .map_err(database_error)?;
        } else {
            transaction
                .execute(
                    "UPDATE edges SET valid_to = ?1 WHERE id = ?2",
                    params![commit_row, edge_row],
                )
                .map_err(database_error)?;
        }
    }
    Ok(())
}

fn current_business_nodes(
    transaction: &Transaction<'_>,
    repository_row: i64,
) -> Result<BTreeMap<StableKey, CurrentBusinessNode>, PortError> {
    let mut statement = transaction
        .prepare(
            "SELECT n.stable_key, nv.content_hash, nv.attributes_json
             FROM nodes n
             JOIN node_versions nv ON nv.node_id = n.id AND nv.valid_to IS NULL
             WHERE n.repository_id = ?1 AND n.retired_commit IS NULL
               AND n.kind IN ('feature', 'requirement', 'invariant', 'decision')
             ORDER BY n.stable_key",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([repository_row], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(database_error)?;
    let mut result = BTreeMap::new();
    for row in rows {
        let (stable_key, content_hash, attributes) = row.map_err(database_error)?;
        let attributes: PlannedNodeAttributes =
            serde_json::from_str(&attributes).map_err(serialization_error)?;
        if let PlannedNodeAttributes::Business { source_uri, .. } = attributes {
            result.insert(
                StableKey::new(stable_key).map_err(domain_error)?,
                CurrentBusinessNode {
                    content_hash,
                    source_uri,
                },
            );
        }
    }
    Ok(result)
}

fn current_symbol_keys(
    transaction: &Transaction<'_>,
    repository_row: i64,
) -> Result<BTreeMap<String, Vec<StableKey>>, PortError> {
    let mut statement = transaction
        .prepare(
            "SELECT n.stable_key, nv.attributes_json
             FROM nodes n
             JOIN node_versions nv ON nv.node_id = n.id AND nv.valid_to IS NULL
             WHERE n.repository_id = ?1 AND n.kind = 'code_symbol'
               AND n.retired_commit IS NULL ORDER BY n.stable_key",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([repository_row], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(database_error)?;
    let mut symbols: BTreeMap<String, Vec<StableKey>> = BTreeMap::new();
    for row in rows {
        let (stable_key, attributes) = row.map_err(database_error)?;
        let attributes: PlannedNodeAttributes =
            serde_json::from_str(&attributes).map_err(serialization_error)?;
        if let PlannedNodeAttributes::Symbol { canonical_path, .. } = attributes {
            add_symbol_lookup(
                &mut symbols,
                canonical_path,
                StableKey::new(stable_key).map_err(domain_error)?,
            );
        }
    }
    Ok(symbols)
}

fn add_symbol_lookup(
    symbols: &mut BTreeMap<String, Vec<StableKey>>,
    canonical_path: String,
    stable_key: StableKey,
) {
    symbols
        .entry(canonical_path)
        .or_default()
        .push(stable_key.clone());
    symbols.insert(stable_key.as_str().to_owned(), vec![stable_key]);
}

fn current_intent_keys(
    transaction: &Transaction<'_>,
    repository_row: i64,
) -> Result<BTreeMap<String, StableKey>, PortError> {
    let current = current_business_nodes(transaction, repository_row)?;
    Ok(current
        .into_keys()
        .filter_map(|key| {
            key.as_str()
                .strip_prefix("intent:")
                .map(|id| (id.to_owned(), key.clone()))
        })
        .collect())
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

fn required_node_row(
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

const fn business_kind(kind: BusinessKind) -> &'static str {
    match kind {
        BusinessKind::Feature => "feature",
        BusinessKind::Requirement => "requirement",
        BusinessKind::Invariant => "invariant",
        BusinessKind::Decision => "decision",
    }
}

const fn relation_kind(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Implements => "implements",
        RelationKind::Enforces => "enforces",
        RelationKind::CoveredBy => "coveredby",
        RelationKind::DependsOn => "dependson",
        RelationKind::Satisfies => "satisfies",
        RelationKind::Contains => "contains",
        RelationKind::Calls => "calls",
        RelationKind::References => "references",
        RelationKind::ReadsFrom => "readsfrom",
        RelationKind::WritesTo => "writesto",
        RelationKind::Emits => "emits",
        RelationKind::Handles => "handles",
    }
}

#[allow(clippy::needless_pass_by_value)]
fn database_error(error: rusqlite::Error) -> PortError {
    PortError::new(format!("SQLite context operation failed: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn serialization_error(error: serde_json::Error) -> PortError {
    PortError::new(format!("stored context data is invalid: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn domain_error(error: ctx_core::domain::InvalidIdentifier) -> PortError {
    PortError::new(format!("business context identifier is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use ctx_app::ports::{GraphStore, IndexStore, RepositoryDescriptor, VerificationStore};
    use ctx_core::{
        business::ExplicitSymbolLink,
        domain::NodeKind,
        explain::explain,
        indexing::{IndexPlan, NodeMutationKind, PlannedNode},
        ir::{SourceRange, SymbolKind},
        verification::{ResolutionScore, SemanticCandidate, VerificationDecision},
    };
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn language_qualified_stable_keys_disambiguate_equal_canonical_paths() {
        let python = StableKey::new("symbol:python:app.run:Function").expect("Python stable key");
        let rust = StableKey::new("symbol:rust:app.run:Function").expect("Rust stable key");
        let mut symbols = BTreeMap::new();

        add_symbol_lookup(&mut symbols, "app.run".to_owned(), python.clone());
        add_symbol_lookup(&mut symbols, "app.run".to_owned(), rust.clone());

        assert_eq!(symbols["app.run"], vec![python.clone(), rust.clone()]);
        assert_eq!(symbols[python.as_str()], vec![python]);
        assert_eq!(symbols[rust.as_str()], vec![rust]);
    }

    #[test]
    fn explicit_links_persist_an_explainable_evidence_chain() {
        let directory = tempdir().expect("temporary directory");
        let mut store = SqliteStore::open(&directory.path().join("ctx.db")).expect("database");
        let repository = RepositoryDescriptor {
            id: RepositoryId::new("repo:test").expect("repository ID"),
            root_path: "/repo".to_owned(),
            remote_url: None,
        };
        let commit = CommitMetadata {
            oid: CommitOid::new("abcdef12").expect("commit"),
            parent_oid: None,
            authored_at: "2026-08-17T00:00:00Z".to_owned(),
        };
        store
            .ensure_repository(&repository, "2026-08-17T00:00:00Z")
            .expect("repository");
        let symbol_key =
            StableKey::new("symbol:python:billing.cancel:Function").expect("symbol stable key");
        let plan = IndexPlan {
            nodes_to_write: vec![PlannedNode {
                stable_key: symbol_key,
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
                },
                mutation: NodeMutationKind::Create,
            }],
            ..IndexPlan::default()
        };
        store
            .apply_index(&repository.id, &commit, "2026-08-17T00:00:00Z", &plan)
            .expect("index");
        let document = BusinessDocument {
            id: "REQ-SUB-014".to_owned(),
            kind: BusinessKind::Requirement,
            title: "Keep access".to_owned(),
            body: "Keep access until paid_until".to_owned(),
            status: "active".to_owned(),
            feature: None,
            implementation: vec![ExplicitSymbolLink {
                symbol: "billing.cancel".to_owned(),
                locator: "implementation[0]".to_owned(),
            }],
            tests: Vec::new(),
            source_uri: ".context/requirements/cancel.yaml".to_owned(),
            content_hash: "document".to_owned(),
        };
        let stats = store
            .sync_context(&repository.id, &commit, "2026-08-17T00:00:00Z", &[document])
            .expect("context");

        assert_eq!(stats.documents_created, 1);
        assert_eq!(stats.explicit_links_created, 1);
        let evidence_chains: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM edges e
                 JOIN edge_evidence ee ON ee.edge_id = e.id
                 JOIN evidence ev ON ev.id = ee.evidence_id
                 JOIN sources s ON s.id = ev.source_id
                 WHERE e.kind = 'implements' AND s.kind = 'documentation'",
                [],
                |row| row.get(0),
            )
            .expect("evidence count");
        assert_eq!(evidence_chains, 1);
        let graph = store.load_graph(&repository.id).expect("graph");
        let explanation = explain("billing.cancel -> REQ-SUB-014", &graph).expect("explanation");
        assert_eq!(explanation.claims.len(), 1);
        assert_eq!(explanation.claims[0].evidence.len(), 1);
        assert_acceptance_preserves_inference(&mut store, &repository.id, &commit);
    }

    fn assert_acceptance_preserves_inference(
        store: &mut SqliteStore,
        repository: &RepositoryId,
        commit: &CommitMetadata,
    ) {
        let candidate = SemanticCandidate {
            fingerprint: "candidate:cancel:enforces:req".to_owned(),
            source: StableKey::new("symbol:python:billing.cancel:Function").expect("source key"),
            source_identifier: "billing.cancel".to_owned(),
            target: StableKey::new("intent:REQ-SUB-014").expect("target key"),
            target_identifier: "REQ-SUB-014".to_owned(),
            relation: RelationKind::Enforces,
            score: ResolutionScore {
                lexical: 0.75,
                structural: 1.0,
                total: 0.8,
                ..ResolutionScore::default()
            },
            evidence: vec!["lexical signal 0.75".to_owned()],
            impact_priority: 3,
        };
        store
            .record_verification(
                repository,
                commit,
                &candidate,
                VerificationDecision::Accept,
                "reviewer@example.test",
                "2026-08-17T00:01:00Z",
            )
            .expect("verification");
        let preserved_inferences: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM edges
                 WHERE epistemic_class = 'inference' AND valid_to IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .expect("inference count");
        let human_assertions: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM edges
                 WHERE epistemic_class = 'assertion' AND provenance_kind = 'human'
                   AND valid_to IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("assertion count");
        assert_eq!(preserved_inferences, 1);
        assert_eq!(human_assertions, 1);
        assert_rejection_is_durable(store, repository, commit, candidate);
    }

    fn assert_rejection_is_durable(
        store: &mut SqliteStore,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        candidate: SemanticCandidate,
    ) {
        let mut rejected = candidate;
        rejected.fingerprint = "candidate:cancel:satisfies:req".to_owned();
        rejected.relation = RelationKind::Satisfies;
        store
            .record_verification(
                repository,
                commit,
                &rejected,
                VerificationDecision::Reject,
                "reviewer@example.test",
                "2026-08-17T00:02:00Z",
            )
            .expect("rejection");
        let durable_rejections: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM edges e
                 JOIN annotations a ON a.edge_id = e.id
                 WHERE e.status = 'rejected' AND a.action = 'reject'",
                [],
                |row| row.get(0),
            )
            .expect("rejection count");
        assert_eq!(durable_rejections, 1);
    }
}
