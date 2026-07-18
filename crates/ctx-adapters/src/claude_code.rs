//! Claude Code CLI agent adapter (prompt3.md PR-AGENT-001): shells out to
//! `claude -p` in headless mode. The prompt contract and response
//! parsing/validation live in [`crate::agent_contract`], shared by every
//! CLI-based [`SemanticAgent`] adapter -- this module only owns process
//! invocation.

use std::process::Command;

use ctx_app::ports::{KnowledgeReviewAgent, PortError, SemanticAgent};
use ctx_core::{
    knowledge::{AgentOutcome, ClusterReview, KnowledgeCandidate},
    neighborhood::ArtifactNeighborhood,
};

use crate::agent_contract::{self, AgentContractError, AgentTransport};

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

impl AgentTransport for SubprocessTransport {
    fn run(&self, prompt: &str) -> Result<String, AgentContractError> {
        let output = Command::new(&self.binary)
            .arg("-p")
            .arg(prompt)
            .output()
            .map_err(|error| AgentContractError::Spawn(format!("{}: {error}", self.binary)))?;
        if !output.status.success() {
            return Err(AgentContractError::ExitFailure(format!(
                "{}: {}",
                self.binary,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

pub struct ClaudeCodeAgent<T> {
    transport: T,
    model: Option<String>,
}

impl<T: AgentTransport> ClaudeCodeAgent<T> {
    pub fn new(transport: T, model: Option<String>) -> Self {
        Self { transport, model }
    }

    /// # Errors
    /// Returns [`AgentContractError`] when the process fails or its output
    /// cannot be parsed and validated as the expected contract.
    pub fn analyze_neighborhood(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
    ) -> Result<AgentOutcome, AgentContractError> {
        agent_contract::analyze(
            &self.transport,
            neighborhood,
            produced_at,
            "claude-code",
            self.model.clone(),
        )
    }
}

impl<T: AgentTransport> SemanticAgent for ClaudeCodeAgent<T> {
    fn analyze(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
    ) -> Result<AgentOutcome, PortError> {
        self.analyze_neighborhood(neighborhood, produced_at)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

impl<T: AgentTransport> KnowledgeReviewAgent for ClaudeCodeAgent<T> {
    fn review(&self, candidates: &[KnowledgeCandidate]) -> Result<ClusterReview, PortError> {
        agent_contract::review(&self.transport, candidates)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON-contract parsing/validation logic is tested once, generically,
    /// in `agent_contract`'s own test module -- this is the one thing that's
    /// actually specific to this vendor: that the real subprocess boundary
    /// invokes `claude -p <prompt>` and surfaces its stdout, using a fake
    /// script standing in for the real binary so this suite never depends on
    /// a real `claude` installation.
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
