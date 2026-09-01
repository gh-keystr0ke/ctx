//! One composition point for selecting a supported agent CLI.

use std::env;

use ctx_adapters::{
    antigravity::{AntigravityAgent, SubprocessTransport as AntigravityTransport},
    claude_code::{ClaudeCodeAgent, SubprocessTransport as ClaudeTransport},
    codex::{CodexAgent, SubprocessTransport as CodexTransport},
};
use ctx_app::ports::{KnowledgeReviewAgent, PortError, SemanticAgent, StaleClaimReviewAgent};
use ctx_core::{
    knowledge::{AgentOutcome, ClusterReview, KnowledgeCandidate},
    neighborhood::ArtifactNeighborhood,
    verification::{StaleClaim, StaleClaimVerdict},
};

use crate::agent_pacing::AgentPacer;

enum AgentKind {
    Claude(ClaudeCodeAgent<ClaudeTransport>),
    Codex(CodexAgent<CodexTransport>),
    Antigravity(AntigravityAgent<AntigravityTransport>),
}

pub(crate) struct ConfiguredAgent {
    inner: AgentKind,
    pacer: Option<AgentPacer>,
}

impl ConfiguredAgent {
    pub(crate) fn from_name(
        name: &str,
        model: Option<String>,
        verbose: bool,
        siga_siga: bool,
    ) -> Result<Self, String> {
        let inner = match name {
            "claude" => AgentKind::Claude(ClaudeCodeAgent::new(
                ClaudeTransport::new(
                    binary("CTX_CLAUDE_CLI_BINARY", "claude"),
                    verbose,
                    model.clone(),
                ),
                model,
            )),
            "codex" => AgentKind::Codex(CodexAgent::new(
                CodexTransport::new(
                    binary("CTX_CODEX_CLI_BINARY", "codex"),
                    verbose,
                    model.clone(),
                ),
                model,
            )),
            "antigravity" => AgentKind::Antigravity(AntigravityAgent::new(
                AntigravityTransport::new(
                    binary("CTX_ANTIGRAVITY_CLI_BINARY", "agy"),
                    verbose,
                    model.clone(),
                ),
                model,
            )),
            other => return Err(other.to_owned()),
        };
        Ok(Self {
            inner,
            pacer: siga_siga.then(AgentPacer::night_mode),
        })
    }

    fn pace(&self) {
        if let Some(pacer) = &self.pacer {
            pacer.pace();
        }
    }
}

fn binary(variable: &str, default: &str) -> String {
    env::var(variable).unwrap_or_else(|_| default.to_owned())
}

impl SemanticAgent for ConfiguredAgent {
    fn input_fingerprint(
        &self,
        neighborhood: &ArtifactNeighborhood,
        allow_ungrounded_symbols: bool,
    ) -> String {
        match &self.inner {
            AgentKind::Claude(agent) => {
                agent.input_fingerprint(neighborhood, allow_ungrounded_symbols)
            }
            AgentKind::Codex(agent) => {
                agent.input_fingerprint(neighborhood, allow_ungrounded_symbols)
            }
            AgentKind::Antigravity(agent) => {
                agent.input_fingerprint(neighborhood, allow_ungrounded_symbols)
            }
        }
    }

    fn analyze(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
        allow_ungrounded_symbols: bool,
    ) -> Result<AgentOutcome, PortError> {
        self.pace();
        match &self.inner {
            AgentKind::Claude(agent) => {
                agent.analyze(neighborhood, produced_at, allow_ungrounded_symbols)
            }
            AgentKind::Codex(agent) => {
                agent.analyze(neighborhood, produced_at, allow_ungrounded_symbols)
            }
            AgentKind::Antigravity(agent) => {
                agent.analyze(neighborhood, produced_at, allow_ungrounded_symbols)
            }
        }
    }
}

impl KnowledgeReviewAgent for ConfiguredAgent {
    fn review(&self, candidates: &[KnowledgeCandidate]) -> Result<ClusterReview, PortError> {
        self.pace();
        match &self.inner {
            AgentKind::Claude(agent) => agent.review(candidates),
            AgentKind::Codex(agent) => agent.review(candidates),
            AgentKind::Antigravity(agent) => agent.review(candidates),
        }
    }
}

impl StaleClaimReviewAgent for ConfiguredAgent {
    fn review_stale_claims(
        &self,
        claims: &[StaleClaim],
    ) -> Result<Vec<StaleClaimVerdict>, PortError> {
        self.pace();
        match &self.inner {
            AgentKind::Claude(agent) => agent.review_stale_claims(claims),
            AgentKind::Codex(agent) => agent.review_stale_claims(claims),
            AgentKind::Antigravity(agent) => agent.review_stale_claims(claims),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_documented_agents_are_supported() {
        assert!(ConfiguredAgent::from_name("claude", None, false, false).is_ok());
        assert!(ConfiguredAgent::from_name("codex", None, false, false).is_ok());
        assert!(ConfiguredAgent::from_name("antigravity", None, false, false).is_ok());
        assert_eq!(
            ConfiguredAgent::from_name("unknown", None, false, false)
                .err()
                .as_deref(),
            Some("unknown")
        );
    }
}
