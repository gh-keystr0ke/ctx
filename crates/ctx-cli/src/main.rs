use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
};

use chrono::Utc;
use clap::{ArgAction, Parser, Subcommand};
use ctx_adapters::{
    analyzer::AnalyzerRegistry,
    antigravity::{AntigravityAgent, SubprocessTransport as AntigravitySubprocessTransport},
    business_context::YamlBusinessContextReader,
    claude_code::{ClaudeCodeAgent, SubprocessTransport as ClaudeSubprocessTransport},
    codex::{CodexAgent, SubprocessTransport as CodexSubprocessTransport},
    context_registry,
    federation::{
        ExportManifest, ExportedDocument, ExportedEndpoint, ExternalCallContract,
        FEDERATION_SCHEMA_VERSION, FederatedRepositoryData, FederationError, FederationSyncState,
        NeighborRegistry, RegistryNeighbor, default_export_path, matching_resolutions,
        neighbor_head, path_template, require_service_name,
    },
    git::{GitRepo, ensure_repository},
    gitlab::{GitLabClient, GitLabConfig, UreqTransport},
    sqlite::SqliteStore,
};
use ctx_app::{
    context::{ContextImportError, ContextImporter},
    enrich::{EnrichError, EnrichRunner},
    index::{IndexError, IndexReport, IndexRunner},
    ingest::{CodeDocIngestRunner, GitIngestRunner, GitLabIngestRunner, IngestError},
    ports::{GitRepository, GraphStore, IndexStore, PortError},
    query::{QueryError, QueryService},
    review::{ReviewError, ReviewRunner},
    status::{IndexState, StatusError, StatusHealth, StatusService},
    verification::{
        CandidateOutcome, KnowledgeVerificationService, ReviewedCandidate, StaleClaimOutcome,
        VerificationError, VerificationService,
    },
};
use ctx_core::business::{BusinessKind, ContextImportStats, Visibility};
use ctx_core::context_pack::ContextRequest;
use ctx_core::domain::{ClaimStatus, CommitOid, NodeKind, RelationKind};
use ctx_core::indexing::PlannedNodeAttributes;
use ctx_core::ir::{ApiParam, ParamSource};
use ctx_core::trace::{
    CallResolution, EndpointTrace, FederationResolver as TraceResolver, LocalCall, TerminalReason,
    TraceBudget, VisitedKey, parse_method_path, resolve_endpoint_seeds, trace_endpoint,
};
use ctx_core::verification::{StaleClaim, VerificationDecision};
use serde::{Deserialize, Serialize};
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
    Explain {
        target: String,
        /// Also trace every HTTP endpoint reachable from this target's own
        /// implementation (a Feature's endpoints, a Requirement's, or the
        /// target itself if it's already a handler), shown as a separate
        /// `Traces:` section -- crossing into synchronized neighbors exactly
        /// like `ctx trace`, gated the same way by `--verbose`.
        #[arg(long)]
        trace: bool,
    },
    /// Trace an HTTP endpoint's request sequence: its handler, the data it
    /// reads/writes, its outbound calls, and (crossing into a synchronized
    /// neighbor via `FEDERATED_MATCH`) that neighbor's own sequence.
    Trace {
        /// An endpoint selector ("METHOD /path", e.g. "POST /check") or any
        /// name `ctx impact`/`ctx explain` would resolve to a handler.
        target: String,
        /// Internal: carries the remaining bounds/visited set across a
        /// recursive invocation into a neighbor's own `ctx` binary. Not a
        /// stable CLI contract -- never set this by hand.
        #[arg(long, hide = true)]
        federation_continuation: Option<String>,
    },
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
    /// Manage local neighboring repository checkouts.
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },
    /// Manage where .context/ and .ctx-candidates/ are stored (ADR-CTX-050):
    /// redirected outside the checkout when it belongs to someone else, so
    /// nothing is ever written into a repository you don't own.
    ContextStore {
        #[command(subcommand)]
        command: ContextStoreCommand,
    },
    /// Write this service's public product and HTTP contract manifest.
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Refresh and resolve public knowledge from every registered neighbor.
    Sync,
    /// Inspect synchronized neighboring repository knowledge.
    Federation {
        #[command(subcommand)]
        command: FederationCommand,
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
        /// Re-review every currently stale semantic claim through an
        /// independent agent instead of the default heuristic candidates:
        /// an `accept` verdict is binding (the claim is reactivated,
        /// precise to that one relationship); a `reject` verdict is never
        /// applied automatically, only ever printed as a suggestion for a
        /// human to act on. Uses `--agent`/`--model` like `--auto`.
        /// Conflicts with `--knowledge`.
        #[arg(long, conflicts_with = "knowledge")]
        stale: bool,
    },
    /// Serve ctx integrations.
    Serve {
        /// Serve the Model Context Protocol over stdio.
        #[arg(long)]
        mcp: bool,
    },
}

#[derive(Debug, Subcommand)]
enum RegistryCommand {
    /// Add a neighboring Git checkout by path.
    Add {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// List registered neighboring checkouts.
    List,
    /// Remove a neighbor by its service name.
    Remove { name: String },
}

#[derive(Debug, Subcommand)]
enum ContextStoreCommand {
    /// Redirect this repository's .context/ and .ctx-candidates/ to `path`.
    /// By default `path` is a plain directory (created if missing) with no
    /// commit-before-index guarantee for documents written there -- pass
    /// `--git` to also turn it into a Git repository (or use one already
    /// there) for the same protection this checkout's own .context/ would
    /// have. Recorded only in this machine's local registry
    /// (`~/.config/ctx/contexts.toml` by default) -- nothing is written into
    /// the current checkout.
    Set {
        path: PathBuf,
        /// Also turn `path` into a Git repository if it isn't one already.
        #[arg(long)]
        git: bool,
    },
    /// Show whether .context/ is currently redirected, and to where.
    Show,
}

#[derive(Debug, Subcommand)]
enum FederationCommand {
    /// List neighbor synchronization and staleness state.
    List,
    /// Show one neighbor's imported public contracts and local resolutions.
    Show { name: String },
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
    #[error(transparent)]
    Federation(#[from] FederationError),
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
    #[error(
        "neighbor '{name}' has federation schema version {actual}, but this ctx supports {expected}; upgrade one side before syncing"
    )]
    FederationSchemaMismatch {
        name: String,
        actual: u32,
        expected: u32,
    },
    #[error("neighbor '{name}' exported itself as service '{exported}'")]
    FederationIdentityMismatch { name: String, exported: String },
    #[error("no synchronized data for neighbor '{0}'; run 'ctx sync' first")]
    NoFederationData(String),
    #[error(
        "ctx export requires an index at HEAD {head}; the current index is {indexed}. Run 'ctx index' first"
    )]
    ExportRequiresCurrentIndex { head: String, indexed: String },
    #[error(transparent)]
    Trace(#[from] ctx_core::trace::TraceError),
    #[error("--federation-continuation is not valid JSON: {0}")]
    InvalidTraceContinuation(String),
    #[error(
        "'{target}' is not an endpoint this repository exposes; it's a neighbor's endpoint reached from {handlers}. Trace from there instead, e.g. `ctx trace {first_handler}`"
    )]
    TraceTargetBelongsToCaller {
        target: String,
        handlers: String,
        first_handler: String,
    },
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
        Command::Explain { target, trace } => explain(cli, &git, target, *trace),
        Command::Trace {
            target,
            federation_continuation,
        } => trace(cli, &git, target, federation_continuation.as_deref()),
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
        Command::Registry { command } => registry(cli, &git, command),
        Command::ContextStore { command } => context_store(cli, &git, command),
        Command::Export { out } => export(cli, &git, out.as_deref()),
        Command::Sync => sync(cli, &git),
        Command::Federation { command } => federation(cli, &git, command),
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
            stale,
        } => {
            if *stale {
                verify_stale(cli, &git, agent, model.clone(), author)
            } else if *auto {
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

#[derive(Serialize)]
struct ContextStoreReport {
    repository: PathBuf,
    context_repository: PathBuf,
    external: bool,
    git_backed: bool,
}

fn context_store(cli: &Cli, git: &GitRepo, command: &ContextStoreCommand) -> Result<(), CliError> {
    match command {
        ContextStoreCommand::Set { path, git: use_git } => {
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                env::current_dir()?.join(path)
            };
            if *use_git {
                ensure_repository(&absolute)?;
            } else {
                fs::create_dir_all(&absolute)?;
            }
            let registry_path = context_registry::set(git.root(), &absolute)?;
            if cli.json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "repository": git.root(),
                        "context_repository": absolute,
                        "git_backed": *use_git,
                        "registry": registry_path,
                    })
                );
            } else {
                println!(
                    "Context store for {} set to {} (recorded in {}).",
                    git.root().display(),
                    absolute.display(),
                    registry_path.display()
                );
                if *use_git {
                    println!(
                        "It's a Git repository: documents there get the same \
                         commit-before-index guarantee as this checkout."
                    );
                } else {
                    println!(
                        "It's a plain directory: documents there are read as-is, with no \
                         commit-before-index guarantee (pass --git for that)."
                    );
                }
                println!("Run 'ctx init' to scaffold .context/ there if it isn't already.");
            }
            Ok(())
        }
        ContextStoreCommand::Show => {
            let report = ContextStoreReport {
                repository: git.root().to_path_buf(),
                context_repository: git.context_root().to_path_buf(),
                external: git.has_external_context(),
                git_backed: git.context_is_git_repository(),
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else if report.external {
                println!(
                    "Context store: {} (external, {})",
                    report.context_repository.display(),
                    if report.git_backed {
                        "Git repository"
                    } else {
                        "plain directory"
                    }
                );
            } else {
                println!(
                    "Context store: {} (inside the repository)",
                    report.context_repository.display()
                );
            }
            Ok(())
        }
    }
}

#[derive(Serialize)]
struct RegistryMutationReport {
    neighbor: RegistryNeighbor,
    changed: bool,
}

fn registry(cli: &Cli, git: &GitRepo, command: &RegistryCommand) -> Result<(), CliError> {
    git.ignore_local_state()?;
    match command {
        RegistryCommand::Add { path, name } => {
            let (_, neighbor, changed) = NeighborRegistry::add(git.root(), path, name.as_deref())?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&RegistryMutationReport { neighbor, changed })?
                );
            } else if changed {
                println!("Registered {} at {}", neighbor.name, neighbor.path);
            } else {
                println!(
                    "{} is already registered at {}",
                    neighbor.name, neighbor.path
                );
            }
        }
        RegistryCommand::List => {
            let registry = NeighborRegistry::load(git.root())?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({"neighbors": registry.neighbors}))?
                );
            } else if registry.neighbors.is_empty() {
                println!("No neighbors registered.");
            } else {
                for neighbor in registry.neighbors {
                    println!("{}\t{}", neighbor.name, neighbor.path);
                }
            }
        }
        RegistryCommand::Remove { name } => {
            let removed = NeighborRegistry::remove(git.root(), name)?;
            let database = git.root().join(".ctx/ctx.db");
            if database.exists() {
                SqliteStore::open(&database, git.root())?.remove_federated_repository(name)?;
            }
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&RegistryMutationReport {
                        neighbor: removed,
                        changed: true
                    })?
                );
            } else {
                println!("Removed neighbor {name}");
            }
        }
    }
    Ok(())
}

fn export(cli: &Cli, git: &GitRepo, out: Option<&Path>) -> Result<(), CliError> {
    let manifest = build_export_manifest(git)?;
    let default_path = default_export_path(git.root());
    let path = out.map_or_else(
        || default_path.clone(),
        |requested| {
            if requested.is_absolute() {
                requested.to_path_buf()
            } else {
                git.root().join(requested)
            }
        },
    );
    manifest.write(&path)?;
    git.ignore_local_state()?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "path": path,
                "service_name": manifest.service_name,
                "source_commit": manifest.source_commit,
                "schema_version": manifest.schema_version,
                "documents": manifest.documents.len(),
                "endpoints": manifest.endpoints.len()
            }))?
        );
    } else {
        println!(
            "Exported {} public document(s) and {} endpoint(s) for {} at {}",
            manifest.documents.len(),
            manifest.endpoints.len(),
            manifest.service_name,
            path.display()
        );
    }
    Ok(())
}

fn build_export_manifest(git: &GitRepo) -> Result<ExportManifest, CliError> {
    let service_name = require_service_name(git)?.to_owned();
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let head = git.head()?;
    let indexed = store.latest_commit(&repository.id)?;
    if indexed.as_ref().map(|commit| &commit.oid) != Some(&head.oid) {
        return Err(CliError::ExportRequiresCurrentIndex {
            head: head.oid.to_string(),
            indexed: indexed
                .map_or_else(|| "not indexed".to_owned(), |commit| commit.oid.to_string()),
        });
    }
    let graph = store.load_graph(&repository.id)?;
    let documents = graph
        .nodes
        .values()
        .filter_map(|node| {
            let PlannedNodeAttributes::Business {
                id,
                status,
                visibility,
                body,
                source_uri,
                ..
            } = &node.attributes
            else {
                return None;
            };
            if *visibility != Visibility::Public {
                return None;
            }
            let kind = match node.kind {
                NodeKind::Feature => BusinessKind::Feature,
                NodeKind::Requirement => BusinessKind::Requirement,
                NodeKind::Invariant => BusinessKind::Invariant,
                NodeKind::Decision => BusinessKind::Decision,
                _ => return None,
            };
            Some(ExportedDocument {
                id: id.clone(),
                kind,
                title: node.name.clone(),
                body: body.clone(),
                status: status.clone(),
                visibility: *visibility,
                source_uri: source_uri.clone(),
                content_hash: node.content_hash.clone(),
            })
        })
        .collect::<Vec<_>>();
    let endpoints = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == RelationKind::Exposes)
        .filter_map(|edge| {
            let source = graph.nodes.get(&edge.source)?;
            let target = graph.nodes.get(&edge.target)?;
            let PlannedNodeAttributes::ApiEndpoint { endpoint } = &target.attributes else {
                return None;
            };
            Some(ExportedEndpoint::from_contract(
                source.identifier().to_owned(),
                endpoint,
                &edge.evidence,
            ))
        })
        .collect::<Vec<_>>();
    Ok(ExportManifest::new(
        service_name,
        head.oid.to_string(),
        documents,
        endpoints,
    ))
}

#[derive(Serialize)]
struct NeighborSyncSuccess {
    name: String,
    path: String,
    source_commit: String,
    documents: usize,
    endpoints: usize,
    resolutions: usize,
}

#[derive(Serialize)]
struct NeighborSyncFailure {
    name: String,
    path: String,
    error: String,
}

#[derive(Serialize)]
struct SyncReport {
    synced: Vec<NeighborSyncSuccess>,
    errors: Vec<NeighborSyncFailure>,
    unresolved_calls: Vec<ExternalCallContract>,
}

fn sync(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    require_service_name(git)?;
    let registry = NeighborRegistry::load(git.root())?;
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let graph = store.load_graph(&repository.id)?;
    let local_commit = git.head()?.oid.to_string();
    let calls = external_call_contracts(&graph);
    let binary = federation_binary()?;
    let synced_at = Utc::now().to_rfc3339();
    let mut successes = Vec::new();
    let mut failures = Vec::new();

    for neighbor in &registry.neighbors {
        let result = sync_neighbor(
            &mut store,
            &binary,
            neighbor,
            &local_commit,
            &synced_at,
            &calls,
        );
        match result {
            Ok(success) => successes.push(success),
            Err(error) => failures.push(NeighborSyncFailure {
                name: neighbor.name.clone(),
                path: neighbor.path.clone(),
                error,
            }),
        }
    }
    let all_endpoints = registry
        .neighbors
        .iter()
        .filter_map(|neighbor| store.federated_repository(&neighbor.name).ok())
        .flat_map(|data| data.endpoints)
        .collect::<Vec<_>>();
    let unresolved_calls = unresolved_calls(&calls, &all_endpoints);
    let report = SyncReport {
        synced: successes,
        errors: failures,
        unresolved_calls,
    };
    print_sync_report(cli, &report)?;
    Ok(())
}

fn sync_neighbor(
    store: &mut SqliteStore,
    binary: &Path,
    neighbor: &RegistryNeighbor,
    local_commit: &str,
    synced_at: &str,
    calls: &[ExternalCallContract],
) -> Result<NeighborSyncSuccess, String> {
    let export_path = PathBuf::from(&neighbor.path).join(".ctx/export.json");
    let manifest = export_neighbor(binary, neighbor, &export_path)?;
    if manifest.schema_version != FEDERATION_SCHEMA_VERSION {
        return Err(CliError::FederationSchemaMismatch {
            name: neighbor.name.clone(),
            actual: manifest.schema_version,
            expected: FEDERATION_SCHEMA_VERSION,
        }
        .to_string());
    }
    if manifest.service_name != neighbor.name {
        return Err(CliError::FederationIdentityMismatch {
            name: neighbor.name.clone(),
            exported: manifest.service_name,
        }
        .to_string());
    }
    let resolutions = matching_resolutions(
        &neighbor.name,
        &manifest.source_commit,
        local_commit,
        synced_at,
        calls,
        &manifest.endpoints,
    );
    let state = FederationSyncState {
        source_repo: neighbor.name.clone(),
        source_path: neighbor.path.clone(),
        source_commit: manifest.source_commit.clone(),
        synced_at: synced_at.to_owned(),
        schema_version: manifest.schema_version,
    };
    store
        .replace_federated_repository(&state, &manifest, &resolutions)
        .map_err(|error| error.to_string())?;
    Ok(NeighborSyncSuccess {
        name: neighbor.name.clone(),
        path: neighbor.path.clone(),
        source_commit: manifest.source_commit,
        documents: manifest.documents.len(),
        endpoints: manifest.endpoints.len(),
        resolutions: resolutions.len(),
    })
}

fn print_sync_report(cli: &Cli, report: &SyncReport) -> Result<(), CliError> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for success in &report.synced {
            println!(
                "Synced {} at {} ({} documents, {} endpoints, {} FEDERATED_MATCH records)",
                success.name,
                short_oid(&success.source_commit),
                success.documents,
                success.endpoints,
                success.resolutions
            );
        }
        for failure in &report.errors {
            eprintln!("Neighbor {} failed: {}", failure.name, failure.error);
        }
        for call in &report.unresolved_calls {
            println!(
                "Unresolved: {} {} from {} does not resolve to any known neighbor",
                call.method.as_str(),
                call.path_template,
                call.handler
            );
        }
    }
    Ok(())
}

fn export_neighbor(
    binary: &Path,
    neighbor: &RegistryNeighbor,
    export_path: &Path,
) -> Result<ExportManifest, String> {
    let output = ProcessCommand::new(binary)
        .current_dir(&neighbor.path)
        .arg("--json")
        .arg("export")
        .arg("--out")
        .arg(export_path)
        .output()
        .map_err(|error| {
            format!(
                "could not run ctx for neighbor '{}': {error}",
                neighbor.name
            )
        })?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if message.is_empty() {
            format!("neighbor ctx exited with {}", output.status)
        } else {
            message
        });
    }
    ExportManifest::read(export_path).map_err(|error| error.to_string())
}

fn external_call_contracts(graph: &ctx_core::graph::GraphSnapshot) -> Vec<ExternalCallContract> {
    let mut calls = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == RelationKind::CallsExternal)
        .filter_map(|edge| {
            let source = graph.nodes.get(&edge.source)?;
            let target = graph.nodes.get(&edge.target)?;
            let PlannedNodeAttributes::ExternalCall { call } = &target.attributes else {
                return None;
            };
            Some(ExternalCallContract {
                stable_key: edge.fingerprint.clone(),
                handler: source.identifier().to_owned(),
                method: call.method,
                url: call.url.clone(),
                path_template: path_template(&call.url)?,
            })
        })
        .collect::<Vec<_>>();
    calls.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    calls.dedup();
    calls
}

fn unresolved_calls(
    calls: &[ExternalCallContract],
    endpoints: &[ExportedEndpoint],
) -> Vec<ExternalCallContract> {
    let resolved = calls
        .iter()
        .filter(|call| {
            endpoints.iter().any(|endpoint| {
                call.method == endpoint.method
                    && path_template(&endpoint.path).as_deref() == Some(call.path_template.as_str())
            })
        })
        .map(|call| call.stable_key.as_str())
        .collect::<BTreeSet<_>>();
    calls
        .iter()
        .filter(|call| !resolved.contains(call.stable_key.as_str()))
        .cloned()
        .collect()
}

#[derive(Serialize)]
struct FederationListEntry {
    name: String,
    path: String,
    synced_at: Option<String>,
    source_commit: Option<String>,
    stale: Option<bool>,
}

#[derive(Serialize)]
struct FederationShowReport {
    name: String,
    state: FederationSyncState,
    documents: Vec<ExportedDocument>,
    endpoints: Vec<ExportedEndpoint>,
    resolutions: Vec<ctx_adapters::federation::FederatedResolution>,
    unresolved_calls: Vec<ExternalCallContract>,
}

fn federation(cli: &Cli, git: &GitRepo, command: &FederationCommand) -> Result<(), CliError> {
    let registry = NeighborRegistry::load(git.root())?;
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path, git.context_root())?;
    match command {
        FederationCommand::List => federation_list(cli, &registry, &store),
        FederationCommand::Show { name } => federation_show(cli, git, &registry, &store, name),
    }
}

fn federation_list(
    cli: &Cli,
    registry: &NeighborRegistry,
    store: &SqliteStore,
) -> Result<(), CliError> {
    let states = store
        .federation_sync_states()?
        .into_iter()
        .map(|state| (state.source_repo.clone(), state))
        .collect::<BTreeMap<_, _>>();
    let entries = registry
        .neighbors
        .iter()
        .map(|neighbor| {
            let state = states.get(&neighbor.name);
            FederationListEntry {
                name: neighbor.name.clone(),
                path: neighbor.path.clone(),
                synced_at: state.map(|value| value.synced_at.clone()),
                source_commit: state.map(|value| value.source_commit.clone()),
                stale: state.and_then(|value| {
                    neighbor_head(Path::new(&neighbor.path)).map(|head| head != value.source_commit)
                }),
            }
        })
        .collect::<Vec<_>>();
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"neighbors": entries}))?
        );
    } else {
        print_federation_list(&entries);
    }
    Ok(())
}

fn print_federation_list(entries: &[FederationListEntry]) {
    if entries.is_empty() {
        println!("No neighbors registered.");
        return;
    }
    println!("NAME\tPATH\tSYNCED_AT\tSOURCE_COMMIT\tSTALE?");
    for entry in entries {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            entry.name,
            entry.path,
            entry.synced_at.as_deref().unwrap_or("never"),
            entry
                .source_commit
                .as_deref()
                .map_or_else(|| "-".to_owned(), short_oid),
            entry
                .stale
                .map_or("unknown", |stale| if stale { "yes" } else { "no" })
        );
    }
}

fn federation_show(
    cli: &Cli,
    git: &GitRepo,
    registry: &NeighborRegistry,
    store: &SqliteStore,
    name: &str,
) -> Result<(), CliError> {
    if !registry
        .neighbors
        .iter()
        .any(|neighbor| neighbor.name == name)
    {
        return Err(FederationError::UnknownNeighbor(name.to_owned()).into());
    }
    let FederatedRepositoryData {
        state,
        documents,
        endpoints,
        resolutions,
    } = store.federated_repository(name)?;
    let state = state.ok_or_else(|| CliError::NoFederationData(name.to_owned()))?;
    let repository = git.descriptor()?;
    let calls = external_call_contracts(&store.load_graph(&repository.id)?);
    let all_endpoints = registry
        .neighbors
        .iter()
        .filter_map(|neighbor| store.federated_repository(&neighbor.name).ok())
        .flat_map(|data| data.endpoints)
        .collect::<Vec<_>>();
    let report = FederationShowReport {
        name: name.to_owned(),
        state,
        documents,
        endpoints,
        resolutions,
        unresolved_calls: unresolved_calls(&calls, &all_endpoints),
    };
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_federation_show(&report);
    }
    Ok(())
}

fn print_federation_show(report: &FederationShowReport) {
    println!(
        "{} at {} (synced {})",
        report.name,
        short_oid(&report.state.source_commit),
        report.state.synced_at
    );
    println!("Public documents:");
    for document in &report.documents {
        println!("  - {}: {}", document.id, document.title);
    }
    println!("Endpoints:");
    for endpoint in &report.endpoints {
        println!(
            "  - {} {} -> {}{}",
            endpoint.method.as_str(),
            endpoint.path,
            endpoint.handler,
            format_params(&endpoint.params)
        );
    }
    println!("FEDERATED_MATCH records:");
    for resolution in &report.resolutions {
        println!(
            "  - {} {} from {} -> {}{}",
            resolution.call.method.as_str(),
            resolution.call.path_template,
            resolution.call.handler,
            resolution.endpoint.handler,
            format_params(&resolution.endpoint.params)
        );
    }
    for call in &report.unresolved_calls {
        println!(
            "  - unresolved: {} {} from {} does not resolve to any known neighbor",
            call.method.as_str(),
            call.path_template,
            call.handler
        );
    }
}

/// Renders a compact `(name:source[?][:type], ...)` contract summary for a
/// human-readable listing, empty when the endpoint declares no parameters.
fn format_params(params: &[ApiParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let rendered = params
        .iter()
        .map(|param| {
            let source = match param.source {
                ParamSource::Path => "path",
                ParamSource::Query => "query",
                ParamSource::Body => "body",
            };
            let optional = if param.required { "" } else { "?" };
            param.type_hint.as_deref().map_or_else(
                || format!("{}:{source}{optional}", param.name),
                |type_hint| format!("{}:{source}{optional}:{type_hint}", param.name),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(" ({rendered})")
}

fn federation_binary() -> Result<PathBuf, CliError> {
    Ok(match env::var_os("CTX_FEDERATION_BINARY") {
        Some(path) => PathBuf::from(path),
        None => env::current_exe()?,
    })
}

#[derive(Serialize, Deserialize)]
struct TraceContinuation {
    budget: TraceBudget,
    visited: BTreeSet<VisitedKey>,
    #[serde(default)]
    verbose: bool,
}

impl TraceContinuation {
    fn decode(raw: &str) -> Result<Self, CliError> {
        serde_json::from_str(raw)
            .map_err(|error| CliError::InvalidTraceContinuation(error.to_string()))
    }
}

/// Matches an outbound call against every registered neighbor's last
/// synchronized manifest (never a live fetch/index/sync -- `ADR-FEDERATION-003`
/// reads one synchronized snapshot per service) and, on a fresh-enough match,
/// continues the trace by invoking that neighbor's own `ctx` binary in its
/// own checkout so only that neighbor's own process decides what of its
/// graph is traceable.
struct CliFederationResolver<'a> {
    registry: &'a NeighborRegistry,
    store: &'a SqliteStore,
    binary: &'a Path,
    verbose: bool,
}

impl TraceResolver for CliFederationResolver<'_> {
    fn resolve(
        &mut self,
        call: &LocalCall,
        budget: TraceBudget,
        visited: &BTreeSet<VisitedKey>,
    ) -> CallResolution {
        let Some(call_template) = path_template(&call.url) else {
            return CallResolution::Unresolved(TerminalReason::NoNeighborMatch);
        };
        for neighbor in &self.registry.neighbors {
            let Ok(data) = self.store.federated_repository(&neighbor.name) else {
                continue;
            };
            let Some(state) = &data.state else {
                continue;
            };
            let Some(endpoint) = data.endpoints.iter().find(|endpoint| {
                endpoint.method == call.method
                    && path_template(&endpoint.path).as_deref() == Some(call_template.as_str())
            }) else {
                continue;
            };
            let Some(current_head) = neighbor_head(Path::new(&neighbor.path)) else {
                return CallResolution::Unresolved(TerminalReason::NeighborUnavailable {
                    service: neighbor.name.clone(),
                });
            };
            if current_head != state.source_commit {
                return CallResolution::Unresolved(TerminalReason::NeighborStale {
                    service: neighbor.name.clone(),
                });
            }
            return self.cross(neighbor, endpoint, budget, visited);
        }
        CallResolution::Unresolved(TerminalReason::NoNeighborMatch)
    }
}

impl CliFederationResolver<'_> {
    fn cross(
        &self,
        neighbor: &RegistryNeighbor,
        endpoint: &ExportedEndpoint,
        budget: TraceBudget,
        visited: &BTreeSet<VisitedKey>,
    ) -> CallResolution {
        let unavailable = || {
            CallResolution::Unresolved(TerminalReason::NeighborUnavailable {
                service: neighbor.name.clone(),
            })
        };
        let Ok(payload) = serde_json::to_string(&TraceContinuation {
            budget,
            visited: visited.clone(),
            verbose: self.verbose,
        }) else {
            return unavailable();
        };
        let target = format!("{} {}", endpoint.method.as_str(), endpoint.path);
        let Ok(output) = ProcessCommand::new(self.binary)
            .current_dir(&neighbor.path)
            .arg("trace")
            .arg(&target)
            .arg("--federation-continuation")
            .arg(&payload)
            .output()
        else {
            return unavailable();
        };
        if !output.status.success() {
            return unavailable();
        }
        let Ok(subtree) = serde_json::from_slice::<EndpointTrace>(&output.stdout) else {
            return unavailable();
        };
        if subtree.service != neighbor.name {
            return unavailable();
        }
        CallResolution::Crosses(Box::new(subtree))
    }
}

fn trace(
    cli: &Cli,
    git: &GitRepo,
    target: &str,
    continuation: Option<&str>,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let graph = store.load_graph(&repository.id)?;
    let local_commit = git.head()?.oid.to_string();
    let service = git.service_name().unwrap_or("").to_owned();
    let registry = NeighborRegistry::load(git.root())?;
    let binary = federation_binary()?;
    let continuation = continuation.map(TraceContinuation::decode).transpose()?;
    let verbose = continuation.as_ref().map_or(cli.verbose > 0, |c| c.verbose);

    let seeds = resolve_endpoint_seeds(target, &graph)
        .map_err(|error| hint_not_found(error, target, &graph))?;
    let mut reports = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let mut budget = continuation
            .as_ref()
            .map_or(TraceBudget::root(), |c| c.budget);
        let mut visited = continuation
            .as_ref()
            .map_or_else(BTreeSet::new, |c| c.visited.clone());
        let mut resolver = CliFederationResolver {
            registry: &registry,
            store: &store,
            binary: &binary,
            verbose,
        };
        let mut report = trace_endpoint(
            seed,
            &graph,
            &service,
            &local_commit,
            &mut budget,
            &mut visited,
            &mut resolver,
        );
        if verbose {
            attach_product_context(&mut report, &graph);
        }
        reports.push(report);
    }

    if continuation.is_some() {
        let root = reports
            .into_iter()
            .next()
            .ok_or_else(|| ctx_core::trace::TraceError::NotFound(target.to_owned()))?;
        println!("{}", serde_json::to_string(&root)?);
        return Ok(());
    }
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"query": target, "traces": reports}))?
        );
        return Ok(());
    }
    let total = reports.len();
    for (index, report) in reports.iter().enumerate() {
        if total > 1 {
            println!("[{}/{total}]", index + 1);
        }
        print_endpoint_trace(report, 0);
        println!();
    }
    Ok(())
}

/// Looks up the Features/Requirements mapped to `trace`'s own handler in
/// `graph` (the graph that produced `trace` -- never a crossed neighbor's,
/// which this process never loaded) and attaches them as a display-only
/// annotation. A no-op when the handler is unmapped or unknown; `ctx-core`'s
/// traversal itself never calls this.
fn attach_product_context(trace: &mut EndpointTrace, graph: &ctx_core::graph::GraphSnapshot) {
    let Some(handler) = trace.handler.clone() else {
        return;
    };
    let Ok(reports) = ctx_core::impact::analyze_impact(&handler, graph) else {
        return;
    };
    let Some(report) = reports.into_iter().next() else {
        return;
    };
    let features = report
        .features
        .iter()
        .map(|node| node.identifier.clone())
        .collect::<Vec<_>>();
    let requirements = report
        .requirements
        .iter()
        .map(|node| node.identifier.clone())
        .collect::<Vec<_>>();
    if features.is_empty() && requirements.is_empty() {
        return;
    }
    trace.product_context = Some(ctx_core::trace::ProductContext {
        features,
        requirements,
    });
}

/// `resolve_endpoint_seeds` only ever looks at this repository's own graph,
/// so a target naming an endpoint this repository merely *calls* (typically
/// copy-pasted from `ctx federation show`'s output, which lists a
/// *neighbor's* endpoints) is honestly "not found" rather than silently
/// jumping repositories. When that's exactly what happened, point at the
/// local handler(s) that already reach it instead of leaving a bare error.
fn hint_not_found(
    error: ctx_core::trace::TraceError,
    target: &str,
    graph: &ctx_core::graph::GraphSnapshot,
) -> CliError {
    let Some((method, path)) = parse_method_path(target) else {
        return error.into();
    };
    let Some(normalized) = path_template(&path) else {
        return error.into();
    };
    let handlers = external_call_contracts(graph)
        .into_iter()
        .filter(|call| {
            call.method == method
                && path_template(&call.url).as_deref() == Some(normalized.as_str())
        })
        .map(|call| call.handler)
        .collect::<BTreeSet<_>>();
    let Some(first_handler) = handlers.iter().next().cloned() else {
        return error.into();
    };
    CliError::TraceTargetBelongsToCaller {
        target: target.to_owned(),
        handlers: handlers.into_iter().collect::<Vec<_>>().join(", "),
        first_handler,
    }
}

fn print_endpoint_trace(trace: &EndpointTrace, indent: usize) {
    let pad = "  ".repeat(indent);
    let service = if trace.service.is_empty() {
        "(local)"
    } else {
        trace.service.as_str()
    };
    match &trace.handler {
        Some(handler) => println!(
            "{pad}{service} {} {} -> {handler}",
            trace.method.as_str(),
            trace.path
        ),
        None => println!("{pad}{service} {} {}", trace.method.as_str(), trace.path),
    }
    if let Some(context) = &trace.product_context {
        if !context.features.is_empty() {
            println!("{pad}  features: {}", context.features.join(", "));
        }
        if !context.requirements.is_empty() {
            println!("{pad}  requirements: {}", context.requirements.join(", "));
        }
    }
    if !trace.reads.is_empty() {
        println!("{pad}  reads: {}", trace.reads.join(", "));
    }
    if !trace.writes.is_empty() {
        println!("{pad}  writes: {}", trace.writes.join(", "));
    }
    for call in &trace.calls {
        println!("{pad}  calls: {} {}", call.method.as_str(), call.url);
        match &call.resolution {
            CallResolution::Crosses(subtree) => print_endpoint_trace(subtree, indent + 2),
            CallResolution::Unresolved(reason) => {
                println!("{pad}    -> {}", describe_terminal(reason));
            }
        }
    }
    if let Some(reason) = &trace.stopped {
        println!("{pad}  (stopped: {})", describe_terminal(reason));
    }
}

fn describe_terminal(reason: &TerminalReason) -> String {
    match reason {
        TerminalReason::NoNeighborMatch => {
            "no synchronized neighbor exposes a matching endpoint; no context available past this call"
                .to_owned()
        }
        TerminalReason::NeighborStale { service } => format!(
            "neighbor '{service}' is stale (run `ctx sync`); stopping rather than tracing possibly-outdated structure"
        ),
        TerminalReason::NeighborUnavailable { service } => {
            format!("neighbor '{service}' has no usable synchronized snapshot")
        }
        TerminalReason::RetiredFact => {
            "this fact is no longer active (code changed since it was indexed)".to_owned()
        }
        TerminalReason::Cycle => "already visited earlier in this trace (cycle)".to_owned(),
        TerminalReason::ServiceTransitionCapReached => format!(
            "reached the {}-service-transition limit",
            ctx_core::trace::MAX_SERVICE_TRANSITIONS
        ),
        TerminalReason::NodeCapReached => {
            format!("reached the {}-node limit", ctx_core::trace::MAX_NODES)
        }
        TerminalReason::BranchCapReached => {
            format!("reached the {}-branch limit", ctx_core::trace::MAX_BRANCHES)
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
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
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
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let now = Utc::now().to_rfc3339();
    let writer = YamlBusinessContextReader::new(git.context_root().to_path_buf());
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
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let now = Utc::now().to_rfc3339();
    let writer = YamlBusinessContextReader::new(git.context_root().to_path_buf());

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
            let review_agent = ClaudeCodeAgent::new(
                ClaudeSubprocessTransport::new(binary, cli.verbose > 0),
                model,
            );
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
            let review_agent = CodexAgent::new(
                CodexSubprocessTransport::new(binary, cli.verbose > 0),
                model,
            );
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
            let review_agent = AntigravityAgent::new(
                AntigravitySubprocessTransport::new(binary, cli.verbose > 0),
                model,
            );
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

/// Bounded excerpt cap so one huge symbol body never dominates a stale-claim
/// review prompt -- there's no token-budget renderer to reuse here (that
/// machinery is `ctx-core`-internal), so a flat byte cap does the same job.
const MAX_STALE_CLAIM_EXCERPT_BYTES: usize = 6000;

/// Fills in each claim's `symbol_excerpt` by reading the current file at the
/// `CodeSymbol` side's own indexed byte range -- safe to do even though the
/// claim went stale, since the *symbol node itself* was already re-indexed
/// fresh (only the semantic edge asserting it still satisfies the product
/// intent is what's marked stale); `graph` is loaded from the same store
/// `stale_claims` used, so its ranges match current code. A file that can't
/// be read, or a range that no longer lands on a valid slice (for example
/// uncommitted working-tree edits since the last `ctx index`), leaves
/// `symbol_excerpt` as `None` rather than guessing or panicking.
fn enrich_stale_claims_with_current_code(
    claims: &mut [StaleClaim],
    graph: &ctx_core::graph::GraphSnapshot,
    repo_root: &Path,
) {
    for claim in claims {
        let Some(symbol) = [&claim.source, &claim.target]
            .into_iter()
            .find(|summary| summary.kind == NodeKind::CodeSymbol)
        else {
            continue;
        };
        let Ok(stable_key) = ctx_core::domain::StableKey::new(symbol.stable_key.clone()) else {
            continue;
        };
        let Some(node) = graph.nodes.get(&stable_key) else {
            continue;
        };
        let PlannedNodeAttributes::Symbol {
            file_path, range, ..
        } = &node.attributes
        else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(repo_root.join(file_path)) else {
            continue;
        };
        let Some(excerpt) = content.get(range.start_byte..range.end_byte) else {
            continue;
        };
        let bytes = excerpt.as_bytes();
        claim.symbol_excerpt = Some(if bytes.len() > MAX_STALE_CLAIM_EXCERPT_BYTES {
            format!(
                "{}\n... (truncated)",
                String::from_utf8_lossy(&bytes[..MAX_STALE_CLAIM_EXCERPT_BYTES])
            )
        } else {
            excerpt.to_owned()
        });
    }
}

fn verify_stale(
    cli: &Cli,
    git: &GitRepo,
    agent: &str,
    model: Option<String>,
    author: &str,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let head = git.head()?;
    let now = Utc::now().to_rfc3339();

    let mut claims = VerificationService::new(&mut store).stale_claims(&repository.id)?;
    if claims.is_empty() {
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "claims_reviewed": 0, "reactivated": 0, "suggested_removals": 0, "results": []
                }))?
            );
        } else {
            println!("No stale semantic claims.");
        }
        return Ok(());
    }
    let graph = store.load_graph(&repository.id)?;
    enrich_stale_claims_with_current_code(&mut claims, &graph, git.root());

    eprintln!("Reviewing {} stale claim(s) via {agent}...", claims.len());
    let (report, results) = match agent {
        "claude" => {
            let binary = env::var("CTX_CLAUDE_CLI_BINARY").unwrap_or_else(|_| "claude".to_owned());
            let review_agent = ClaudeCodeAgent::new(
                ClaudeSubprocessTransport::new(binary, cli.verbose > 0),
                model,
            );
            VerificationService::new(&mut store).review_stale_claims(
                &repository.id,
                &head,
                &claims,
                &review_agent,
                author,
                &now,
            )?
        }
        "codex" => {
            let binary = env::var("CTX_CODEX_CLI_BINARY").unwrap_or_else(|_| "codex".to_owned());
            let review_agent = CodexAgent::new(
                CodexSubprocessTransport::new(binary, cli.verbose > 0),
                model,
            );
            VerificationService::new(&mut store).review_stale_claims(
                &repository.id,
                &head,
                &claims,
                &review_agent,
                author,
                &now,
            )?
        }
        "antigravity" => {
            let binary =
                env::var("CTX_ANTIGRAVITY_CLI_BINARY").unwrap_or_else(|_| "agy".to_owned());
            let review_agent = AntigravityAgent::new(
                AntigravitySubprocessTransport::new(binary, cli.verbose > 0),
                model,
            );
            VerificationService::new(&mut store).review_stale_claims(
                &repository.id,
                &head,
                &claims,
                &review_agent,
                author,
                &now,
            )?
        }
        other => return Err(CliError::UnsupportedAgent(other.to_owned())),
    };
    print_stale_review(cli, agent, &report, &results)
}

fn print_stale_review(
    cli: &Cli,
    agent: &str,
    report: &ctx_app::verification::StaleClaimReviewReport,
    results: &[ctx_app::verification::ReviewedStaleClaim],
) -> Result<(), CliError> {
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"report": report, "results": results}))?
        );
        return Ok(());
    }
    for result in results {
        match &result.outcome {
            StaleClaimOutcome::Reactivated => {
                println!("  -> reactivated: {} -> {}", result.source, result.target);
            }
            StaleClaimOutcome::SuggestedRemoval { reasoning } => {
                println!(
                    "  -> suggest removing: {} -> {} ({reasoning})",
                    result.source, result.target
                );
            }
            StaleClaimOutcome::AlreadyChanged => {
                println!(
                    "  -> already changed, skipped: {} -> {}",
                    result.source, result.target
                );
            }
        }
    }
    println!(
        "Reviewed {} stale claim(s) via {agent}: {} reactivated, {} suggested for removal (not applied automatically)",
        report.claims_reviewed, report.reactivated, report.suggested_removals
    );
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
    let store = SqliteStore::open(&database_path, git.context_root())?;
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
    let store = SqliteStore::open(&database_path, git.context_root())?;
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
    print_api_findings(&report.api_findings);
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

fn print_api_findings(findings: &[ctx_core::review::ApiFinding]) {
    if findings.is_empty() {
        return;
    }
    println!("Observed API changes (deterministic; not proven requirement impact):");
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
    let store = SqliteStore::open(&database_path, git.context_root())?;
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
        print_nodes("API contracts", &report.api_contracts);
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

fn explain(cli: &Cli, git: &GitRepo, target: &str, want_trace: bool) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let explanations = QueryService::new(&store).explain(&repository.id, target)?;
    let traces = want_trace
        .then(|| traces_for_implementation(cli, git, &store, &explanations))
        .transpose()?;
    if cli.json {
        let mut payload = json!({"query": target, "matches": explanations});
        if let Some(traces) = &traces {
            payload["traces"] = serde_json::to_value(traces)?;
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
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
        if let Some(visibility) = explanation
            .subjects
            .iter()
            .find_map(|subject| subject.visibility)
        {
            println!("  Visibility: {}", visibility.as_str());
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
    if let Some(traces) = &traces {
        println!("Traces:");
        if traces.is_empty() {
            println!("  (no HTTP endpoint is reachable from this target's own implementation)");
        }
        for report in traces {
            print_endpoint_trace(report, 1);
            println!();
        }
    }
    Ok(())
}

/// Every distinct HTTP endpoint reachable from `explanations`' subjects'
/// own mapped implementation (a Feature's endpoints via its Requirements, a
/// Requirement's own, or the target's own endpoint if it's already a
/// handler), each fully traced exactly like `ctx trace` -- same bounds, same
/// federation crossing, same `--verbose` gate.
fn traces_for_implementation(
    cli: &Cli,
    git: &GitRepo,
    store: &SqliteStore,
    explanations: &[ctx_core::explain::Explanation],
) -> Result<Vec<EndpointTrace>, CliError> {
    let repository = git.descriptor()?;
    let graph = store.load_graph(&repository.id)?;
    let local_commit = git.head()?.oid.to_string();
    let service = git.service_name().unwrap_or("").to_owned();
    let registry = NeighborRegistry::load(git.root())?;
    let binary = federation_binary()?;
    let verbose = cli.verbose > 0;

    let mut endpoint_keys = BTreeSet::new();
    for subject in explanations
        .iter()
        .flat_map(|explanation| &explanation.subjects)
    {
        let Ok(impact_reports) = ctx_core::impact::analyze_impact(&subject.identifier, &graph)
        else {
            continue;
        };
        for symbol in impact_reports
            .iter()
            .flat_map(|report| &report.implementation)
        {
            for edge in &graph.edges {
                if edge.source.as_str() == symbol.stable_key
                    && edge.kind == RelationKind::Exposes
                    && edge.status == ClaimStatus::Active
                {
                    endpoint_keys.insert(edge.target.clone());
                }
            }
        }
    }
    let mut endpoints = endpoint_keys
        .iter()
        .filter_map(|key| graph.nodes.get(key))
        .collect::<Vec<_>>();
    endpoints.sort_by_key(|node| node.stable_key.clone());

    let mut traces = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        let mut budget = TraceBudget::root();
        let mut visited = BTreeSet::new();
        let mut resolver = CliFederationResolver {
            registry: &registry,
            store,
            binary: &binary,
            verbose,
        };
        let mut report = trace_endpoint(
            endpoint,
            &graph,
            &service,
            &local_commit,
            &mut budget,
            &mut visited,
            &mut resolver,
        );
        if verbose {
            attach_product_context(&mut report, &graph);
        }
        traces.push(report);
    }
    Ok(traces)
}

fn find(cli: &Cli, git: &GitRepo, target: &str) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let matches = QueryService::new(&store).find(&repository.id, target)?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"query": target, "matches": matches}))?
        );
        return Ok(());
    }
    let total = matches.len();
    if total > 1 {
        println!("{total} symbols found\n");
    } else if total == 0 {
        println!("0 symbols found");
    }
    for (index, symbol_match) in matches.into_iter().enumerate() {
        if total > 1 {
            println!("[{}/{total}]", index + 1);
        }
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
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
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
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
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
            let claude_agent = ClaudeCodeAgent::new(
                ClaudeSubprocessTransport::new(binary, cli.verbose > 0),
                model,
            );
            EnrichRunner::new(&claude_agent, &mut store).run_with_progress(
                &repository.id,
                &now,
                allow_ungrounded_symbols,
                &mut report_progress,
            )?
        }
        "codex" => {
            let binary = env::var("CTX_CODEX_CLI_BINARY").unwrap_or_else(|_| "codex".to_owned());
            let codex_agent = CodexAgent::new(
                CodexSubprocessTransport::new(binary, cli.verbose > 0),
                model,
            );
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
            let antigravity_agent = AntigravityAgent::new(
                AntigravitySubprocessTransport::new(binary, cli.verbose > 0),
                model,
            );
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
    // The context store may be redirected via `ctx context-store set`
    // (ADR-CTX-050) to a plain directory (the default) or to a Git
    // repository (`--git`); either way it just needs to exist, which
    // `create_dir_all` below already guarantees. Nothing here forces it to
    // become a Git repository -- that choice was made, if at all, at
    // `context-store set` time.
    for directory in ["features", "requirements", "invariants", "decisions"] {
        fs::create_dir_all(git.context_root().join(".context").join(directory))?;
    }
    // Git-tracked pending-candidate queue (ADR-EXT-004) -- unlike .ctx/,
    // meant to be committed once a real candidate file exists in it.
    fs::create_dir_all(git.context_root().join(".ctx-candidates"))?;
    let database_path = ctx_directory.join("ctx.db");
    SqliteStore::open(&database_path, git.context_root())?;
    git.ignore_local_state()?;
    let languages = GitRepo::discover(git.root())?.source_scope().languages;

    if cli.json {
        println!(
            "{}",
            json!({
                "ok": true,
                "repository": git.root(),
                "context_repository": git.context_root(),
                "context_git_backed": git.context_is_git_repository(),
                "database": database_path,
                "languages": languages
            })
        );
    } else {
        println!("Initialized ctx in {}", ctx_directory.display());
        if git.has_external_context() {
            println!(
                "Context store: {} ({})",
                git.context_root().display(),
                if git.context_is_git_repository() {
                    "Git repository"
                } else {
                    "plain directory"
                }
            );
        }
        println!(
            "Enabled analyzers: {}. Next: add source code to Git, then run 'ctx index'.",
            languages.join(", ")
        );
    }
    Ok(())
}

fn index(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let analyzer = AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?;
    let now = Utc::now().to_rfc3339();
    let code = IndexRunner::new(git, &analyzer, &mut store).run(&now)?;
    let repository = git.descriptor()?;
    let head = git.head()?;
    let reader = YamlBusinessContextReader::new(git.context_root().to_path_buf());
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
    let store = SqliteStore::open(&database_path, git.context_root())?;
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
    println!(
        "  Public documents: {} out of {}",
        status.knowledge.public_documents,
        status.knowledge.features
            + status.knowledge.requirements
            + status.knowledge.invariants
            + status.knowledge.decisions
    );
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
