-- One human decision per (method, path_template) call shape that matched
-- more than one registered neighbor during `ctx sync`. Deliberately not
-- keyed by source_repo and never touched by replace_federated_repository's
-- per-neighbor wipe, so the decision survives every later sync instead of
-- being re-asked. resolved_neighbor NULL means "explicitly not any local
-- neighbor" (a genuine third party), not "unresolved".
CREATE TABLE federated_call_overrides (
    method            TEXT NOT NULL,
    path_template     TEXT NOT NULL,
    resolved_neighbor TEXT,
    decided_by        TEXT NOT NULL,
    decided_at        TEXT NOT NULL,
    PRIMARY KEY(method, path_template)
);
