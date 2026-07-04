//! The imperative shell: builds a temporary Git repository for one
//! [`EvaluationCase`], drives it through the same application use cases the
//! CLI exposes, and records their results into a [`CaseRun`].
//!
//! This calls `ctx-app`/`ctx-adapters` directly instead of shelling out to
//! the compiled `ctx` binary, so a case's outcome is the real typed
//! `ReviewReport`/`ImpactReport`/`ContextPack`, not a re-parsed JSON blob,
//! and so no review/impact/context logic is duplicated here.

use std::{fs, path::Path, process::Command};

use chrono::Utc;
use ctx_adapters::{
    analyzer::AnalyzerRegistry,
    business_context::YamlBusinessContextReader,
    git::{GitError, GitRepo},
    sqlite::{SqliteStore, SqliteStoreError},
};
use ctx_app::{
    context::{ContextImportError, ContextImporter},
    index::{IndexError, IndexRunner},
    ports::{GitRepository, PortError},
    query::{QueryError, QueryService},
    review::{ReviewError, ReviewRunner},
    status::{StatusError, StatusService},
};
use ctx_core::context_pack::ContextRequest;
use thiserror::Error;

use crate::{
    cases::{EvaluationCase, Step},
    report::CaseRun,
};

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("failed to execute git {args}: {stderr}")]
    Git { args: String, stderr: String },
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Discover(#[from] GitError),
    #[error(transparent)]
    Database(#[from] SqliteStoreError),
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Context(#[from] ContextImportError),
    #[error(transparent)]
    Review(#[from] ReviewError),
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error(transparent)]
    Status(#[from] StatusError),
}

/// Builds a fresh temporary Git repository, replays `case`'s steps against
/// it through the real `ctx-app` use cases, and returns the recorded run.
///
/// # Errors
///
/// Returns [`HarnessError`] when Git, the filesystem, the database, or any
/// of the driven use cases fail.
pub fn run_case(case: &EvaluationCase) -> Result<CaseRun, HarnessError> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    init_repository(root)?;
    fs::create_dir_all(root.join(".ctx"))?;
    let mut store = SqliteStore::open(&root.join(".ctx").join("ctx.db"))?;

    let mut run = CaseRun::default();
    for step in &case.steps {
        // Rediscovered on every step, matching how each real `ctx` invocation
        // re-reads `.ctx/config.toml` from disk: a case that writes its own
        // config (for example to enable a non-default language) must take
        // effect immediately, not only for a case run before it wrote one.
        let git = GitRepo::discover(root)?;
        let analyzer = AnalyzerRegistry::builtins(root, &git.source_scope().languages)?;
        match step {
            Step::WriteFiles(files) => write_files(root, files)?,
            Step::Commit(message) => commit_all(root, message)?,
            Step::Index => run_index(&git, &analyzer, &mut store)?,
            Step::Review { base } => {
                run.review = Some(run_review(&git, &analyzer, &store, base)?);
            }
            Step::Impact { target } => {
                run.impact = Some(run_impact(&git, &store, target)?);
            }
            Step::Context {
                task,
                symbols,
                token_budget,
            } => {
                run.context = Some(run_context(&git, &store, task, symbols, *token_budget)?);
            }
            Step::Status => {
                run.status = Some(StatusService::new(&git, &store).inspect()?);
            }
        }
    }
    Ok(run)
}

fn init_repository(root: &Path) -> Result<(), HarnessError> {
    run_git(root, &["init", "--quiet"])?;
    run_git(root, &["config", "user.name", "ctx-eval"])?;
    run_git(root, &["config", "user.email", "ctx-eval@example.invalid"])?;
    Ok(())
}

fn write_files(root: &Path, files: &[(&str, String)]) -> Result<(), HarnessError> {
    for (path, content) in files {
        let destination = root.join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, content)?;
    }
    Ok(())
}

fn commit_all(root: &Path, message: &str) -> Result<(), HarnessError> {
    run_git(root, &["add", "-A"])?;
    run_git(root, &["commit", "--quiet", "-m", message])?;
    Ok(())
}

fn run_git(root: &Path, args: &[&str]) -> Result<(), HarnessError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(HarnessError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn run_index(
    git: &GitRepo,
    analyzer: &AnalyzerRegistry,
    store: &mut SqliteStore,
) -> Result<(), HarnessError> {
    let now = Utc::now().to_rfc3339();
    IndexRunner::new(git, analyzer, store).run(&now)?;
    let repository = git.descriptor().map_err(HarnessError::Port)?;
    let head = git.head().map_err(HarnessError::Port)?;
    let reader = YamlBusinessContextReader::new(git.root().to_path_buf());
    ContextImporter::new(&reader, store).run(&repository, &head, &now)?;
    Ok(())
}

fn run_review(
    git: &GitRepo,
    analyzer: &AnalyzerRegistry,
    store: &SqliteStore,
    base: &str,
) -> Result<ctx_core::review::ReviewReport, HarnessError> {
    let repository = git.descriptor().map_err(HarnessError::Port)?;
    ReviewRunner::new(git, analyzer, store)
        .run(&repository.id, base, false)
        .map_err(HarnessError::from)
}

fn run_impact(
    git: &GitRepo,
    store: &SqliteStore,
    target: &str,
) -> Result<ctx_core::impact::ImpactReport, HarnessError> {
    let repository = git.descriptor().map_err(HarnessError::Port)?;
    let mut reports = QueryService::new(store)
        .impact(&repository.id, target)
        .map_err(HarnessError::from)?;
    // Eval fixtures target fully-qualified symbols/IDs, which resolve to
    // exactly one match; ambiguous short-name fan-out is exercised directly
    // in ctx-core's and ctx-cli's own tests instead.
    Ok(reports.remove(0))
}

fn run_context(
    git: &GitRepo,
    store: &SqliteStore,
    task: &str,
    symbols: &[&str],
    token_budget: usize,
) -> Result<ctx_core::context_pack::ContextPack, HarnessError> {
    let repository = git.descriptor().map_err(HarnessError::Port)?;
    let request = ContextRequest {
        task: task.to_owned(),
        files: Vec::new(),
        symbols: symbols.iter().map(|symbol| (*symbol).to_owned()).collect(),
        token_budget,
    };
    QueryService::new(store)
        .context(&repository.id, &request)
        .map_err(HarnessError::from)
}
