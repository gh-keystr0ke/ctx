CREATE TABLE ingest_cursors (
    id             INTEGER PRIMARY KEY,
    repository_id  INTEGER NOT NULL REFERENCES repositories(id),
    provider       TEXT NOT NULL,
    cursor         TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    UNIQUE(repository_id, provider)
);
