//! Git-synced pending knowledge-candidate queue (`ADR-EXT-004`): one YAML
//! file per candidate under `.ctx-candidates/`, a sibling of `.context/`
//! deliberately kept outside it so [`crate::business_context`]'s recursive
//! `.context/` scan never tries to parse a candidate file as a
//! `BusinessDocument`. Unlike `.ctx/ctx.db` (gitignored, per-checkout),
//! this directory is ordinary Git-tracked content: two developers proposing
//! the identical candidate converge on the same filename (a content hash of
//! its fingerprint) and never conflict, and a decision recorded by one is
//! visible to the other after an ordinary `git pull`.
//!
//! [`crate::sqlite::SqliteStore`]'s [`ctx_app::ports::KnowledgeCandidateStore`]
//! implementation is the only caller of this module; the trait's
//! `repository` parameter is intentionally unused here, since one checkout
//! root implies exactly one `.ctx-candidates/` directory.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use ctx_app::ports::PortError;
use ctx_core::{
    artifact::ArtifactRef,
    knowledge::{AcceptedKnowledgeRecord, DecisionMethod, KnowledgeCandidate, KnowledgeDecision},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
enum CandidateQueueError {
    #[error("could not access candidate queue at '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid candidate YAML in '{path}': {message}")]
    Yaml { path: String, message: String },
}

#[allow(clippy::needless_pass_by_value)]
fn port_error(error: CandidateQueueError) -> PortError {
    PortError::new(error.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CandidateStatus {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredCandidate {
    #[serde(flatten)]
    candidate: KnowledgeCandidate,
    status: CandidateStatus,
    resulting_document_id: Option<String>,
    decided_by: Option<String>,
    decided_at: Option<String>,
    decision_method: Option<DecisionMethod>,
}

fn candidates_dir(root: &Path) -> PathBuf {
    root.join(".ctx-candidates")
}

/// Content-addressed: two callers proposing the identical `fingerprint`
/// always compute the same filename, so an independent re-proposal is a
/// git no-op rather than a second file or a conflict.
fn candidate_filename(fingerprint: &str) -> String {
    let hash = blake3::hash(fingerprint.as_bytes()).to_hex().to_string();
    format!("{}.yaml", &hash[..16])
}

fn read_all(root: &Path) -> Result<Vec<StoredCandidate>, PortError> {
    let directory = candidates_dir(root);
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&directory)
        .map_err(|source| {
            port_error(CandidateQueueError::Io {
                path: directory.display().to_string(),
                source,
            })
        })?;
    let mut stored = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            port_error(CandidateQueueError::Io {
                path: directory.display().to_string(),
                source,
            })
        })?;
        let path = entry.path();
        let is_yaml = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml"));
        if !is_yaml {
            continue;
        }
        let content = fs::read_to_string(&path).map_err(|source| {
            port_error(CandidateQueueError::Io {
                path: path.display().to_string(),
                source,
            })
        })?;
        let candidate: StoredCandidate =
            serde_yaml::from_str(&content).map_err(|error| {
                port_error(CandidateQueueError::Yaml {
                    path: path.display().to_string(),
                    message: error.to_string(),
                })
            })?;
        stored.push(candidate);
    }
    stored.sort_by(|a, b| a.candidate.fingerprint.cmp(&b.candidate.fingerprint));
    Ok(stored)
}

fn write_stored(root: &Path, stored: &StoredCandidate) -> Result<(), PortError> {
    let directory = candidates_dir(root);
    fs::create_dir_all(&directory).map_err(|source| {
        port_error(CandidateQueueError::Io {
            path: directory.display().to_string(),
            source,
        })
    })?;
    let path = directory.join(candidate_filename(&stored.candidate.fingerprint));
    let yaml = serde_yaml::to_string(stored).map_err(|error| {
        port_error(CandidateQueueError::Yaml {
            path: path.display().to_string(),
            message: error.to_string(),
        })
    })?;
    fs::write(&path, yaml).map_err(|source| {
        port_error(CandidateQueueError::Io {
            path: path.display().to_string(),
            source,
        })
    })
}

/// Idempotently persists `candidates`: a fingerprint whose file already
/// exists (whichever status it holds) is left untouched rather than
/// reverted to pending by a later re-analysis (PR-INCR-001/002) -- first
/// content wins, matching the SQL-backed implementation's prior
/// `ON CONFLICT DO NOTHING` semantics.
pub fn upsert(root: &Path, candidates: &[KnowledgeCandidate]) -> Result<(), PortError> {
    for candidate in candidates {
        let path = candidates_dir(root).join(candidate_filename(&candidate.fingerprint));
        if path.exists() {
            continue;
        }
        write_stored(
            root,
            &StoredCandidate {
                candidate: candidate.clone(),
                status: CandidateStatus::Pending,
                resulting_document_id: None,
                decided_by: None,
                decided_at: None,
                decision_method: None,
            },
        )?;
    }
    Ok(())
}

/// Every candidate still awaiting a human decision, in deterministic
/// (fingerprint-sorted) order.
pub fn pending(root: &Path) -> Result<Vec<KnowledgeCandidate>, PortError> {
    Ok(read_all(root)?
        .into_iter()
        .filter(|stored| stored.status == CandidateStatus::Pending)
        .map(|stored| stored.candidate)
        .collect())
}

/// Records a decision on the still-pending candidate identified by
/// `fingerprint`. Errors -- rather than silently reverting an
/// already-decided candidate -- when no file matches `fingerprint` or it is
/// no longer pending, exactly like the prior SQL `UPDATE ... WHERE
/// status = 'pending'` affecting zero rows.
pub fn record_decision(
    root: &Path,
    fingerprint: &str,
    decision: &KnowledgeDecision,
    author: &str,
    timestamp: &str,
) -> Result<(), PortError> {
    let path = candidates_dir(root).join(candidate_filename(fingerprint));
    let not_pending = || {
        PortError::new(format!(
            "knowledge candidate '{fingerprint}' is not currently pending"
        ))
    };
    if !path.exists() {
        return Err(not_pending());
    }
    let content = fs::read_to_string(&path).map_err(|source| {
        port_error(CandidateQueueError::Io {
            path: path.display().to_string(),
            source,
        })
    })?;
    let mut stored: StoredCandidate = serde_yaml::from_str(&content).map_err(|error| {
        port_error(CandidateQueueError::Yaml {
            path: path.display().to_string(),
            message: error.to_string(),
        })
    })?;
    if stored.status != CandidateStatus::Pending {
        return Err(not_pending());
    }
    let (status, resulting_document_id, method) = match decision {
        KnowledgeDecision::Accept {
            document_id,
            method,
        } => (CandidateStatus::Accepted, Some(document_id.clone()), *method),
        KnowledgeDecision::Reject { method } => (CandidateStatus::Rejected, None, *method),
    };
    stored.status = status;
    stored.resulting_document_id = resulting_document_id;
    stored.decided_by = Some(author.to_owned());
    stored.decided_at = Some(timestamp.to_owned());
    stored.decision_method = Some(method);
    write_stored(root, &stored)
}

/// Accepted candidates' evidence, grouped by the document they became --
/// reused by `REQ-MAP-001`'s heuristic scoring without a second AI call.
pub fn accepted_evidence(root: &Path) -> Result<BTreeMap<String, Vec<ArtifactRef>>, PortError> {
    let mut evidence_by_document = BTreeMap::new();
    for stored in read_all(root)? {
        if stored.status != CandidateStatus::Accepted {
            continue;
        }
        if let Some(document_id) = stored.resulting_document_id {
            evidence_by_document
                .entry(document_id)
                .or_insert_with(Vec::new)
                .extend(stored.candidate.evidence);
        }
    }
    Ok(evidence_by_document)
}

/// The accepted candidate record behind `document_id`, if any -- read by
/// `ctx explain` (Phase 9, `INV-PROVENANCE-001`) to render the full
/// artifact-to-inference-to-verification chain.
pub fn accepted_record_for_document(
    root: &Path,
    document_id: &str,
) -> Result<Option<AcceptedKnowledgeRecord>, PortError> {
    for stored in read_all(root)? {
        if stored.status != CandidateStatus::Accepted {
            continue;
        }
        if stored.resulting_document_id.as_deref() != Some(document_id) {
            continue;
        }
        let decided_by = stored.decided_by.ok_or_else(|| {
            PortError::new(format!(
                "accepted candidate for '{document_id}' is missing decided_by"
            ))
        })?;
        let decided_at = stored.decided_at.ok_or_else(|| {
            PortError::new(format!(
                "accepted candidate for '{document_id}' is missing decided_at"
            ))
        })?;
        let decision_method = stored.decision_method.ok_or_else(|| {
            PortError::new(format!(
                "accepted candidate for '{document_id}' is missing decision_method"
            ))
        })?;
        return Ok(Some(AcceptedKnowledgeRecord {
            candidate: stored.candidate,
            decided_by,
            decided_at,
            decision_method,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use ctx_core::{
        artifact::{ArtifactIdentity, ArtifactKind, ArtifactProvider},
        business::BusinessKind,
        knowledge::AgentProvenance,
    };
    use tempfile::tempdir;

    use super::*;

    fn candidate(statement: &str) -> KnowledgeCandidate {
        KnowledgeCandidate {
            fingerprint: KnowledgeCandidate::fingerprint_for(BusinessKind::Requirement, statement),
            kind: BusinessKind::Requirement,
            statement: statement.to_owned(),
            evidence: vec![ArtifactRef {
                identity: ArtifactIdentity {
                    provider: ArtifactProvider::GitLab,
                    kind: ArtifactKind::Issue,
                    external_id: "317".to_owned(),
                },
                locator: "body".to_owned(),
                excerpt: "excerpt".to_owned(),
            }],
            implementation_candidates: vec!["module.function".to_owned()],
            test_candidates: vec!["module.tests.covers_it".to_owned()],
            provenance: AgentProvenance {
                producer: "claude-code".to_owned(),
                model: None,
                input_artifact_ids: vec!["gitlab:issue:317".to_owned()],
                produced_at: "2026-08-26T00:00:00Z".to_owned(),
                fingerprint: "prompt:v1".to_owned(),
            },
        }
    }

    #[test]
    fn upserting_two_candidates_writes_two_files() {
        let directory = tempdir().expect("tempdir");
        let first = candidate("First requirement.");
        let second = candidate("Second requirement.");

        upsert(directory.path(), &[first.clone(), second.clone()]).expect("upsert");

        let mut files: Vec<_> = fs::read_dir(directory.path().join(".ctx-candidates"))
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        files.sort();
        assert_eq!(files.len(), 2);

        let pending_candidates = pending(directory.path()).expect("pending");
        assert_eq!(pending_candidates.len(), 2);
        assert!(pending_candidates.contains(&first));
        assert!(pending_candidates.contains(&second));
    }

    #[test]
    fn re_upserting_the_same_fingerprint_is_a_no_op() {
        let directory = tempdir().expect("tempdir");
        let original = candidate("Stable statement.");
        upsert(directory.path(), std::slice::from_ref(&original)).expect("first upsert");

        // Same (kind, statement) -> same fingerprint -> same file; only the
        // evidence differs here to prove the second write never lands.
        let mut resubmitted = original.clone();
        resubmitted.evidence[0].excerpt = "a different excerpt".to_owned();
        upsert(directory.path(), std::slice::from_ref(&resubmitted)).expect("second upsert");

        let pending_candidates = pending(directory.path()).expect("pending");
        assert_eq!(pending_candidates, vec![original]);
    }

    #[test]
    fn deciding_a_pending_candidate_rewrites_its_file_in_place() {
        let directory = tempdir().expect("tempdir");
        let original = candidate("Cancellation preserves access.");
        upsert(directory.path(), std::slice::from_ref(&original)).expect("upsert");

        record_decision(
            directory.path(),
            &original.fingerprint,
            &KnowledgeDecision::Accept {
                document_id: "REQ-SUB-001".to_owned(),
                method: DecisionMethod::Human,
            },
            "alice",
            "2026-08-26T01:00:00Z",
        )
        .expect("record decision");

        assert!(pending(directory.path()).expect("pending").is_empty());
        let record = accepted_record_for_document(directory.path(), "REQ-SUB-001")
            .expect("read accepted record")
            .expect("accepted record present");
        assert_eq!(record.candidate, original);
        assert_eq!(record.decided_by, "alice");
        assert_eq!(record.decision_method, DecisionMethod::Human);

        let files: Vec<_> = fs::read_dir(directory.path().join(".ctx-candidates"))
            .expect("read dir")
            .collect();
        assert_eq!(files.len(), 1, "the same file is rewritten, not duplicated");
    }

    #[test]
    fn deciding_an_unknown_fingerprint_errors() {
        let directory = tempdir().expect("tempdir");
        let error = record_decision(
            directory.path(),
            "knowledge:Requirement:never proposed",
            &KnowledgeDecision::Reject {
                method: DecisionMethod::Human,
            },
            "alice",
            "2026-08-26T01:00:00Z",
        )
        .expect_err("unknown fingerprint must fail");
        assert!(error.to_string().contains("is not currently pending"));
    }

    #[test]
    fn deciding_an_already_decided_fingerprint_errors() {
        let directory = tempdir().expect("tempdir");
        let original = candidate("Already decided.");
        upsert(directory.path(), std::slice::from_ref(&original)).expect("upsert");
        record_decision(
            directory.path(),
            &original.fingerprint,
            &KnowledgeDecision::Reject {
                method: DecisionMethod::Human,
            },
            "alice",
            "2026-08-26T01:00:00Z",
        )
        .expect("first decision");

        let error = record_decision(
            directory.path(),
            &original.fingerprint,
            &KnowledgeDecision::Accept {
                document_id: "REQ-SUB-002".to_owned(),
                method: DecisionMethod::Human,
            },
            "bob",
            "2026-08-26T02:00:00Z",
        )
        .expect_err("already-decided fingerprint must fail");
        assert!(error.to_string().contains("is not currently pending"));
    }

    #[test]
    fn accepted_evidence_groups_by_resulting_document_id() {
        let directory = tempdir().expect("tempdir");
        let first = candidate("First accepted.");
        let second = candidate("Second accepted, same document.");
        upsert(directory.path(), &[first.clone(), second.clone()]).expect("upsert");
        for candidate in [&first, &second] {
            record_decision(
                directory.path(),
                &candidate.fingerprint,
                &KnowledgeDecision::Accept {
                    document_id: "REQ-SUB-003".to_owned(),
                    method: DecisionMethod::Human,
                },
                "alice",
                "2026-08-26T01:00:00Z",
            )
            .expect("record decision");
        }

        let evidence = accepted_evidence(directory.path()).expect("accepted evidence");
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[&"REQ-SUB-003".to_owned()].len(), 2);
    }

    #[test]
    fn accepted_record_for_document_round_trips_decision_metadata() {
        let directory = tempdir().expect("tempdir");
        let original = candidate("Round trips decision metadata.");
        upsert(directory.path(), std::slice::from_ref(&original)).expect("upsert");
        record_decision(
            directory.path(),
            &original.fingerprint,
            &KnowledgeDecision::Accept {
                document_id: "REQ-SUB-004".to_owned(),
                method: DecisionMethod::Agent,
            },
            "auto-verify",
            "2026-08-26T03:00:00Z",
        )
        .expect("record decision");

        let record = accepted_record_for_document(directory.path(), "REQ-SUB-004")
            .expect("read")
            .expect("present");
        assert_eq!(record.decided_by, "auto-verify");
        assert_eq!(record.decided_at, "2026-08-26T03:00:00Z");
        assert_eq!(record.decision_method, DecisionMethod::Agent);
    }

    #[test]
    fn a_stray_non_yaml_file_is_ignored() {
        let directory = tempdir().expect("tempdir");
        let queue_dir = directory.path().join(".ctx-candidates");
        fs::create_dir_all(&queue_dir).expect("create queue dir");
        fs::write(queue_dir.join("README.md"), "not a candidate").expect("write stray file");

        assert!(pending(directory.path()).expect("pending").is_empty());
        assert!(read_all(directory.path()).expect("read_all").is_empty());
    }

    #[test]
    fn pending_lists_only_pending_status_in_deterministic_order() {
        let directory = tempdir().expect("tempdir");
        let accepted = candidate("Already accepted, must not reappear.");
        let still_pending = candidate("Still pending.");
        upsert(
            directory.path(),
            &[accepted.clone(), still_pending.clone()],
        )
        .expect("upsert");
        record_decision(
            directory.path(),
            &accepted.fingerprint,
            &KnowledgeDecision::Accept {
                document_id: "REQ-SUB-005".to_owned(),
                method: DecisionMethod::Human,
            },
            "alice",
            "2026-08-26T01:00:00Z",
        )
        .expect("record decision");

        let first = pending(directory.path()).expect("pending first read");
        let second = pending(directory.path()).expect("pending second read");
        assert_eq!(first, vec![still_pending]);
        assert_eq!(first, second, "order is deterministic across repeated reads");
    }
}
