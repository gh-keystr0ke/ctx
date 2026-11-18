//! One composition point for selecting a supported agent CLI.

use std::path::Path;

use ctx_adapters::agent_registry;
use ctx_app::ports::{
    KnowledgeReviewAgent, PortError, ResearchAgent, SemanticAgent, StaleClaimReviewAgent,
};
use ctx_core::{
    knowledge::{AgentOutcome, ClusterReview, KnowledgeCandidate},
    neighborhood::ArtifactNeighborhood,
    verification::{StaleClaim, StaleClaimVerdict},
};

use crate::agent_pacing::AgentPacer;

pub(crate) struct ConfiguredAgent {
    inner: agent_registry::ConfiguredAgent,
    pacer: Option<AgentPacer>,
}

impl ConfiguredAgent {
    pub(crate) fn from_name(
        name: &str,
        model: Option<String>,
        verbose: bool,
        siga_siga: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            inner: agent_registry::ConfiguredAgent::from_name(name, model, verbose)?,
            pacer: siga_siga.then(AgentPacer::night_mode),
        })
    }

    pub(crate) fn with_ledger(mut self, root: &Path) -> Self {
        self.inner = self.inner.with_ledger(&root.join(".ctx/logs/agents"));
        self
    }

    pub(crate) fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.inner = self.inner.with_timeout(timeout);
        self
    }

    fn pace(&self) {
        if let Some(pacer) = &self.pacer {
            pacer.pace();
        }
    }
}

impl SemanticAgent for ConfiguredAgent {
    fn input_fingerprint(
        &self,
        neighborhood: &ArtifactNeighborhood,
        allow_ungrounded_symbols: bool,
    ) -> String {
        self.inner
            .input_fingerprint(neighborhood, allow_ungrounded_symbols)
    }

    fn analyze(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
        allow_ungrounded_symbols: bool,
    ) -> Result<AgentOutcome, PortError> {
        self.pace();
        self.inner
            .analyze(neighborhood, produced_at, allow_ungrounded_symbols)
    }
}

impl KnowledgeReviewAgent for ConfiguredAgent {
    fn review(&self, candidates: &[KnowledgeCandidate]) -> Result<ClusterReview, PortError> {
        self.pace();
        self.inner.review(candidates)
    }
}

impl StaleClaimReviewAgent for ConfiguredAgent {
    fn review_stale_claims(
        &self,
        claims: &[StaleClaim],
    ) -> Result<Vec<StaleClaimVerdict>, PortError> {
        self.pace();
        self.inner.review_stale_claims(claims)
    }
}

impl ctx_adapters::agent_contract::AgentTransport for ConfiguredAgent {
    fn run(
        &self,
        prompt: &str,
    ) -> Result<String, ctx_adapters::agent_contract::AgentContractError> {
        self.pace();
        self.inner.run(prompt)
    }
}

impl ResearchAgent for ConfiguredAgent {
    fn research(
        &self,
        input: &ctx_core::coverage::research::ResearchInput,
    ) -> Result<ctx_core::coverage::research::ResearchJudgment, PortError> {
        self.pace();
        self.inner.research(input)
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
