use std::{fs::OpenOptions, io, path::Path};

use clap::ValueEnum;
use ctx_adapters::{analyzer::AnalyzerRegistry, git::GitRepo, sqlite::SqliteStore};
use ctx_app::{ports::GitRepository, review::ReviewRunner};
use ctx_core::{domain::NodeKind, review::ReviewReport};
use serde_json::{Value, json};

use crate::{
    CliError, database_path,
    hooks_command::{read_missing, write_file},
};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Hook {
    ClaudePreCommit,
    GitPreCommit,
}

pub(crate) fn run(hook: Hook, git: &GitRepo) -> Result<(), CliError> {
    match hook {
        Hook::GitPreCommit => {
            if let Some(reason) = review_staged(git)? {
                eprintln!("{reason}");
                return Err(CliError::ReviewBlocked);
            }
        }
        Hook::ClaudePreCommit => {
            let input: Value = serde_json::from_reader(io::stdin().lock())?;
            if input["tool_name"] != "Bash" {
                return Ok(());
            }
            let Some(command) = input.pointer("/tool_input/command").and_then(Value::as_str) else {
                return Ok(());
            };
            if !runs_git_commit(command) {
                return Ok(());
            }
            let cwd = input["cwd"].as_str().map_or(git.root(), Path::new);
            let target = GitRepo::discover(cwd)?;
            if let Some(reason) = review_staged(&target)? {
                println!(
                    "{}",
                    json!({"hookSpecificOutput": {
                        "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason
                    }})
                );
            }
        }
    }
    Ok(())
}

fn runs_git_commit(command: &str) -> bool {
    command.split([';', '&', '|', '\n', '(']).any(|segment| {
        let mut words = segment.split_whitespace().peekable();
        while words
            .peek()
            .is_some_and(|word| word.contains('=') || matches!(*word, "command" | "env"))
        {
            words.next();
        }
        if words.next().and_then(|word| word.rsplit('/').next()) != Some("git") {
            return false;
        }
        while let Some(word) = words.next() {
            match word {
                "-C" | "-c" | "--git-dir" | "--work-tree" => {
                    words.next();
                }
                option if option.starts_with('-') => {}
                subcommand => return subcommand == "commit",
            }
        }
        false
    })
}

fn review_staged(git: &GitRepo) -> Result<Option<String>, CliError> {
    if !git.root().join(".ctx/ctx.db").is_file() || git.head_oid().is_err() {
        return Ok(None);
    }
    let store = SqliteStore::open(&database_path(git.root())?, git.context_root())?;
    let analyzer = AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?;
    let report = ReviewRunner::new(git, &analyzer, &store).run_staged(&git.descriptor()?.id)?;
    acknowledge(git, &report)
}

fn acknowledge(git: &GitRepo, report: &ReviewReport) -> Result<Option<String>, CliError> {
    let invariants = report
        .findings_by_intent
        .iter()
        .filter(|finding| finding.affected_intent.kind == NodeKind::Invariant)
        .collect::<Vec<_>>();
    let schema = report
        .schema_findings
        .iter()
        .filter(|finding| finding.destructive)
        .collect::<Vec<_>>();
    let api = report
        .api_findings
        .iter()
        .filter(|finding| finding.destructive)
        .collect::<Vec<_>>();
    let lock_path = git.root().join(".ctx/commit-review.lock");
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock()?;
    let acknowledged = git.root().join(".ctx/commit-review");
    if invariants.is_empty() && schema.is_empty() && api.is_empty() {
        if acknowledged.exists() {
            write_file(&acknowledged, "", false)?;
        }
        return Ok(None);
    }
    let fingerprint = blake3::hash(&serde_json::to_vec(&(&invariants, &schema, &api))?)
        .to_hex()
        .to_string();
    if read_missing(&acknowledged)? == fingerprint {
        return Ok(None);
    }
    write_file(&acknowledged, &fingerprint, false)?;
    let mut lines = vec![
        "ctx review paused this commit. Review the staged findings before retrying.".to_owned(),
    ];
    for finding in invariants {
        let symbols = finding
            .signals
            .iter()
            .map(|signal| signal.changed_entity.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "{} — {symbols}",
            finding.affected_intent.identifier
        ));
    }
    for finding in schema {
        lines.extend(
            finding
                .changes
                .iter()
                .filter(|change| change.destructive)
                .map(|change| change.description()),
        );
    }
    for finding in api {
        lines.extend(
            finding
                .changes
                .iter()
                .filter(|change| change.destructive)
                .map(|change| change.description()),
        );
    }
    Ok(Some(lines.join("\n")))
}
