use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
};

use chrono::Utc;
use clap::{ArgAction, Parser, Subcommand};
use ctx_adapters::local_index::{
    LocalIndexError, ensure_fresh, index_local, lock_index, prepare_head_index,
};
use ctx_adapters::{
    analyzer::AnalyzerRegistry,
    business_context::YamlBusinessContextReader,
    context_registry,
    federation::{
        ExportManifest, ExportedDocument, ExportedEndpoint, ExternalCallContract,
        FEDERATION_SCHEMA_VERSION, FederatedRepositoryData, FederationError, FederationSyncState,
        NeighborRegistry, RegistryNeighbor, ambiguous_call_shapes, default_export_path,
        matching_resolutions, neighbor_head, path_template, require_service_name,
    },
    git::{GitRepo, ensure_repository},
    pyright::PyrightError,
    sqlite::SqliteStore,
};
use ctx_app::status::IndexFreshness;
use ctx_app::{
    enrich::{EnrichError, EnrichRunner},
    ingest::IngestError,
    ports::{CommitMetadata, GitRepository, GraphStore, IndexStore, PortError},
    query::{QueryError, QueryService},
    report::ReportError,
    review::{ReviewError, ReviewRunner},
    status::StatusError,
    type_inference::{DEFAULT_TYPE_INFERENCE_CONFIDENCE, InferTypesError},
    verification::{
        CandidateOutcome, KnowledgeVerificationService, ReviewedCandidate, StaleClaimOutcome,
        VerificationError, VerificationService,
    },
};
use ctx_core::business::{BusinessKind, Visibility};
use ctx_core::context_pack::ContextRequest;
use ctx_core::domain::{ClaimStatus, NodeKind, RelationKind, RepositoryId};
use ctx_core::graph::{GraphEvidence, GraphSnapshot};
use ctx_core::indexing::PlannedNodeAttributes;
use ctx_core::ir::{ApiEndpoint, ApiParam, ParamSource};
use ctx_core::trace::{
    CallResolution, EndpointTrace, FederationResolver as TraceResolver, LocalCall, TerminalReason,
    TraceBudget, VisitedKey, parse_method_path, resolve_endpoint_seeds, trace_endpoint,
};
use ctx_core::verification::{StaleClaim, VerificationDecision};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

mod agent_dispatch;
mod agent_pacing;
mod artifacts_command;
mod commit_hook;
mod coverage_command;
mod diagnostics;
mod federation_command;
mod hooks_command;
mod ingest_command;
mod repair_command;
mod report_command;
mod review_format;
mod setup_command;
mod status_command;
mod tab_title;
mod type_inference_command;
mod verify_command;
mod worktree_command;

use agent_dispatch::ConfiguredAgent;
use artifacts_command::{ArtifactsCommand, artifacts};
use federation_command::{
    CliFederationResolver, FederationCommand, attach_product_context, federation,
    federation_binary, print_endpoint_trace, sync, trace,
};
use ingest_command::IngestScopeArg;
use report_command::{ReportCommand, report};
use status_command::status;
use type_inference_command::{infer_types, parse_inference_confidence};
use verify_command::require_current_index;

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
#[allow(
    clippy::struct_excessive_bools,
    reason = "clap parses independent command-line flags"
)]
struct Cli {
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Show more detail. Repeat to increase the verbosity.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,
    /// Refresh an outdated index before a query when its inputs are clean.
    #[arg(long, global = true)]
    refresh: bool,
    /// Write full trace diagnostics to a timestamped file under `.ctx/logs`.
    #[arg(long, global = true)]
    debug: bool,
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    const fn pace_agent(&self) -> bool {
        match self.command {
            Command::Enrich { pace_agent, .. } | Command::Verify { pace_agent, .. } => pace_agent,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum FindKind {
    Requirement,
}

#[derive(Clone, Copy)]
struct EnrichOptions {
    allow_ungrounded_symbols: bool,
    scope: IngestScopeArg,
    related_depth: usize,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize local storage and business-context directories.
    Init,
    /// Incrementally index the repository's current Git commit.
    Index {
        /// Validate and index the exact Git HEAD snapshot, even on a dirty worktree.
        #[arg(long)]
        from_head: bool,
        /// Build a separate index from uncommitted worktree files.
        #[arg(long, conflicts_with = "from_head")]
        preview: bool,
        /// Suppress successful index output.
        #[arg(long)]
        quiet: bool,
    },
    /// Report structural coverage and ranked behavioral decisions.
    Coverage(coverage_command::Options),
    /// Find and repair context drift.
    Repair {
        #[command(subcommand)]
        command: repair_command::RepairCommand,
    },
    /// Manage repository Git hooks.
    Hooks {
        #[command(subcommand)]
        command: hooks_command::HooksCommand,
    },
    /// Infer typed Python ORM writes with a separate Pyright semantic pass.
    InferTypes {
        /// Pyright Type Server executable. It must implement the Type Server Protocol.
        #[arg(long, default_value = "pyright-typeserver")]
        pyright: PathBuf,
        /// Python interpreter to analyze with (defaults to autodetected
        /// project venv). Need not belong to a conventional venv layout.
        #[arg(long)]
        python: Option<PathBuf>,
        /// Project virtualenv directory (defaults to `<repo>/.venv` or `<repo>/venv`).
        #[arg(long)]
        venv: Option<PathBuf>,
        /// Stable epistemic confidence for the resulting Inference edges.
        #[arg(
            long,
            default_value_t = DEFAULT_TYPE_INFERENCE_CONFIDENCE,
            value_parser = parse_inference_confidence
        )]
        confidence: f32,
        /// Per-request Type Server timeout in milliseconds.
        #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u64).range(1..))]
        timeout_ms: u64,
    },
    /// Show current index health and counts.
    Status,
    /// Validate worktree context documents and their symbol links.
    Check,
    /// Show bounded product and implementation impact for a file or symbol.
    Impact {
        /// Indexed symbol, file, product ID, or data contract.
        target: String,
        /// Query the isolated working-tree preview.
        #[arg(long, conflicts_with = "refresh")]
        preview: bool,
    },
    /// Explain a node or a directed `source -> target` claim.
    Explain {
        /// Indexed node or directed claim to explain.
        target: String,
        /// Also trace every HTTP endpoint reachable from this target's own
        /// implementation (a Feature's endpoints, a Requirement's, or the
        /// target itself if it's already a handler), shown as a separate
        /// `Traces:` section -- crossing into synchronized neighbors exactly
        /// like `ctx trace`, gated the same way by `--verbose`.
        #[arg(long)]
        trace: bool,
        /// Query the isolated working-tree preview.
        #[arg(long, conflicts_with = "refresh")]
        preview: bool,
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
    Find {
        /// Indexed name or requirement search terms.
        target: String,
        /// Restrict lookup to a node kind.
        #[arg(long, value_enum)]
        kind: Option<FindKind>,
    },
    /// Inspect and maintain locally stored external artifacts.
    Artifacts {
        #[command(subcommand)]
        command: ArtifactsCommand,
    },
    /// Ingest external development artifacts as evidence-backed source material.
    Ingest {
        /// The source to ingest from ("git": commit messages and branch
        /// names; "code-comments": code comments and docstrings; "gitlab":
        /// issues, merge requests, and their comments — needs a `[gitlab]`
        /// section in .ctx/config.toml and a `CTX_GITLAB_TOKEN` env var;
        /// "jira": only Jira Cloud issues already referenced by an
        /// already-known artifact (commit, branch, GitLab issue/MR), plus
        /// one hop of issues Jira itself reports as related — never the
        /// whole project; needs a `[jira]` section in .ctx/config.toml and
        /// `CTX_JIRA_EMAIL`/`CTX_JIRA_TOKEN` env vars; run `ctx ingest
        /// git`/`gitlab` first so there is something to reference).
        source: String,
        /// Only ingest commits after this OID (branches are always re-synced).
        #[arg(long)]
        since: Option<String>,
        /// Limit network ingestion to artifacts deterministically connected
        /// to this repository's Git and Jira business context.
        #[arg(long, value_enum, default_value = "all")]
        scope: IngestScopeArg,
        /// Jira `RelatedIssue` hops allowed in business-linked scope.
        #[arg(long, default_value_t = 0)]
        related_depth: usize,
        /// Treat code comments/docstrings at HEAD as a complete snapshot and
        /// remove stored entries no longer present.
        #[arg(long)]
        reconcile: bool,
        /// Ignore local provider cursors and skip caches, revalidating Jira
        /// or GitLab state from scratch.
        #[arg(long)]
        refresh: bool,
    },
    /// Analyze ingested artifacts with an AI agent for candidate product
    /// knowledge, queued for human verification via `ctx verify`.
    Enrich {
        /// Pace AI-agent calls and take regular breaks.
        #[arg(long, alias = "siga-siga")]
        pace_agent: bool,
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
        /// In business-linked scope, send only Jira-anchored bundles to the
        /// agent; Git/MR artifacts are supporting evidence, never subjects.
        #[arg(long, value_enum, default_value = "all")]
        scope: IngestScopeArg,
        /// Jira `RelatedIssue` hops admitted into business-linked bundles.
        #[arg(long, default_value_t = 0)]
        related_depth: usize,
    },
    /// Review a branch or working-tree diff in product terms.
    Review {
        /// Git revision, or auto for the merge base with the default branch.
        #[arg(long, default_value = "HEAD")]
        base: String,
        /// Output format for the report.
        #[arg(long, value_enum, default_value = "text")]
        format: review_format::ReviewFormat,
        /// Return exit code 1 if the report meets this condition.
        #[arg(long, value_enum)]
        fail_on: Option<review_format::FailOn>,
        /// List every test structurally reachable from the changed code
        /// (calls, containment, data/API/event interactions) — no
        /// product-intent gating, no confidence threshold, maximally broad.
        /// Answers "what should I run to check this diff", as opposed to the
        /// narrower `related_tests` already shown per finding. Pass a number
        /// to cap the walk to that many call-graph hops; bare `--related-tests`
        /// (or `--related-tests=deep`) walks until the reachable
        /// neighborhood is exhausted.
        #[arg(long, num_args = 0..=1, default_missing_value = "deep")]
        related_tests: Option<String>,
    },
    /// Compile bounded context for a coding task.
    Context {
        /// Task to use for lexical ranking.
        task: String,
        /// Restrict context to the neighborhood of this file.
        #[arg(long)]
        file: Vec<String>,
        /// Restrict context to the neighborhood of this symbol.
        #[arg(long)]
        symbol: Vec<String>,
        #[arg(long, default_value_t = 4_000)]
        token_budget: usize,
        /// Query the isolated working-tree preview.
        #[arg(long, conflicts_with = "refresh")]
        preview: bool,
    },
    /// Manage local neighboring repository checkouts.
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },
    /// Manage where .context/ and .ctx-candidates/ are stored:
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
    /// Generate an internal, static repository report.
    Report {
        #[command(subcommand)]
        command: ReportCommand,
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
        /// Pace AI-agent calls and take regular breaks.
        #[arg(long, alias = "siga-siga")]
        pace_agent: bool,
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
    /// Configure MCP and install the ctx skill for a coding agent.
    Setup {
        /// Agent to configure in this checkout.
        #[arg(value_enum)]
        agent: setup_command::Agent,
        /// Install the agent's opt-in review hook.
        #[arg(long)]
        hooks: bool,
        /// Show the planned diff without writing files.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run a configured review hook.
    Hook {
        /// Hook integration to run.
        #[arg(value_enum)]
        hook: commit_hook::Hook,
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

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Git(#[from] ctx_adapters::git::GitError),
    #[error(transparent)]
    Sqlite(#[from] ctx_adapters::sqlite::SqliteStoreError),
    #[error(transparent)]
    Index(#[from] LocalIndexError),
    #[error(transparent)]
    InferTypes(#[from] InferTypesError),
    #[error(transparent)]
    Pyright(#[from] PyrightError),
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error(transparent)]
    Review(#[from] ReviewError),
    #[error("review found blocking changes")]
    ReviewBlocked,
    #[error("context validation failed")]
    CheckFailed,
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
    ArtifactPrune(#[from] ctx_app::artifact_prune::ArtifactPruneError),
    #[error(transparent)]
    Federation(#[from] FederationError),
    #[error(transparent)]
    Report(#[from] ReportError),
    #[error(transparent)]
    ReportRender(#[from] ctx_report::RenderError),
    #[error("unsupported ingest source '{0}'; supported: git, code-comments, gitlab, jira")]
    UnsupportedIngestSource(String),
    #[error("--refresh is supported only for Jira and GitLab ingestion")]
    UnsupportedRefreshSource,
    #[error("unsupported agent '{0}'; supported: claude, codex, antigravity")]
    UnsupportedAgent(String),
    #[error("--knowledge --accept requires --id <STABLE-ID>")]
    MissingKnowledgeId,
    #[error("invalid --since commit OID: {0}")]
    InvalidSinceOid(String),
    #[error("invalid GitLab configuration: {0}")]
    InvalidGitLabConfig(String),
    #[error("invalid Jira configuration: {0}")]
    InvalidJiraConfig(String),
    #[error("serve currently requires '--mcp'")]
    UnsupportedServe,
    #[error("invalid --related-tests value '{0}'; expected a hop count or 'deep'")]
    InvalidRelatedTestsDepth(String),
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
    #[error("'ctx federation resolve' requires a call shape, e.g. \"POST /v1/items\" (or --list)")]
    FederationResolveMissingCall,
    #[error("'{0}' is not a valid call shape; expected \"METHOD /path\", e.g. \"POST /v1/items\"")]
    FederationResolveInvalidCall(String),
    #[error("'ctx federation resolve' requires exactly one of --neighbor <name> or --external")]
    FederationResolveRequiresChoice,
    #[error(
        "ctx {command} requires an index at HEAD {head}; the current index is {indexed}. Run 'ctx index' first"
    )]
    RequiresCurrentIndex {
        command: &'static str,
        head: String,
        indexed: String,
    },
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::ReviewBlocked | CliError::CheckFailed) => ExitCode::FAILURE,
        Err(CliError::Index(LocalIndexError::InvalidFiles(files))) => {
            if cli.json {
                println!("{}", json!({"ok": false, "invalid_files": files}));
            } else {
                for file in files {
                    eprintln!("{}:{}: {}", file.path, file.line, file.reason);
                }
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            if cli.json {
                let output = json!({"ok": false, "error": error.to_string()});
                eprintln!("{output}");
            } else {
                eprintln!("error: {error}");
            }
            if matches!(cli.command, Command::Review { .. }) {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn execute(cli: &Cli) -> Result<(), CliError> {
    let current = env::current_dir()?;
    let git = GitRepo::discover(&current)?;
    let diagnostics = diagnostics::init(cli.verbose, cli.debug, git.root())?;
    if cli.debug {
        git.ignore_local_state()?;
    }
    if let Some(path) = diagnostics.debug_path() {
        eprintln!("Debug log: {}", path.display());
    }
    let started = std::time::Instant::now();
    tracing::info!(command = cli.command.name(), "command started");
    let result = run(cli, &git);
    match &result {
        Ok(()) => tracing::info!(
            command = cli.command.name(),
            elapsed_ms = started.elapsed().as_millis(),
            "command completed"
        ),
        Err(error) => tracing::info!(
            command = cli.command.name(),
            elapsed_ms = started.elapsed().as_millis(),
            error = %error,
            "command failed"
        ),
    }
    result
}

fn run(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    match &cli.command {
        Command::Init => initialize(cli, git),
        Command::Index {
            from_head,
            quiet,
            preview,
        } => {
            if *preview {
                worktree_command::index_preview(cli, git, *quiet)
            } else {
                index(cli, git, *from_head, *quiet)
            }
        }
        Command::Coverage(options) => coverage_command::run(cli, git, options),
        Command::Repair { command } => repair_command::run(cli, git, command),
        Command::Check => worktree_command::check(cli, git),
        Command::Hooks { command } => hooks_command::run(cli, git, command),
        command @ Command::InferTypes { .. } => infer_types_command(cli, git, command),
        Command::Status => status(cli, git),
        Command::Impact { target, preview } => impact(cli, git, target, *preview),
        Command::Explain {
            target,
            trace,
            preview,
        } => explain(cli, git, target, *trace, *preview),
        Command::Trace {
            target,
            federation_continuation,
        } => trace(cli, git, target, federation_continuation.as_deref()),
        Command::Find { target, kind } => find(cli, git, target, *kind),
        Command::Artifacts { command } => artifacts(cli, git, command),
        command @ Command::Ingest { .. } => ingest_command::from_command(cli, git, command),
        command @ Command::Enrich { .. } => enrich_command(cli, git, command),
        Command::Review {
            base,
            related_tests,
            format,
            fail_on,
        } => review(cli, git, base, related_tests.as_deref(), *format, *fail_on),
        Command::Context {
            task,
            file,
            symbol,
            token_budget,
            preview,
        } => context(cli, git, task, file, symbol, *token_budget, *preview),
        Command::Registry { command } => registry(cli, git, command),
        Command::ContextStore { command } => context_store(cli, git, command),
        Command::Export { out } => export(cli, git, out.as_deref()),
        Command::Report { command } => report(cli, git, command),
        Command::Sync => sync(cli, git),
        Command::Federation { command } => federation(cli, git, command),
        command @ Command::Verify { .. } => verify_command::from_command(cli, git, command),
        Command::Setup {
            agent,
            hooks,
            dry_run,
        } => setup_command::run(cli, git, *agent, *hooks, *dry_run),
        Command::Hook { hook } => commit_hook::run(*hook, git),
        Command::Serve { mcp } => {
            if *mcp {
                ctx_mcp::serve_stdio(git).map_err(CliError::from)
            } else {
                Err(CliError::UnsupportedServe)
            }
        }
    }
}

impl Command {
    const fn name(&self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Index { .. } => "index",
            Self::Hooks { .. } => "hooks",
            Self::InferTypes { .. } => "infer-types",
            Self::Status => "status",
            Self::Check => "check",
            Self::Coverage(_) => "coverage",
            Self::Repair { .. } => "repair",
            Self::Impact { .. } => "impact",
            Self::Explain { .. } => "explain",
            Self::Trace { .. } => "trace",
            Self::Find { .. } => "find",
            Self::Artifacts { .. } => "artifacts",
            Self::Ingest { .. } => "ingest",
            Self::Enrich { .. } => "enrich",
            Self::Review { .. } => "review",
            Self::Context { .. } => "context",
            Self::Registry { .. } => "registry",
            Self::ContextStore { .. } => "context-store",
            Self::Export { .. } => "export",
            Self::Report { .. } => "report",
            Self::Sync => "sync",
            Self::Federation { .. } => "federation",
            Self::Verify { .. } => "verify",
            Self::Serve { .. } => "serve",
            Self::Setup { .. } => "setup",
            Self::Hook { .. } => "hook",
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
    require_current_index(&store, &repository.id, &head, "export")?;
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
    let endpoints = merged_export_endpoints(&graph);
    Ok(ExportManifest::new(
        service_name,
        head.oid.to_string(),
        documents,
        endpoints,
    ))
}

/// Merges every `Exposes` edge that targets the same `(method, path)`
/// endpoint into one exported entry, instead of exporting one per declaring
/// symbol. A real code handler is preferred over an `OpenAPI` operation symbol
/// for the trace-facing `handler`, since it points at callable code; when
/// only an `OpenAPI` operation declares the endpoint, that operation symbol is
/// used instead. Evidence from every contributing edge (code and `OpenAPI`
/// alike) is kept.
fn merged_export_endpoints(graph: &GraphSnapshot) -> Vec<ExportedEndpoint> {
    struct Merged<'a> {
        endpoint: &'a ApiEndpoint,
        handler: String,
        handler_is_openapi: bool,
        evidence: Vec<GraphEvidence>,
    }

    let mut merged: BTreeMap<String, Merged<'_>> = BTreeMap::new();
    for edge in graph
        .edges
        .iter()
        .filter(|edge| edge.kind == RelationKind::Exposes)
    {
        let Some(source) = graph.nodes.get(&edge.source) else {
            continue;
        };
        let Some(target) = graph.nodes.get(&edge.target) else {
            continue;
        };
        let PlannedNodeAttributes::ApiEndpoint { endpoint } = &target.attributes else {
            continue;
        };
        let identifier = format!("{} {}", endpoint.method.as_str(), endpoint.path);
        let source_is_openapi = matches!(
            &source.attributes,
            PlannedNodeAttributes::Symbol { api_endpoints, .. }
                if api_endpoints.iter().any(|declared| declared.openapi.is_some())
        );
        match merged.entry(identifier) {
            Entry::Vacant(slot) => {
                slot.insert(Merged {
                    endpoint,
                    handler: source.identifier().to_owned(),
                    handler_is_openapi: source_is_openapi,
                    evidence: edge.evidence.clone(),
                });
            }
            Entry::Occupied(mut slot) => {
                let entry = slot.get_mut();
                entry.evidence.extend(edge.evidence.iter().cloned());
                if entry.handler_is_openapi && !source_is_openapi {
                    source.identifier().clone_into(&mut entry.handler);
                    entry.handler_is_openapi = false;
                }
            }
        }
    }
    merged
        .into_values()
        .map(|entry| {
            ExportedEndpoint::from_contract(entry.handler, entry.endpoint, &entry.evidence)
        })
        .collect()
}

fn context(
    cli: &Cli,
    git: &GitRepo,
    task: &str,
    files: &[String],
    symbols: &[String],
    token_budget: usize,
    preview: bool,
) -> Result<(), CliError> {
    let repository = git.descriptor()?;
    let (store, freshness) = worktree_command::query_store(cli, git, &repository.id, preview)?;
    let request = ContextRequest {
        task: task.to_owned(),
        files: files.to_vec(),
        symbols: symbols.to_vec(),
        token_budget,
    };
    let pack = QueryService::new(&store).context(&repository.id, &request)?;
    if cli.json {
        let mut payload = serde_json::to_value(&pack)?;
        payload["index"] = serde_json::to_value(&freshness)?;
        if preview {
            worktree_command::mark_preview(&mut payload);
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
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

fn parse_related_tests_mode(
    value: Option<&str>,
) -> Result<ctx_core::review::RelatedTestsMode, CliError> {
    use ctx_core::review::RelatedTestsMode;
    match value {
        None => Ok(RelatedTestsMode::Off),
        Some("deep") => Ok(RelatedTestsMode::Broad { max_depth: None }),
        Some(depth) => depth
            .parse::<usize>()
            .map(|max_depth| RelatedTestsMode::Broad {
                max_depth: Some(max_depth),
            })
            .map_err(|_| CliError::InvalidRelatedTestsDepth(depth.to_owned())),
    }
}

fn review(
    cli: &Cli,
    git: &GitRepo,
    base: &str,
    related_tests: Option<&str>,
    format: review_format::ReviewFormat,
    fail_on: Option<review_format::FailOn>,
) -> Result<(), CliError> {
    let related_tests_mode = parse_related_tests_mode(related_tests)?;
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path, git.context_root())?;
    let analyzer = AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?;
    let repository = git.descriptor()?;
    let report = ReviewRunner::new(git, &analyzer, &store).run(
        &repository.id,
        base,
        cli.verbose > 0,
        related_tests_mode,
    )?;
    let format = if cli.json {
        review_format::ReviewFormat::Json
    } else {
        format
    };
    match format {
        review_format::ReviewFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        review_format::ReviewFormat::Markdown => review_format::print_markdown(&report),
        review_format::ReviewFormat::Text => review_format::print_text(
            &report,
            cli.verbose > 0,
            related_tests_mode != ctx_core::review::RelatedTestsMode::Off,
        ),
    }
    if fail_on.is_some_and(|policy| policy.blocks(&report)) {
        return Err(CliError::ReviewBlocked);
    }
    Ok(())
}

fn impact(cli: &Cli, git: &GitRepo, target: &str, preview: bool) -> Result<(), CliError> {
    let repository = git.descriptor()?;
    let (store, freshness) = worktree_command::query_store(cli, git, &repository.id, preview)?;
    let reports = QueryService::new(&store).impact(&repository.id, target)?;
    if cli.json {
        let mut payload = json!({"query": target, "matches": reports, "index": freshness});
        if preview {
            worktree_command::mark_preview(&mut payload);
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
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

fn explain(
    cli: &Cli,
    git: &GitRepo,
    target: &str,
    want_trace: bool,
    preview: bool,
) -> Result<(), CliError> {
    let repository = git.descriptor()?;
    let (store, freshness) = worktree_command::query_store(cli, git, &repository.id, preview)?;
    let explanations = QueryService::new(&store).explain(&repository.id, target)?;
    let gaps = coverage_command::explain(git, &store, &repository.id, &explanations)?;
    let traces = want_trace
        .then(|| traces_for_implementation(cli, git, &store, &explanations, &freshness))
        .transpose()?;
    if cli.json {
        let mut payload = json!({"query": target, "matches": explanations, "index": freshness});
        if !gaps.is_empty() {
            payload["behavioral"] = json!(gaps);
        }
        if let Some(traces) = &traces {
            payload["traces"] = serde_json::to_value(traces)?;
        }
        if preview {
            worktree_command::mark_preview(&mut payload);
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
        if !explanation.artifact_history.is_empty() {
            println!("  History:");
            for entry in &explanation.artifact_history {
                println!(
                    "    {:?} ({:?}): {} — {}",
                    entry.artifact.identity.kind,
                    entry.kind,
                    entry.artifact.identity.external_id,
                    entry.artifact.title
                );
            }
        }
        print_claims(explanation.claims);
        if total > 1 {
            println!();
        }
    }
    coverage_command::print_explanation(&gaps);
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
    freshness: &IndexFreshness,
) -> Result<Vec<EndpointTrace>, CliError> {
    let repository = git.descriptor()?;
    let graph = store.load_graph(&repository.id)?;
    let local_commit = freshness
        .commit
        .as_ref()
        .unwrap_or(&freshness.head)
        .to_string();
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

fn find(cli: &Cli, git: &GitRepo, target: &str, kind: Option<FindKind>) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let freshness = query_freshness(cli, git, &mut store, &repository.id)?;
    let query = QueryService::new(&store);
    if matches!(kind, Some(FindKind::Requirement)) {
        let matches = query.find_requirements(&repository.id, target)?;
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"query": target, "matches": matches, "index": freshness})
                )?
            );
        } else {
            print_nodes("Requirements", &matches);
        }
        return Ok(());
    }
    let matches = query.find(&repository.id, target)?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"query": target, "matches": matches, "index": freshness})
            )?
        );
        return Ok(());
    }
    if matches.is_empty() {
        println!("0 symbols found");
    }
    for symbol_match in matches {
        let kind = symbol_match.symbol_kind.map_or_else(
            || format!("{:?}", symbol_match.node_kind),
            |kind| format!("{kind:?}"),
        );
        println!("{kind:<12}  {}", symbol_match.identifier);
    }
    Ok(())
}

fn enrich(
    cli: &Cli,
    git: &GitRepo,
    agent: &str,
    model: Option<String>,
    options: EnrichOptions,
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
    let configured_agent =
        ConfiguredAgent::from_name(agent, model, cli.verbose > 0, cli.pace_agent())
            .map_err(CliError::UnsupportedAgent)?
            .with_ledger(git.root());
    let report = match options.scope {
        IngestScopeArg::All => EnrichRunner::new(&configured_agent, &mut store).run_with_progress(
            &repository.id,
            &now,
            options.allow_ungrounded_symbols,
            &mut report_progress,
        )?,
        IngestScopeArg::BusinessLinked => EnrichRunner::new(&configured_agent, &mut store)
            .run_business_linked_with_progress(
                &repository.id,
                &now,
                options.allow_ungrounded_symbols,
                options.related_depth,
                &mut report_progress,
            )?,
    };
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Analyzed {} artifact neighborhood(s), {} candidate(s) proposed \
             ({} skipped, no business anchor; {} skipped, already pending; \
             {} skipped, covered by a known parent)",
            report.neighborhoods_analyzed,
            report.candidates_proposed,
            report.artifacts_skipped_no_business_anchor,
            report.artifacts_skipped_already_pending,
            report.artifacts_skipped_covered_by_parent
        );
        if report.artifacts_deferred > 0 {
            println!(
                "{} artifact(s) got an unusable agent answer and were deferred; run `ctx enrich` again to retry them",
                report.artifacts_deferred
            );
        }
    }
    Ok(())
}

fn enrich_command(cli: &Cli, git: &GitRepo, command: &Command) -> Result<(), CliError> {
    let Command::Enrich {
        agent,
        model,
        allow_ungrounded_symbols,
        scope,
        related_depth,
        ..
    } = command
    else {
        unreachable!("enrich_command is called only for Command::Enrich")
    };
    enrich(
        cli,
        git,
        agent,
        model.clone(),
        EnrichOptions {
            allow_ungrounded_symbols: *allow_ungrounded_symbols,
            scope: *scope,
            related_depth: *related_depth,
        },
    )
}

fn infer_types_command(cli: &Cli, git: &GitRepo, command: &Command) -> Result<(), CliError> {
    let Command::InferTypes {
        pyright,
        python,
        venv,
        confidence,
        timeout_ms,
    } = command
    else {
        unreachable!("infer_types_command is called only for Command::InferTypes")
    };
    infer_types(
        cli,
        git,
        pyright,
        python.as_deref(),
        venv.as_deref(),
        *confidence,
        *timeout_ms,
    )
}

fn print_claims(claims: Vec<ctx_core::explain::ClaimExplanation>) {
    for claim in claims {
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
}

fn print_nodes(label: &str, nodes: &[ctx_core::graph::NodeSummary]) {
    if nodes.is_empty() {
        return;
    }
    println!("{label}:");
    for node in nodes {
        if matches!(node.kind, ctx_core::domain::NodeKind::Requirement) {
            println!(
                "  - {} — {}",
                node.identifier,
                node.name.split_whitespace().collect::<Vec<_>>().join(" ")
            );
        } else {
            println!("  - {}", node.identifier);
        }
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
        println!("Next: ctx setup claude");
        println!("Next: ctx hooks install");
    }
    Ok(())
}

fn index(cli: &Cli, git: &GitRepo, from_head: bool, quiet: bool) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let report = if from_head {
        let _lock = lock_index(git)?;
        let snapshot = prepare_head_index(git)?;
        let mut store = SqliteStore::open(&database_path, git.context_root())?;
        snapshot.persist(git, &mut store)?
    } else {
        let mut store = SqliteStore::open(&database_path, git.context_root())?;
        index_local(git, &mut store)?
    };
    if quiet && !cli.json {
        for failure in &report.code.failed_files {
            eprintln!("{}: {}", failure.path, failure.error);
        }
        return Ok(());
    }
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
        if let Some(count) = report.uncommitted_files_ignored {
            println!("{count} uncommitted file(s) excluded from the index");
        }
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

fn query_freshness(
    cli: &Cli,
    git: &GitRepo,
    store: &mut SqliteStore,
    repository: &ctx_core::domain::RepositoryId,
) -> Result<IndexFreshness, CliError> {
    let refresh = cli.refresh || git.auto_index() == ctx_adapters::git::AutoIndex::Always;
    let freshness = ensure_fresh(git, store, repository, refresh)?;
    if !cli.json
        && let Some(warning) = freshness.warning()
    {
        eprintln!("{warning}");
    }
    Ok(freshness)
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

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
