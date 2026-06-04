use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;

mod context;
mod graph;
mod index;
mod verification;

const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/001_initial.sql"))];

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

/// Concrete `SQLite` source of truth for a local repository.
pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    /// Opens a database, configures `SQLite` for local concurrent reads, and
    /// applies every pending schema migration atomically.
    ///
    /// # Errors
    ///
    /// Returns [`SqliteStoreError`] when the file cannot be opened, `SQLite`
    /// configuration fails, or a migration cannot be applied.
    pub fn open(path: &Path) -> Result<Self, SqliteStoreError> {
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
                transaction.execute_batch(migration)?;
                transaction.execute(
                    "INSERT INTO schema_migrations(version) VALUES (?1)",
                    [version],
                )?;
            }
        }
        transaction.commit()?;

        Ok(Self { connection })
    }

    pub const fn connection(&self) -> &Connection {
        &self.connection
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

        let store = SqliteStore::open(&database).expect("create database");
        let table_count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
                    'repositories', 'commits', 'nodes', 'node_versions', 'edges',
                    'sources', 'evidence', 'edge_evidence', 'annotations', 'aliases',
                    'derivations'
                )",
                [],
                |row| row.get(0),
            )
            .expect("query schema");
        assert_eq!(table_count, 11);

        drop(store);
        SqliteStore::open(&database).expect("migrations are idempotent");
    }
}
