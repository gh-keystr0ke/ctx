use clap::{Subcommand, ValueEnum};
use ctx_adapters::{git::GitRepo, sqlite::SqliteStore, worktree::WorktreeSnapshot};
use ctx_app::{
    ports::{GitRepository, GraphStore},
    status::index_freshness,
};
use ctx_core::repair::indexed_drift;
use serde_json::json;

use crate::{Cli, CliError, database_path};

#[derive(Debug, Subcommand)]
pub(crate) enum RepairCommand {
    /// Inspect context drift without writing changes.
    Plan {
        /// Return an error status if the plan finds drift.
        #[arg(long)]
        fail_on: Option<FailOn>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum FailOn {
    Drift,
}

pub(crate) fn run(cli: &Cli, git: &GitRepo, command: &RepairCommand) -> Result<(), CliError> {
    match command {
        RepairCommand::Plan { fail_on } => plan(cli, git, fail_on.is_some()),
    }
}

fn plan(cli: &Cli, git: &GitRepo, fail_on_drift: bool) -> Result<(), CliError> {
    let issues = WorktreeSnapshot::read(git)?.issues();
    let path = database_path(git.root())?;
    let store = SqliteStore::open_preview(&path, git.context_root())?;
    let repository = git.descriptor()?;
    let freshness = index_freshness(git, &store, &repository.id)?;
    let drift = if freshness.commit.is_some() {
        indexed_drift(&store.load_graph(&repository.id)?)
    } else {
        Default::default()
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
