CREATE INDEX IF NOT EXISTS nodes_by_key ON nodes(repository_id, stable_key);
CREATE INDEX IF NOT EXISTS edge_evidence_by_evidence ON edge_evidence(evidence_id);
CREATE INDEX IF NOT EXISTS derivations_by_edge ON derivations(edge_id);
CREATE INDEX IF NOT EXISTS derivations_by_node_version ON derivations(node_version_id);
CREATE INDEX IF NOT EXISTS evidence_by_source ON evidence(source_id);
CREATE INDEX IF NOT EXISTS derivations_by_source ON derivations(source_uri, edge_id);
CREATE INDEX IF NOT EXISTS sources_by_content ON sources(repository_id, uri, content_hash);
CREATE INDEX IF NOT EXISTS annotations_by_edge ON annotations(edge_id);
CREATE INDEX IF NOT EXISTS artifact_links_by_repository ON artifact_links(repository_id);
