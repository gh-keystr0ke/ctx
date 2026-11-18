use ctx_app::ports::PortError;
use ctx_core::domain::RepositoryId;
use rusqlite::params;

use super::SqliteStore;

#[derive(Debug)]
pub struct HistoricalSymbol {
    pub retired_at: Option<String>,
    pub successor: Option<SymbolSuccessor>,
}

#[derive(Debug)]
pub struct SymbolSuccessor {
    pub canonical_path: String,
    pub changed_at: String,
}

impl SqliteStore {
    /// Finds a unique historical owner of a canonical name and its continuous successor.
    ///
    /// # Errors
    /// Returns an error if the symbol history cannot be read.
    pub fn historical_symbol(
        &self,
        repository: &RepositoryId,
        canonical_path: &str,
    ) -> Result<Option<HistoricalSymbol>, PortError> {
        let database_error = |error: rusqlite::Error| PortError::new(error.to_string());
        let mut statement = self
            .connection
            .prepare_cached(
                "WITH owners AS (
                    SELECT v.node_id, max(v.valid_from) from_id
                    FROM node_versions v
                    JOIN nodes n ON n.id = v.node_id
                    JOIN repositories r ON r.id = n.repository_id
                    WHERE r.stable_id = ?1 AND n.kind = 'code_symbol'
                      AND json_extract(v.attributes_json, '$.canonical_path') = ?2
                    GROUP BY v.node_id
                )
                SELECT retired.oid,
                       json_extract(current.attributes_json, '$.canonical_path'),
                       changed.oid
                FROM owners o
                JOIN nodes n ON n.id = o.node_id
                JOIN node_versions old ON old.node_id = o.node_id AND old.valid_from = o.from_id
                LEFT JOIN commits retired ON retired.id = n.retired_commit
                LEFT JOIN commits changed ON changed.id = old.valid_to
                LEFT JOIN node_versions current
                  ON current.node_id = o.node_id AND current.valid_to IS NULL
                  AND NOT EXISTS (
                     SELECT 1 FROM node_versions v
                     WHERE v.node_id = o.node_id AND v.valid_from >= o.from_id
                       AND v.valid_to IS NOT NULL
                       AND NOT EXISTS (
                          SELECT 1 FROM node_versions following
                          WHERE following.node_id = o.node_id AND following.valid_from = v.valid_to
                       )
                  )
                LIMIT 2",
            )
            .map_err(database_error)?;
        let mut rows = statement
            .query_map(params![repository.as_str(), canonical_path], |row| {
                let current: Option<String> = row.get(1)?;
                let changed_at: Option<String> = row.get(2)?;
                Ok(HistoricalSymbol {
                    retired_at: row.get(0)?,
                    successor: current
                        .zip(changed_at)
                        .filter(|(current, _)| current != canonical_path)
                        .map(|(canonical_path, changed_at)| SymbolSuccessor {
                            canonical_path,
                            changed_at,
                        }),
                })
            })
            .map_err(database_error)?;
        let first = rows.next().transpose().map_err(database_error)?;
        if rows.next().transpose().map_err(database_error)?.is_some() {
            return Ok(None);
        }
        Ok(first)
    }
}
