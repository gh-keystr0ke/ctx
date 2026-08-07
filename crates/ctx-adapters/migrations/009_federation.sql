CREATE TABLE federated_syncs (
    source_repo     TEXT PRIMARY KEY,
    source_path     TEXT NOT NULL,
    source_commit   TEXT NOT NULL,
    synced_at       TEXT NOT NULL,
    schema_version  INTEGER NOT NULL
);

CREATE TABLE federated_documents (
    source_repo     TEXT NOT NULL,
    document_id     TEXT NOT NULL,
    document_json   TEXT NOT NULL,
    source_commit   TEXT NOT NULL,
    synced_at       TEXT NOT NULL,
    PRIMARY KEY(source_repo, document_id)
);

CREATE TABLE federated_endpoints (
    source_repo     TEXT NOT NULL,
    method          TEXT NOT NULL,
    path            TEXT NOT NULL,
    handler         TEXT NOT NULL,
    endpoint_json   TEXT NOT NULL,
    source_commit   TEXT NOT NULL,
    synced_at       TEXT NOT NULL,
    PRIMARY KEY(source_repo, method, path, handler)
);

CREATE TABLE federated_external_call_resolutions (
    source_repo     TEXT NOT NULL,
    local_call_key  TEXT NOT NULL,
    endpoint_method TEXT NOT NULL,
    endpoint_path   TEXT NOT NULL,
    endpoint_handler TEXT NOT NULL,
    status          TEXT NOT NULL,
    call_json       TEXT NOT NULL,
    endpoint_json   TEXT NOT NULL,
    local_commit    TEXT NOT NULL,
    source_commit   TEXT NOT NULL,
    synced_at       TEXT NOT NULL,
    PRIMARY KEY(
        source_repo,
        local_call_key,
        endpoint_method,
        endpoint_path,
        endpoint_handler
    )
);

CREATE INDEX federated_documents_by_repo
ON federated_documents(source_repo);

CREATE INDEX federated_endpoints_by_contract
ON federated_endpoints(method, path);

CREATE INDEX federated_resolutions_by_repo
ON federated_external_call_resolutions(source_repo, status);
