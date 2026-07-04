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
    /// not asserted mappings.
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
}
