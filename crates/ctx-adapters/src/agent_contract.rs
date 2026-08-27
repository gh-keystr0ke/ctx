//! Shared vendor-agnostic logic behind every CLI-based `SemanticAgent`
//! adapter (prompt3.md PR-AGENT-001/PR-P05, `ADR-AGENT-001`): the prompt
//! contract, JSON-response parsing, and evidence/path validation are
//! identical regardless of which concrete CLI produced the response -- only
//! process invocation (`claude_code`, `codex`, `antigravity`) differs per
//! vendor. Malformed or unparseable output is rejected, never guessed at, so
//! a candidate can never enter the pending queue without traceable evidence
//! (PR-AI-004) -- and every candidate stays `INFERENCE` until a human
//! accepts it through `ctx verify` (Phase 6), never asserted here.
//!
//! Adding a new agent means adding a new thin transport implementing
//! [`AgentTransport`] plus a call into [`analyze`] -- never touching this
//! validation logic, and never touching `ctx-core`/`ctx-app`.

use std::collections::BTreeSet;
use std::ops::Range;

use ctx_core::{
    artifact::{ArtifactIdentity, ArtifactKind, ArtifactProvider},
    business::BusinessKind,
    knowledge::{
        AgentOutcome, AgentProvenance, CandidateReviewDecision, ClusterReview, KnowledgeCandidate,
        ReviewVerdict,
    },
    neighborhood::{ArtifactNeighborhood, render_neighborhood},
    verification::{StaleClaim, StaleClaimVerdict},
};
use serde::Deserialize;
use thiserror::Error;

/// Kept well under typical CLI-agent context windows: this is one artifact's
/// bounded neighborhood, not a whole-repository prompt (PR-AGENT-003).
pub(crate) const DEFAULT_TOKEN_BUDGET: usize = 6000;

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

/// Appended to the prompt, after [`SYSTEM_PROMPT`]'s own rule restricting
/// `implementation_candidates`/`test_candidates` to paths literally present
/// in the neighborhood, only when the caller passes
/// `allow_ungrounded_symbols = true` to [`analyze`]. Explicitly names and
/// overrides that specific rule rather than just contradicting it, since an
/// agent given two silently conflicting instructions in one prompt tends to
/// follow the stricter one anyway.
const ALLOW_UNGROUNDED_SYMBOLS_HINT: &str = "\n\nException to the implementation_candidates/test_candidates rule above: you may also propose implementation and test symbols based on your heuristic knowledge of the repository, even if they are not listed in the Valid artifact ids or Changed symbols above.";

#[derive(Debug, Error)]
pub enum AgentContractError {
    #[error("agent CLI could not be started: {0}")]
    Spawn(String),
    #[error("agent CLI exited with a failure: {0}")]
    ExitFailure(String),
    #[error("agent CLI output was not a single valid JSON object: {0}")]
    InvalidJson(String),
    #[error("agent prompt exceeds its configured byte budget: {0}")]
    PromptBudget(String),
}

/// Minimal process boundary every vendor's `SubprocessTransport` implements:
/// give it a prompt, get back raw text output. Tests inject canned output
/// instead of running a real subprocess for every case.
pub trait AgentTransport {
    /// # Errors
    /// Returns [`AgentContractError`] when the process cannot be started or
    /// exits with a failure status.
    fn run(&self, prompt: &str) -> Result<String, AgentContractError>;
}

/// Composes the bounded prompt, runs it through `transport`, and parses and
/// validates the response into a real [`AgentOutcome`] -- the entire body of
/// every vendor's `SemanticAgent::analyze` implementation.
///
/// # Errors
/// Returns [`AgentContractError`] when the transport fails or its output
/// cannot be parsed and validated as the expected contract.
///
/// # Panics
/// Never in practice: `DEFAULT_TOKEN_BUDGET` is a nonzero constant, the only
/// input `render_neighborhood` rejects.
pub fn analyze<T: AgentTransport>(
    transport: &T,
    neighborhood: &ArtifactNeighborhood,
    produced_at: &str,
    producer: &str,
    model: Option<String>,
    allow_ungrounded_symbols: bool,
) -> Result<AgentOutcome, AgentContractError> {
    let known_ids = known_artifact_ids(neighborhood);
    let rendered = render_neighborhood(neighborhood, DEFAULT_TOKEN_BUDGET)
        .expect("DEFAULT_TOKEN_BUDGET is a nonzero constant");
    let mut prompt = format!(
        "{SYSTEM_PROMPT}\n\nValid artifact ids for this neighborhood: {}\n\n{}",
        known_ids.join(", "),
        rendered.text
    );
    if allow_ungrounded_symbols {
        prompt.push_str(ALLOW_UNGROUNDED_SYMBOLS_HINT);
    }
    let fingerprint = blake3::hash(prompt.as_bytes()).to_hex().to_string();
    let raw = transport.run(&prompt)?;
    let object = extract_json_object(&raw)
        .ok_or_else(|| AgentContractError::InvalidJson("no JSON object found".to_owned()))?;
    let parsed: RawAgentOutput = serde_json::from_str(object)
        .map_err(|error| AgentContractError::InvalidJson(error.to_string()))?;
    let provenance = AgentProvenance {
        producer: producer.to_owned(),
        model,
        input_artifact_ids: known_ids,
        produced_at: produced_at.to_owned(),
        fingerprint,
    };
    Ok(to_outcome(
        parsed,
        neighborhood,
        &provenance,
        allow_ungrounded_symbols,
    ))
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
/// neighborhood (always enforced, regardless of `allow_ungrounded_symbols`),
/// an implementation/test candidate naming a path this neighborhood never
/// surfaced (enforced only when `allow_ungrounded_symbols` is `false`), or a
/// candidate left with no evidence at all once invalid entries are dropped.
/// A `relevant` outcome that loses every candidate this way degrades to
/// `InsufficientEvidence` rather than silently claiming `Relevant` with an
/// empty list (PR-P02, PR-AI-004).
fn to_outcome(
    parsed: RawAgentOutput,
    neighborhood: &ArtifactNeighborhood,
    provenance: &AgentProvenance,
    allow_ungrounded_symbols: bool,
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
                    .filter(|path| {
                        allow_ungrounded_symbols || known_symbols.contains(path.as_str())
                    })
                    .collect(),
                test_candidates: candidate
                    .test_candidates
                    .into_iter()
                    .filter(|path| allow_ungrounded_symbols || known_tests.contains(path.as_str()))
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

const REVIEW_SYSTEM_PROMPT: &str = r#"You are an independent second-opinion reviewer for AI-derived product-knowledge candidates. Another agent already extracted these candidates and judged them "relevant" to propose -- your job is to critically re-examine each one on its own merits and decide whether it should actually be accepted, not to rubber-stamp the earlier judgment.

Below is one cluster of candidates grouped together because their statements share enough vocabulary to plausibly describe the same underlying flow -- this grouping is only a lexical hint about what to review together, not a judgment that they must be merged.

For each candidate, decide "accept" (a well-grounded, evidence-backed statement worth keeping as real product knowledge) or "reject" (weak, unsupported by its own evidence, redundant with another candidate in this cluster, or not actually meaningful product knowledge).

Respond with exactly one JSON object and nothing else: no prose, no markdown fence, no explanation outside the JSON.

{"decisions":[{"fingerprint":"<exact fingerprint from below>","verdict":"accept|reject"}],"merged_statement":"<a single statement consolidating every accepted candidate in this cluster -- include this field only if two or more candidates are accepted AND they genuinely restate the same knowledge rather than being merely similar-sounding; omit the field entirely otherwise>"}

Rules:
- decisions must include exactly one entry for every candidate fingerprint listed below -- no more, no fewer, and never an invented fingerprint.
- Prefer "reject" over accepting a candidate whose evidence doesn't actually support its statement.
- Only include merged_statement when confident the accepted candidates describe the same knowledge, not just related knowledge -- when in doubt, omit it and let them stay separate documents."#;

/// Runs one [`crate::agent_contract`]-extracted cluster of candidates
/// through an independent second-opinion review (`ctx verify --knowledge
/// --auto`): the same [`AgentTransport`] boundary every vendor already
/// implements for extraction, a different prompt and response contract.
/// Never trusts a fingerprint the agent didn't see, and never accepts a
/// `decisions` list that doesn't cover the input candidates exactly once
/// each -- malformed review output is rejected outright rather than guessed
/// at, the same discipline [`analyze`] already applies to extraction output.
///
/// # Errors
/// Returns [`AgentContractError`] when the transport fails, the response
/// isn't valid JSON, or `decisions` names an unknown or duplicate
/// fingerprint, or omits one of the input candidates.
pub fn review<T: AgentTransport>(
    transport: &T,
    candidates: &[KnowledgeCandidate],
) -> Result<ClusterReview, AgentContractError> {
    let prompt = format!("{REVIEW_SYSTEM_PROMPT}\n\n{}", render_cluster(candidates));
    let raw = transport.run(&prompt)?;
    let object = extract_json_object(&raw)
        .ok_or_else(|| AgentContractError::InvalidJson("no JSON object found".to_owned()))?;
    let parsed: RawReviewOutput = serde_json::from_str(object)
        .map_err(|error| AgentContractError::InvalidJson(error.to_string()))?;

    let known: BTreeSet<&str> = candidates
        .iter()
        .map(|candidate| candidate.fingerprint.as_str())
        .collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut decisions = Vec::with_capacity(parsed.decisions.len());
    for raw_decision in parsed.decisions {
        if !known.contains(raw_decision.fingerprint.as_str()) {
            return Err(AgentContractError::InvalidJson(format!(
                "review decided an unknown fingerprint: {}",
                raw_decision.fingerprint
            )));
        }
        if !seen.insert(raw_decision.fingerprint.clone()) {
            return Err(AgentContractError::InvalidJson(format!(
                "review decided the same fingerprint twice: {}",
                raw_decision.fingerprint
            )));
        }
        decisions.push(CandidateReviewDecision {
            fingerprint: raw_decision.fingerprint,
            verdict: raw_decision.verdict,
        });
    }
    if seen.len() != known.len() {
        return Err(AgentContractError::InvalidJson(
            "review did not decide every candidate in the cluster".to_owned(),
        ));
    }

    Ok(ClusterReview {
        decisions,
        merged_statement: parsed.merged_statement,
    })
}

fn render_cluster(candidates: &[KnowledgeCandidate]) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
    for candidate in candidates {
        let _ = writeln!(
            text,
            "- fingerprint: {}\n  kind: {:?}\n  statement: {}",
            candidate.fingerprint, candidate.kind, candidate.statement
        );
        for evidence in &candidate.evidence {
            let _ = writeln!(text, "  evidence: {}", evidence.excerpt);
        }
    }
    text
}

#[derive(Deserialize)]
struct RawReviewOutput {
    decisions: Vec<RawReviewDecision>,
    #[serde(default)]
    merged_statement: Option<String>,
}

#[derive(Deserialize)]
struct RawReviewDecision {
    fingerprint: String,
    verdict: ReviewVerdict,
}

const STALE_CLAIM_REVIEW_SYSTEM_PROMPT: &str = r#"You are re-verifying already-established product-knowledge mappings whose underlying code changed since they were last confirmed. Each claim below already asserts that a specific piece of code implements, enforces, satisfies, or is covered by a specific Feature/Requirement/Invariant/Decision, and includes the product intent's own statement plus the current source of the code side -- decide whether that mapping is still accurate given what's shown, not by re-deriving it from scratch.

For each claim, decide "accept" (the current code shown still genuinely satisfies the stated product intent) or "reject" (the code changed in a way that no longer supports this mapping -- it looks like it moved elsewhere, was removed, or now does something materially different) based only on the text given below.

Respond with exactly one JSON object and nothing else: no prose, no markdown fence, no explanation outside the JSON.

{"decisions":[{"fingerprint":"<exact fingerprint from below>","verdict":"accept|reject","reasoning":"<one or two sentences citing what the shown code and product intent actually say>"}]}

Rules:
- decisions must include exactly one entry for every claim fingerprint listed below -- no more, no fewer, and never an invented fingerprint.
- reasoning is required for every decision, accept or reject, and must cite the claim's own shown code/intent text -- never a generic statement.
- A claim with no current code shown (the symbol could not be read) cannot be confirmed from its code -- prefer "reject" for it unless the product intent alone makes the mapping obviously still valid.
- Prefer "reject" when you cannot actually confirm the mapping still holds, rather than accepting on uncertainty."#;

const DEFAULT_STALE_REVIEW_PROMPT_BYTES: usize = 64 * 1024;
const DEFAULT_STALE_REVIEW_BATCH_CLAIMS: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaleClaimReviewBudget {
    pub max_prompt_bytes: usize,
    pub max_claims: usize,
}

impl Default for StaleClaimReviewBudget {
    fn default() -> Self {
        Self {
            max_prompt_bytes: DEFAULT_STALE_REVIEW_PROMPT_BYTES,
            max_claims: DEFAULT_STALE_REVIEW_BATCH_CLAIMS,
        }
    }
}

/// Runs every currently stale semantic claim through an independent
/// re-review (`ctx verify --stale`): the same [`AgentTransport`] boundary
/// every vendor already implements, a different prompt and response
/// contract from both extraction and knowledge-candidate review. Never
/// trusts a fingerprint the agent didn't see, and never accepts a
/// `decisions` list that doesn't cover the input claims exactly once each --
/// the same discipline [`review`] already applies.
///
/// # Errors
/// Returns [`AgentContractError`] when the transport fails, the response
/// isn't valid JSON, or `decisions` names an unknown or duplicate
/// fingerprint, or omits one of the input claims.
pub fn review_stale_claims<T: AgentTransport>(
    transport: &T,
    claims: &[StaleClaim],
) -> Result<Vec<StaleClaimVerdict>, AgentContractError> {
    review_stale_claims_with_budget(transport, claims, StaleClaimReviewBudget::default())
}

/// Reviews stale claims in independently validated, byte-bounded batches.
/// Every claim is included exactly once; an individual claim that cannot fit
/// the configured budget fails explicitly instead of producing an oversized
/// CLI argument or silently truncating evidence.
///
/// # Errors
/// Returns [`AgentContractError`] when a prompt cannot fit its budget, the
/// transport fails, or any batch response violates the review contract.
pub fn review_stale_claims_with_budget<T: AgentTransport>(
    transport: &T,
    claims: &[StaleClaim],
    budget: StaleClaimReviewBudget,
) -> Result<Vec<StaleClaimVerdict>, AgentContractError> {
    let mut verdicts = Vec::with_capacity(claims.len());
    for range in stale_claim_batches(claims, budget)? {
        verdicts.extend(review_stale_claim_batch(transport, &claims[range])?);
    }
    Ok(verdicts)
}

fn review_stale_claim_batch<T: AgentTransport>(
    transport: &T,
    claims: &[StaleClaim],
) -> Result<Vec<StaleClaimVerdict>, AgentContractError> {
    let prompt = format!(
        "{STALE_CLAIM_REVIEW_SYSTEM_PROMPT}\n\n{}",
        render_stale_claims(claims)
    );
    let raw = transport.run(&prompt)?;
    let object = extract_json_object(&raw)
        .ok_or_else(|| AgentContractError::InvalidJson("no JSON object found".to_owned()))?;
    let parsed: RawStaleClaimReviewOutput = serde_json::from_str(object)
        .map_err(|error| AgentContractError::InvalidJson(error.to_string()))?;

    let known: BTreeSet<&str> = claims
        .iter()
        .map(|claim| claim.fingerprint.as_str())
        .collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut decisions = Vec::with_capacity(parsed.decisions.len());
    for raw_decision in parsed.decisions {
        if !known.contains(raw_decision.fingerprint.as_str()) {
            return Err(AgentContractError::InvalidJson(format!(
                "stale-claim review decided an unknown fingerprint: {}",
                raw_decision.fingerprint
            )));
        }
        if !seen.insert(raw_decision.fingerprint.clone()) {
            return Err(AgentContractError::InvalidJson(format!(
                "stale-claim review decided the same fingerprint twice: {}",
                raw_decision.fingerprint
            )));
        }
        decisions.push(StaleClaimVerdict {
            fingerprint: raw_decision.fingerprint,
            verdict: raw_decision.verdict,
            reasoning: raw_decision.reasoning,
        });
    }
    if seen.len() != known.len() {
        return Err(AgentContractError::InvalidJson(
            "stale-claim review did not decide every claim".to_owned(),
        ));
    }

    Ok(decisions)
}

fn stale_claim_batches(
    claims: &[StaleClaim],
    budget: StaleClaimReviewBudget,
) -> Result<Vec<Range<usize>>, AgentContractError> {
    if budget.max_prompt_bytes <= STALE_CLAIM_REVIEW_SYSTEM_PROMPT.len() + 2
        || budget.max_claims == 0
    {
        return Err(AgentContractError::PromptBudget(
            "budget must fit the system prompt and at least one claim".to_owned(),
        ));
    }
    let base_bytes = STALE_CLAIM_REVIEW_SYSTEM_PROMPT.len() + 2;
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut bytes = base_bytes;
    for (index, claim) in claims.iter().enumerate() {
        let claim_bytes = render_stale_claim(claim).len();
        if base_bytes + claim_bytes > budget.max_prompt_bytes {
            return Err(AgentContractError::PromptBudget(format!(
                "claim '{}' needs {} bytes but the limit is {}",
                claim.fingerprint,
                base_bytes + claim_bytes,
                budget.max_prompt_bytes
            )));
        }
        let count = index - start;
        if count == budget.max_claims || bytes + claim_bytes > budget.max_prompt_bytes {
            ranges.push(start..index);
            start = index;
            bytes = base_bytes;
        }
        bytes += claim_bytes;
    }
    if start < claims.len() {
        ranges.push(start..claims.len());
    }
    Ok(ranges)
}

fn render_stale_claims(claims: &[StaleClaim]) -> String {
    let mut text = String::new();
    for claim in claims {
        text.push_str(&render_stale_claim(claim));
    }
    text
}

fn render_stale_claim(claim: &StaleClaim) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
    let _ = writeln!(
        text,
        "- fingerprint: {}\n  relation: {:?}\n  source: {} ({:?})\n  target: {} ({:?})",
        claim.fingerprint,
        claim.relation,
        claim.source.identifier,
        claim.source.kind,
        claim.target.identifier,
        claim.target.kind
    );
    for locator in &claim.evidence_locators {
        let _ = writeln!(text, "  declared at: {locator}");
    }
    let _ = writeln!(text, "  product intent: {}", claim.intent_statement);
    if let Some(excerpt) = &claim.symbol_excerpt {
        let _ = writeln!(text, "  current code:\n{excerpt}");
    }
    text
}

#[derive(Deserialize)]
struct RawStaleClaimReviewOutput {
    decisions: Vec<RawStaleClaimDecision>,
}

#[derive(Deserialize)]
struct RawStaleClaimDecision {
    fingerprint: String,
    verdict: ReviewVerdict,
    reasoning: String,
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use ctx_core::{
        artifact::{Artifact, ArtifactIdentity, ArtifactKind, ArtifactProvider},
        neighborhood::build_neighborhood,
    };

    use super::*;

    struct FakeTransport {
        response: String,
    }

    impl AgentTransport for FakeTransport {
        fn run(&self, _prompt: &str) -> Result<String, AgentContractError> {
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
            project: ctx_core::domain::Project("billing/subscriptions".to_owned()),
            title: "Cancellation removes prepaid access".to_owned(),
            body: "A cancelled prepaid subscription must remain usable until paid_until."
                .to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: ctx_core::domain::Url("gitlab:317".to_owned()),
            content_hash: "hash".to_owned(),
        }
    }

    fn run(
        response: &str,
        neighborhood: &ArtifactNeighborhood,
    ) -> Result<AgentOutcome, AgentContractError> {
        run_with_ungrounded_symbols(response, neighborhood, false)
    }

    fn run_with_ungrounded_symbols(
        response: &str,
        neighborhood: &ArtifactNeighborhood,
        allow_ungrounded_symbols: bool,
    ) -> Result<AgentOutcome, AgentContractError> {
        let transport = FakeTransport {
            response: response.to_owned(),
        };
        analyze(
            &transport,
            neighborhood,
            "2026-08-21T00:00:00Z",
            "test-agent",
            None,
            allow_ungrounded_symbols,
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

        let outcome = run(response, &neighborhood).expect("parsed outcome");

        let AgentOutcome::Relevant(candidates) = outcome else {
            panic!("expected a relevant outcome");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].evidence[0].identity, subject.identity);
        assert_eq!(candidates[0].provenance.producer, "test-agent");
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

        let outcome = run(response, &neighborhood).expect("parsed outcome");

        assert_eq!(outcome, AgentOutcome::InsufficientEvidence);
    }

    #[test]
    fn an_implementation_candidate_outside_the_neighborhood_is_dropped_by_default() {
        let subject = issue();
        let neighborhood = build_neighborhood(
            &subject,
            &[],
            std::slice::from_ref(&subject),
            &ctx_core::graph::GraphSnapshot::default(),
        );
        let response = r#"{"outcome":"relevant","candidates":[{"kind":"requirement","statement":"Cancellation preserves paid access until paid_until.","evidence":[{"artifact_id":"gitlab:issue:317","locator":"body","excerpt":"must remain usable until paid_until"}],"implementation_candidates":["src/billing/subscriptions.rs"]}]}"#;

        let outcome = run(response, &neighborhood).expect("parsed outcome");

        let AgentOutcome::Relevant(candidates) = outcome else {
            panic!("expected a relevant outcome");
        };
        assert!(candidates[0].implementation_candidates.is_empty());
    }

    #[test]
    fn allow_ungrounded_symbols_keeps_an_implementation_candidate_outside_the_neighborhood() {
        let subject = issue();
        let neighborhood = build_neighborhood(
            &subject,
            &[],
            std::slice::from_ref(&subject),
            &ctx_core::graph::GraphSnapshot::default(),
        );
        let response = r#"{"outcome":"relevant","candidates":[{"kind":"requirement","statement":"Cancellation preserves paid access until paid_until.","evidence":[{"artifact_id":"gitlab:issue:317","locator":"body","excerpt":"must remain usable until paid_until"}],"implementation_candidates":["src/billing/subscriptions.rs"],"test_candidates":["src/billing/subscriptions_test.rs"]}]}"#;

        let outcome =
            run_with_ungrounded_symbols(response, &neighborhood, true).expect("parsed outcome");

        let AgentOutcome::Relevant(candidates) = outcome else {
            panic!("expected a relevant outcome");
        };
        assert_eq!(
            candidates[0].implementation_candidates,
            vec!["src/billing/subscriptions.rs".to_owned()]
        );
        assert_eq!(
            candidates[0].test_candidates,
            vec!["src/billing/subscriptions_test.rs".to_owned()]
        );
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
            run(r#"{"outcome":"not_relevant"}"#, &neighborhood).expect("parsed"),
            AgentOutcome::NotRelevant
        );
        assert_eq!(
            run(r#"{"outcome":"insufficient_evidence"}"#, &neighborhood).expect("parsed"),
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

        let result = run("not json at all, no braces here", &neighborhood);

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

        let outcome = run(response, &neighborhood).expect("parsed outcome");

        assert_eq!(outcome, AgentOutcome::NotRelevant);
    }

    fn review_candidate(fingerprint: &str, statement: &str) -> KnowledgeCandidate {
        KnowledgeCandidate {
            fingerprint: fingerprint.to_owned(),
            kind: BusinessKind::Requirement,
            statement: statement.to_owned(),
            evidence: vec![ctx_core::artifact::ArtifactRef {
                identity: ArtifactIdentity {
                    provider: ArtifactProvider::GitLab,
                    kind: ArtifactKind::Issue,
                    external_id: "317".to_owned(),
                },
                locator: "body".to_owned(),
                excerpt: "excerpt".to_owned(),
            }],
            implementation_candidates: Vec::new(),
            test_candidates: Vec::new(),
            provenance: AgentProvenance {
                producer: "test-agent".to_owned(),
                model: None,
                input_artifact_ids: vec!["gitlab:issue:317".to_owned()],
                produced_at: "2026-08-21T00:00:00Z".to_owned(),
                fingerprint: "fp".to_owned(),
            },
        }
    }

    #[test]
    fn review_accepts_and_merges_when_the_agent_says_so() {
        let candidates = vec![
            review_candidate("fp1", "Cancellation preserves paid access."),
            review_candidate("fp2", "Cancellation must preserve paid access."),
        ];
        let transport = FakeTransport {
            response: r#"{"decisions":[{"fingerprint":"fp1","verdict":"accept"},{"fingerprint":"fp2","verdict":"accept"}],"merged_statement":"Cancellation preserves paid access."}"#.to_owned(),
        };

        let result = review(&transport, &candidates).expect("parsed review");

        assert_eq!(result.decisions.len(), 2);
        assert!(
            result
                .decisions
                .iter()
                .all(|decision| decision.verdict == ReviewVerdict::Accept)
        );
        assert_eq!(
            result.merged_statement,
            Some("Cancellation preserves paid access.".to_owned())
        );
    }

    #[test]
    fn review_can_reject_a_candidate_extraction_already_called_relevant() {
        let candidates = vec![
            review_candidate("fp1", "Cancellation preserves paid access."),
            review_candidate("fp2", "Vague unsupported statement."),
        ];
        let transport = FakeTransport {
            response: r#"{"decisions":[{"fingerprint":"fp1","verdict":"accept"},{"fingerprint":"fp2","verdict":"reject"}]}"#
                .to_owned(),
        };

        let result = review(&transport, &candidates).expect("parsed review");

        assert_eq!(result.merged_statement, None);
        let fp2 = result
            .decisions
            .iter()
            .find(|decision| decision.fingerprint == "fp2")
            .expect("fp2 decision");
        assert_eq!(fp2.verdict, ReviewVerdict::Reject);
    }

    #[test]
    fn review_rejects_an_invented_fingerprint_not_trusted() {
        let candidates = vec![review_candidate(
            "fp1",
            "Cancellation preserves paid access.",
        )];
        let transport = FakeTransport {
            response: r#"{"decisions":[{"fingerprint":"fp-invented","verdict":"accept"}]}"#
                .to_owned(),
        };

        let result = review(&transport, &candidates);

        assert!(result.is_err());
    }

    #[test]
    fn review_rejects_a_decision_list_missing_a_candidate() {
        let candidates = vec![
            review_candidate("fp1", "Cancellation preserves paid access."),
            review_candidate("fp2", "A second, distinct statement."),
        ];
        let transport = FakeTransport {
            response: r#"{"decisions":[{"fingerprint":"fp1","verdict":"accept"}]}"#.to_owned(),
        };

        let result = review(&transport, &candidates);

        assert!(result.is_err());
    }

    fn stale_claim(fingerprint: &str) -> StaleClaim {
        use ctx_core::{domain::NodeKind, graph::NodeSummary};

        StaleClaim {
            fingerprint: fingerprint.to_owned(),
            relation: ctx_core::domain::RelationKind::Implements,
            source: NodeSummary {
                stable_key: "symbol:python:billing.cancel:Function".to_owned(),
                kind: NodeKind::CodeSymbol,
                identifier: "billing.cancel".to_owned(),
                name: "cancel".to_owned(),
                visibility: None,
            },
            target: NodeSummary {
                stable_key: "intent:REQ-SUB-014".to_owned(),
                kind: NodeKind::Requirement,
                identifier: "REQ-SUB-014".to_owned(),
                name: "Keep access".to_owned(),
                visibility: None,
            },
            evidence_locators: vec![
                ".context/requirements/cancel.yaml#implementation[0]".to_owned(),
            ],
            intent_statement: "Keep access until paid_until".to_owned(),
            symbol_excerpt: Some("def cancel():\n    ...".to_owned()),
        }
    }

    #[derive(Default)]
    struct EchoStaleReviewTransport {
        prompts: RefCell<Vec<String>>,
    }

    impl AgentTransport for EchoStaleReviewTransport {
        fn run(&self, prompt: &str) -> Result<String, AgentContractError> {
            self.prompts.borrow_mut().push(prompt.to_owned());
            let decisions: Vec<_> = prompt
                .lines()
                .filter_map(|line| line.strip_prefix("- fingerprint: "))
                .map(|fingerprint| {
                    serde_json::json!({
                        "fingerprint": fingerprint,
                        "verdict": "accept",
                        "reasoning": "the shown code still supports the shown intent"
                    })
                })
                .collect();
            Ok(serde_json::json!({"decisions": decisions}).to_string())
        }
    }

    #[test]
    fn stale_claim_review_batches_without_dropping_claims() {
        let claims: Vec<_> = (1..=5)
            .map(|index| stale_claim(&format!("fp{index}")))
            .collect();
        let transport = EchoStaleReviewTransport::default();
        let budget = StaleClaimReviewBudget {
            max_prompt_bytes: 64 * 1024,
            max_claims: 2,
        };

        let result = review_stale_claims_with_budget(&transport, &claims, budget)
            .expect("three bounded batches");

        assert_eq!(result.len(), claims.len());
        let prompts = transport.prompts.borrow();
        assert_eq!(prompts.len(), 3);
        assert!(
            prompts
                .iter()
                .all(|prompt| prompt.len() <= budget.max_prompt_bytes)
        );
    }

    #[test]
    fn one_claim_larger_than_the_prompt_budget_fails_explicitly() {
        let transport = EchoStaleReviewTransport::default();
        let claims = vec![stale_claim("fp1")];
        let error = review_stale_claims_with_budget(
            &transport,
            &claims,
            StaleClaimReviewBudget {
                max_prompt_bytes: STALE_CLAIM_REVIEW_SYSTEM_PROMPT.len() + 10,
                max_claims: 1,
            },
        )
        .expect_err("claim cannot be silently truncated");

        assert!(matches!(error, AgentContractError::PromptBudget(_)));
        assert!(transport.prompts.borrow().is_empty());
    }

    #[test]
    fn review_stale_claims_accepts_when_the_agent_confirms_the_mapping_still_holds() {
        let claims = vec![stale_claim("fp1")];
        let transport = FakeTransport {
            response: r#"{"decisions":[{"fingerprint":"fp1","verdict":"accept","reasoning":"billing.cancel still preserves paid access in the current code."}]}"#.to_owned(),
        };

        let result = review_stale_claims(&transport, &claims).expect("parsed review");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].fingerprint, "fp1");
        assert_eq!(result[0].verdict, ReviewVerdict::Accept);
        assert!(result[0].reasoning.contains("billing.cancel"));
    }

    #[test]
    fn review_stale_claims_can_reject_a_mapping_that_no_longer_holds() {
        let claims = vec![stale_claim("fp1")];
        let transport = FakeTransport {
            response: r#"{"decisions":[{"fingerprint":"fp1","verdict":"reject","reasoning":"billing.cancel was removed; the logic now lives in billing.terminate."}]}"#.to_owned(),
        };

        let result = review_stale_claims(&transport, &claims).expect("parsed review");

        assert_eq!(result[0].verdict, ReviewVerdict::Reject);
    }

    #[test]
    fn review_stale_claims_rejects_an_invented_fingerprint_not_trusted() {
        let claims = vec![stale_claim("fp1")];
        let transport = FakeTransport {
            response: r#"{"decisions":[{"fingerprint":"fp-invented","verdict":"accept","reasoning":"..."}]}"#
                .to_owned(),
        };

        let result = review_stale_claims(&transport, &claims);

        assert!(result.is_err());
    }

    #[test]
    fn review_stale_claims_rejects_a_decision_list_missing_a_claim() {
        let claims = vec![stale_claim("fp1"), stale_claim("fp2")];
        let transport = FakeTransport {
            response:
                r#"{"decisions":[{"fingerprint":"fp1","verdict":"accept","reasoning":"..."}]}"#
                    .to_owned(),
        };

        let result = review_stale_claims(&transport, &claims);

        assert!(result.is_err());
    }
}
