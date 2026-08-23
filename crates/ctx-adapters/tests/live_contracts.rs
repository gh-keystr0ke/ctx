//! Opt-in compatibility smoke tests for external services and agent CLIs.
//!
//! These compile in normal CI but are ignored because they require locally
//! authenticated third-party installations/accounts. Run with:
//! `cargo test -p ctx-adapters --test live_contracts -- --ignored`.

use std::{collections::BTreeSet, env, process::Command};

use ctx_adapters::{
    gitlab::{GitLabClient, UreqTransport as GitLabTransport},
    jira::{JiraClient, UreqTransport as JiraTransport},
};

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set for this ignored live test"))
}

#[test]
#[ignore = "requires CTX_LIVE_GITLAB_PROJECT and network access"]
fn gitlab_live_api_still_matches_the_ingestion_contract() {
    let project = required("CTX_LIVE_GITLAB_PROJECT");
    let base_url = env::var("CTX_LIVE_GITLAB_BASE_URL")
        .unwrap_or_else(|_| "https://gitlab.com/api/v4".to_owned());
    let token = env::var("CTX_GITLAB_TOKEN").ok();
    let client = GitLabClient::new(GitLabTransport::new(base_url, token), project);

    let (artifacts, _links) = client
        .fetch_issue_and_mr_artifacts(None)
        .expect("live GitLab response must match the normalized contract");

    assert!(
        artifacts
            .iter()
            .all(|artifact| !artifact.identity.external_id.is_empty())
    );
}

#[test]
#[ignore = "requires CTX_LIVE_JIRA_* credentials and network access"]
fn jira_live_api_still_matches_the_ingestion_contract() {
    let base_url = required("CTX_LIVE_JIRA_BASE_URL");
    let project = required("CTX_LIVE_JIRA_PROJECT");
    let issue = required("CTX_LIVE_JIRA_ISSUE");
    let email = required("CTX_JIRA_EMAIL");
    let token = required("CTX_JIRA_TOKEN");
    let client = JiraClient::new(
        JiraTransport::new(&base_url, &email, &token),
        project,
        base_url,
    );

    let (artifacts, _links) = client
        .fetch_issue_artifacts_for_keys(&BTreeSet::from([issue]))
        .expect("live Jira response must match the normalized contract");

    assert!(!artifacts.is_empty());
}

fn assert_cli_version(binary: &str) {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .unwrap_or_else(|error| panic!("failed to start {binary}: {error}"));
    assert!(
        output.status.success(),
        "{binary} --version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.is_empty() || !output.stderr.is_empty());
}

#[test]
#[ignore = "requires locally installed agent CLIs"]
fn supported_agent_cli_versions_are_detectable() {
    assert_cli_version(&env::var("CTX_CLAUDE_CLI_BINARY").unwrap_or_else(|_| "claude".to_owned()));
    assert_cli_version(&env::var("CTX_CODEX_CLI_BINARY").unwrap_or_else(|_| "codex".to_owned()));
    assert_cli_version(
        &env::var("CTX_ANTIGRAVITY_CLI_BINARY").unwrap_or_else(|_| "agy".to_owned()),
    );
}
