use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use chrono::Utc;
use clap::{ArgAction, Parser, Subcommand};
use ctx_adapters::{git::GitRepo, python::PythonAnalyzer, sqlite::SqliteStore};
use ctx_app::{
    index::{IndexError, IndexRunner},
    ports::{GitRepository, IndexStore, PortError},
};
use serde_json::json;
use thiserror::Error;

const DEFAULT_CONFIG: &str = r#"language = "python"

[paths]
include = ["src", "tests"]
exclude = ["generated", "vendor", "build", "dist", "target", ".venv"]
"#;

#[derive(Debug, Parser)]
#[command(
    name = "ctx",
    version,
    about = "Trusted product context for code changes"
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize local storage and business-context directories.
    Init,
    /// Incrementally index the repository's current Git commit.
    Index,
    /// Show current index health and counts.
    Status,
}

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Git(#[from] ctx_adapters::git::GitError),
    #[error(transparent)]
    Sqlite(#[from] ctx_adapters::sqlite::SqliteStoreError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("repository operation failed: {0}")]
    Port(#[from] PortError),
    #[error("ctx is not initialized; run 'ctx init' first")]
    NotInitialized,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if cli.json {
                let output = json!({"ok": false, "error": error.to_string()});
                eprintln!("{output}");
            } else {
                eprintln!("error: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), CliError> {
    let current = env::current_dir()?;
    let git = GitRepo::discover(&current)?;
    match cli.command {
        Command::Init => initialize(cli, &git),
        Command::Index => index(cli, &git),
        Command::Status => status(cli, &git),
    }
}

fn initialize(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    let ctx_directory = git.root().join(".ctx");
    fs::create_dir_all(&ctx_directory)?;
    let config_path = ctx_directory.join("config.toml");
    if !config_path.exists() {
        fs::write(&config_path, DEFAULT_CONFIG)?;
    }
    for directory in ["features", "requirements", "invariants", "decisions"] {
        fs::create_dir_all(git.root().join(".context").join(directory))?;
    }
    let database_path = ctx_directory.join("ctx.db");
    SqliteStore::open(&database_path)?;

    if cli.json {
        println!(
            "{}",
            json!({
                "ok": true,
                "repository": git.root(),
                "database": database_path,
                "language": "python"
            })
        );
    } else {
        println!("Initialized ctx in {}", ctx_directory.display());
        println!("Next: add Python code to Git, then run 'ctx index'.");
    }
    Ok(())
}

fn index(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path)?;
    let analyzer = PythonAnalyzer::new(git.root().to_path_buf());
    let now = Utc::now().to_rfc3339();
    let report = IndexRunner::new(git, &analyzer, &mut store).run(&now)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.already_current {
        println!("Index is current at {}", short_oid(&report.commit));
    } else {
        println!("Indexed commit {}", short_oid(&report.commit));
        println!(
            "{} files parsed, {} nodes created, {} versioned, {} retired",
            report.stats.files_reparsed,
            report.stats.nodes_created,
            report.stats.nodes_versioned,
            report.stats.nodes_retired
        );
        if cli.verbose > 0 {
            println!(
                "{} edges recomputed; {} semantic links marked stale",
                report.stats.edges_recomputed, report.stats.semantic_links_marked_stale
            );
        }
    }
    Ok(())
}

fn status(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let status = store.status(&repository.id)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        let commit = status
            .last_indexed_commit
            .as_ref()
            .map_or("not indexed".to_owned(), |oid| short_oid(oid.as_str()));
        println!("Repository: {}", repository.root_path);
        println!("Last indexed commit: {commit}");
        println!("Files: {}", status.files);
        println!("Symbols: {}", status.symbols);
        println!("Active relationships: {}", status.active_edges);
        println!(
            "Stale semantic relationships: {}",
            status.stale_semantic_edges
        );
    }
    Ok(())
}

fn database_path(root: &Path) -> Result<PathBuf, CliError> {
    let path = root.join(".ctx").join("ctx.db");
    if path.exists() {
        Ok(path)
    } else {
        Err(CliError::NotInitialized)
    }
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(12).collect()
}

impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::Port(PortError::new(format!(
            "could not render JSON output: {error}"
        )))
    }
}
