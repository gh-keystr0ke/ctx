//! AI-derived typed knowledge candidates (prompt3.md PR-AI-*, PR-VERIFY-*):
//! a Feature/Requirement/Invariant/Decision statement an agent extracted
//! from a bounded artifact neighborhood, with its evidence and full
//! provenance attached. A [`KnowledgeCandidate`] is always `INFERENCE`
//! until a human decision promotes it (PR-P02) — this module never
//! constructs a [`crate::domain::Edge`] or [`crate::business::BusinessDocument`]
//! itself.

use serde::{Deserialize, Serialize};

use crate::{artifact::ArtifactRef, business::BusinessKind};

/// Provenance for one agent's output (PR-AI-005): who produced it, what it
/// read, and when — independent of which concrete CLI agent ran.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentProvenance {
    /// The interchangeable agent's own name (`"claude-code"`, `"codex"`,
    /// ...) — never treated as part of the ontology (PR-P05), only as a
    /// label on the claim.
    pub producer: String,
    pub model: Option<String>,
    /// Formatted identities of every artifact the agent's bounded input
    /// neighborhood was built from.
    pub input_artifact_ids: Vec<String>,
    pub produced_at: String,
    /// A deterministic fingerprint of the exact prompt/configuration that
    /// produced this output, for reproducibility auditing.
    pub fingerprint: String,
}

/// One typed candidate an agent proposed from a bounded artifact
/// neighborhood (PR-AI-003). Never itself a `FACT` or an accepted
/// `ASSERTION` — see [`crate::verification`] and PR-VERIFY-001/002 for the
/// human-verification step that can promote it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeCandidate {
    /// Deterministic within one repository: stable across repeated identical
    /// extraction so re-analysis recognizes "already proposed" rather than
    /// duplicating (PR-INCR-001/002).
    pub fingerprint: String,
    pub kind: BusinessKind,
    pub statement: String,
    /// Concrete evidence excerpts backing `statement` — never the agent's
    /// own reasoning (PR-AI-004). Empty evidence means the candidate should
    /// not have been proposed at all.
    pub evidence: Vec<ArtifactRef>,
    /// Candidate implementation anchors the agent found in the same
    /// bounded neighborhood (PR-MAP-001) — themselves still inferences,
    /// not asserted mappings. Paths named here are outside the neighborhood
    /// only when `ctx enrich --allow-ungrounded-symbols` was used to relax
    /// this grounding for `implementation_candidates`/`test_candidates`.
    pub implementation_candidates: Vec<String>,
    pub test_candidates: Vec<String>,
    pub provenance: AgentProvenance,
}

impl KnowledgeCandidate {
    /// Deterministic fingerprint for one `(kind, statement)` pair, matching
    /// this codebase's existing plain-string fingerprint convention (see
    /// `ctx_core::verification::SemanticCandidate::fingerprint`) rather than
    /// a content hash — candidates are compared for reuse by kind and exact
    /// restated text, not by a rehashed proxy of the same content.
    #[must_use]
    pub fn fingerprint_for(kind: BusinessKind, statement: &str) -> String {
        format!("knowledge:{kind:?}:{statement}")
    }

    /// A short title for kinds whose `.context/*.yaml` shape needs one
    /// separate from the full statement (Feature's `name`, Decision's
    /// `title`) — the agent contract has no dedicated title field, so this
    /// takes the statement's first sentence, or the whole thing truncated to
    /// a bounded length when no sentence break exists, never inventing text
    /// the statement doesn't contain.
    #[must_use]
    pub fn derived_title(&self) -> String {
        const MAX_CHARS: usize = 80;
        let first_sentence = self
            .statement
            .split_once(". ")
            .map_or(self.statement.as_str(), |(sentence, _)| sentence);
        if first_sentence.chars().count() <= MAX_CHARS {
            return first_sentence.trim_end_matches('.').to_owned();
        }
        let mut truncated: String = first_sentence.chars().take(MAX_CHARS).collect();
        truncated.push('\u{2026}');
        truncated
    }
}

/// What an agent decided about one bounded artifact neighborhood
/// (PR-AI-002). Absence of extracted knowledge is always preferred to a
/// fabricated candidate (PR-P02, FR-01).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AgentOutcome {
    Relevant(Vec<KnowledgeCandidate>),
    NotRelevant,
    InsufficientEvidence,
}

/// Who made a decision on a pending [`KnowledgeCandidate`] -- a human
/// (`ctx verify --knowledge`) or an agent's own independent second-opinion
/// review (`ctx verify --knowledge --auto`). Kept as a real, structured
/// field rather than folded into the free-text `decided_by` author string,
/// so `ctx explain` can never render an agent's own decision as though a
/// human made it (`INV-EPISTEMIC-001` still holds either way: a human
/// explicitly configured and triggered `--auto`, but the resulting
/// document's provenance says so honestly rather than looking identical to
/// a human review that never happened).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMethod {
    Human,
    Agent,
}

/// A candidate that was accepted, with the decision recorded alongside it
/// (PR-VERIFY-002) -- read back by `ctx explain` (Phase 9) to render the
/// full artifact -> agent-inference -> decision chain behind the document
/// it became.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptedKnowledgeRecord {
    pub candidate: KnowledgeCandidate,
    pub decided_by: String,
    pub decided_at: String,
    pub decision_method: DecisionMethod,
}

/// A decision on one pending [`KnowledgeCandidate`] (PR-VERIFY-001). Unlike
/// the heuristic `SemanticCandidate` accept path (which only asserts an
/// already-known claim), accepting a `KnowledgeCandidate` creates a new
/// product-knowledge entity, so acceptance carries the chosen stable ID
/// that entity will have (PR-VERIFY-002 keeps the original candidate row,
/// status `accepted`, pointing at this same ID, rather than discarding the
/// chain once the resulting document exists).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KnowledgeDecision {
    Accept {
        document_id: String,
        method: DecisionMethod,
    },
    Reject {
        method: DecisionMethod,
    },
}

/// One candidate's outcome from an agent's independent second-opinion review
/// (`ctx verify --knowledge --auto`) -- deliberately not a mechanical
/// bulk-accept of everything extraction already called `relevant`: the
/// review agent re-examines each candidate on its own and can reject it,
/// per the same evidence-vs-reasoning discipline extraction itself follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Accept,
    Reject,
}

/// One candidate's verdict within a [`ClusterReview`], identified by the
/// same fingerprint the candidate was proposed and stored under -- never a
/// position/index, so a review agent can never accidentally decide the
/// wrong candidate through reordering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CandidateReviewDecision {
    pub fingerprint: String,
    pub verdict: ReviewVerdict,
}

/// A review agent's independent second opinion on one
/// [`crate::verification::CandidateCluster`] -- the pending candidates
/// [`crate::verification::cluster_candidates`] grouped by shared vocabulary
/// as plausibly describing one underlying flow. `merged_statement` is
/// `Some` only when the agent judges two or more accepted candidates in the
/// cluster to be genuinely the same knowledge restated, not merely lexically
/// similar -- consolidating them into one document instead of one per
/// candidate (the user's explicit "не плодить дубли и объединять их" ask).
/// `None` means the agent judged the accepted candidates in this cluster
/// distinct enough to stay as separate documents even though they clustered
/// lexically -- clustering is only a hint for what to review together, never
/// a decision to merge on its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClusterReview {
    pub decisions: Vec<CandidateReviewDecision>,
    pub merged_statement: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_for_the_same_kind_and_statement() {
        let first = KnowledgeCandidate::fingerprint_for(
            BusinessKind::Requirement,
            "Cancellation preserves paid access.",
        );
        let second = KnowledgeCandidate::fingerprint_for(
            BusinessKind::Requirement,
            "Cancellation preserves paid access.",
        );
        let different_kind = KnowledgeCandidate::fingerprint_for(
            BusinessKind::Invariant,
            "Cancellation preserves paid access.",
        );
        assert_eq!(first, second);
        assert_ne!(first, different_kind);
    }

    fn candidate(statement: &str) -> KnowledgeCandidate {
        KnowledgeCandidate {
            fingerprint: KnowledgeCandidate::fingerprint_for(BusinessKind::Decision, statement),
            kind: BusinessKind::Decision,
            statement: statement.to_owned(),
            evidence: Vec::new(),
            implementation_candidates: Vec::new(),
            test_candidates: Vec::new(),
            provenance: AgentProvenance {
                producer: "test".to_owned(),
                model: None,
                input_artifact_ids: Vec::new(),
                produced_at: "2026-08-21T00:00:00Z".to_owned(),
                fingerprint: "fp".to_owned(),
            },
        }
    }

    #[test]
    fn derived_title_takes_the_first_sentence() {
        let title = candidate("Cancellation stays reversible until period end. It preserves paid access until paid_until.").derived_title();
        assert_eq!(title, "Cancellation stays reversible until period end");
    }

    #[test]
    fn derived_title_truncates_a_long_sentence_without_a_break() {
        let long = "a".repeat(120);
        let title = candidate(&long).derived_title();
        assert_eq!(title.chars().count(), 81);
        assert!(title.ends_with('\u{2026}'));
    }

    #[test]
    fn derived_title_keeps_a_short_single_sentence_whole() {
        let title = candidate("Short decision.").derived_title();
        assert_eq!(title, "Short decision");
    }
}
