CREATE TABLE repositories (
    id              INTEGER PRIMARY KEY,
    stable_id       TEXT NOT NULL UNIQUE,
    root_path       TEXT NOT NULL UNIQUE,
    remote_url      TEXT,
    created_at      TEXT NOT NULL
);

CREATE TABLE commits (
    id              INTEGER PRIMARY KEY,
    repository_id   INTEGER NOT NULL REFERENCES repositories(id),
    oid             TEXT NOT NULL,
    parent_oid      TEXT,
    authored_at     TEXT NOT NULL,
    indexed_at      TEXT NOT NULL,
    UNIQUE(repository_id, oid)
);

CREATE TABLE nodes (
    id              INTEGER PRIMARY KEY,
    repository_id   INTEGER NOT NULL REFERENCES repositories(id),
    kind            TEXT NOT NULL,
    stable_key      TEXT NOT NULL,
    created_commit  INTEGER REFERENCES commits(id),
    retired_commit  INTEGER REFERENCES commits(id),
    UNIQUE(repository_id, kind, stable_key)
);

CREATE TABLE node_versions (
    id              INTEGER PRIMARY KEY,
    node_id         INTEGER NOT NULL REFERENCES nodes(id),
    valid_from      INTEGER NOT NULL REFERENCES commits(id),
    valid_to        INTEGER REFERENCES commits(id),
    name            TEXT NOT NULL,
    content_hash    TEXT NOT NULL,
    attributes_json TEXT NOT NULL,
    UNIQUE(node_id, valid_from)
);

CREATE TABLE edges (
    id               INTEGER PRIMARY KEY,
    repository_id    INTEGER NOT NULL REFERENCES repositories(id),
    src_node_id      INTEGER NOT NULL REFERENCES nodes(id),
    dst_node_id      INTEGER NOT NULL REFERENCES nodes(id),
    kind             TEXT NOT NULL,
    epistemic_class  TEXT NOT NULL,
    provenance_kind  TEXT NOT NULL,
    confidence       REAL NOT NULL CHECK(confidence >= 0 AND confidence <= 1),
    status           TEXT NOT NULL CHECK(status IN ('active', 'stale', 'rejected')),
    valid_from       INTEGER NOT NULL REFERENCES commits(id),
    valid_to         INTEGER REFERENCES commits(id),
    producer         TEXT NOT NULL,
    fingerprint      TEXT NOT NULL,
    stale_reason     TEXT,
    UNIQUE(repository_id, fingerprint, valid_from)
);

CREATE INDEX edges_by_source ON edges(src_node_id, kind, valid_to);
CREATE INDEX edges_by_target ON edges(dst_node_id, kind, valid_to);
CREATE INDEX edges_by_kind ON edges(kind, valid_to);

CREATE TABLE sources (
    id              INTEGER PRIMARY KEY,
    repository_id   INTEGER NOT NULL REFERENCES repositories(id),
    kind            TEXT NOT NULL,
    uri             TEXT NOT NULL,
    commit_id       INTEGER REFERENCES commits(id),
    author          TEXT,
    timestamp       TEXT NOT NULL,
    content_hash    TEXT NOT NULL,
    metadata_json   TEXT NOT NULL
);

CREATE TABLE evidence (
    id              INTEGER PRIMARY KEY,
    source_id       INTEGER NOT NULL REFERENCES sources(id),
    locator         TEXT NOT NULL,
    excerpt_hash    TEXT NOT NULL,
    strength        REAL NOT NULL CHECK(strength >= 0 AND strength <= 1),
    attributes_json TEXT NOT NULL
);

CREATE TABLE edge_evidence (
    edge_id         INTEGER NOT NULL REFERENCES edges(id),
    evidence_id     INTEGER NOT NULL REFERENCES evidence(id),
    PRIMARY KEY(edge_id, evidence_id)
);

CREATE TABLE annotations (
    id              INTEGER PRIMARY KEY,
    edge_id         INTEGER NOT NULL REFERENCES edges(id),
    action          TEXT NOT NULL CHECK(action IN ('confirm', 'reject', 'comment')),
    author          TEXT NOT NULL,
    comment         TEXT,
    created_at      TEXT NOT NULL
);

CREATE TABLE aliases (
    id              INTEGER PRIMARY KEY,
    repository_id   INTEGER NOT NULL REFERENCES repositories(id),
    node_id         INTEGER NOT NULL REFERENCES nodes(id),
    alias           TEXT NOT NULL,
    UNIQUE(repository_id, alias, node_id)
);

CREATE TABLE derivations (
    id               INTEGER PRIMARY KEY,
    edge_id          INTEGER REFERENCES edges(id),
    node_version_id  INTEGER REFERENCES node_versions(id),
    producer         TEXT NOT NULL,
    source_uri       TEXT NOT NULL,
    input_fingerprint TEXT NOT NULL,
    CHECK((edge_id IS NOT NULL) != (node_version_id IS NOT NULL))
);

