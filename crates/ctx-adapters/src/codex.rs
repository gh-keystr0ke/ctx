//! `OpenAI` Codex CLI agent adapter (prompt3.md PR-AGENT-001/ADR-AGENT-001,
//! the second interchangeable agent): shells out to `codex exec` in headless
//! mode. `codex exec` defaults to a read-only sandbox and
//! `approval_policy = "never"`, prints only the final agent message to
//! stdout, and streams progress to stderr -- exactly the same simple
//! text-in/text-out contract [`crate::claude_code`] uses, so no extra
//! sandbox/approval flags are needed for a pure read-and-respond analysis
//! task that never asks Codex to edit anything. The prompt contract and
//! response parsing/validation live in [`crate::agent_contract`], shared by
//! every CLI-based `SemanticAgent` adapter.

use std::process::Command;

use ctx_app::ports::{KnowledgeReviewAgent, PortError, SemanticAgent};
use ctx_core::{
    knowledge::{AgentOutcome, ClusterReview, KnowledgeCandidate},
    neighborhood::ArtifactNeighborhood,
};

use crate::agent_contract::{self, AgentContractError, AgentTransport};

pub struct SubprocessTransport {
    binary: String,
    verbose: bool,
}

impl SubprocessTransport {
    #[must_use]
    pub fn new(binary: impl Into<String>, verbose: bool) -> Self {
        Self {
            binary: binary.into(),
            verbose,
        }
    }
}

impl Default for SubprocessTransport {
    fn default() -> Self {
        Self::new("codex", false)
    }
}

impl AgentTransport for SubprocessTransport {
    fn run(&self, prompt: &str) -> Result<String, AgentContractError> {
        if self.verbose {
            eprintln!(
                "--- AGENT PROMPT ({}) ---\n{}\n--- END PROMPT ---",
                self.binary, prompt
            );
        }
        let output = Command::new(&self.binary)
            .arg("exec")
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

pub struct CodexAgent<T> {
    transport: T,
    model: Option<String>,
}

impl<T: AgentTransport> CodexAgent<T> {
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
        allow_ungrounded_symbols: bool,
    ) -> Result<AgentOutcome, AgentContractError> {
        agent_contract::analyze(
            &self.transport,
            neighborhood,
            produced_at,
            "codex",
            self.model.clone(),
            allow_ungrounded_symbols,
        )
    }
}

impl<T: AgentTransport> SemanticAgent for CodexAgent<T> {
    fn analyze(
        &self,
        neighborhood: &ArtifactNeighborhood,
        produced_at: &str,
        allow_ungrounded_symbols: bool,
    ) -> Result<AgentOutcome, PortError> {
        self.analyze_neighborhood(neighborhood, produced_at, allow_ungrounded_symbols)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

impl<T: AgentTransport> KnowledgeReviewAgent for CodexAgent<T> {
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
    /// invokes `codex exec <prompt>` and surfaces its stdout, using a fake
    /// script standing in for the real binary so this suite never depends on
    /// a real `codex` installation.
    #[test]
    fn subprocess_transport_invokes_codex_exec_and_returns_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("codex");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"exec\" ]; then echo \"{\\\"outcome\\\":\\\"not_relevant\\\",\\\"prompt_arg\\\":\\\"$2\\\"}\"; else exit 1; fi\n",
        )
        .expect("write fake codex script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script_path, permissions).expect("chmod");
        }

        let transport = SubprocessTransport::new(script_path.to_string_lossy().into_owned(), false);
        let output = transport.run("hello neighborhood").expect("script output");

        assert!(output.contains("not_relevant"));
        assert!(output.contains("hello neighborhood"));
    }
}
