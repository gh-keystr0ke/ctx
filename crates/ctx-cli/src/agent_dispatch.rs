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

pub(crate) enum ConfiguredAgent {
    Claude(ClaudeCodeAgent<ClaudeTransport>),
    Codex(CodexAgent<CodexTransport>),
    Antigravity(AntigravityAgent<AntigravityTransport>),
}

impl ConfiguredAgent {
    pub(crate) fn from_name(
        name: &str,
        model: Option<String>,
        verbose: bool,
    ) -> Result<Self, String> {
        match name {
            "claude" => Ok(Self::Claude(ClaudeCodeAgent::new(
                ClaudeTransport::new(
                    binary("CTX_CLAUDE_CLI_BINARY", "claude"),
                    verbose,
                    model.clone(),
                ),
                model,
            ))),
            "codex" => Ok(Self::Codex(CodexAgent::new(
                CodexTransport::new(
                    binary("CTX_CODEX_CLI_BINARY", "codex"),
                    verbose,
                    model.clone(),
                ),
                model,
            ))),
            "antigravity" => Ok(Self::Antigravity(AntigravityAgent::new(
                AntigravityTransport::new(
                    binary("CTX_ANTIGRAVITY_CLI_BINARY", "agy"),
                    verbose,
                    model.clone(),
                ),
                model,
            ))),
            other => Err(other.to_owned()),
        }
    }
}

fn binary(variable: &str, default: &str) -> String {
    env::var(variable).unwrap_or_else(|_| default.to_owned())
}

impl SemanticAgent for ConfiguredAgent {
    fn analyze(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
        allow_ungrounded_symbols: bool,
    ) -> Result<AgentOutcome, PortError> {
        match self {
            Self::Claude(agent) => {
                agent.analyze(neighborhood, produced_at, allow_ungrounded_symbols)
            }
            Self::Codex(agent) => {
                agent.analyze(neighborhood, produced_at, allow_ungrounded_symbols)
            }
            Self::Antigravity(agent) => {
                agent.analyze(neighborhood, produced_at, allow_ungrounded_symbols)
            }
        }
    }
}

impl KnowledgeReviewAgent for ConfiguredAgent {
    fn review(&self, candidates: &[KnowledgeCandidate]) -> Result<ClusterReview, PortError> {
        match self {
            Self::Claude(agent) => agent.review(candidates),
            Self::Codex(agent) => agent.review(candidates),
            Self::Antigravity(agent) => agent.review(candidates),
        }
    }
}

impl StaleClaimReviewAgent for ConfiguredAgent {
    fn review_stale_claims(
        &self,
        claims: &[StaleClaim],
    ) -> Result<Vec<StaleClaimVerdict>, PortError> {
        match self {
            Self::Claude(agent) => agent.review_stale_claims(claims),
            Self::Codex(agent) => agent.review_stale_claims(claims),
            Self::Antigravity(agent) => agent.review_stale_claims(claims),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_documented_agents_are_supported() {
        assert!(ConfiguredAgent::from_name("claude", None, false).is_ok());
        assert!(ConfiguredAgent::from_name("codex", None, false).is_ok());
        assert!(ConfiguredAgent::from_name("antigravity", None, false).is_ok());
        assert_eq!(
            ConfiguredAgent::from_name("unknown", None, false)
                .err()
                .as_deref(),
            Some("unknown")
        );
    }
}
