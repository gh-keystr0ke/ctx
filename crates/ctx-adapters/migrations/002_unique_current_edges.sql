WITH current_versions AS (
    SELECT
        id,
        LEAD(valid_from) OVER (
            PARTITION BY repository_id, fingerprint
            ORDER BY id
        ) AS next_valid_from
    FROM edges
    WHERE valid_to IS NULL
)
UPDATE edges
SET valid_to = (
    SELECT next_valid_from
    FROM current_versions
    WHERE current_versions.id = edges.id
)
WHERE id IN (
    SELECT id
    FROM current_versions
    WHERE next_valid_from IS NOT NULL
);

CREATE UNIQUE INDEX edges_one_current_fingerprint
ON edges(repository_id, fingerprint)
WHERE valid_to IS NULL;
