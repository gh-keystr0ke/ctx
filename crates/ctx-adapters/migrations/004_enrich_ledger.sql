CREATE TABLE artifact_analysis (
    id             INTEGER PRIMARY KEY,
    artifact_id    INTEGER NOT NULL REFERENCES artifacts(id),
    content_hash   TEXT NOT NULL,
    analyzed_at    TEXT NOT NULL,
    UNIQUE(artifact_id)
);
