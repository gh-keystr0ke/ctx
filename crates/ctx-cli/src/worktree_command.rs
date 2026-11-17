use std::{
    fs, io,
    path::{Path, PathBuf},
};

use ctx_adapters::{
    git::GitRepo,
    sqlite::SqliteStore,
    worktree::{PREVIEW_NOTICE, WorktreeSnapshot},
};
use ctx_app::{ports::GitRepository, status::IndexFreshness};
use ctx_core::domain::RepositoryId;
use serde_json::{Value, json};

use crate::{Cli, CliError, database_path, query_freshness};

pub(crate) fn check(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    let issues = WorktreeSnapshot::read(git)?.issues();
    if cli.json {
        println!("{}", json!({"ok": issues.is_empty(), "issues": issues}));
    } else if issues.is_empty() {
        println!("Context validation passed.");
    } else {
        for issue in &issues {
            eprintln!("{}#{}: {}", issue.path, issue.locator, issue.message);
            if !issue.suggestions.is_empty() {
                eprintln!("  Try: {}", issue.suggestions.join(", "));
            }
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(CliError::CheckFailed)
    }
}

pub(crate) fn index_preview(cli: &Cli, git: &GitRepo, quiet: bool) -> Result<(), CliError> {
    let head = git.head()?;
    let snapshot = WorktreeSnapshot::read(git)?;
    let destination = preview_path(git.root());
    let directory = git.root().join(".ctx");
    fs::create_dir_all(&directory)?;
    git.ignore_local_state()?;
    let temp = tempfile::NamedTempFile::new_in(directory)?;
    let mut store = SqliteStore::open(temp.path(), git.context_root())?;
    let report = snapshot.persist_preview(git, head, &mut store)?;
    drop(store);
    temp.persist(destination).map_err(|error| error.error)?;
    if cli.json {
        let mut output = serde_json::to_value(&report)?;
        mark_preview(&mut output);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if !quiet {
        println!("{PREVIEW_NOTICE}");
        println!("Indexed {}", report.code.commit);
    }
    Ok(())
}

fn preview_path(root: &Path) -> PathBuf {
    root.join(".ctx/preview.db")
}

pub(crate) fn open_preview(git: &GitRepo) -> Result<SqliteStore, CliError> {
    let path = preview_path(git.root());
    if !path.is_file() {
        return Err(io::Error::other("no preview index; run 'ctx index --preview'").into());
    }
    Ok(SqliteStore::open_preview(&path, git.context_root())?)
}

pub(crate) fn query_store(
    cli: &Cli,
    git: &GitRepo,
    repository: &RepositoryId,
    preview: bool,
) -> Result<(SqliteStore, IndexFreshness), CliError> {
    if preview {
        let store = open_preview(git)?;
        let mut freshness = ctx_app::status::index_freshness(git, &store, repository)?;
        if !freshness
            .commit
            .as_ref()
            .is_some_and(ctx_core::domain::CommitOid::is_worktree)
        {
            return Err(io::Error::other("preview database contains no worktree snapshot").into());
        }
        freshness.state = ctx_app::status::FreshnessState::Preview;
        freshness.reason = Some(PREVIEW_NOTICE.to_owned());
        if !cli.json {
            println!("{PREVIEW_NOTICE}");
        }
        return Ok((store, freshness));
    }
    let mut store = SqliteStore::open(&database_path(git.root())?, git.context_root())?;
    let freshness = query_freshness(cli, git, &mut store, repository)?;
    Ok((store, freshness))
}

pub(crate) fn mark_preview(output: &mut Value) {
    output["preview"] = json!(true);
    output["valid_for_git"] = json!(false);
    output["notice"] = json!(PREVIEW_NOTICE);
}
