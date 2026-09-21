CREATE TABLE unavailable_external_keys (
    id             INTEGER PRIMARY KEY,
    repository_id  INTEGER NOT NULL REFERENCES repositories(id),
    provider       TEXT NOT NULL,
    external_key   TEXT NOT NULL,
    checked_at     TEXT NOT NULL,
    UNIQUE(repository_id, provider, external_key)
);

CREATE INDEX unavailable_external_keys_repository_provider
    ON unavailable_external_keys(repository_id, provider);
