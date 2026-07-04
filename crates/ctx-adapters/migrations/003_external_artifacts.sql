CREATE TABLE artifacts (
    id                   INTEGER PRIMARY KEY,
    repository_id        INTEGER NOT NULL REFERENCES repositories(id),
    provider             TEXT NOT NULL,
    kind                 TEXT NOT NULL,
    external_id          TEXT NOT NULL,
    project              TEXT NOT NULL,
    title                TEXT NOT NULL,
    body                 TEXT NOT NULL,
    author               TEXT,
    external_created_at  TEXT,
    external_updated_at  TEXT,
    source_locator       TEXT NOT NULL,
    content_hash         TEXT NOT NULL,
    ingested_at          TEXT NOT NULL,
    ingest_version       TEXT NOT NULL,
    UNIQUE(repository_id, provider, kind, external_id)
);

CREATE INDEX artifacts_by_kind ON artifacts(repository_id, kind);

CREATE TABLE artifact_links (
    id                   INTEGER PRIMARY KEY,
    repository_id        INTEGER NOT NULL REFERENCES repositories(id),
    source_artifact_id   INTEGER NOT NULL REFERENCES artifacts(id),
    target_artifact_id   INTEGER REFERENCES artifacts(id),
    target_node_id       INTEGER REFERENCES nodes(id),
    kind                 TEXT NOT NULL,
    evidence_locator      TEXT NOT NULL,
    CHECK((target_artifact_id IS NOT NULL) != (target_node_id IS NOT NULL))
);

CREATE INDEX artifact_links_by_source ON artifact_links(source_artifact_id);

CREATE TABLE knowledge_candidates (
    id                             INTEGER PRIMARY KEY,
    repository_id                  INTEGER NOT NULL REFERENCES repositories(id),
    fingerprint                    TEXT NOT NULL,
    kind                           TEXT NOT NULL,
    statement                      TEXT NOT NULL,
    evidence_json                  TEXT NOT NULL,
    implementation_candidates_json TEXT NOT NULL,
    test_candidates_json           TEXT NOT NULL,
    agent_producer                 TEXT NOT NULL,
    agent_model                    TEXT,
    input_artifact_ids_json        TEXT NOT NULL,
    produced_at                    TEXT NOT NULL,
    agent_fingerprint              TEXT NOT NULL,
    status                         TEXT NOT NULL CHECK(status IN ('pending', 'accepted', 'rejected')),
    resulting_document_id          TEXT,
    decided_by                     TEXT,
    decided_at                     TEXT,
    UNIQUE(repository_id, fingerprint)
);

CREATE INDEX knowledge_candidates_by_status ON knowledge_candidates(repository_id, status);
