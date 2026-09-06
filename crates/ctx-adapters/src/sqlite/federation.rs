use ctx_app::ports::PortError;
use ctx_core::ir::HttpMethod;
use rusqlite::{OptionalExtension, params};

use crate::federation::{
    ExportManifest, ExportedDocument, ExportedEndpoint, FederatedCallOverride,
    FederatedRepositoryData, FederatedResolution, FederationSyncState,
};

use super::SqliteStore;

impl SqliteStore {
    /// Atomically replaces one neighbor's imported snapshot and resolutions.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or any database operation fails.
    pub fn replace_federated_repository(
        &mut self,
        state: &FederationSyncState,
        manifest: &ExportManifest,
        resolutions: &[FederatedResolution],
    ) -> Result<(), PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        for table in [
            "federated_external_call_resolutions",
            "federated_endpoints",
            "federated_documents",
            "federated_syncs",
        ] {
            transaction
                .execute(
                    &format!("DELETE FROM {table} WHERE source_repo = ?1"),
                    [&state.source_repo],
                )
                .map_err(database_error)?;
        }
        transaction
            .execute(
                "INSERT INTO federated_syncs(
                    source_repo, source_path, source_commit, synced_at, schema_version
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    state.source_repo,
                    state.source_path,
                    state.source_commit,
                    state.synced_at,
                    state.schema_version
                ],
            )
            .map_err(database_error)?;
        for document in &manifest.documents {
            transaction
                .execute(
                    "INSERT INTO federated_documents(
                        source_repo, document_id, document_json, source_commit, synced_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        state.source_repo,
                        document.id,
                        serialize(document)?,
                        state.source_commit,
                        state.synced_at
                    ],
                )
                .map_err(database_error)?;
        }
        for endpoint in &manifest.endpoints {
            transaction
                .execute(
                    "INSERT INTO federated_endpoints(
                        source_repo, method, path, handler, endpoint_json,
                        source_commit, synced_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        state.source_repo,
                        endpoint.method.as_str(),
                        endpoint.path,
                        endpoint.handler,
                        serialize(endpoint)?,
                        state.source_commit,
                        state.synced_at
                    ],
                )
                .map_err(database_error)?;
        }
        for resolution in resolutions {
            transaction
                .execute(
                    "INSERT INTO federated_external_call_resolutions(
                        source_repo, local_call_key, endpoint_method, endpoint_path,
                        endpoint_handler, status, call_json, endpoint_json, local_commit,
                        source_commit, synced_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        state.source_repo,
                        resolution.call.stable_key,
                        resolution.endpoint.method.as_str(),
                        resolution.endpoint.path,
                        resolution.endpoint.handler,
                        resolution.status,
                        serialize(&resolution.call)?,
                        serialize(&resolution.endpoint)?,
                        resolution.local_commit,
                        state.source_commit,
                        state.synced_at
                    ],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)
    }

    /// Loads one neighbor's isolated federation snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when stored JSON is invalid or a database read fails.
    pub fn federated_repository(
        &self,
        source_repo: &str,
    ) -> Result<FederatedRepositoryData, PortError> {
        let state = self
            .connection
            .query_row(
                "SELECT source_repo, source_path, source_commit, synced_at, schema_version
                 FROM federated_syncs WHERE source_repo = ?1",
                [source_repo],
                |row| {
                    Ok(FederationSyncState {
                        source_repo: row.get(0)?,
                        source_path: row.get(1)?,
                        source_commit: row.get(2)?,
                        synced_at: row.get(3)?,
                        schema_version: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(database_error)?;
        let documents = query_json_rows::<ExportedDocument>(
            &self.connection,
            "SELECT document_json FROM federated_documents
             WHERE source_repo = ?1 ORDER BY document_id",
            source_repo,
        )?;
        let endpoints = query_json_rows::<ExportedEndpoint>(
            &self.connection,
            "SELECT endpoint_json FROM federated_endpoints
             WHERE source_repo = ?1 ORDER BY method, path, handler",
            source_repo,
        )?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT status, call_json, endpoint_json, local_commit,
                        source_commit, synced_at
                 FROM federated_external_call_resolutions
                 WHERE source_repo = ?1
                 ORDER BY local_call_key, endpoint_method, endpoint_path, endpoint_handler",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([source_repo], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(database_error)?;
        let mut resolutions = Vec::new();
        for row in rows {
            let (status, call, endpoint, local_commit, source_commit, synced_at) =
                row.map_err(database_error)?;
            resolutions.push(FederatedResolution {
                source_repo: source_repo.to_owned(),
                source_commit,
                local_commit,
                synced_at,
                status,
                call: deserialize(&call)?,
                endpoint: deserialize(&endpoint)?,
            });
        }
        Ok(FederatedRepositoryData {
            state,
            documents,
            endpoints,
            resolutions,
        })
    }

    /// Lists the latest successful synchronization state for every neighbor.
    ///
    /// # Errors
    ///
    /// Returns an error when the database query fails.
    pub fn federation_sync_states(&self) -> Result<Vec<FederationSyncState>, PortError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT source_repo, source_path, source_commit, synced_at, schema_version
                 FROM federated_syncs ORDER BY source_repo",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(FederationSyncState {
                    source_repo: row.get(0)?,
                    source_path: row.get(1)?,
                    source_commit: row.get(2)?,
                    synced_at: row.get(3)?,
                    schema_version: row.get(4)?,
                })
            })
            .map_err(database_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(database_error)
    }

    /// Records (or replaces) the human decision for one ambiguous
    /// `(method, path_template)` call shape. `resolved_neighbor: None`
    /// records an explicit "not any local neighbor" decision, distinct from
    /// no decision existing at all. Never touched by
    /// [`Self::replace_federated_repository`]'s per-neighbor wipe, so this
    /// survives every later `ctx sync`.
    ///
    /// # Errors
    ///
    /// Returns an error when the database write fails.
    pub fn set_federated_call_override(
        &mut self,
        method: HttpMethod,
        path_template: &str,
        resolved_neighbor: Option<&str>,
        decided_by: &str,
        decided_at: &str,
    ) -> Result<(), PortError> {
        self.connection
            .execute(
                "INSERT INTO federated_call_overrides(
                    method, path_template, resolved_neighbor, decided_by, decided_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(method, path_template) DO UPDATE SET
                    resolved_neighbor = excluded.resolved_neighbor,
                    decided_by = excluded.decided_by,
                    decided_at = excluded.decided_at",
                params![
                    serialize(&method)?,
                    path_template,
                    resolved_neighbor,
                    decided_by,
                    decided_at
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    /// The recorded human decision for one `(method, path_template)` call
    /// shape, if any.
    ///
    /// # Errors
    ///
    /// Returns an error when the database read or stored JSON is invalid.
    pub fn federated_call_override(
        &self,
        method: HttpMethod,
        path_template: &str,
    ) -> Result<Option<FederatedCallOverride>, PortError> {
        self.connection
            .query_row(
                "SELECT resolved_neighbor, decided_by, decided_at
                 FROM federated_call_overrides
                 WHERE method = ?1 AND path_template = ?2",
                params![serialize(&method)?, path_template],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?
            .map(|(resolved_neighbor, decided_by, decided_at)| {
                Ok(FederatedCallOverride {
                    method,
                    path_template: path_template.to_owned(),
                    resolved_neighbor,
                    decided_by,
                    decided_at,
                })
            })
            .transpose()
    }

    /// Lists every recorded call-shape override, for `ctx federation
    /// resolve --list`.
    ///
    /// # Errors
    ///
    /// Returns an error when the database read or stored JSON is invalid.
    pub fn list_federated_call_overrides(&self) -> Result<Vec<FederatedCallOverride>, PortError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT method, path_template, resolved_neighbor, decided_by, decided_at
                 FROM federated_call_overrides ORDER BY path_template, method",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(database_error)?;
        let mut overrides = Vec::new();
        for row in rows {
            let (method, path_template, resolved_neighbor, decided_by, decided_at) =
                row.map_err(database_error)?;
            overrides.push(FederatedCallOverride {
                method: deserialize(&method)?,
                path_template,
                resolved_neighbor,
                decided_by,
                decided_at,
            });
        }
        Ok(overrides)
    }

    /// Removes one neighbor's isolated cached data without touching local graph tables.
    ///
    /// # Errors
    ///
    /// Returns an error when the database transaction fails.
    pub fn remove_federated_repository(&mut self, source_repo: &str) -> Result<(), PortError> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        for table in [
            "federated_external_call_resolutions",
            "federated_endpoints",
            "federated_documents",
            "federated_syncs",
        ] {
            transaction
                .execute(
                    &format!("DELETE FROM {table} WHERE source_repo = ?1"),
                    [source_repo],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)
    }
}

fn query_json_rows<T: serde::de::DeserializeOwned>(
    connection: &rusqlite::Connection,
    sql: &str,
    source_repo: &str,
) -> Result<Vec<T>, PortError> {
    let mut statement = connection.prepare(sql).map_err(database_error)?;
    let rows = statement
        .query_map([source_repo], |row| row.get::<_, String>(0))
        .map_err(database_error)?;
    let mut values = Vec::new();
    for row in rows {
        values.push(deserialize(&row.map_err(database_error)?)?);
    }
    Ok(values)
}

fn serialize(value: &impl serde::Serialize) -> Result<String, PortError> {
    serde_json::to_string(value)
        .map_err(|error| PortError::new(format!("could not serialize federated data: {error}")))
}

fn deserialize<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, PortError> {
    serde_json::from_str(value)
        .map_err(|error| PortError::new(format!("stored federated data is invalid: {error}")))
}

#[allow(clippy::needless_pass_by_value)]
fn database_error(error: rusqlite::Error) -> PortError {
    PortError::new(format!("federation database operation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use ctx_core::{
        business::{BusinessKind, Visibility},
        ir::HttpMethod,
    };

    use crate::federation::{
        ExportManifest, ExportedDocument, ExternalCallContract, FEDERATION_SCHEMA_VERSION,
    };

    use super::*;

    #[test]
    fn federated_snapshots_round_trip_without_entering_local_graph_tables() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut store = SqliteStore::open(&directory.path().join("ctx.db"), directory.path())
            .expect("database");
        let document = ExportedDocument {
            id: "REQ-PUBLIC".to_owned(),
            kind: BusinessKind::Requirement,
            title: "Public".to_owned(),
            body: "Stable contract".to_owned(),
            status: "active".to_owned(),
            visibility: Visibility::Public,
            source_uri: ".context/public.yaml".to_owned(),
            content_hash: "hash".to_owned(),
        };
        let manifest = ExportManifest::new(
            "billing".to_owned(),
            "neighbor-commit".to_owned(),
            vec![document.clone()],
            Vec::new(),
        );
        let state = FederationSyncState {
            source_repo: "billing".to_owned(),
            source_path: "/work/billing".to_owned(),
            source_commit: "neighbor-commit".to_owned(),
            synced_at: "2026-08-26T00:00:00Z".to_owned(),
            schema_version: FEDERATION_SCHEMA_VERSION,
        };
        let resolution = FederatedResolution {
            source_repo: "billing".to_owned(),
            source_commit: "neighbor-commit".to_owned(),
            local_commit: "local-commit".to_owned(),
            synced_at: state.synced_at.clone(),
            status: "FEDERATED_MATCH".to_owned(),
            call: ExternalCallContract {
                stable_key: "external:post".to_owned(),
                handler: "caller.charge".to_owned(),
                method: HttpMethod::Post,
                url: "https://billing/charges".to_owned(),
                path_template: "/charges".to_owned(),
            },
            endpoint: crate::federation::ExportedEndpoint {
                handler: "billing.charge".to_owned(),
                method: HttpMethod::Post,
                path: "/charges".to_owned(),
                params: Vec::new(),
                return_type: None,
                framework: "python_http_framework".to_owned(),
                openapi: None,
                evidence: Vec::new(),
            },
        };

        store
            .replace_federated_repository(&state, &manifest, std::slice::from_ref(&resolution))
            .expect("replace snapshot");
        let loaded = store
            .federated_repository("billing")
            .expect("load snapshot");

        assert_eq!(loaded.state, Some(state));
        assert_eq!(loaded.documents, vec![document]);
        assert_eq!(loaded.resolutions, vec![resolution]);
        let local_nodes: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
            .expect("local node count");
        assert_eq!(local_nodes, 0);
    }

    #[test]
    fn call_overrides_round_trip_and_survive_unrelated_neighbor_replacement() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut store = SqliteStore::open(&directory.path().join("ctx.db"), directory.path())
            .expect("database");

        assert_eq!(
            store
                .federated_call_override(HttpMethod::Post, "/v1/items")
                .expect("lookup"),
            None
        );

        store
            .set_federated_call_override(
                HttpMethod::Post,
                "/v1/items",
                Some("billing"),
                "alice",
                "2026-09-06T00:00:00Z",
            )
            .expect("record override");

        let loaded = store
            .federated_call_override(HttpMethod::Post, "/v1/items")
            .expect("lookup")
            .expect("override recorded");
        assert_eq!(loaded.resolved_neighbor.as_deref(), Some("billing"));
        assert_eq!(loaded.decided_by, "alice");

        // Replacing an unrelated neighbor's snapshot must never touch the
        // override -- it is keyed by call shape, not by source_repo, and
        // must outlive every later `ctx sync`.
        let manifest = ExportManifest::new(
            "inventory".to_owned(),
            "inventory-commit".to_owned(),
            Vec::new(),
            Vec::new(),
        );
        let state = FederationSyncState {
            source_repo: "inventory".to_owned(),
            source_path: "/work/inventory".to_owned(),
            source_commit: "inventory-commit".to_owned(),
            synced_at: "2026-09-06T00:00:01Z".to_owned(),
            schema_version: FEDERATION_SCHEMA_VERSION,
        };
        store
            .replace_federated_repository(&state, &manifest, &[])
            .expect("replace unrelated neighbor");

        let still_there = store
            .federated_call_override(HttpMethod::Post, "/v1/items")
            .expect("lookup")
            .expect("override survives");
        assert_eq!(still_there.resolved_neighbor.as_deref(), Some("billing"));

        // Re-recording the same shape replaces the prior decision rather
        // than erroring or duplicating.
        store
            .set_federated_call_override(
                HttpMethod::Post,
                "/v1/items",
                None,
                "bob",
                "2026-09-06T00:00:02Z",
            )
            .expect("overwrite decision");
        let overwritten = store
            .federated_call_override(HttpMethod::Post, "/v1/items")
            .expect("lookup")
            .expect("override still recorded");
        assert_eq!(overwritten.resolved_neighbor, None);
        assert_eq!(overwritten.decided_by, "bob");

        assert_eq!(
            store
                .list_federated_call_overrides()
                .expect("list overrides")
                .len(),
            1
        );
    }
}
