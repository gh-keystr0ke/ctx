-- ADR-EXT-004: the pending knowledge-candidate queue moved to a git-tracked
-- file per candidate under .ctx-candidates/, so it survives across
-- checkouts instead of living only in this gitignored local database.
DROP TABLE knowledge_candidates;
