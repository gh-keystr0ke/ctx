ALTER TABLE artifact_analysis
ADD COLUMN input_fingerprint TEXT NOT NULL DEFAULT '';

UPDATE artifact_analysis
SET input_fingerprint = content_hash
WHERE input_fingerprint = '';

CREATE INDEX artifact_links_by_target_artifact
ON artifact_links(target_artifact_id);
