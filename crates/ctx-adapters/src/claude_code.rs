//! Claude Code CLI agent adapter (prompt3.md PR-AGENT-001): the first
//! concrete [`SemanticAgent`], shelling out to `claude -p` in headless mode
//! with one bounded artifact neighborhood and a strict JSON output contract.
//! Malformed or unparseable output is rejected, never guessed at, so a
//! candidate can never enter the pending queue without traceable evidence
//! (PR-AI-004) -- and every candidate the agent proposes stays `INFERENCE`
//! until a human accepts it through `ctx verify` (Phase 6), never asserted
//! here.
//!
//! Process invocation goes through [`ClaudeCliTransport`] so the JSON
//! contract's parsing/validation logic can be tested without spawning a real
//! `claude` process every time; [`SubprocessTransport`] is the real
//! implementation.

use std::process::Command;

use ctx_app::ports::{PortError, SemanticAgent};
use ctx_core::{
    artifact::{ArtifactIdentity, ArtifactKind, ArtifactProvider},
    business::BusinessKind,
    knowledge::{AgentOutcome, AgentProvenance, KnowledgeCandidate},
    neighborhood::{ArtifactNeighborhood, render_neighborhood},
};
use serde::Deserialize;
use thiserror::Error;

/// Kept well under typical CLI-agent context windows: this is one artifact's
/// bounded neighborhood, not a whole-repository prompt (PR-AGENT-003).
const DEFAULT_TOKEN_BUDGET: usize = 6000;

const SYSTEM_PROMPT: &str = r#"You are analyzing one bounded artifact neighborhood from a software repository to decide whether it states new product knowledge -- a Feature, Requirement, Invariant, or Decision -- that is not already covered by the "Already-known related product knowledge" section below, if present.

Respond with exactly one JSON object and nothing else: no prose, no markdown fence, no explanation outside the JSON.

If the artifact states no new product knowledge, respond:
{"outcome":"not_relevant"}

If the artifact might be relevant but there is not enough concrete evidence in the text to state a candidate, respond:
{"outcome":"insufficient_evidence"}

If the artifact states new product knowledge, respond:
{"outcome":"relevant","candidates":[{"kind":"feature|requirement|invariant|decision","statement":"...","evidence":[{"artifact_id":"<one of the artifact ids listed below>","locator":"title|body","excerpt":"<short verbatim excerpt from that artifact>"}],"implementation_candidates":["<only paths from the Changed symbols list below>"],"test_candidates":["<only paths from the Tests list below>"]}]}

Rules:
- Every candidate's evidence array must be non-empty and every excerpt must be copied verbatim from the artifact text below -- never paraphrased or invented.
- Every evidence artifact_id must be one of the ids listed below. Never invent an id.
- implementation_candidates and test_candidates may only name paths that literally appear in the Changed symbols / Tests sections below. Omit either list entirely if nothing applies.
- Prefer "not_relevant" or "insufficient_evidence" over a fabricated or speculative candidate."#;

#[derive(Debug, Error)]
pub enum ClaudeCodeError {
    #[error("claude CLI could not be started: {0}")]
    Spawn(String),
    #[error("claude CLI exited with a failure: {0}")]
    ExitFailure(String),
    #[error("claude CLI output was not a single valid JSON object: {0}")]
    InvalidJson(String),
}

/// Minimal process boundary: the real implementation
/// ([`SubprocessTransport`]) spawns `claude`, while tests inject canned
/// output instead of running a real subprocess for every case.
pub trait ClaudeCliTransport {
    /// # Errors
    /// Returns [`ClaudeCodeError`] when the process cannot be started or
    /// exits with a failure status.
    fn run(&self, prompt: &str) -> Result<String, ClaudeCodeError>;
}

pub struct SubprocessTransport {
    binary: String,
}

impl SubprocessTransport {
    #[must_use]
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

impl Default for SubprocessTransport {
    fn default() -> Self {
        Self::new("claude")
    }
}

impl ClaudeCliTransport for SubprocessTransport {
    fn run(&self, prompt: &str) -> Result<String, ClaudeCodeError> {
        let output = Command::new(&self.binary)
            .arg("-p")
            .arg(prompt)
            .output()
            .map_err(|error| ClaudeCodeError::Spawn(error.to_string()))?;
        if !output.status.success() {
            return Err(ClaudeCodeError::ExitFailure(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

pub struct ClaudeCodeAgent<T> {
    transport: T,
    model: Option<String>,
}

impl<T: ClaudeCliTransport> ClaudeCodeAgent<T> {
    pub fn new(transport: T, model: Option<String>) -> Self {
        Self { transport, model }
    }

    /// # Errors
    /// Returns [`ClaudeCodeError`] when the process fails or its output
    /// cannot be parsed and validated as the expected contract.
    ///
    /// # Panics
    /// Never in practice: `DEFAULT_TOKEN_BUDGET` is a nonzero constant, the
    /// only input `render_neighborhood` rejects.
    pub fn analyze_neighborhood(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
    ) -> Result<AgentOutcome, ClaudeCodeError> {
        let known_ids = known_artifact_ids(neighborhood);
        let rendered = render_neighborhood(neighborhood, DEFAULT_TOKEN_BUDGET)
            .expect("DEFAULT_TOKEN_BUDGET is a nonzero constant");
        let prompt = format!(
            "{SYSTEM_PROMPT}\n\nValid artifact ids for this neighborhood: {}\n\n{}",
            known_ids.join(", "),
            rendered.text
        );
        let fingerprint = blake3::hash(prompt.as_bytes()).to_hex().to_string();
        let raw = self.transport.run(&prompt)?;
        let object = extract_json_object(&raw)
            .ok_or_else(|| ClaudeCodeError::InvalidJson("no JSON object found".to_owned()))?;
        let parsed: RawAgentOutput = serde_json::from_str(object)
            .map_err(|error| ClaudeCodeError::InvalidJson(error.to_string()))?;
        let provenance = AgentProvenance {
            producer: "claude-code".to_owned(),
            model: self.model.clone(),
            input_artifact_ids: known_ids,
            produced_at: produced_at.to_owned(),
            fingerprint,
        };
        Ok(to_outcome(parsed, neighborhood, &provenance))
    }
}

impl<T: ClaudeCliTransport> SemanticAgent for ClaudeCodeAgent<T> {
    fn analyze(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
    ) -> Result<AgentOutcome, PortError> {
        self.analyze_neighborhood(neighborhood, produced_at)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

fn known_artifact_ids(neighborhood: &ArtifactNeighborhood) -> Vec<String> {
    let mut ids = vec![format_identity(&neighborhood.subject.identity)];
    ids.extend(
        neighborhood
            .linked_artifacts
            .iter()
            .map(|linked| format_identity(&linked.artifact.identity)),
    );
    ids
}

fn format_identity(identity: &ArtifactIdentity) -> String {
    format!(
        "{}:{}:{}",
        provider_tag(identity.provider),
        kind_tag(identity.kind),
        identity.external_id
    )
}

const fn provider_tag(provider: ArtifactProvider) -> &'static str {
    match provider {
        ArtifactProvider::Git => "git",
        ArtifactProvider::GitLab => "gitlab",
        ArtifactProvider::GitHub => "github",
        ArtifactProvider::Jira => "jira",
        ArtifactProvider::Code => "code",
    }
}

const fn kind_tag(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Commit => "commit",
        ArtifactKind::Branch => "branch",
        ArtifactKind::Issue => "issue",
        ArtifactKind::MergeRequest => "merge_request",
        ArtifactKind::PullRequest => "pull_request",
        ArtifactKind::Comment => "comment",
        ArtifactKind::ReviewComment => "review_comment",
        ArtifactKind::CodeComment => "code_comment",
        ArtifactKind::Docstring => "docstring",
        ArtifactKind::Documentation => "documentation",
    }
}

/// Finds the first top-level `{...}` object in `raw`, defensively tolerating
/// a stray markdown fence or trailing text around it despite the system
/// prompt asking for bare JSON -- but never attempting to repair malformed
/// JSON inside that span; `serde_json` alone decides validity.
fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end >= start).then(|| &raw[start..=end])
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum RawAgentOutput {
    Relevant {
        #[serde(default)]
        candidates: Vec<RawCandidate>,
    },
    NotRelevant,
    InsufficientEvidence,
}

#[derive(Deserialize)]
struct RawCandidate {
    kind: BusinessKind,
    statement: String,
    #[serde(default)]
    evidence: Vec<RawEvidence>,
    #[serde(default)]
    implementation_candidates: Vec<String>,
    #[serde(default)]
    test_candidates: Vec<String>,
}

#[derive(Deserialize)]
struct RawEvidence {
    artifact_id: String,
    locator: String,
    excerpt: String,
}

/// Converts the raw, untrusted contract into a real [`AgentOutcome`],
/// dropping (never fabricating a substitute for) anything that fails
/// validation: an evidence entry citing an artifact id outside this
/// neighborhood, an implementation/test candidate naming a path this
/// neighborhood never surfaced, or a candidate left with no evidence at all
/// once invalid entries are dropped. A `relevant` outcome that loses every
/// candidate this way degrades to `InsufficientEvidence` rather than
/// silently claiming `Relevant` with an empty list (PR-P02, PR-AI-004).
fn to_outcome(
    parsed: RawAgentOutput,
    neighborhood: &ArtifactNeighborhood,
    provenance: &AgentProvenance,
) -> AgentOutcome {
    let candidates = match parsed {
        RawAgentOutput::NotRelevant => return AgentOutcome::NotRelevant,
        RawAgentOutput::InsufficientEvidence => return AgentOutcome::InsufficientEvidence,
        RawAgentOutput::Relevant { candidates } => candidates,
    };
    let known_ids: std::collections::BTreeSet<_> =
        known_artifact_ids(neighborhood).into_iter().collect();
    let known_symbols: std::collections::BTreeSet<_> = neighborhood
        .changed_symbols
        .iter()
        .map(String::as_str)
        .collect();
    let known_tests: std::collections::BTreeSet<_> = neighborhood
        .nearby_tests
        .iter()
        .map(String::as_str)
        .collect();

    let accepted: Vec<KnowledgeCandidate> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let evidence: Vec<_> = candidate
                .evidence
                .into_iter()
                .filter(|item| known_ids.contains(&item.artifact_id))
                .filter_map(|item| {
                    resolve_identity(&item.artifact_id, neighborhood).map(|identity| {
                        ctx_core::artifact::ArtifactRef {
                            identity,
                            locator: item.locator,
                            excerpt: item.excerpt,
                        }
                    })
                })
                .collect();
            if evidence.is_empty() {
                return None;
            }
            Some(KnowledgeCandidate {
                fingerprint: KnowledgeCandidate::fingerprint_for(
                    candidate.kind,
                    &candidate.statement,
                ),
                kind: candidate.kind,
                statement: candidate.statement,
                evidence,
                implementation_candidates: candidate
                    .implementation_candidates
                    .into_iter()
                    .filter(|path| known_symbols.contains(path.as_str()))
                    .collect(),
                test_candidates: candidate
                    .test_candidates
                    .into_iter()
                    .filter(|path| known_tests.contains(path.as_str()))
                    .collect(),
                provenance: provenance.clone(),
            })
        })
        .collect();

    if accepted.is_empty() {
        AgentOutcome::InsufficientEvidence
    } else {
        AgentOutcome::Relevant(accepted)
    }
}

fn resolve_identity(id: &str, neighborhood: &ArtifactNeighborhood) -> Option<ArtifactIdentity> {
    if format_identity(&neighborhood.subject.identity) == id {
        return Some(neighborhood.subject.identity.clone());
    }
    neighborhood
        .linked_artifacts
        .iter()
        .find(|linked| format_identity(&linked.artifact.identity) == id)
        .map(|linked| linked.artifact.identity.clone())
}

#[cfg(test)]
mod tests {
    use ctx_core::{
        artifact::{Artifact, ArtifactIdentity, ArtifactKind, ArtifactProvider},
        neighborhood::build_neighborhood,
    };

    use super::*;

    struct FakeTransport {
        response: String,
    }

    impl ClaudeCliTransport for FakeTransport {
        fn run(&self, _prompt: &str) -> Result<String, ClaudeCodeError> {
            Ok(self.response.clone())
        }
    }

    fn issue() -> Artifact {
        Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: ArtifactKind::Issue,
                external_id: "317".to_owned(),
            },
            project: "billing/subscriptions".to_owned(),
            title: "Cancellation removes prepaid access".to_owned(),
            body: "A cancelled prepaid subscription must remain usable until paid_until."
                .to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: "gitlab:317".to_owned(),
            content_hash: "hash".to_owned(),
        }
    }

    fn agent(response: &str) -> ClaudeCodeAgent<FakeTransport> {
        ClaudeCodeAgent::new(
            FakeTransport {
                response: response.to_owned(),
            },
            None,
        )
    }

    #[test]
    fn a_well_formed_relevant_response_becomes_a_grounded_candidate() {
        let subject = issue();
        let neighborhood = build_neighborhood(
            &subject,
            &[],
            std::slice::from_ref(&subject),
            &ctx_core::graph::GraphSnapshot::default(),
        );
        let response = r#"{"outcome":"relevant","candidates":[{"kind":"requirement","statement":"Cancellation preserves paid access until paid_until.","evidence":[{"artifact_id":"gitlab:issue:317","locator":"body","excerpt":"must remain usable until paid_until"}]}]}"#;

        let outcome = agent(response)
            .analyze_neighborhood(&neighborhood, "2026-08-21T00:00:00Z")
            .expect("parsed outcome");

        let AgentOutcome::Relevant(candidates) = outcome else {
            panic!("expected a relevant outcome");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].evidence[0].identity, subject.identity);
        assert_eq!(candidates[0].provenance.producer, "claude-code");
    }

    #[test]
    fn an_evidence_id_outside_the_neighborhood_is_dropped_not_trusted() {
        let subject = issue();
        let neighborhood = build_neighborhood(
            &subject,
            &[],
            std::slice::from_ref(&subject),
            &ctx_core::graph::GraphSnapshot::default(),
        );
        let response = r#"{"outcome":"relevant","candidates":[{"kind":"requirement","statement":"fabricated","evidence":[{"artifact_id":"gitlab:issue:999","locator":"body","excerpt":"invented"}]}]}"#;

        let outcome = agent(response)
            .analyze_neighborhood(&neighborhood, "2026-08-21T00:00:00Z")
            .expect("parsed outcome");

        assert_eq!(outcome, AgentOutcome::InsufficientEvidence);
    }

    #[test]
    fn not_relevant_and_insufficient_evidence_pass_through_unchanged() {
        let subject = issue();
        let neighborhood = build_neighborhood(
            &subject,
            &[],
            std::slice::from_ref(&subject),
            &ctx_core::graph::GraphSnapshot::default(),
        );

        assert_eq!(
            agent(r#"{"outcome":"not_relevant"}"#)
                .analyze_neighborhood(&neighborhood, "2026-08-21T00:00:00Z")
                .expect("parsed"),
            AgentOutcome::NotRelevant
        );
        assert_eq!(
            agent(r#"{"outcome":"insufficient_evidence"}"#)
                .analyze_neighborhood(&neighborhood, "2026-08-21T00:00:00Z")
                .expect("parsed"),
            AgentOutcome::InsufficientEvidence
        );
    }

    #[test]
    fn malformed_output_is_rejected_not_guessed_at() {
        let subject = issue();
        let neighborhood = build_neighborhood(
            &subject,
            &[],
            std::slice::from_ref(&subject),
            &ctx_core::graph::GraphSnapshot::default(),
        );

        let result = agent("not json at all, no braces here")
            .analyze_neighborhood(&neighborhood, "2026-08-21T00:00:00Z");

        assert!(result.is_err());
    }

    #[test]
    fn a_markdown_fenced_response_is_still_extracted() {
        let subject = issue();
        let neighborhood = build_neighborhood(
            &subject,
            &[],
            std::slice::from_ref(&subject),
            &ctx_core::graph::GraphSnapshot::default(),
        );
        let response = "```json\n{\"outcome\":\"not_relevant\"}\n```";

        let outcome = agent(response)
            .analyze_neighborhood(&neighborhood, "2026-08-21T00:00:00Z")
            .expect("parsed outcome");

        assert_eq!(outcome, AgentOutcome::NotRelevant);
    }

    /// Not a unit test of the JSON contract (those above cover that): a
    /// dedicated check that the real subprocess boundary itself invokes
    /// `claude -p <prompt>` and surfaces its stdout, using a fake script
    /// standing in for the real binary so this suite never depends on a
    /// real `claude` installation.
    #[test]
    fn subprocess_transport_invokes_claude_p_and_returns_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("claude");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ]; then echo \"{\\\"outcome\\\":\\\"not_relevant\\\",\\\"prompt_arg\\\":\\\"$2\\\"}\"; else exit 1; fi\n",
        )
        .expect("write fake claude script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script_path, permissions).expect("chmod");
        }

        let transport = SubprocessTransport::new(script_path.to_string_lossy().into_owned());
        let output = transport.run("hello neighborhood").expect("script output");

        assert!(output.contains("not_relevant"));
        assert!(output.contains("hello neighborhood"));
    }
}
