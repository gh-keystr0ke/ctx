use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use chrono::Utc;
use clap::{ArgAction, Parser, Subcommand};
use ctx_adapters::{
    business_context::YamlBusinessContextReader, git::GitRepo, python::PythonAnalyzer,
    sqlite::SqliteStore,
};
use ctx_app::{
    context::{ContextImportError, ContextImporter},
    index::{IndexError, IndexReport, IndexRunner},
    ports::{GitRepository, IndexStore, PortError},
    query::{QueryError, QueryService},
    review::{ReviewError, ReviewRunner},
    verification::{VerificationError, VerificationService},
};
use ctx_core::business::ContextImportStats;
use ctx_core::context_pack::ContextRequest;
use ctx_core::verification::VerificationDecision;
use serde::Serialize;
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
    /// Show bounded product and implementation impact for a file or symbol.
    Impact { target: String },
    /// Explain a node or a directed `source -> target` claim.
    Explain { target: String },
    /// Review a branch or working-tree diff in product terms.
    Review {
        #[arg(long, default_value = "HEAD")]
        base: String,
    },
    /// Compile bounded context for a coding task.
    Context {
        task: String,
        #[arg(long)]
        file: Vec<String>,
        #[arg(long)]
        symbol: Vec<String>,
        #[arg(long, default_value_t = 4_000)]
        token_budget: usize,
    },
    /// Review and accept/reject heuristic semantic candidates.
    Verify {
        #[arg(long, conflicts_with = "reject")]
        accept: Option<String>,
        #[arg(long, conflicts_with = "accept")]
        reject: Option<String>,
        #[arg(long, default_value = "local-user")]
        author: String,
    },
}

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Git(#[from] ctx_adapters::git::GitError),
    #[error(transparent)]
    Sqlite(#[from] ctx_adapters::sqlite::SqliteStoreError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Context(#[from] ContextImportError),
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error(transparent)]
    Review(#[from] ReviewError),
    #[error(transparent)]
    Verification(#[from] VerificationError),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("repository operation failed: {0}")]
    Port(#[from] PortError),
    #[error("ctx is not initialized; run 'ctx init' first")]
    NotInitialized,
}

#[derive(Serialize)]
struct FullIndexReport {
    #[serde(flatten)]
    code: IndexReport,
    business_context: ContextImportStats,
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
    match &cli.command {
        Command::Init => initialize(cli, &git),
        Command::Index => index(cli, &git),
        Command::Status => status(cli, &git),
        Command::Impact { target } => impact(cli, &git, target),
        Command::Explain { target } => explain(cli, &git, target),
        Command::Review { base } => review(cli, &git, base),
        Command::Context {
            task,
            file,
            symbol,
            token_budget,
        } => context(cli, &git, task, file, symbol, *token_budget),
        Command::Verify {
            accept,
            reject,
            author,
        } => verify(cli, &git, accept.as_deref(), reject.as_deref(), author),
    }
}

fn verify(
    cli: &Cli,
    git: &GitRepo,
    accept: Option<&str>,
    reject: Option<&str>,
    author: &str,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let head = git.head()?;
    let now = Utc::now().to_rfc3339();
    let mut service = VerificationService::new(&mut store);
    if let Some((fingerprint, decision)) = accept
        .map(|value| (value, VerificationDecision::Accept))
        .or_else(|| reject.map(|value| (value, VerificationDecision::Reject)))
    {
        service.decide(&repository.id, &head, fingerprint, decision, author, &now)?;
        if cli.json {
            println!(
                "{}",
                json!({"ok": true, "fingerprint": fingerprint, "decision": decision})
            );
        } else {
            println!("Recorded {decision:?} for {fingerprint}");
        }
        return Ok(());
    }
    let candidates = service.candidates(&repository.id)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&candidates)?);
        return Ok(());
    }
    if candidates.is_empty() {
        println!("No high-confidence semantic candidates.");
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        print_candidates(&candidates);
        return Ok(());
    }
    for candidate in candidates {
        println!();
        println!(
            "Possible relation: {} {:?} {}",
            candidate.source_identifier, candidate.relation, candidate.target_identifier
        );
        println!("Confidence score: {:.2}", candidate.score.total);
        for evidence in &candidate.evidence {
            println!("  - {evidence}");
        }
        loop {
            print!("[y] accept  [n] reject  [s] skip  [e] explain: ");
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            match answer.trim().to_ascii_lowercase().as_str() {
                "y" => {
                    service.decide(
                        &repository.id,
                        &head,
                        &candidate.fingerprint,
                        VerificationDecision::Accept,
                        author,
                        &now,
                    )?;
                    break;
                }
                "n" => {
                    service.decide(
                        &repository.id,
                        &head,
                        &candidate.fingerprint,
                        VerificationDecision::Reject,
                        author,
                        &now,
                    )?;
                    break;
                }
                "s" => break,
                "e" => println!("Score breakdown: {:#?}", candidate.score),
                _ => println!("Please enter y, n, s, or e."),
            }
        }
    }
    Ok(())
}

fn print_candidates(candidates: &[ctx_core::verification::SemanticCandidate]) {
    for candidate in candidates {
        println!(
            "{}: {} {:?} {} ({:.2})",
            candidate.fingerprint,
            candidate.source_identifier,
            candidate.relation,
            candidate.target_identifier,
            candidate.score.total
        );
    }
}

fn context(
    cli: &Cli,
    git: &GitRepo,
    task: &str,
    files: &[String],
    symbols: &[String],
    token_budget: usize,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let request = ContextRequest {
        task: task.to_owned(),
        files: files.to_vec(),
        symbols: symbols.to_vec(),
        token_budget,
    };
    let pack = QueryService::new(&store).context(&repository.id, &request)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&pack)?);
        return Ok(());
    }
    println!("Task: {}", pack.task);
    println!(
        "Context budget: {}/{} estimated tokens{}",
        pack.estimated_tokens,
        pack.token_budget,
        if pack.truncated { " (truncated)" } else { "" }
    );
    let mut current_priority = None;
    for item in pack.items {
        if current_priority != Some(item.priority) {
            println!();
            println!("{:?}:", item.priority);
            current_priority = Some(item.priority);
        }
        println!("- {} — {}", item.identifier, item.title);
        for line in item.content.lines() {
            println!("  {line}");
        }
    }
    if !pack.evidence.is_empty() {
        println!();
        println!("Evidence:");
        for evidence in pack.evidence {
            println!(
                "- {} ({:?}, {:?}, {:.2})",
                evidence.claim, evidence.claim_class, evidence.status, evidence.confidence
            );
            for source in evidence.sources {
                println!("  {source}");
            }
        }
    }
    if !pack.uncertainties.is_empty() {
        println!();
        println!("Uncertainty:");
        for uncertainty in pack.uncertainties {
            println!("- {}: {}", uncertainty.relationship, uncertainty.reason);
        }
    }
    Ok(())
}

fn review(cli: &Cli, git: &GitRepo, base: &str) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path)?;
    let analyzer = PythonAnalyzer::new(git.root().to_path_buf());
    let repository = git.descriptor()?;
    let report =
        ReviewRunner::new(git, &analyzer, &store).run(&repository.id, base, cli.verbose > 0)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if report.findings.is_empty() {
        println!("No high-confidence product-impact findings.");
    }
    for finding in report.findings {
        println!(
            "{:?} — {} may be affected",
            finding.severity, finding.affected_intent.identifier
        );
        println!("Changed: {}", finding.changed_entity);
        println!("Product context: {}", finding.affected_intent.name);
        println!("Detected change: {:?}", finding.change_kind);
        println!("Why this is relevant: {}", finding.reason);
        for evidence in finding.evidence {
            println!("Evidence: {evidence}");
        }
        if finding.related_tests.is_empty() {
            println!("Related tests: none explicitly linked");
        } else {
            let tests = finding
                .related_tests
                .iter()
                .map(|test| test.identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "Related tests: {tests} ({})",
                if finding.tests_modified {
                    "modified"
                } else {
                    "not modified"
                }
            );
        }
        if finding.possible_requirement_drift {
            println!("Uncertainty: product context was not modified; check for requirement drift.");
        }
        println!("Suggested reviewer action: {}", finding.suggested_action);
        println!();
    }
    if !report.stale_relationships.is_empty() {
        println!("Stale semantic relationships touching changed code:");
        for relationship in report.stale_relationships {
            println!("  - {relationship}");
        }
    }
    if cli.verbose > 0 {
        println!(
            "Suppressed {} formatting/rename/refactor changes.",
            report.suppressed_non_behavioral_changes
        );
    }
    Ok(())
}

fn impact(cli: &Cli, git: &GitRepo, target: &str) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let report = QueryService::new(&store).impact(&repository.id, target)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Impact for {target}");
    print_nodes("Features", &report.features);
    print_nodes("Requirements", &report.requirements);
    print_nodes("Invariants", &report.invariants);
    print_nodes("Decisions", &report.decisions);
    print_nodes("Implementation", &report.implementation);
    print_nodes("Tests", &report.tests);
    if !report.uncertainties.is_empty() {
        println!("Uncertainty:");
        for uncertainty in report.uncertainties {
            println!(
                "  - {} ({}, confidence {:.2})",
                uncertainty.relationship, uncertainty.reason, uncertainty.confidence
            );
        }
    }
    Ok(())
}

fn explain(cli: &Cli, git: &GitRepo, target: &str) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let explanation = QueryService::new(&store).explain(&repository.id, target)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&explanation)?);
        return Ok(());
    }
    println!("Explanation for {target}");
    for claim in explanation.claims {
        println!("- {}", claim.claim);
        println!(
            "  {:?}, {:?}, confidence {:.2}, valid from {}",
            claim.claim_class, claim.status, claim.confidence, claim.valid_from
        );
        println!("  Provenance: {:?} ({})", claim.provenance, claim.producer);
        if let Some(reason) = claim.stale_reason {
            println!("  Stale because: {reason}");
        }
        for evidence in claim.evidence {
            println!(
                "  Evidence: {}#{} at {}",
                evidence.source_uri,
                evidence.locator,
                evidence.commit.as_deref().unwrap_or("unknown")
            );
        }
    }
    Ok(())
}

fn print_nodes(label: &str, nodes: &[ctx_core::graph::NodeSummary]) {
    if nodes.is_empty() {
        return;
    }
    println!("{label}:");
    for node in nodes {
        println!("  - {}", node.identifier);
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
    let code = IndexRunner::new(git, &analyzer, &mut store).run(&now)?;
    let repository = git.descriptor()?;
    let head = git.head()?;
    let reader = YamlBusinessContextReader::new(git.root().to_path_buf());
    let business_context =
        ContextImporter::new(&reader, &mut store).run(&repository, &head, &now)?;
    let report = FullIndexReport {
        code,
        business_context,
    };
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.code.already_current {
        println!("Index is current at {}", short_oid(&report.code.commit));
    } else {
        println!("Indexed commit {}", short_oid(&report.code.commit));
        println!(
            "{} files parsed, {} nodes created, {} versioned, {} retired",
            report.code.stats.files_reparsed,
            report.code.stats.nodes_created,
            report.code.stats.nodes_versioned,
            report.code.stats.nodes_retired
        );
        if cli.verbose > 0 {
            println!(
                "{} edges recomputed; {} semantic links marked stale",
                report.code.stats.edges_recomputed, report.code.stats.semantic_links_marked_stale
            );
        }
    }
    if !cli.json {
        println!(
            "Business context: {} created, {} versioned, {} explicit links, {} unresolved",
            report.business_context.documents_created,
            report.business_context.documents_versioned,
            report.business_context.explicit_links_created,
            report.business_context.unresolved_symbols.len()
        );
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
