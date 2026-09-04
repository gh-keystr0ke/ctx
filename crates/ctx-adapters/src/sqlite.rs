use std::path::{Path, PathBuf};

use rusqlite::Connection;
use thiserror::Error;

mod artifacts;
mod context;
mod federation;
mod graph;
mod index;
mod type_inference;
mod verification;

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/001_initial.sql")),
    (
        2,
        include_str!("../migrations/002_unique_current_edges.sql"),
    ),
    (3, include_str!("../migrations/003_external_artifacts.sql")),
    (4, include_str!("../migrations/004_enrich_ledger.sql")),
    (5, include_str!("../migrations/005_ingest_cursors.sql")),
    (6, include_str!("../migrations/006_decision_method.sql")),
    (
        7,
        include_str!("../migrations/007_drop_knowledge_candidates.sql"),
    ),
    (8, include_str!("../migrations/008_document_visibility.sql")),
    (9, include_str!("../migrations/009_federation.sql")),
    (
        10,
        include_str!("../migrations/010_artifact_reconciliation.sql"),
    ),
];

#[derive(Debug, Error)]
pub enum SqliteStoreError {
    #[error("failed to open ctx database at {path}: {source}")]
    Open {
        path: String,
        source: rusqlite::Error,
    },
    #[error("failed to configure or migrate ctx database: {0}")]
    Migration(#[from] rusqlite::Error),
}

/// Concrete `SQLite` source of truth for a local repository, plus the
/// repository's checkout root -- needed only by the
/// [`ctx_app::ports::KnowledgeCandidateStore`] impl (`artifacts.rs`), which
/// reads/writes the git-tracked `.ctx-candidates/` queue directly on disk
/// rather than through `connection` (`ADR-EXT-004`).
pub struct SqliteStore {
    connection: Connection,
    root: PathBuf,
}

impl SqliteStore {
    /// Opens a database rooted at `root`, configures `SQLite` for local
    /// concurrent reads, and applies every pending schema migration
    /// atomically.
    ///
    /// # Errors
    ///
    /// Returns [`SqliteStoreError`] when the file cannot be opened, `SQLite`
    /// configuration fails, or a migration cannot be applied.
    pub fn open(path: &Path, root: &Path) -> Result<Self, SqliteStoreError> {
        let started = std::time::Instant::now();
        tracing::debug!(path = %path.display(), "SQLite store opening");
        let mut connection = Connection::open(path).map_err(|source| SqliteStoreError::Open {
            path: path.display().to_string(),
            source,
        })?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;

        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);",
        )?;
        for &(version, migration) in MIGRATIONS {
            let applied = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
                [version],
                |row| row.get::<_, bool>(0),
            )?;
            if !applied {
                tracing::trace!(version, "SQLite migration applying");
                transaction.execute_batch(migration)?;
                transaction.execute(
                    "INSERT INTO schema_migrations(version) VALUES (?1)",
                    [version],
                )?;
            }
        }
        transaction.commit()?;
        tracing::debug!(
            path = %path.display(),
            elapsed_ms = started.elapsed().as_millis(),
            "SQLite store opened"
        );

        Ok(Self {
            connection,
            root: root.to_path_buf(),
        })
    }

    pub const fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn creates_and_reopens_a_repository_database() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("ctx.db");

        let store = SqliteStore::open(&database, directory.path()).expect("create database");
        let table_count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
                    'repositories', 'commits', 'nodes', 'node_versions', 'edges',
                    'sources', 'evidence', 'edge_evidence', 'annotations', 'aliases',
                    'derivations', 'artifacts', 'artifact_links',
                    'artifact_analysis', 'ingest_cursors'
                )",
                [],
                |row| row.get(0),
            )
            .expect("query schema");
        assert_eq!(table_count, 15);

        drop(store);
        SqliteStore::open(&database, directory.path()).expect("migrations are idempotent");
    }

    #[test]
    fn migration_closes_duplicate_current_edges_and_enforces_uniqueness() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("legacy.db");
        create_legacy_database_with_duplicate_edges(&database);

        let store =
            SqliteStore::open(&database, directory.path()).expect("migrate legacy database");
        let current: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE fingerprint = 'same' AND valid_to IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("current edges");
        let closed_at: i64 = store
            .connection()
            .query_row("SELECT valid_to FROM edges WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("closed edge");
        let index_exists: bool = store
            .connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type = 'index' AND name = 'edges_one_current_fingerprint')",
                [],
                |row| row.get(0),
            )
            .expect("unique index");

        assert_eq!(current, 1);
        assert_eq!(closed_at, 2);
        assert!(index_exists);
    }

    #[test]
    fn existing_business_documents_upgrade_as_private_without_data_loss() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("legacy-visibility.db");
        let connection = Connection::open(&database).expect("legacy database");
        connection
            .execute_batch(&format!(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
                 {};
                 INSERT INTO schema_migrations(version) VALUES (1);
                 INSERT INTO repositories(id, stable_id, root_path, created_at)
                    VALUES (1, 'repo:test', '/repo', '2026-08-17T00:00:00Z');
                 INSERT INTO commits(id, repository_id, oid, authored_at, indexed_at)
                    VALUES (1, 1, 'aaaaaaaa', '2026-08-17T00:00:00Z', '2026-08-17T00:00:00Z');
                 INSERT INTO nodes(id, repository_id, kind, stable_key, created_commit)
                    VALUES (1, 1, 'requirement', 'intent:REQ-LEGACY-001', 1);
                 INSERT INTO node_versions(
                    node_id, valid_from, name, content_hash, attributes_json
                 ) VALUES (
                    1, 1, 'Legacy requirement', 'hash',
                    '{{\"type\":\"business\",\"id\":\"REQ-LEGACY-001\",\"status\":\"active\",\"body\":\"Keep it\",\"feature\":null,\"source_uri\":\"legacy.yaml\"}}'
                 );",
                include_str!("../migrations/001_initial.sql")
            ))
            .expect("legacy schema and document");
        drop(connection);

        let store = SqliteStore::open(&database, directory.path()).expect("upgrade database");
        let (name, visibility): (String, String) = store
            .connection()
            .query_row(
                "SELECT name, visibility FROM node_versions WHERE node_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("upgraded document");
        assert_eq!(name, "Legacy requirement");
        assert_eq!(visibility, "private");
    }

    fn create_legacy_database_with_duplicate_edges(database: &Path) {
        let connection = Connection::open(database).expect("legacy database");
        connection
            .execute_batch(&format!(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
                 {};
                 INSERT INTO schema_migrations(version) VALUES (1);
                 INSERT INTO repositories(id, stable_id, root_path, created_at)
                    VALUES (1, 'repo:test', '/repo', '2026-08-17T00:00:00Z');
                 INSERT INTO commits(id, repository_id, oid, authored_at, indexed_at) VALUES
                    (1, 1, 'aaaaaaaa', '2026-08-17T00:00:00Z', '2026-08-17T00:00:00Z'),
                    (2, 1, 'bbbbbbbb', '2026-08-17T00:01:00Z', '2026-08-17T00:01:00Z');
                 INSERT INTO nodes(id, repository_id, kind, stable_key, created_commit) VALUES
                    (1, 1, 'file', 'file:a.py', 1),
                    (2, 1, 'code_symbol', 'symbol:a', 1);
                 INSERT INTO edges(
                    id, repository_id, src_node_id, dst_node_id, kind,
                    epistemic_class, provenance_kind, confidence, status,
                    valid_from, producer, fingerprint
                 ) VALUES
                    (1, 1, 1, 2, 'contains', 'fact', 'staticanalysis', 1, 'active', 1, 'test', 'same'),
                    (2, 1, 1, 2, 'contains', 'fact', 'staticanalysis', 1, 'active', 2, 'test', 'same');",
                include_str!("../migrations/001_initial.sql")
            ))
            .expect("legacy schema and data");
    }
}
