use std::{
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
};

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
            let cwd = input["cwd"].as_str().map_or(git.root(), Path::new);
            if let Some(action) = invocation(command, cwd) {
                let reason = match action {
                    CommitInvocation::Review(directory) => review_staged(&GitRepo::discover(&directory)?)?,
                    CommitInvocation::StageFirst => Some("Stage your changes in a separate command, then run git commit without paths or --all.".to_owned()),
                };
                if let Some(reason) = reason {
                    println!(
                        "{}",
                        json!({"hookSpecificOutput": {
                            "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason
                        }})
                    );
                }
            }
        }
    }
    Ok(())
}

enum CommitInvocation {
    Review(PathBuf),
    StageFirst,
}

fn shell_segments(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (offset, character) in command.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_none() {
            match character {
                '\'' | '"' | '`' => quote = Some(character),
                ';' | '&' | '|' | '\n' | '(' | ')' => {
                    segments.push(&command[start..offset]);
                    start = offset + character.len_utf8();
                }
                _ => {}
            }
        }
    }
    segments.push(&command[start..]);
    segments
}

fn invocation(command: &str, cwd: &Path) -> Option<CommitInvocation> {
    let mut directory = cwd.to_path_buf();
    let mut stages_first = false;
    let mut candidate = None;
    for segment in shell_segments(command) {
        let Some(words) = shlex::split(segment) else {
            continue;
        };
        let mut words = words.iter().map(String::as_str).peekable();
        while words
            .peek()
            .is_some_and(|word| word.contains('=') || matches!(*word, "command" | "env"))
        {
            words.next();
        }
        match words.next().and_then(|word| word.rsplit('/').next()) {
            Some("cd") => {
                if let Some(path) = words.next() {
                    directory.push(path);
                }
            }
            Some("git") => {
                let mut target = directory.clone();
                while let Some(word) = words.next() {
                    match word {
                        "-C" => {
                            if let Some(path) = words.next() {
                                target.push(path);
                            }
                        }
                        "-c" | "--git-dir" | "--work-tree" => {
                            words.next();
                        }
                        option if option.starts_with('-') => {}
                        "add" | "rm" | "mv" | "reset" | "apply" | "update-index" | "restore" => {
                            stages_first = true;
                            break;
                        }
                        "commit" => {
                            if stages_first || candidate.is_some() || commit_changes_stage(words) {
                                return Some(CommitInvocation::StageFirst);
                            }
                            candidate = Some(CommitInvocation::Review(target));
                            break;
                        }
                        _ => break,
                    }
                }
            }
            _ => {}
        }
    }
    candidate
}

fn commit_changes_stage(mut words: impl Iterator<Item = impl AsRef<str>>) -> bool {
    while let Some(word) = words.next() {
        match word.as_ref() {
            "-a"
            | "--all"
            | "-i"
            | "--include"
            | "-o"
            | "--only"
            | "-p"
            | "--patch"
            | "--interactive"
            | "--pathspec-from-file" => return true,
            "-m" | "--message" | "-F" | "--file" | "-c" | "-C" | "--reuse-message"
            | "--reedit-message" | "--author" | "--date" | "--cleanup" | "--fixup" | "--squash"
            | "-t" | "--template" | "--trailer" => {
                words.next();
            }
            flag if flag.starts_with("--pathspec-from-file=") => return true,
            flag if flag.starts_with('-')
                && !flag.starts_with("--")
                && flag[1..].starts_with('a') =>
            {
                return true;
            }
            flag if flag.starts_with('-') => {}
            _ => return true,
        }
    }
    false
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
                .map(ctx_core::schema::SchemaChange::description),
        );
    }
    for finding in api {
        lines.extend(
            finding
                .changes
                .iter()
                .filter(|change| change.destructive)
                .map(ctx_core::review::ApiChange::description),
        );
    }
    Ok(Some(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_staged_commits_and_requires_separate_staging() {
        let root = Path::new("/project");
        assert!(
            matches!(invocation("git -C 'space dir' commit -m 'one; two'", root),
            Some(CommitInvocation::Review(path)) if path == root.join("space dir"))
        );
        assert!(matches!(
            invocation("git add . && git commit -m change", root),
            Some(CommitInvocation::StageFirst)
        ));
        assert!(matches!(
            invocation("git commit -am change", root),
            Some(CommitInvocation::StageFirst)
        ));
        assert!(invocation("printf 'hello; git commit'", root).is_none());
    }
}
