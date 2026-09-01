//! Google Antigravity CLI (`agy`) agent adapter (prompt3.md
//! PR-AGENT-001/ADR-AGENT-001, the third interchangeable agent): shells out
//! to `agy -p` in headless/print mode. Headless mode writes the response to
//! stdout and diagnostics to stderr; by default, tool calls that would need
//! approval (shell commands, file writes) are soft-denied rather than
//! hanging on a prompt, and reading/writing files is scoped to the active
//! workspace -- none of which this adapter needs, since the entire bounded
//! neighborhood is inlined into the prompt text itself rather than asking
//! the agent to go read anything. No `--dangerously-skip-permissions` or
//! other elevated flag is used. The prompt contract and response
//! parsing/validation live in [`crate::agent_contract`], shared by every
//! CLI-based `SemanticAgent` adapter.

use std::process::Command;

use ctx_app::ports::{KnowledgeReviewAgent, PortError, SemanticAgent, StaleClaimReviewAgent};
use ctx_core::{
    knowledge::{AgentOutcome, ClusterReview, KnowledgeCandidate},
    neighborhood::ArtifactNeighborhood,
    verification::{StaleClaim, StaleClaimVerdict},
};

use crate::agent_contract::{self, AgentContractError, AgentTransport};

pub struct SubprocessTransport {
    binary: String,
    verbose: bool,
    model: Option<String>,
}

impl SubprocessTransport {
    #[must_use]
    pub fn new(binary: impl Into<String>, verbose: bool, model: Option<String>) -> Self {
        Self {
            binary: binary.into(),
            verbose,
            model,
        }
    }
}

impl Default for SubprocessTransport {
    fn default() -> Self {
        Self::new("agy", false, None)
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
        let mut command = Command::new(&self.binary);
        command.arg("-p");
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        let output = command
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

pub struct AntigravityAgent<T> {
    transport: T,
    model: Option<String>,
}

impl<T: AgentTransport> AntigravityAgent<T> {
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
            "antigravity",
            self.model.clone(),
            allow_ungrounded_symbols,
        )
    }
}

impl<T: AgentTransport> SemanticAgent for AntigravityAgent<T> {
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

impl<T: AgentTransport> KnowledgeReviewAgent for AntigravityAgent<T> {
    fn review(&self, candidates: &[KnowledgeCandidate]) -> Result<ClusterReview, PortError> {
        agent_contract::review(&self.transport, candidates)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

impl<T: AgentTransport> StaleClaimReviewAgent for AntigravityAgent<T> {
    fn review_stale_claims(
        &self,
        claims: &[StaleClaim],
    ) -> Result<Vec<StaleClaimVerdict>, PortError> {
        agent_contract::review_stale_claims(&self.transport, claims)
            .map_err(|error| PortError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON-contract parsing/validation logic is tested once, generically,
    /// in `agent_contract`'s own test module -- this is the one thing that's
    /// actually specific to this vendor: that the real subprocess boundary
    /// invokes `agy -p <prompt>` and surfaces its stdout, using a fake script
    /// standing in for the real binary so this suite never depends on a real
    /// `agy` installation.
    #[test]
    fn subprocess_transport_invokes_agy_p_and_returns_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("agy");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ]; then echo \"{\\\"outcome\\\":\\\"not_relevant\\\",\\\"prompt_arg\\\":\\\"$2\\\"}\"; else exit 1; fi\n",
        )
        .expect("write fake agy script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script_path, permissions).expect("chmod");
        }

        let transport =
            SubprocessTransport::new(script_path.to_string_lossy().into_owned(), false, None);
        let output = transport.run("hello neighborhood").expect("script output");

        assert!(output.contains("not_relevant"));
        assert!(output.contains("hello neighborhood"));
    }

    /// Regression test for a live bug: `--model` was accepted by every layer
    /// above this one but never actually reached the `agy` argv, so
    /// `ctx enrich --agent antigravity --model gemini-3-pro` silently ran
    /// agy's own default model.
    #[test]
    fn subprocess_transport_passes_model_flag_to_agy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("agy");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ] && [ \"$2\" = \"--model\" ] && [ \"$3\" = \"gemini-3-pro\" ]; then echo \"{\\\"outcome\\\":\\\"not_relevant\\\",\\\"model_arg\\\":\\\"$3\\\"}\"; else exit 1; fi\n",
        )
        .expect("write fake agy script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script_path, permissions).expect("chmod");
        }

        let transport = SubprocessTransport::new(
            script_path.to_string_lossy().into_owned(),
            false,
            Some("gemini-3-pro".to_owned()),
        );
        let output = transport.run("hello neighborhood").expect("script output");

        assert!(output.contains("\"model_arg\":\"gemini-3-pro\""));
    }

    /// When no `--model` is configured, none is passed to `agy` either.
    #[test]
    fn subprocess_transport_omits_model_flag_when_none_configured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("agy");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ] && [ \"$2\" = \"hello neighborhood\" ] && [ $# -eq 2 ]; then echo '{\"outcome\":\"not_relevant\"}'; else exit 1; fi\n",
        )
        .expect("write fake agy script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script_path, permissions).expect("chmod");
        }

        let transport =
            SubprocessTransport::new(script_path.to_string_lossy().into_owned(), false, None);
        let output = transport.run("hello neighborhood").expect("script output");

        assert!(output.contains("not_relevant"));
    }
}
