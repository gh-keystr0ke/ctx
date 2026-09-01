use ctx_app::ports::{
    ArtifactLinkStore, ArtifactMaintenanceStore, ArtifactReconcileReport, ArtifactRepository,
    IngestCursorStore, KnowledgeCandidateStore, PortError,
};
use ctx_core::{
    artifact::{
        Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkKind,
        ArtifactLinkTarget, ArtifactProvider, ArtifactRef,
    },
    domain::{RepositoryId, StableKey},
    knowledge::{KnowledgeCandidate, KnowledgeDecision},
};
use rusqlite::{OptionalExtension, Transaction, params};

use super::SqliteStore;
use crate::candidate_queue;

impl ArtifactRepository for SqliteStore {
    fn upsert_artifact(
        &mut self,
        repository: &RepositoryId,
        artifact: &Artifact,
        ingested_at: &str,
        ingest_version: &str,
    ) -> Result<(), PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        transaction
            .execute(
                "INSERT INTO artifacts(
                    repository_id, provider, kind, external_id, project, title, body,
                    author, external_created_at, external_updated_at, source_locator,
                    content_hash, ingested_at, ingest_version
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(repository_id, provider, kind, external_id) DO UPDATE SET
                    project = excluded.project,
                    title = excluded.title,
                    body = excluded.body,
                    author = excluded.author,
                    external_created_at = excluded.external_created_at,
                    external_updated_at = excluded.external_updated_at,
                    source_locator = excluded.source_locator,
                    content_hash = excluded.content_hash,
                    ingested_at = excluded.ingested_at,
                    ingest_version = excluded.ingest_version",
                params![
                    repository_row,
                    provider_str(artifact.identity.provider),
                    artifact_kind_str(artifact.identity.kind),
                    artifact.identity.external_id,
                    artifact.project.as_str(),
                    artifact.title,
                    artifact.body,
                    artifact.author,
                    artifact
                        .external_created_at
                        .as_ref()
                        .map(ctx_core::domain::Timestamp::as_str),
                    artifact
                        .external_updated_at
                        .as_ref()
                        .map(ctx_core::domain::Timestamp::as_str),
                    artifact.source_locator.as_str(),
                    artifact.content_hash,
                    ingested_at,
                    ingest_version,
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    fn list_artifacts(&self, repository: &RepositoryId) -> Result<Vec<Artifact>, PortError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT a.provider, a.kind, a.external_id, a.project, a.title, a.body,
                        a.author, a.external_created_at, a.external_updated_at,
                        a.source_locator, a.content_hash
                 FROM artifacts a
                 JOIN repositories r ON r.id = a.repository_id
                 WHERE r.stable_id = ?1
                 ORDER BY a.provider, a.kind, a.external_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([repository.as_str()], row_to_artifact_columns)
            .map_err(database_error)?;
        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(artifact_from_columns(row.map_err(database_error)?)?);
        }
        Ok(artifacts)
    }

    fn mark_analyzed(
        &mut self,
        repository: &RepositoryId,
        identity: &ArtifactIdentity,
        content_hash: &str,
        input_fingerprint: &str,
        analyzed_at: &str,
    ) -> Result<(), PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        let Some(artifact_row) = artifact_row(&transaction, repository_row, identity)? else {
            return Err(PortError::new(format!(
                "artifact '{}:{:?}:{}' is not stored yet",
                provider_str(identity.provider),
                identity.kind,
                identity.external_id
            )));
        };
        transaction
            .execute(
                "INSERT INTO artifact_analysis(
                    artifact_id, content_hash, analyzed_at, input_fingerprint
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(artifact_id) DO UPDATE SET
                    content_hash = excluded.content_hash,
                    analyzed_at = excluded.analyzed_at,
                    input_fingerprint = excluded.input_fingerprint",
                params![artifact_row, content_hash, analyzed_at, input_fingerprint],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    fn analyzed_input_fingerprints(
        &self,
        repository: &RepositoryId,
    ) -> Result<std::collections::HashMap<ArtifactIdentity, String>, PortError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT a.provider, a.kind, a.external_id,
                        COALESCE(aa.input_fingerprint, aa.content_hash)
                 FROM artifact_analysis aa
                 JOIN artifacts a ON a.id = aa.artifact_id
                 JOIN repositories r ON r.id = a.repository_id
                 WHERE r.stable_id = ?1",
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
        let mut hashes = std::collections::HashMap::new();
        for row in rows {
            let (provider, kind, external_id, content_hash) = row.map_err(database_error)?;
            hashes.insert(
                ArtifactIdentity {
                    provider: parse_provider(&provider)?,
                    kind: parse_artifact_kind(&kind)?,
                    external_id,
                },
                content_hash,
            );
        }
        Ok(hashes)
    }
}

impl ArtifactLinkStore for SqliteStore {
    fn persist_links(
        &mut self,
        repository: &RepositoryId,
        links: &[ArtifactLink],
    ) -> Result<(), PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        persist_link_rows(&transaction, repository_row, links)?;
        transaction.commit().map_err(database_error)
    }

    fn list_links(&self, repository: &RepositoryId) -> Result<Vec<ArtifactLink>, PortError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT
                    source.provider, source.kind, source.external_id,
                    target_artifact.provider, target_artifact.kind, target_artifact.external_id,
                    target_node.stable_key,
                    al.kind, al.evidence_locator
                 FROM artifact_links al
                 JOIN artifacts source ON source.id = al.source_artifact_id
                 JOIN repositories r ON r.id = al.repository_id
                 LEFT JOIN artifacts target_artifact ON target_artifact.id = al.target_artifact_id
                 LEFT JOIN nodes target_node ON target_node.id = al.target_node_id
                 WHERE r.stable_id = ?1
                 ORDER BY al.id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([repository.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .map_err(database_error)?;
        let mut links = Vec::new();
        for row in rows {
            let (
                source_provider,
                source_kind,
                source_external_id,
                target_provider,
                target_kind,
                target_external_id,
                target_stable_key,
                kind,
                evidence_locator,
            ) = row.map_err(database_error)?;
            let source = ArtifactIdentity {
                provider: parse_provider(&source_provider)?,
                kind: parse_artifact_kind(&source_kind)?,
                external_id: source_external_id,
            };
            let target = if let Some(stable_key) = target_stable_key {
                ArtifactLinkTarget::CodeSymbol(StableKey::new(stable_key).map_err(domain_error)?)
            } else {
                let (Some(provider), Some(kind), Some(external_id)) =
                    (target_provider, target_kind, target_external_id)
                else {
                    return Err(PortError::new(
                        "artifact link has neither a target artifact nor a target node",
                    ));
                };
                ArtifactLinkTarget::Artifact(ArtifactIdentity {
                    provider: parse_provider(&provider)?,
                    kind: parse_artifact_kind(&kind)?,
                    external_id,
                })
            };
            links.push(ArtifactLink {
                source,
                target,
                kind: parse_link_kind(&kind)?,
                evidence_locator,
            });
        }
        Ok(links)
    }
}

impl ArtifactMaintenanceStore for SqliteStore {
    fn replace_outgoing_links(
        &mut self,
        repository: &RepositoryId,
        source: &ArtifactIdentity,
        links: &[ArtifactLink],
    ) -> Result<(), PortError> {
        if links.iter().any(|link| link.source != *source) {
            return Err(PortError::new(
                "replacement link batch contains a different source artifact",
            ));
        }
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        let Some(source_row) = artifact_row(&transaction, repository_row, source)? else {
            return Err(PortError::new(format!(
                "artifact '{}:{:?}:{}' is not stored yet",
                provider_str(source.provider),
                source.kind,
                source.external_id
            )));
        };
        transaction
            .execute(
                "DELETE FROM artifact_links
                 WHERE repository_id = ?1 AND source_artifact_id = ?2",
                params![repository_row, source_row],
            )
            .map_err(database_error)?;
        persist_link_rows(&transaction, repository_row, links)?;
        transaction.commit().map_err(database_error)
    }

    fn reconcile_snapshot(
        &mut self,
        repository: &RepositoryId,
        provider: ArtifactProvider,
        kinds: &[ArtifactKind],
        current: &std::collections::HashSet<ArtifactIdentity>,
    ) -> Result<ArtifactReconcileReport, PortError> {
        if kinds.is_empty() {
            return Ok(ArtifactReconcileReport::default());
        }
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        let stored = artifacts_for_provider(&transaction, repository_row, provider)?;
        let removed: Vec<_> = stored
            .into_iter()
            .filter(|identity| kinds.contains(&identity.kind) && !current.contains(identity))
            .collect();
        delete_artifact_rows(&transaction, repository_row, &removed)?;
        transaction.commit().map_err(database_error)?;
        Ok(ArtifactReconcileReport { removed })
    }

    fn delete_artifacts(
        &mut self,
        repository: &RepositoryId,
        identities: &[ArtifactIdentity],
    ) -> Result<ArtifactReconcileReport, PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        let removed = delete_artifact_rows(&transaction, repository_row, identities)?;
        transaction.commit().map_err(database_error)?;
        Ok(ArtifactReconcileReport { removed })
    }
}

/// Backed by the git-tracked `.ctx-candidates/` file queue (`ADR-EXT-004`),
/// not this struct's SQL connection -- every method below ignores
/// `repository`, since one checkout root implies exactly one
/// `.ctx-candidates/` directory. See `crate::candidate_queue` for the
/// actual read/write logic.
impl KnowledgeCandidateStore for SqliteStore {
    fn upsert_candidates(
        &mut self,
        _repository: &RepositoryId,
        candidates: &[KnowledgeCandidate],
    ) -> Result<(), PortError> {
        candidate_queue::upsert(self.root(), candidates)
    }

    fn pending_candidates(
        &self,
        _repository: &RepositoryId,
    ) -> Result<Vec<KnowledgeCandidate>, PortError> {
        candidate_queue::pending(self.root())
    }

    fn record_decision(
        &mut self,
        _repository: &RepositoryId,
        fingerprint: &str,
        decision: &KnowledgeDecision,
        author: &str,
        timestamp: &str,
    ) -> Result<(), PortError> {
        candidate_queue::record_decision(self.root(), fingerprint, decision, author, timestamp)
    }

    fn accepted_evidence(
        &self,
        _repository: &RepositoryId,
    ) -> Result<std::collections::BTreeMap<String, Vec<ArtifactRef>>, PortError> {
        candidate_queue::accepted_evidence(self.root())
    }

    fn accepted_record_for_document(
        &self,
        _repository: &RepositoryId,
        document_id: &str,
    ) -> Result<Option<ctx_core::knowledge::AcceptedKnowledgeRecord>, PortError> {
        candidate_queue::accepted_record_for_document(self.root(), document_id)
    }
}

impl IngestCursorStore for SqliteStore {
    fn sync_cursor(
        &self,
        repository: &RepositoryId,
        provider: &str,
    ) -> Result<Option<String>, PortError> {
        self.connection
            .query_row(
                "SELECT ic.cursor
                 FROM ingest_cursors ic
                 JOIN repositories r ON r.id = ic.repository_id
                 WHERE r.stable_id = ?1 AND ic.provider = ?2",
                params![repository.as_str(), provider],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)
    }

    fn set_sync_cursor(
        &mut self,
        repository: &RepositoryId,
        provider: &str,
        cursor: &str,
    ) -> Result<(), PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        let repository_row = repository_row(&transaction, repository)?;
        transaction
            .execute(
                "INSERT INTO ingest_cursors(repository_id, provider, cursor, updated_at)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(repository_id, provider) DO UPDATE SET
                    cursor = excluded.cursor,
                    updated_at = excluded.updated_at",
                params![repository_row, provider, cursor],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }
}

fn persist_link_rows(
    transaction: &Transaction<'_>,
    repository_row: i64,
    links: &[ArtifactLink],
) -> Result<(), PortError> {
    for link in links {
        let Some(source_row) = artifact_row(transaction, repository_row, &link.source)? else {
            // The source artifact isn't stored yet (out-of-order ingestion);
            // skip rather than abort the whole batch, matching this project's
            // "one bad reference never blocks the rest" policy.
            continue;
        };
        let (target_artifact_row, target_node_row) = match &link.target {
            ArtifactLinkTarget::Artifact(identity) => {
                let Some(row) = artifact_row(transaction, repository_row, identity)? else {
                    continue;
                };
                (Some(row), None)
            }
            ArtifactLinkTarget::CodeSymbol(stable_key) => {
                let Some(row) = node_row(transaction, repository_row, stable_key)? else {
                    continue;
                };
                (None, Some(row))
            }
        };
        // SQLite's UNIQUE indexes never consider two NULLs equal, and exactly
        // one target is NULL. Use a NULL-safe existence check for idempotency.
        let existing_row: Option<i64> = transaction
            .query_row(
                "SELECT id FROM artifact_links
                 WHERE repository_id = ?1 AND source_artifact_id = ?2
                   AND target_artifact_id IS ?3 AND target_node_id IS ?4 AND kind = ?5",
                params![
                    repository_row,
                    source_row,
                    target_artifact_row,
                    target_node_row,
                    link_kind_str(link.kind),
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        if let Some(existing_row) = existing_row {
            transaction
                .execute(
                    "UPDATE artifact_links SET evidence_locator = ?2 WHERE id = ?1",
                    params![existing_row, link.evidence_locator],
                )
                .map_err(database_error)?;
        } else {
            transaction
                .execute(
                    "INSERT INTO artifact_links(
                        repository_id, source_artifact_id, target_artifact_id,
                        target_node_id, kind, evidence_locator
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        repository_row,
                        source_row,
                        target_artifact_row,
                        target_node_row,
                        link_kind_str(link.kind),
                        link.evidence_locator,
                    ],
                )
                .map_err(database_error)?;
        }
    }
    Ok(())
}

fn artifacts_for_provider(
    transaction: &Transaction<'_>,
    repository_row: i64,
    provider: ArtifactProvider,
) -> Result<Vec<ArtifactIdentity>, PortError> {
    let mut statement = transaction
        .prepare(
            "SELECT kind, external_id FROM artifacts
             WHERE repository_id = ?1 AND provider = ?2
             ORDER BY kind, external_id",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map(params![repository_row, provider_str(provider)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(database_error)?;
    let mut identities = Vec::new();
    for row in rows {
        let (kind, external_id) = row.map_err(database_error)?;
        identities.push(ArtifactIdentity {
            provider,
            kind: parse_artifact_kind(&kind)?,
            external_id,
        });
    }
    Ok(identities)
}

fn delete_artifact_rows(
    transaction: &Transaction<'_>,
    repository_row: i64,
    identities: &[ArtifactIdentity],
) -> Result<Vec<ArtifactIdentity>, PortError> {
    let mut removed = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for identity in identities {
        if !seen.insert(identity.clone()) {
            continue;
        }
        let Some(row) = artifact_row(transaction, repository_row, identity)? else {
            continue;
        };
        transaction
            .execute(
                "DELETE FROM artifact_links
                 WHERE repository_id = ?1
                   AND (source_artifact_id = ?2 OR target_artifact_id = ?2)",
                params![repository_row, row],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM artifact_analysis WHERE artifact_id = ?1",
                [row],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM artifacts WHERE id = ?1 AND repository_id = ?2",
                params![row, repository_row],
            )
            .map_err(database_error)?;
        removed.push(identity.clone());
    }
    Ok(removed)
}

type ArtifactColumns = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
);

fn row_to_artifact_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactColumns> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn artifact_from_columns(columns: ArtifactColumns) -> Result<Artifact, PortError> {
    let (
        provider,
        kind,
        external_id,
        project,
        title,
        body,
        author,
        external_created_at,
        external_updated_at,
        source_locator,
        content_hash,
    ) = columns;
    Ok(Artifact {
        identity: ArtifactIdentity {
            provider: parse_provider(&provider)?,
            kind: parse_artifact_kind(&kind)?,
            external_id,
        },
        project: ctx_core::domain::Project(project),
        title,
        body,
        author,
        external_created_at: external_created_at.map(ctx_core::domain::Timestamp),
        external_updated_at: external_updated_at.map(ctx_core::domain::Timestamp),
        source_locator: ctx_core::domain::Url(source_locator),
        content_hash,
    })
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

fn artifact_row(
    transaction: &Transaction<'_>,
    repository_row: i64,
    identity: &ArtifactIdentity,
) -> Result<Option<i64>, PortError> {
    transaction
        .query_row(
            "SELECT id FROM artifacts
             WHERE repository_id = ?1 AND provider = ?2 AND kind = ?3 AND external_id = ?4",
            params![
                repository_row,
                provider_str(identity.provider),
                artifact_kind_str(identity.kind),
                identity.external_id,
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)
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

const fn provider_str(provider: ArtifactProvider) -> &'static str {
    match provider {
        ArtifactProvider::Git => "git",
        ArtifactProvider::GitLab => "gitlab",
        ArtifactProvider::GitHub => "github",
        ArtifactProvider::Jira => "jira",
        ArtifactProvider::Code => "code",
    }
}

fn parse_provider(value: &str) -> Result<ArtifactProvider, PortError> {
    match value {
        "git" => Ok(ArtifactProvider::Git),
        "gitlab" => Ok(ArtifactProvider::GitLab),
        "github" => Ok(ArtifactProvider::GitHub),
        "jira" => Ok(ArtifactProvider::Jira),
        "code" => Ok(ArtifactProvider::Code),
        other => Err(PortError::new(format!(
            "unknown artifact provider '{other}'"
        ))),
    }
}

const fn artifact_kind_str(kind: ArtifactKind) -> &'static str {
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

fn parse_artifact_kind(value: &str) -> Result<ArtifactKind, PortError> {
    match value {
        "commit" => Ok(ArtifactKind::Commit),
        "branch" => Ok(ArtifactKind::Branch),
        "issue" => Ok(ArtifactKind::Issue),
        "merge_request" => Ok(ArtifactKind::MergeRequest),
        "pull_request" => Ok(ArtifactKind::PullRequest),
        "comment" => Ok(ArtifactKind::Comment),
        "review_comment" => Ok(ArtifactKind::ReviewComment),
        "code_comment" => Ok(ArtifactKind::CodeComment),
        "docstring" => Ok(ArtifactKind::Docstring),
        "documentation" => Ok(ArtifactKind::Documentation),
        other => Err(PortError::new(format!("unknown artifact kind '{other}'"))),
    }
}

const fn link_kind_str(kind: ArtifactLinkKind) -> &'static str {
    match kind {
        ArtifactLinkKind::ContainsCommit => "contains_commit",
        ArtifactLinkKind::References => "references",
        ArtifactLinkKind::ChangedSymbol => "changed_symbol",
        ArtifactLinkKind::Discusses => "discusses",
        ArtifactLinkKind::CommentsOn => "comments_on",
        ArtifactLinkKind::RelatedIssue => "related_issue",
    }
}

fn parse_link_kind(value: &str) -> Result<ArtifactLinkKind, PortError> {
    match value {
        "contains_commit" => Ok(ArtifactLinkKind::ContainsCommit),
        "references" => Ok(ArtifactLinkKind::References),
        "changed_symbol" => Ok(ArtifactLinkKind::ChangedSymbol),
        "discusses" => Ok(ArtifactLinkKind::Discusses),
        "comments_on" => Ok(ArtifactLinkKind::CommentsOn),
        "related_issue" => Ok(ArtifactLinkKind::RelatedIssue),
        other => Err(PortError::new(format!(
            "unknown artifact link kind '{other}'"
        ))),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn database_error(error: rusqlite::Error) -> PortError {
    PortError::new(format!("SQLite artifact operation failed: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn domain_error(error: ctx_core::domain::InvalidIdentifier) -> PortError {
    PortError::new(format!("artifact link identifier is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use ctx_app::ports::{IndexStore, RepositoryDescriptor};
    use ctx_core::{business::BusinessKind, knowledge::AgentProvenance};
    use tempfile::tempdir;

    use super::*;

    fn open_repository(directory: &std::path::Path) -> (SqliteStore, RepositoryId) {
        let mut store = SqliteStore::open(&directory.join("ctx.db"), directory).expect("database");
        let repository = RepositoryDescriptor {
            id: RepositoryId::new("repo:test").expect("repository ID"),
            root_path: "/repo".to_owned(),
            remote_url: None,
        };
        store
            .ensure_repository(&repository, "2026-08-21T00:00:00Z")
            .expect("repository");
        (store, repository.id)
    }

    fn sample_artifact(external_id: &str, body: &str) -> Artifact {
        Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: ArtifactKind::MergeRequest,
                external_id: external_id.to_owned(),
            },
            project: ctx_core::domain::Project("billing/subscriptions".to_owned()),
            title: "Fix cancellation semantics".to_owned(),
            body: body.to_owned(),
            author: Some("alice".to_owned()),
            external_created_at: Some(ctx_core::domain::Timestamp(
                "2026-08-01T00:00:00Z".to_owned(),
            )),
            external_updated_at: Some(ctx_core::domain::Timestamp(
                "2026-08-02T00:00:00Z".to_owned(),
            )),
            source_locator: ctx_core::domain::Url(
                "https://gitlab.example/billing/subscriptions/-/merge_requests/842".to_owned(),
            ),
            content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
        }
    }

    fn artifact_with(
        provider: ArtifactProvider,
        kind: ArtifactKind,
        external_id: &str,
    ) -> Artifact {
        Artifact {
            identity: ArtifactIdentity {
                provider,
                kind,
                external_id: external_id.to_owned(),
            },
            ..sample_artifact(external_id, external_id)
        }
    }

    #[test]
    fn re_syncing_the_same_external_object_versions_it_instead_of_duplicating() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());

        store
            .upsert_artifact(
                &repository,
                &sample_artifact("842", "PAY-317. Users lose access immediately."),
                "2026-08-21T00:00:00Z",
                "v1",
            )
            .expect("first sync");
        store
            .upsert_artifact(
                &repository,
                &sample_artifact("842", "PAY-317. Updated description."),
                "2026-08-21T01:00:00Z",
                "v1",
            )
            .expect("second sync");

        let artifacts = store.list_artifacts(&repository).expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].body, "PAY-317. Updated description.");
    }

    #[test]
    fn links_between_stored_artifacts_round_trip() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());
        let issue = Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: ArtifactKind::Issue,
                external_id: "317".to_owned(),
            },
            ..sample_artifact("issue-317", "A cancelled subscription must remain usable.")
        };
        let mr = sample_artifact("842", "Fixes #317.");
        store
            .upsert_artifact(&repository, &issue, "2026-08-21T00:00:00Z", "v1")
            .expect("issue sync");
        store
            .upsert_artifact(&repository, &mr, "2026-08-21T00:00:00Z", "v1")
            .expect("mr sync");

        let link = ArtifactLink {
            source: mr.identity.clone(),
            target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
            kind: ArtifactLinkKind::References,
            evidence_locator: "body:#317".to_owned(),
        };
        store
            .persist_links(&repository, std::slice::from_ref(&link))
            .expect("persist link");
        // Idempotent re-ingestion must not duplicate the link.
        store
            .persist_links(&repository, std::slice::from_ref(&link))
            .expect("persist link again");

        let links = store.list_links(&repository).expect("links");
        assert_eq!(links, vec![link]);
    }

    #[test]
    fn replacing_outgoing_links_removes_relationships_missing_from_the_new_snapshot() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());
        let mr = sample_artifact("842", "Fixes PSI-317.");
        let first = artifact_with(ArtifactProvider::Jira, ArtifactKind::Issue, "PSI-317");
        let second = artifact_with(ArtifactProvider::Jira, ArtifactKind::Issue, "PSI-318");
        for artifact in [&mr, &first, &second] {
            store
                .upsert_artifact(&repository, artifact, "2026-08-21T00:00:00Z", "v1")
                .expect("artifact");
        }
        let old_link = ArtifactLink {
            source: mr.identity.clone(),
            target: ArtifactLinkTarget::Artifact(first.identity.clone()),
            kind: ArtifactLinkKind::References,
            evidence_locator: "body:PSI-317".to_owned(),
        };
        store
            .persist_links(&repository, std::slice::from_ref(&old_link))
            .expect("old link");
        let current_link = ArtifactLink {
            source: mr.identity.clone(),
            target: ArtifactLinkTarget::Artifact(second.identity.clone()),
            kind: ArtifactLinkKind::References,
            evidence_locator: "body:PSI-318".to_owned(),
        };

        store
            .replace_outgoing_links(
                &repository,
                &mr.identity,
                std::slice::from_ref(&current_link),
            )
            .expect("replace links");

        assert_eq!(
            store.list_links(&repository).expect("links"),
            vec![current_link]
        );
    }

    #[test]
    fn reconciling_a_snapshot_removes_absent_artifacts_and_all_dependencies() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());
        let stale = artifact_with(
            ArtifactProvider::Code,
            ArtifactKind::CodeComment,
            "src/lib.rs:1",
        );
        let current = artifact_with(
            ArtifactProvider::Code,
            ArtifactKind::Docstring,
            "src/lib.rs:20",
        );
        let commit = artifact_with(ArtifactProvider::Git, ArtifactKind::Commit, "abc123");
        for artifact in [&stale, &current, &commit] {
            store
                .upsert_artifact(&repository, artifact, "2026-08-21T00:00:00Z", "v1")
                .expect("artifact");
        }
        let links = vec![
            ArtifactLink {
                source: stale.identity.clone(),
                target: ArtifactLinkTarget::Artifact(commit.identity.clone()),
                kind: ArtifactLinkKind::References,
                evidence_locator: "outgoing".to_owned(),
            },
            ArtifactLink {
                source: commit.identity.clone(),
                target: ArtifactLinkTarget::Artifact(stale.identity.clone()),
                kind: ArtifactLinkKind::References,
                evidence_locator: "incoming".to_owned(),
            },
        ];
        store.persist_links(&repository, &links).expect("links");
        store
            .mark_analyzed(
                &repository,
                &stale.identity,
                &stale.content_hash,
                "bundle-v1",
                "2026-08-21T01:00:00Z",
            )
            .expect("analysis");
        let current_identities = std::collections::HashSet::from([current.identity.clone()]);

        let report = store
            .reconcile_snapshot(
                &repository,
                ArtifactProvider::Code,
                &[ArtifactKind::CodeComment, ArtifactKind::Docstring],
                &current_identities,
            )
            .expect("reconcile");

        assert_eq!(report.removed, vec![stale.identity.clone()]);
        let retained = store.list_artifacts(&repository).expect("artifacts");
        assert_eq!(retained.len(), 2);
        assert!(
            retained
                .iter()
                .any(|artifact| artifact.identity == current.identity)
        );
        assert!(
            retained
                .iter()
                .any(|artifact| artifact.identity == commit.identity)
        );
        assert!(store.list_links(&repository).expect("links").is_empty());
        assert!(
            store
                .analyzed_input_fingerprints(&repository)
                .expect("analysis")
                .is_empty()
        );

        let repeated = store
            .reconcile_snapshot(
                &repository,
                ArtifactProvider::Code,
                &[ArtifactKind::CodeComment, ArtifactKind::Docstring],
                &current_identities,
            )
            .expect("idempotent reconcile");
        assert!(repeated.removed.is_empty());
    }

    #[test]
    fn explicit_deletion_ignores_unknown_identities_and_reports_stored_ones() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());
        let stored = sample_artifact("842", "Fixes PSI-317.");
        let unknown = sample_artifact("999", "Unknown.");
        store
            .upsert_artifact(&repository, &stored, "2026-08-21T00:00:00Z", "v1")
            .expect("artifact");

        let report = store
            .delete_artifacts(
                &repository,
                &[
                    unknown.identity,
                    stored.identity.clone(),
                    stored.identity.clone(),
                ],
            )
            .expect("delete");

        assert_eq!(report.removed, vec![stored.identity]);
        assert!(
            store
                .list_artifacts(&repository)
                .expect("artifacts")
                .is_empty()
        );
    }

    #[test]
    fn pending_candidates_round_trip_and_stay_out_once_decided() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());
        let candidate = KnowledgeCandidate {
            fingerprint: KnowledgeCandidate::fingerprint_for(
                BusinessKind::Requirement,
                "Cancellation preserves paid access.",
            ),
            kind: BusinessKind::Requirement,
            statement: "Cancellation preserves paid access.".to_owned(),
            evidence: vec![ArtifactRef {
                identity: ArtifactIdentity {
                    provider: ArtifactProvider::GitLab,
                    kind: ArtifactKind::Issue,
                    external_id: "317".to_owned(),
                },
                locator: "description".to_owned(),
                excerpt: "must remain usable until paid_until".to_owned(),
            }],
            implementation_candidates: vec!["SubscriptionService.cancel".to_owned()],
            test_candidates: vec!["test_cancel_preserves_access".to_owned()],
            provenance: AgentProvenance {
                producer: "claude-code".to_owned(),
                model: Some("claude-sonnet-5".to_owned()),
                input_artifact_ids: vec!["gitlab:issue:317".to_owned()],
                produced_at: "2026-08-21T00:00:00Z".to_owned(),
                fingerprint: "prompt:v1".to_owned(),
            },
        };
        store
            .upsert_candidates(&repository, std::slice::from_ref(&candidate))
            .expect("persist candidate");
        // Re-analysis proposing the exact same candidate must not duplicate it.
        store
            .upsert_candidates(&repository, std::slice::from_ref(&candidate))
            .expect("persist candidate again");

        let pending = store.pending_candidates(&repository).expect("pending");
        assert_eq!(pending, vec![candidate]);
    }

    #[test]
    fn accepting_a_candidate_removes_it_from_pending_and_records_the_resulting_document() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());
        let candidate = KnowledgeCandidate {
            fingerprint: KnowledgeCandidate::fingerprint_for(
                BusinessKind::Requirement,
                "Cancellation preserves paid access.",
            ),
            kind: BusinessKind::Requirement,
            statement: "Cancellation preserves paid access.".to_owned(),
            evidence: vec![ArtifactRef {
                identity: ArtifactIdentity {
                    provider: ArtifactProvider::GitLab,
                    kind: ArtifactKind::Issue,
                    external_id: "317".to_owned(),
                },
                locator: "description".to_owned(),
                excerpt: "must remain usable until paid_until".to_owned(),
            }],
            implementation_candidates: Vec::new(),
            test_candidates: Vec::new(),
            provenance: AgentProvenance {
                producer: "claude-code".to_owned(),
                model: None,
                input_artifact_ids: vec!["gitlab:issue:317".to_owned()],
                produced_at: "2026-08-21T00:00:00Z".to_owned(),
                fingerprint: "prompt:v1".to_owned(),
            },
        };
        store
            .upsert_candidates(&repository, std::slice::from_ref(&candidate))
            .expect("persist candidate");

        store
            .record_decision(
                &repository,
                &candidate.fingerprint,
                &KnowledgeDecision::Accept {
                    document_id: "REQ-SUB-014".to_owned(),
                    method: ctx_core::knowledge::DecisionMethod::Human,
                },
                "alice",
                "2026-08-21T02:00:00Z",
            )
            .expect("record accept");

        assert!(
            store
                .pending_candidates(&repository)
                .expect("pending")
                .is_empty()
        );

        let record = store
            .accepted_record_for_document(&repository, "REQ-SUB-014")
            .expect("read accepted record")
            .expect("record exists");
        assert_eq!(
            record.decision_method,
            ctx_core::knowledge::DecisionMethod::Human
        );

        let error = store
            .record_decision(
                &repository,
                &candidate.fingerprint,
                &KnowledgeDecision::Reject {
                    method: ctx_core::knowledge::DecisionMethod::Human,
                },
                "alice",
                "2026-08-21T03:00:00Z",
            )
            .expect_err("an already-decided candidate cannot be decided again");
        assert!(error.to_string().contains("not currently pending"));
    }

    #[test]
    fn accepted_record_reports_an_agent_decision_honestly() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());
        let candidate = KnowledgeCandidate {
            fingerprint: KnowledgeCandidate::fingerprint_for(
                BusinessKind::Invariant,
                "Never delete paid history.",
            ),
            kind: BusinessKind::Invariant,
            statement: "Never delete paid history.".to_owned(),
            evidence: vec![ArtifactRef {
                identity: ArtifactIdentity {
                    provider: ArtifactProvider::GitLab,
                    kind: ArtifactKind::Issue,
                    external_id: "842".to_owned(),
                },
                locator: "description".to_owned(),
                excerpt: "must retain paid history forever".to_owned(),
            }],
            implementation_candidates: Vec::new(),
            test_candidates: Vec::new(),
            provenance: AgentProvenance {
                producer: "claude-code".to_owned(),
                model: None,
                input_artifact_ids: vec!["gitlab:issue:842".to_owned()],
                produced_at: "2026-08-21T00:00:00Z".to_owned(),
                fingerprint: "prompt:v1".to_owned(),
            },
        };
        store
            .upsert_candidates(&repository, std::slice::from_ref(&candidate))
            .expect("persist candidate");

        store
            .record_decision(
                &repository,
                &candidate.fingerprint,
                &KnowledgeDecision::Accept {
                    document_id: "INV-SUB-002".to_owned(),
                    method: ctx_core::knowledge::DecisionMethod::Agent,
                },
                "claude-code",
                "2026-08-21T02:00:00Z",
            )
            .expect("record accept");

        let record = store
            .accepted_record_for_document(&repository, "INV-SUB-002")
            .expect("read accepted record")
            .expect("record exists");
        assert_eq!(
            record.decision_method,
            ctx_core::knowledge::DecisionMethod::Agent
        );
    }

    #[test]
    fn marking_an_artifact_analyzed_round_trips_its_input_fingerprint() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());
        let artifact = sample_artifact("842", "PAY-317. Users lose access immediately.");
        store
            .upsert_artifact(&repository, &artifact, "2026-08-21T00:00:00Z", "v1")
            .expect("sync artifact");

        assert!(
            store
                .analyzed_input_fingerprints(&repository)
                .expect("analyzed hashes")
                .is_empty()
        );

        store
            .mark_analyzed(
                &repository,
                &artifact.identity,
                &artifact.content_hash,
                "bundle-v1",
                "2026-08-21T01:00:00Z",
            )
            .expect("mark analyzed");

        let hashes = store
            .analyzed_input_fingerprints(&repository)
            .expect("analyzed hashes");
        assert_eq!(
            hashes.get(&artifact.identity),
            Some(&"bundle-v1".to_owned())
        );

        // Re-marking with a new content hash (the artifact changed since
        // last analysis) updates the record in place rather than duplicating.
        store
            .mark_analyzed(
                &repository,
                &artifact.identity,
                "new-hash",
                "bundle-v2",
                "2026-08-21T02:00:00Z",
            )
            .expect("re-mark analyzed");
        let updated = store
            .analyzed_input_fingerprints(&repository)
            .expect("analyzed hashes");
        assert_eq!(updated.len(), 1);
        assert_eq!(
            updated.get(&artifact.identity),
            Some(&"bundle-v2".to_owned())
        );
    }

    #[test]
    fn sync_cursor_round_trips_and_advances_in_place() {
        let directory = tempdir().expect("temporary directory");
        let (mut store, repository) = open_repository(directory.path());

        assert_eq!(
            store.sync_cursor(&repository, "gitlab").expect("cursor"),
            None,
            "no cursor stored yet"
        );

        store
            .set_sync_cursor(&repository, "gitlab", "2026-08-21T00:00:00Z")
            .expect("set cursor");
        assert_eq!(
            store.sync_cursor(&repository, "gitlab").expect("cursor"),
            Some("2026-08-21T00:00:00Z".to_owned())
        );

        store
            .set_sync_cursor(&repository, "gitlab", "2026-08-21T01:00:00Z")
            .expect("advance cursor");
        assert_eq!(
            store.sync_cursor(&repository, "gitlab").expect("cursor"),
            Some("2026-08-21T01:00:00Z".to_owned()),
            "re-setting must update the existing row, not create a second one"
        );

        // A different provider's cursor is tracked independently.
        assert_eq!(
            store.sync_cursor(&repository, "code").expect("cursor"),
            None
        );
    }
}
