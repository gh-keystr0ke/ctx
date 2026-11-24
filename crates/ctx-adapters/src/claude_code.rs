//! Claude Code CLI agent adapter (prompt3.md PR-AGENT-001): shells out to
//! `claude -p --safe-mode [--tools ""] [--model M] -- <prompt>` in headless
//! mode. `--safe-mode` disables
//! CLAUDE.md, skills, plugins, hooks, MCP servers, and custom
//! commands/agents -- none of which this adapter needs, since the entire
//! bounded neighborhood is inlined into the prompt text itself and the
//! contract is a single self-contained prompt-in/JSON-out call. Leaving them
//! on would burn context on unrelated project config for every enrich/review
//! call and risks a project's own CLAUDE.md instructions leaking into a
//! prompt whose output is parsed as strict JSON. Unlike `--bare`,
//! `--safe-mode` leaves auth (OAuth/keychain included), model selection, and
//! permissions untouched, so it works for users without an
//! `ANTHROPIC_API_KEY`. The prompt contract and response parsing/validation
//! live in [`crate::agent_contract`], shared by every CLI-based
//! [`SemanticAgent`] adapter -- this module only owns process invocation.

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
    model: Option<String>,
}

impl SubprocessTransport {
    #[must_use]
    pub fn new(binary: impl Into<String>, _verbose: bool, model: Option<String>) -> Self {
        Self {
            binary: binary.into(),
            model,
        }
    }
}

impl Default for SubprocessTransport {
    fn default() -> Self {
        Self::new("claude", false, None)
    }
}

impl AgentTransport for SubprocessTransport {
    fn run(&self, prompt: &str) -> Result<String, AgentContractError> {
        self.run_with_timeout(prompt, None)
    }
}

impl SubprocessTransport {
    pub(crate) fn run_with_timeout(
        &self,
        prompt: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<String, AgentContractError> {
        tracing::debug!(
            agent = "claude",
            binary = self.binary,
            model = ?self.model,
            "starting agent subprocess"
        );
        let started = std::time::Instant::now();
        let mut command = Command::new(&self.binary);
        command.arg("-p").arg("--safe-mode");
        if timeout.is_some() {
            command.args(["--tools", ""]);
        }
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        // `--tools <tools...>` is variadic in the Claude Code CLI: without an
        // option after it, it takes the prompt as more tool names and
        // `claude -p` exits with "Input must be provided". The `--` ends
        // option parsing, so the prompt is always one positional argument,
        // also when it starts with `-`.
        command.arg("--").arg(prompt);
        let output = agent_contract::run_subprocess(&mut command, &self.binary, timeout)?;
        tracing::debug!(
            agent = "claude",
            status = ?output.status.code(),
            elapsed_ms = started.elapsed().as_millis(),
            "agent subprocess completed"
        );
        tracing::trace!(
            agent = "claude",
            stdout = %String::from_utf8_lossy(&output.stdout),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "agent subprocess output"
        );
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
        allow_ungrounded_symbols: bool,
    ) -> Result<AgentOutcome, AgentContractError> {
        agent_contract::analyze(
            &self.transport,
            neighborhood,
            produced_at,
            "claude-code",
            self.model.clone(),
            allow_ungrounded_symbols,
        )
    }
}

impl<T: AgentTransport> SemanticAgent for ClaudeCodeAgent<T> {
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
        self.analyze_neighborhood(neighborhood, produced_at, allow_ungrounded_symbols)
            .map_err(AgentContractError::into_port_error)
    }
}

impl<T: AgentTransport> KnowledgeReviewAgent for ClaudeCodeAgent<T> {
    fn review(&self, candidates: &[KnowledgeCandidate]) -> Result<ClusterReview, PortError> {
        agent_contract::review(&self.transport, candidates)
            .map_err(AgentContractError::into_port_error)
    }
}

impl<T: AgentTransport> StaleClaimReviewAgent for ClaudeCodeAgent<T> {
    fn review_stale_claims(
        &self,
        claims: &[StaleClaim],
    ) -> Result<Vec<StaleClaimVerdict>, PortError> {
        agent_contract::review_stale_claims(&self.transport, claims)
            .map_err(AgentContractError::into_port_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON-contract parsing/validation logic is tested once, generically,
    /// in `agent_contract`'s own test module -- this is the one thing that's
    /// actually specific to this vendor: that the real subprocess boundary
    /// invokes `claude -p --safe-mode -- <prompt>` and surfaces its stdout,
    /// using a fake script standing in for the real binary so this suite
    /// never depends on a real `claude` installation.
    #[test]
    fn subprocess_transport_invokes_claude_p_safe_mode_and_returns_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("claude");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ] && [ \"$2\" = \"--safe-mode\" ] && [ \"$3\" = \"--\" ]; then echo \"{\\\"outcome\\\":\\\"not_relevant\\\",\\\"prompt_arg\\\":\\\"$4\\\"}\"; else exit 1; fi\n",
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

        let transport =
            SubprocessTransport::new(script_path.to_string_lossy().into_owned(), false, None);
        let output = transport.run("hello neighborhood").expect("script output");

        assert!(output.contains("not_relevant"));
        assert!(output.contains("hello neighborhood"));
    }

    /// `--safe-mode` (CLAUDE.md/skills/plugins/hooks/MCP/custom
    /// commands-and-agents disabled, but auth/model/permissions untouched)
    /// must always be on this adapter's argv, model or no model -- it is
    /// deliberately not gated behind a flag, since this call is always a
    /// single self-contained prompt-in/JSON-out contract that never needs
    /// project customizations and should never pay their context cost.
    #[test]
    fn subprocess_transport_always_passes_safe_mode_even_with_a_model_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("claude");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ] && [ \"$2\" = \"--safe-mode\" ] && [ \"$3\" = \"--model\" ] && [ \"$4\" = \"haiku\" ]; then echo '{\"outcome\":\"not_relevant\"}'; else exit 1; fi\n",
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

        let transport = SubprocessTransport::new(
            script_path.to_string_lossy().into_owned(),
            false,
            Some("haiku".to_owned()),
        );
        let output = transport.run("hello neighborhood").expect("script output");

        assert!(output.contains("not_relevant"));
    }

    /// Regression test for a live bug: `--model` was accepted by every layer
    /// above this one (CLI flag, `ConfiguredAgent`, `ClaudeCodeAgent`) and
    /// recorded into `AgentProvenance`, but never actually reached the
    /// `claude` argv -- so `ctx enrich --model haiku` silently ran whatever
    /// model `claude` defaults to while still labeling the output "haiku".
    #[test]
    fn subprocess_transport_passes_model_flag_to_claude() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("claude");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ] && [ \"$2\" = \"--safe-mode\" ] && [ \"$3\" = \"--model\" ] && [ \"$4\" = \"haiku\" ]; then echo \"{\\\"outcome\\\":\\\"not_relevant\\\",\\\"model_arg\\\":\\\"$4\\\"}\"; else exit 1; fi\n",
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

        let transport = SubprocessTransport::new(
            script_path.to_string_lossy().into_owned(),
            false,
            Some("haiku".to_owned()),
        );
        let output = transport.run("hello neighborhood").expect("script output");

        assert!(output.contains("\"model_arg\":\"haiku\""));
    }

    /// The counterpart to the regression test above: when no `--model` is
    /// given, none is passed to `claude` either -- it must fall back to the
    /// CLI's own default rather than this adapter inventing one.
    #[test]
    fn subprocess_transport_omits_model_flag_when_none_configured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("claude");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"-p\" ] && [ \"$2\" = \"--safe-mode\" ] && [ \"$3\" = \"--\" ] && [ \"$4\" = \"hello neighborhood\" ] && [ $# -eq 4 ]; then echo '{\"outcome\":\"not_relevant\"}'; else exit 1; fi\n",
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

        let transport =
            SubprocessTransport::new(script_path.to_string_lossy().into_owned(), false, None);
        let output = transport.run("hello neighborhood").expect("script output");

        assert!(output.contains("not_relevant"));
    }

    /// Writes a fake `claude` that records its full argv, NUL-separated, to
    /// `argv` next to it and prints a valid contract response.
    fn recording_fake_claude(dir: &std::path::Path) -> (String, std::path::PathBuf) {
        let script_path = dir.join("claude");
        let argv_path = dir.join("argv");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{}'\necho '{{\"outcome\":\"not_relevant\"}}'\n",
                argv_path.display()
            ),
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
        (script_path.to_string_lossy().into_owned(), argv_path)
    }

    fn recorded_argv(path: &std::path::Path) -> Vec<String> {
        let raw = std::fs::read_to_string(path).expect("recorded argv");
        let mut argv: Vec<String> = raw.split('\0').map(str::to_owned).collect();
        assert_eq!(argv.pop().as_deref(), Some(""), "argv ends with NUL");
        argv
    }

    /// Regression test for a live bug: `--tools <tools...>` is variadic in
    /// the Claude Code CLI, so `--tools ""` directly before the prompt took
    /// the prompt as more tool names. Every `coverage research` and
    /// `repair propose --agent claude` call (both set a timeout) then failed
    /// with "Input must be provided" unless `--model` happened to end the
    /// list. The prompt must come after `--` as the last argument.
    #[test]
    fn subprocess_transport_with_timeout_ends_options_before_the_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (binary, argv_path) = recording_fake_claude(dir.path());
        let timeout = Some(std::time::Duration::from_secs(30));

        SubprocessTransport::new(binary.clone(), false, None)
            .run_with_timeout("hello neighborhood", timeout)
            .expect("script output");
        assert_eq!(
            recorded_argv(&argv_path),
            [
                "-p",
                "--safe-mode",
                "--tools",
                "",
                "--",
                "hello neighborhood"
            ]
        );

        SubprocessTransport::new(binary, false, Some("opus".to_owned()))
            .run_with_timeout("hello neighborhood", timeout)
            .expect("script output");
        assert_eq!(
            recorded_argv(&argv_path),
            [
                "-p",
                "--safe-mode",
                "--tools",
                "",
                "--model",
                "opus",
                "--",
                "hello neighborhood"
            ]
        );
    }

    /// A prompt that contains `!=` (seen in the live failure as an "unknown
    /// tool") or starts with `-` reaches `claude` as one positional argument.
    #[test]
    fn subprocess_transport_passes_option_like_prompts_as_one_argument() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (binary, argv_path) = recording_fake_claude(dir.path());
        let transport = SubprocessTransport::new(binary, false, None);

        for prompt in ["status != 'paid' at app.py:12", "--model haiku is a prompt"] {
            transport
                .run_with_timeout(prompt, Some(std::time::Duration::from_secs(30)))
                .expect("script output");
            let argv = recorded_argv(&argv_path);
            assert_eq!(argv[argv.len() - 2..], ["--", prompt]);

            transport.run(prompt).expect("script output");
            assert_eq!(
                recorded_argv(&argv_path),
                ["-p", "--safe-mode", "--", prompt]
            );
        }
    }
}
