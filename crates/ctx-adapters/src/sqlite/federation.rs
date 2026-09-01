use ctx_app::ports::PortError;
use rusqlite::{OptionalExtension, params};

use crate::federation::{
    ExportManifest, ExportedDocument, ExportedEndpoint, FederatedRepositoryData,
    FederatedResolution, FederationSyncState,
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
}
