mod apply;

use clap::{Subcommand, ValueEnum};
use ctx_adapters::{git::GitRepo, sqlite::SqliteStore, worktree::WorktreeSnapshot};
use ctx_app::{
    ports::{GitRepository, GraphStore},
    status::index_freshness,
};
use ctx_core::{
    domain::RepositoryId,
    repair::{IndexedDrift, indexed_drift},
};
use serde::Serialize;
use serde_json::json;

use crate::{Cli, CliError, database_path};

#[derive(Debug, Subcommand)]
pub(crate) enum RepairCommand {
    /// Apply links backed by committed identity evidence.
    Apply {
        #[arg(long, value_enum)]
        policy: RepairPolicy,
    },
    /// Inspect context drift without writing changes.
    Plan {
        /// Return an error status if the plan finds drift.
        #[arg(long)]
        fail_on: Option<FailOn>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum RepairPolicy {
    Safe,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum FailOn {
    Drift,
}

pub(crate) fn run(cli: &Cli, git: &GitRepo, command: &RepairCommand) -> Result<(), CliError> {
    match command {
        RepairCommand::Apply {
            policy: RepairPolicy::Safe,
        } => apply::apply(cli, git),
        RepairCommand::Plan { fail_on } => plan(cli, git, fail_on.is_some()),
    }
}

fn plan(cli: &Cli, git: &GitRepo, fail_on_drift: bool) -> Result<(), CliError> {
    let snapshot = WorktreeSnapshot::read(git)?;
    let issues = snapshot.issues();
    let path = database_path(git.root())?;
    let store = SqliteStore::open_preview(&path, git.context_root())?;
    let repository = git.descriptor()?;
    let freshness = index_freshness(git, &store, &repository.id)?;
    let drift = if freshness.commit.is_some() {
        indexed_drift(&store.load_graph(&repository.id)?)
    } else {
        IndexedDrift::default()
    };
    let links = if freshness.commit.as_ref() == Some(&freshness.head) {
        link_repairs(&snapshot, &issues, &store, &repository.id)?
    } else {
        link_repairs_without_history(&issues)
    };
    let count = issues.len()
        + drift.stale_claims.len()
        + drift.duplicate_documents.len()
        + drift.unmapped_documents.len();
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "validation_issues": issues,
                "index": freshness,
                "links": links,
                "indexed": drift,
                "count": count,
            }))?
        );
    } else {
        if let Some(notice) = freshness.warning() {
            eprintln!("{notice}");
        }
        for issue in &issues {
            println!("{}#{}: {}", issue.path, issue.locator, issue.message);
        }
        for link in &links {
            let replacement = link
                .replacement
                .as_deref()
                .map_or_else(String::new, |target| format!(" -> {target}"));
            println!("{} {}{replacement}", link.class, link.original);
        }
        for claim in &drift.stale_claims {
            println!("R5 stale claim: {claim}");
        }
        for documents in &drift.duplicate_documents {
            println!(
                "R7 possible duplicate: {} -> {}",
                documents.superseded, documents.retained
            );
        }
        for document in &drift.unmapped_documents {
            println!("R8 missing mapping: {document}");
        }
        println!("{count} drift finding(s)");
    }
    if fail_on_drift && count != 0 {
        return Err(CliError::CheckFailed);
    }
    Ok(())
}

#[derive(Serialize)]
struct LinkRepair<'a> {
    class: &'static str,
    path: &'a str,
    locator: &'a str,
    original: &'a str,
    #[serde(skip)]
    test: bool,
    replacement: Option<String>,
    commit: Option<String>,
}

fn link_repairs_without_history(
    issues: &[ctx_adapters::worktree::ContextIssue],
) -> Vec<LinkRepair<'_>> {
    issues
        .iter()
        .filter_map(|issue| {
            issue.link.as_ref().map(|link| LinkRepair {
                class: "R3",
                path: &issue.path,
                locator: &issue.locator,
                original: &link.symbol,
                test: link.test,
                replacement: None,
                commit: None,
            })
        })
        .collect()
}

fn link_repairs<'a>(
    snapshot: &WorktreeSnapshot,
    issues: &'a [ctx_adapters::worktree::ContextIssue],
    store: &SqliteStore,
    repository: &RepositoryId,
) -> Result<Vec<LinkRepair<'a>>, CliError> {
    let mut repairs = link_repairs_without_history(issues);
    for repair in &mut repairs {
        let Some(history) = store.historical_symbol(repository, repair.original)? else {
            continue;
        };
        if let Some(successor) = history.successor
            && snapshot.contains_unique_symbol(&successor.canonical_path)
        {
            repair.class = if repair.test { "R2" } else { "R1" };
            repair.replacement = Some(successor.canonical_path);
            repair.commit = Some(successor.changed_at);
        } else if let Some(commit) = history.retired_at {
            repair.class = "R4";
            repair.commit = Some(commit);
        }
    }
    Ok(repairs)
}
