use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use chrono::Utc;
use clap::{ArgAction, Parser, Subcommand};
use ctx_adapters::{
    analyzer::AnalyzerRegistry,
    antigravity::{AntigravityAgent, SubprocessTransport as AntigravitySubprocessTransport},
    business_context::YamlBusinessContextReader,
    claude_code::{ClaudeCodeAgent, SubprocessTransport as ClaudeSubprocessTransport},
    codex::{CodexAgent, SubprocessTransport as CodexSubprocessTransport},
    git::GitRepo,
    gitlab::{GitLabClient, GitLabConfig, UreqTransport},
    sqlite::SqliteStore,
};
use ctx_app::{
    context::{ContextImportError, ContextImporter},
    enrich::{EnrichError, EnrichRunner},
    index::{IndexError, IndexReport, IndexRunner},
    ingest::{CodeDocIngestRunner, GitIngestRunner, GitLabIngestRunner, IngestError},
    ports::{GitRepository, IndexStore, PortError},
    query::{QueryError, QueryService},
    review::{ReviewError, ReviewRunner},
    status::{IndexState, StatusError, StatusHealth, StatusService},
    verification::{
        CandidateOutcome, KnowledgeVerificationService, ReviewedCandidate, VerificationError,
        VerificationService,
    },
};
use ctx_core::business::ContextImportStats;
use ctx_core::context_pack::ContextRequest;
use ctx_core::domain::CommitOid;
use ctx_core::verification::VerificationDecision;
use serde::Serialize;
use serde_json::json;
use thiserror::Error;

mod tab_title;

const DEFAULT_CONFIG: &str = r#"languages = ["python", "rust", "go"]

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
    /// Discover indexed symbols/nodes by short or exact name.
    Find { target: String },
    /// Ingest external development artifacts as evidence-backed source material.
    Ingest {
        /// The source to ingest from ("git": commit messages and branch
        /// names; "code-comments": code comments and docstrings; "gitlab":
        /// issues, merge requests, and their comments — needs a [gitlab]
        /// section in .ctx/config.toml and a `CTX_GITLAB_TOKEN` env var).
        source: String,
        /// Only ingest commits after this OID (branches are always re-synced).
        #[arg(long)]
        since: Option<String>,
    },
    /// Analyze ingested artifacts with an AI agent for candidate product
    /// knowledge, queued for human verification via `ctx verify`.
    Enrich {
        /// The agent to run: "claude" (headless Claude Code CLI, `claude
        /// -p`), "codex" (headless `OpenAI` Codex CLI, `codex exec`), or
        /// "antigravity" (headless Google Antigravity CLI, `agy -p`).
        #[arg(long, default_value = "claude")]
        agent: String,
        /// Model name to pass to the agent CLI, if it supports one.
        /// Unset uses the agent's own default model.
        #[arg(long)]
        model: Option<String>,
        /// Allow the agent to propose implementation/test candidates from its
        /// own heuristic knowledge of the repository, even when they are not
        /// among the neighborhood's changed symbols or nearby tests. Evidence
        /// artifact-id grounding is unaffected and stays strict either way.
        #[arg(long)]
        allow_ungrounded_symbols: bool,
    },
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
    /// Review and accept/reject heuristic semantic candidates, or
    /// (`--knowledge`) AI-derived candidates from `ctx enrich`.
    Verify {
        #[arg(long, conflicts_with = "reject")]
        accept: Option<String>,
        #[arg(long, conflicts_with = "accept")]
        reject: Option<String>,
        #[arg(long, default_value = "local-user")]
        author: String,
        /// Verify pending AI-derived knowledge candidates instead of the
        /// default heuristic implementation-link candidates.
        #[arg(long)]
        knowledge: bool,
        /// The stable ID (e.g. "REQ-SUB-014") to allocate a `--knowledge
        /// --accept`ed candidate. Required together with `--knowledge
        /// --accept`, ignored otherwise.
        #[arg(long, requires = "accept")]
        id: Option<String>,
        /// With `--knowledge --accept` or `--knowledge --auto`: create the
        /// document even if it looks like a restatement of an already-active
        /// one.
        #[arg(long)]
        force: bool,
        /// Run every pending `--knowledge` candidate through an independent
        /// second-opinion review agent instead of a human: clusters related
        /// candidates, lets the agent accept/reject each on its own merits
        /// and merge a cluster into one document where warranted, and
        /// records every resulting decision as agent-made (`ctx explain`
        /// renders it as "Auto-verified", never as a human review).
        /// Requires `--knowledge` and `--id-prefix`.
        #[arg(long, requires_all = ["knowledge", "id_prefix"])]
        auto: bool,
        /// The agent to run with `--auto`: "claude", "codex", or
        /// "antigravity" (same set as `ctx enrich --agent`).
        #[arg(long, default_value = "claude")]
        agent: String,
        /// Model name to pass to the `--auto` agent CLI, if it supports one.
        #[arg(long)]
        model: Option<String>,
        /// The prefix `--auto` allocates stable IDs under (e.g. "SUB" ->
        /// `REQ-SUB-001`, `INV-SUB-001`, ...). Required together with
        /// `--auto`, ignored otherwise.
        #[arg(long)]
        id_prefix: Option<String>,
    },
    /// Serve ctx integrations.
    Serve {
        /// Serve the Model Context Protocol over stdio.
        #[arg(long)]
        mcp: bool,
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
    Status(#[from] StatusError),
    #[error(transparent)]
    Verification(#[from] VerificationError),
    #[error(transparent)]
    Mcp(#[from] ctx_mcp::McpServerError),
    #[error(transparent)]
    Ingest(#[from] IngestError),
    #[error(transparent)]
    Enrich(#[from] EnrichError),
    #[error("unsupported ingest source '{0}'; supported: git, code-comments, gitlab")]
    UnsupportedIngestSource(String),
    #[error("unsupported agent '{0}'; supported: claude, codex, antigravity")]
    UnsupportedAgent(String),
    #[error("--knowledge --accept requires --id <STABLE-ID>")]
    MissingKnowledgeId,
    #[error("invalid --since commit OID: {0}")]
    InvalidSinceOid(String),
    #[error("invalid GitLab configuration: {0}")]
    InvalidGitLabConfig(String),
    #[error("serve currently requires '--mcp'")]
    UnsupportedServe,
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
        Command::Find { target } => find(cli, &git, target),
        Command::Ingest { source, since } => ingest(cli, &git, source, since.as_deref()),
        Command::Enrich {
            agent,
            model,
            allow_ungrounded_symbols,
        } => enrich(cli, &git, agent, model.clone(), *allow_ungrounded_symbols),
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
            knowledge,
            id,
            force,
            auto,
            agent,
            model,
            id_prefix,
        } => {
            if *auto {
                verify_knowledge_auto(
                    cli,
                    &git,
                    agent,
                    model.clone(),
                    id_prefix.as_deref().expect("clap requires id_prefix"),
                    author,
                    *force,
                )
            } else if *knowledge {
                verify_knowledge(
                    cli,
                    &git,
                    accept.as_deref(),
                    reject.as_deref(),
                    id.as_deref(),
                    author,
                    *force,
                )
            } else {
                verify(cli, &git, accept.as_deref(), reject.as_deref(), author)
            }
        }
        Command::Serve { mcp } => {
            if *mcp {
                ctx_mcp::serve_stdio(&git).map_err(CliError::from)
            } else {
                Err(CliError::UnsupportedServe)
            }
        }
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

fn verify_knowledge(
    cli: &Cli,
    git: &GitRepo,
    accept: Option<&str>,
    reject: Option<&str>,
    id: Option<&str>,
    author: &str,
    force: bool,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let now = Utc::now().to_rfc3339();
    let writer = YamlBusinessContextReader::new(git.root().to_path_buf());
    let mut service = KnowledgeVerificationService::new(&mut store, &writer);

    if let Some(fingerprint) = accept {
        let document_id = id.ok_or(CliError::MissingKnowledgeId)?;
        let path = service.accept(
            &repository.id,
            fingerprint,
            document_id,
            author,
            &now,
            force,
            ctx_core::knowledge::DecisionMethod::Human,
        )?;
        if cli.json {
            println!(
                "{}",
                json!({"ok": true, "fingerprint": fingerprint, "id": document_id, "path": path})
            );
        } else {
            println!("Accepted {fingerprint} as {document_id} -> {path}");
        }
        return Ok(());
    }
    if let Some(fingerprint) = reject {
        service.reject(
            &repository.id,
            fingerprint,
            author,
            &now,
            ctx_core::knowledge::DecisionMethod::Human,
        )?;
        if cli.json {
            println!("{}", json!({"ok": true, "fingerprint": fingerprint}));
        } else {
            println!("Rejected {fingerprint}");
        }
        return Ok(());
    }

    let candidates = service.candidates(&repository.id)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&candidates)?);
        return Ok(());
    }
    if candidates.is_empty() {
        println!("No pending AI-derived knowledge candidates.");
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        print_knowledge_candidates(&candidates);
        return Ok(());
    }
    for candidate in candidates {
        review_knowledge_candidate_interactively(
            &mut service,
            &repository.id,
            &candidate,
            author,
            &now,
            force,
        )?;
    }
    Ok(())
}

/// Collapses `statement` to one printable line for `--auto`'s per-candidate
/// result output, truncated so one long candidate can't push a cluster's
/// whole result block off-screen.
fn summarize_statement(statement: &str) -> String {
    const MAX_CHARS: usize = 100;
    let collapsed = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_CHARS {
        return format!("\"{collapsed}\"");
    }
    let truncated: String = collapsed.chars().take(MAX_CHARS).collect();
    format!("\"{truncated}…\"")
}

#[allow(clippy::too_many_arguments)]
fn verify_knowledge_auto(
    cli: &Cli,
    git: &GitRepo,
    agent: &str,
    model: Option<String>,
    id_prefix: &str,
    author: &str,
    force: bool,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let now = Utc::now().to_rfc3339();
    let writer = YamlBusinessContextReader::new(git.root().to_path_buf());

    // Same reasoning as `enrich`'s own progress output: a real review call
    // per cluster can take tens of seconds, and with several clusters,
    // silence the whole time looks indistinguishable from a hang. Printed
    // to stderr so --json output stays parseable.
    //
    // Also mirrored into the terminal tab title, so the same
    // `[position/total]` is visible at a glance across several open tabs
    // without switching to the one actually running `--auto`.
    let mut report_progress =
        |position: usize, total: usize, cluster: &ctx_core::verification::CandidateCluster| {
            tab_title::set_title(&format!("ctx verify --auto [{position}/{total}] ({agent})"));
            eprintln!(
                "[{position}/{total}] reviewing cluster ({:?}, {} candidate(s)) via {agent}...",
                cluster.kind,
                cluster.fingerprints.len()
            );
        };
    // Printed right after each cluster's decisions are recorded, so the
    // "reviewing cluster..." line above is never the last thing shown for
    // it -- a real user asked what a cluster's outcome actually was right
    // after the progress-output fix landed, since until now `--auto` never
    // showed one.
    let mut report_result = |_position: usize, _total: usize, reviewed: &[ReviewedCandidate]| {
        for candidate in reviewed {
            let summary = summarize_statement(&candidate.statement);
            match &candidate.outcome {
                CandidateOutcome::Accepted { document_id } => {
                    eprintln!("    -> accepted {document_id}: {summary}");
                }
                CandidateOutcome::Rejected => {
                    eprintln!("    -> rejected: {summary}");
                }
                CandidateOutcome::SkippedPossibleDuplicate { existing_id } => {
                    eprintln!("    -> skipped (possible duplicate of {existing_id}): {summary}");
                }
            }
        }
    };
    let report = match agent {
        "claude" => {
            let binary = env::var("CTX_CLAUDE_CLI_BINARY").unwrap_or_else(|_| "claude".to_owned());
            let review_agent = ClaudeCodeAgent::new(ClaudeSubprocessTransport::new(binary, cli.verbose > 0), model);
            KnowledgeVerificationService::new(&mut store, &writer).auto_with_progress(
                &repository.id,
                id_prefix,
                author,
                &now,
                force,
                &review_agent,
                &mut report_progress,
                &mut report_result,
            )?
        }
        "codex" => {
            let binary = env::var("CTX_CODEX_CLI_BINARY").unwrap_or_else(|_| "codex".to_owned());
            let review_agent = CodexAgent::new(CodexSubprocessTransport::new(binary, cli.verbose > 0), model);
            KnowledgeVerificationService::new(&mut store, &writer).auto_with_progress(
                &repository.id,
                id_prefix,
                author,
                &now,
                force,
                &review_agent,
                &mut report_progress,
                &mut report_result,
            )?
        }
        "antigravity" => {
            let binary =
                env::var("CTX_ANTIGRAVITY_CLI_BINARY").unwrap_or_else(|_| "agy".to_owned());
            let review_agent =
                AntigravityAgent::new(AntigravitySubprocessTransport::new(binary, cli.verbose > 0), model);
            KnowledgeVerificationService::new(&mut store, &writer).auto_with_progress(
                &repository.id,
                id_prefix,
                author,
                &now,
                force,
                &review_agent,
                &mut report_progress,
                &mut report_result,
            )?
        }
        other => return Err(CliError::UnsupportedAgent(other.to_owned())),
    };
    tab_title::set_title("ctx");
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Reviewed {} cluster(s) via {agent}: {} document(s) written, {} candidate(s) accepted, {} rejected, {} left pending as possible duplicates",
            report.clusters_reviewed,
            report.documents_written,
            report.candidates_accepted,
            report.candidates_rejected,
            report.candidates_skipped_possible_duplicate
        );
    }
    Ok(())
}

fn review_knowledge_candidate_interactively(
    service: &mut KnowledgeVerificationService<'_, SqliteStore, YamlBusinessContextReader>,
    repository: &ctx_core::domain::RepositoryId,
    candidate: &ctx_core::knowledge::KnowledgeCandidate,
    author: &str,
    now: &str,
    force: bool,
) -> Result<(), CliError> {
    println!();
    println!("Candidate ({:?}): {}", candidate.kind, candidate.statement);
    for evidence in &candidate.evidence {
        println!("  evidence: {} — {}", evidence.locator, evidence.excerpt);
    }
    if !candidate.implementation_candidates.is_empty() {
        println!(
            "  implementation candidates: {}",
            candidate.implementation_candidates.join(", ")
        );
    }
    if !candidate.test_candidates.is_empty() {
        println!(
            "  test candidates: {}",
            candidate.test_candidates.join(", ")
        );
    }
    loop {
        print!("[y] accept  [n] reject  [s] skip: ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        match answer.trim().to_ascii_lowercase().as_str() {
            "y" => {
                print!("Stable ID to allocate (e.g. REQ-SUB-014): ");
                io::stdout().flush()?;
                let mut chosen_id = String::new();
                io::stdin().read_line(&mut chosen_id)?;
                let chosen_id = chosen_id.trim();
                if chosen_id.is_empty() {
                    println!("An ID is required to accept.");
                    continue;
                }
                accept_knowledge_candidate_interactively(
                    service, repository, candidate, chosen_id, author, now, force,
                )?;
                break;
            }
            "n" => {
                service.reject(
                    repository,
                    &candidate.fingerprint,
                    author,
                    now,
                    ctx_core::knowledge::DecisionMethod::Human,
                )?;
                break;
            }
            "s" => break,
            _ => println!("Please enter y, n, or s."),
        }
    }
    Ok(())
}

fn accept_knowledge_candidate_interactively(
    service: &mut KnowledgeVerificationService<'_, SqliteStore, YamlBusinessContextReader>,
    repository: &ctx_core::domain::RepositoryId,
    candidate: &ctx_core::knowledge::KnowledgeCandidate,
    chosen_id: &str,
    author: &str,
    now: &str,
    force: bool,
) -> Result<(), CliError> {
    match service.accept(
        repository,
        &candidate.fingerprint,
        chosen_id,
        author,
        now,
        force,
        ctx_core::knowledge::DecisionMethod::Human,
    ) {
        Ok(path) => {
            println!("Accepted as {chosen_id} -> {path}");
            Ok(())
        }
        Err(VerificationError::PossibleDuplicate { existing_id, .. }) => {
            print!(
                "Looks like a restatement of already-active {existing_id} — create {chosen_id} anyway? [y/n]: "
            );
            io::stdout().flush()?;
            let mut confirm = String::new();
            io::stdin().read_line(&mut confirm)?;
            if confirm.trim().eq_ignore_ascii_case("y") {
                let path = service.accept(
                    repository,
                    &candidate.fingerprint,
                    chosen_id,
                    author,
                    now,
                    true,
                    ctx_core::knowledge::DecisionMethod::Human,
                )?;
                println!("Accepted as {chosen_id} -> {path}");
            } else {
                println!(
                    "Skipped -- consider attaching this evidence to {existing_id} manually instead."
                );
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn print_knowledge_candidates(candidates: &[ctx_core::knowledge::KnowledgeCandidate]) {
    for candidate in candidates {
        println!(
            "{}: ({:?}) {}",
            candidate.fingerprint, candidate.kind, candidate.statement
        );
    }
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
    let analyzer = AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?;
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
    print_schema_findings(&report.schema_findings);
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

fn print_schema_findings(findings: &[ctx_core::review::SchemaFinding]) {
    if findings.is_empty() {
        return;
    }
    println!("Observed schema changes (deterministic; not proven requirement impact):");
    for finding in findings {
        println!(
            "{} — {}",
            if finding.destructive {
                "DESTRUCTIVE"
            } else {
                "informational"
            },
            finding.source_symbol
        );
        for change in &finding.changes {
            println!(
                "  - [{}] {}",
                if change.destructive {
                    "destructive"
                } else {
                    "informational"
                },
                change.description()
            );
        }
        if finding.related_intents.is_empty() {
            println!("  Possible product impact: no known mapping found (not proven unrelated).");
        } else {
            let intents = finding
                .related_intents
                .iter()
                .map(|intent| intent.identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  Possible product impact: {intents}");
        }
        if !finding.related_tests.is_empty() {
            let tests = finding
                .related_tests
                .iter()
                .map(|test| test.identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  Related tests: {tests}");
        }
        println!();
    }
}

fn impact(cli: &Cli, git: &GitRepo, target: &str) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let reports = QueryService::new(&store).impact(&repository.id, target)?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"query": target, "matches": reports}))?
        );
        return Ok(());
    }
    let total = reports.len();
    if total > 1 {
        println!("{total} symbols matched \"{target}\"\n");
    }
    for (index, report) in reports.into_iter().enumerate() {
        if total > 1 {
            let subject = report
                .selected
                .first()
                .map_or(target, |node| node.identifier.as_str());
            println!("[{}/{total}]", index + 1);
            println!("Impact for {subject}");
        } else {
            println!("Impact for {target}");
        }
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
        if total > 1 {
            println!();
        }
    }
    Ok(())
}

fn explain(cli: &Cli, git: &GitRepo, target: &str) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let explanations = QueryService::new(&store).explain(&repository.id, target)?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"query": target, "matches": explanations}))?
        );
        return Ok(());
    }
    let total = explanations.len();
    if total > 1 {
        println!("{total} symbols matched \"{target}\"\n");
    }
    for (index, explanation) in explanations.into_iter().enumerate() {
        if total > 1 {
            let subject = explanation
                .subjects
                .first()
                .map_or(target, |summary| summary.identifier.as_str());
            println!("[{}/{total}]", index + 1);
            println!("Explanation for {subject}");
        } else {
            println!("Explanation for {target}");
        }
        if let Some(provenance) = &explanation.knowledge_provenance {
            println!("  Derived from: {}", provenance.derived_from.join(", "));
            println!(
                "  Inferred by: {} ({})",
                provenance.agent_producer,
                provenance.agent_model.as_deref().unwrap_or("unknown model")
            );
            match provenance.decision_method {
                ctx_core::knowledge::DecisionMethod::Human => println!(
                    "  Verified by: {} at {}",
                    provenance.decided_by, provenance.decided_at
                ),
                ctx_core::knowledge::DecisionMethod::Agent => println!(
                    "  Auto-verified by: {} at {} (no human review)",
                    provenance.decided_by, provenance.decided_at
                ),
            }
        }
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
        if total > 1 {
            println!();
        }
    }
    Ok(())
}

fn find(cli: &Cli, git: &GitRepo, target: &str) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let matches = QueryService::new(&store).find(&repository.id, target)?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"query": target, "matches": matches}))?
        );
        return Ok(());
    }
    println!("{} symbols found\n", matches.len());
    for symbol_match in matches {
        let kind = symbol_match.symbol_kind.map_or_else(
            || format!("{:?}", symbol_match.node_kind),
            |kind| format!("{kind:?}"),
        );
        println!("{kind:<12}  {}", symbol_match.identifier);
    }
    Ok(())
}

fn ingest(cli: &Cli, git: &GitRepo, source: &str, since: Option<&str>) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let now = Utc::now().to_rfc3339();
    // Ingestion is meant to work standalone, before or independent of
    // `ctx index` (prompt3.md's own end-to-end scenario starts from a
    // project with no prior `.context`), so it registers the repository row
    // itself rather than assuming `ctx index` already ran.
    store.ensure_repository(&repository, &now)?;
    let report = match source {
        "git" => {
            let since_oid = since
                .map(CommitOid::new)
                .transpose()
                .map_err(|error| CliError::InvalidSinceOid(error.to_string()))?;
            GitIngestRunner::new(git, &mut store).run(&repository.id, since_oid.as_ref(), &now)?
        }
        "code-comments" => {
            let analyzer = AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?;
            CodeDocIngestRunner::new(git, &analyzer, &mut store).run(&repository.id, &now)?
        }
        "gitlab" => {
            let config = GitLabConfig::load(git.root())
                .map_err(|error| CliError::InvalidGitLabConfig(error.to_string()))?;
            let client = GitLabClient::new(
                UreqTransport::new(config.base_url, config.token),
                config.project,
            );
            GitLabIngestRunner::new(&client, &mut store).run(&repository.id, &now)?
        }
        other => return Err(CliError::UnsupportedIngestSource(other.to_owned())),
    };
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Ingested {} artifact(s), {} link(s) created",
            report.artifacts_ingested, report.links_created
        );
    }
    Ok(())
}

fn enrich(
    cli: &Cli,
    git: &GitRepo,
    agent: &str,
    model: Option<String>,
    allow_ungrounded_symbols: bool,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path)?;
    let repository = git.descriptor()?;
    let now = Utc::now().to_rfc3339();
    store.ensure_repository(&repository, &now)?;
    // Each binary override is overridable for tests, which stand in a fake
    // script instead of depending on a real CLI installation (mirrors
    // CTX_GITLAB_TOKEN's env-var escape hatch for GitLab config).
    //
    // Printed to stderr (never stdout, so --json output stays parseable) so
    // a real agent call -- which can easily take tens of seconds per
    // artifact -- never looks indistinguishable from a hang across a large
    // ingested set.
    let mut report_progress =
        |position: usize, total: usize, subject: &ctx_core::artifact::Artifact| {
            eprintln!(
                "[{position}/{total}] analyzing {:?} {} via {agent}...",
                subject.identity.kind, subject.identity.external_id
            );
        };
    let report = match agent {
        "claude" => {
            let binary = env::var("CTX_CLAUDE_CLI_BINARY").unwrap_or_else(|_| "claude".to_owned());
            let claude_agent = ClaudeCodeAgent::new(ClaudeSubprocessTransport::new(binary, cli.verbose > 0), model);
            EnrichRunner::new(&claude_agent, &mut store).run_with_progress(
                &repository.id,
                &now,
                allow_ungrounded_symbols,
                &mut report_progress,
            )?
        }
        "codex" => {
            let binary = env::var("CTX_CODEX_CLI_BINARY").unwrap_or_else(|_| "codex".to_owned());
            let codex_agent = CodexAgent::new(CodexSubprocessTransport::new(binary, cli.verbose > 0), model);
            EnrichRunner::new(&codex_agent, &mut store).run_with_progress(
                &repository.id,
                &now,
                allow_ungrounded_symbols,
                &mut report_progress,
            )?
        }
        "antigravity" => {
            let binary =
                env::var("CTX_ANTIGRAVITY_CLI_BINARY").unwrap_or_else(|_| "agy".to_owned());
            let antigravity_agent =
                AntigravityAgent::new(AntigravitySubprocessTransport::new(binary, cli.verbose > 0), model);
            EnrichRunner::new(&antigravity_agent, &mut store).run_with_progress(
                &repository.id,
                &now,
                allow_ungrounded_symbols,
                &mut report_progress,
            )?
        }
        other => return Err(CliError::UnsupportedAgent(other.to_owned())),
    };
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Analyzed {} artifact neighborhood(s), {} candidate(s) proposed ({} skipped, already pending)",
            report.neighborhoods_analyzed,
            report.candidates_proposed,
            report.artifacts_skipped_already_pending
        );
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
    git.ignore_local_database()?;
    let languages = GitRepo::discover(git.root())?.source_scope().languages;

    if cli.json {
        println!(
            "{}",
            json!({
                "ok": true,
                "repository": git.root(),
                "database": database_path,
                "languages": languages
            })
        );
    } else {
        println!("Initialized ctx in {}", ctx_directory.display());
        println!(
            "Enabled analyzers: {}. Next: add source code to Git, then run 'ctx index'.",
            languages.join(", ")
        );
    }
    Ok(())
}

fn index(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path)?;
    let analyzer = AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?;
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
        if !report.code.failed_files.is_empty() {
            println!(
                "{} file(s) skipped due to analysis errors (retried on the next index):",
                report.code.failed_files.len()
            );
            for failed in &report.code.failed_files {
                println!("  {}: {}", failed.path, failed.error);
            }
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
    let status = StatusService::new(git, &store).inspect()?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    println!("Repository: {}", status.repository);
    println!("Health: {}", health_label(status.health));
    println!(
        "Index: {} (HEAD {})",
        index_state_label(status.index_state),
        short_oid(status.head_commit.as_str())
    );
    if let Some(indexed) = &status.knowledge.last_indexed_commit {
        println!("Last indexed commit: {}", short_oid(indexed.as_str()));
    }
    println!(
        "Source scope: {} [{}]",
        status.source_scope.languages.join(", "),
        status.source_scope.include.join(", ")
    );
    println!();
    println!("Code:");
    println!("  Files: {}", status.knowledge.files);
    println!("  Symbols: {}", status.knowledge.symbols);
    println!("  Database entities: {}", status.knowledge.db_entities);
    println!("Product context:");
    println!("  Features: {}", status.knowledge.features);
    println!("  Requirements: {}", status.knowledge.requirements);
    println!("  Invariants: {}", status.knowledge.invariants);
    println!("  Decisions: {}", status.knowledge.decisions);
    println!("Relationships:");
    println!("  Structural facts: {}", status.knowledge.structural_facts);
    println!(
        "  Active assertions: {}",
        status.knowledge.active_assertions
    );
    println!(
        "  Active inferences: {}",
        status.knowledge.active_inferences
    );
    println!(
        "  Stale semantics: {}",
        status.knowledge.stale_semantic_edges
    );
    println!(
        "  Rejected inferences: {}",
        status.knowledge.rejected_semantic_edges
    );
    if status.uncommitted_index_inputs.is_empty() {
        println!("Index inputs: clean");
    } else {
        println!("Index inputs differing from HEAD:");
        for path in &status.uncommitted_index_inputs {
            println!("  - {path}");
        }
    }
    if !status.schema_divergences.is_empty() {
        println!("SQLAlchemy/migration schema divergences (best-effort, presence-only):");
        for divergence in &status.schema_divergences {
            let label = match divergence.kind {
                ctx_core::schema::DivergenceKind::ExpectedByOrmOnly => {
                    "ORM expects this column; no migration declares it"
                }
                ctx_core::schema::DivergenceKind::DeclaredByMigrationOnly => {
                    "a migration declares this column; the ORM model has no field for it"
                }
            };
            println!("  - {}.{}: {label}", divergence.entity, divergence.column);
        }
    }
    if !status.notices.is_empty() {
        println!();
        println!("Why this health state:");
        for notice in &status.notices {
            println!("  - {notice}");
        }
    }
    if !status.suggested_actions.is_empty() {
        println!("Next actions:");
        for action in &status.suggested_actions {
            println!("  - {action}");
        }
    }
    Ok(())
}

const fn health_label(health: StatusHealth) -> &'static str {
    match health {
        StatusHealth::Ready => "ready",
        StatusHealth::NeedsIndex => "needs index",
        StatusHealth::NeedsContext => "needs product context",
        StatusHealth::NeedsMappings => "needs semantic mappings",
        StatusHealth::NeedsAttention => "needs attention",
    }
}

const fn index_state_label(state: IndexState) -> &'static str {
    match state {
        IndexState::NotIndexed => "not indexed",
        IndexState::Behind => "behind",
        IndexState::Current => "current",
    }
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
