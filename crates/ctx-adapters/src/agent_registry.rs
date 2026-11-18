//! Shared selection, budgets, and a record of each CLI agent invocation.

use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ctx_app::ports::{KnowledgeReviewAgent, PortError, SemanticAgent, StaleClaimReviewAgent};
use ctx_core::{
    knowledge::{AgentOutcome, ClusterReview, KnowledgeCandidate},
    neighborhood::ArtifactNeighborhood,
    verification::{StaleClaim, StaleClaimVerdict},
};
use serde::Serialize;

use crate::{
    agent_contract::{self, AgentContractError, AgentTransport},
    antigravity, claude_code, codex,
};

enum Transport {
    Claude(claude_code::SubprocessTransport),
    Codex(codex::SubprocessTransport),
    Antigravity(antigravity::SubprocessTransport),
}

pub struct ConfiguredAgent {
    transport: Transport,
    name: &'static str,
    model: Option<String>,
    ledger: Option<PathBuf>,
    timeout: Option<Duration>,
}

impl ConfiguredAgent {
    /// Selects a supported CLI agent.
    ///
    /// # Errors
    /// Returns the unrecognized agent name.
    pub fn from_name(name: &str, model: Option<String>, verbose: bool) -> Result<Self, String> {
        let binary =
            |variable, default: &str| env::var(variable).unwrap_or_else(|_| default.to_owned());
        let (transport, name) = match name {
            "claude" => (
                Transport::Claude(claude_code::SubprocessTransport::new(
                    binary("CTX_CLAUDE_CLI_BINARY", "claude"),
                    verbose,
                    model.clone(),
                )),
                "claude-code",
            ),
            "codex" => (
                Transport::Codex(codex::SubprocessTransport::new(
                    binary("CTX_CODEX_CLI_BINARY", "codex"),
                    verbose,
                    model.clone(),
                )),
                "codex",
            ),
            "antigravity" => (
                Transport::Antigravity(antigravity::SubprocessTransport::new(
                    binary("CTX_ANTIGRAVITY_CLI_BINARY", "agy"),
                    verbose,
                    model.clone(),
                )),
                "antigravity",
            ),
            other => return Err(other.to_owned()),
        };
        Ok(Self {
            transport,
            name,
            model,
            ledger: None,
            timeout: None,
        })
    }

    #[must_use]
    pub fn with_ledger(mut self, directory: &Path) -> Self {
        self.ledger = Some(directory.to_owned());
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

impl AgentTransport for ConfiguredAgent {
    fn run(&self, prompt: &str) -> Result<String, AgentContractError> {
        let started = Instant::now();
        let result = match &self.transport {
            Transport::Claude(agent) => agent.run_with_timeout(prompt, self.timeout),
            Transport::Codex(agent) => agent.run_with_timeout(prompt, self.timeout),
            Transport::Antigravity(agent) => agent.run_with_timeout(prompt, self.timeout),
        };
        if let Some(directory) = &self.ledger {
            let record = AgentRun {
                agent: self.name,
                model: self.model.as_deref(),
                adapter_version: env!("CARGO_PKG_VERSION"),
                timestamp: chrono::Utc::now().to_rfc3339(),
                prompt_fingerprint: blake3::hash(prompt.as_bytes()).to_hex().to_string(),
                input_bytes: prompt.len(),
                timeout_ms: self.timeout.map(|timeout| timeout.as_millis()),
                elapsed_ms: started.elapsed().as_millis(),
                outcome: if result.is_ok() { "response" } else { "failed" },
            };
            if let Err(error) = record.write(directory) {
                tracing::warn!(%error, "could not record agent run");
            }
        }
        result
    }
}

#[derive(Serialize)]
struct AgentRun<'a> {
    agent: &'a str,
    model: Option<&'a str>,
    adapter_version: &'a str,
    timestamp: String,
    prompt_fingerprint: String,
    input_bytes: usize,
    timeout_ms: Option<u128>,
    elapsed_ms: u128,
    outcome: &'a str,
}

impl AgentRun<'_> {
    fn write(&self, directory: &Path) -> io::Result<()> {
        fs::create_dir_all(directory)?;
        let mut file = tempfile::Builder::new()
            .prefix("agent-")
            .suffix(".json")
            .tempfile_in(directory)?;
        serde_json::to_writer(file.as_file_mut(), self).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.keep().map_err(|error| error.error)?;
        Ok(())
    }
}

impl SemanticAgent for ConfiguredAgent {
    fn input_fingerprint(
        &self,
        neighborhood: &ArtifactNeighborhood,
        allow_ungrounded_symbols: bool,
    ) -> String {
        agent_contract::input_fingerprint(neighborhood, allow_ungrounded_symbols)
    }

    fn analyze(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
        allow_ungrounded_symbols: bool,
    ) -> Result<AgentOutcome, PortError> {
        agent_contract::analyze(
            self,
            neighborhood,
            produced_at,
            self.name,
            self.model.clone(),
            allow_ungrounded_symbols,
        )
        .map_err(AgentContractError::into_port_error)
    }
}

impl KnowledgeReviewAgent for ConfiguredAgent {
    fn review(&self, candidates: &[KnowledgeCandidate]) -> Result<ClusterReview, PortError> {
        agent_contract::review(self, candidates).map_err(AgentContractError::into_port_error)
    }
}

impl StaleClaimReviewAgent for ConfiguredAgent {
    fn review_stale_claims(
        &self,
        claims: &[StaleClaim],
    ) -> Result<Vec<StaleClaimVerdict>, PortError> {
        agent_contract::review_stale_claims(self, claims)
            .map_err(AgentContractError::into_port_error)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn records_failed_requests_without_prompt_contents() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("fake-agent");
        fs::write(&script, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let ledger = root.path().join("ledger");
        let agent = ConfiguredAgent {
            transport: Transport::Codex(codex::SubprocessTransport::new(
                script.to_string_lossy(),
                false,
                None,
            )),
            name: "codex",
            model: None,
            ledger: None,
            timeout: None,
        }
        .with_ledger(&ledger)
        .with_timeout(Duration::from_millis(20));

        assert!(
            agent
                .run("private input")
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        let path = fs::read_dir(ledger)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let record = fs::read_to_string(path).unwrap();
        assert!(!record.contains("private input"));
        let record: serde_json::Value = serde_json::from_str(&record).unwrap();
        assert_eq!(record["outcome"], "failed");
        assert_eq!(record["input_bytes"], 13);
        assert_eq!(record["timeout_ms"], 20);
    }
}

impl ctx_app::ports::ResearchAgent for ConfiguredAgent {
    fn research(
        &self,
        input: &ctx_core::coverage::research::ResearchInput,
    ) -> Result<ctx_core::coverage::research::ResearchJudgment, PortError> {
        agent_contract::research(self, input, self.model())
            .map_err(AgentContractError::into_port_error)
    }
}
