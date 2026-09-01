use chrono::Utc;
use clap::ValueEnum;
use ctx_adapters::{
    analyzer::AnalyzerRegistry,
    git::GitRepo,
    gitlab::{GitLabClient, GitLabConfig, UreqTransport as GitLabUreqTransport},
    jira::{JiraClient, JiraConfig, UreqTransport as JiraUreqTransport},
    sqlite::SqliteStore,
};
use ctx_app::{
    ingest::{
        ArtifactIngestScope, CodeDocIngestRunner, GitIngestRunner, GitLabIngestRunner,
        JiraIngestRunner,
    },
    ports::{GitRepository, IndexStore},
};
use ctx_core::domain::CommitOid;

use super::{Cli, CliError, database_path};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(super) enum IngestScopeArg {
    #[default]
    All,
    BusinessLinked,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct IngestOptions<'a> {
    pub since: Option<&'a str>,
    pub scope: IngestScopeArg,
    pub related_depth: usize,
    pub reconcile: bool,
}

pub(super) fn ingest(
    cli: &Cli,
    git: &GitRepo,
    source: &str,
    options: IngestOptions<'_>,
) -> Result<(), CliError> {
    tracing::info!(
        source,
        since = options.since,
        scope = ?options.scope,
        related_depth = options.related_depth,
        reconcile = options.reconcile,
        "ingest started"
    );
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let now = Utc::now().to_rfc3339();
    // Ingestion works before `ctx index`, so it owns repository registration.
    store.ensure_repository(&repository, &now)?;
    let report = match source {
        "git" => {
            let since_oid = options
                .since
                .map(CommitOid::new)
                .transpose()
                .map_err(|error| CliError::InvalidSinceOid(error.to_string()))?;
            GitIngestRunner::new(git, &mut store).run(&repository.id, since_oid.as_ref(), &now)?
        }
        "code-comments" => {
            let analyzer = AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?;
            CodeDocIngestRunner::new(git, &analyzer, &mut store).run_with_reconcile(
                &repository.id,
                &now,
                options.reconcile,
            )?
        }
        "gitlab" => ingest_gitlab(git, &mut store, &repository.id, &now, &options)?,
        "jira" => ingest_jira(git, &mut store, &repository.id, &now, &options)?,
        other => return Err(CliError::UnsupportedIngestSource(other.to_owned())),
    };
    tracing::info!(
        source,
        artifacts = report.artifacts_ingested,
        links = report.links_created,
        removed = report.artifacts_removed,
        "ingest completed"
    );
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Ingested {} artifact(s), {} link(s) created, {} artifact(s) removed",
            report.artifacts_ingested, report.links_created, report.artifacts_removed
        );
    }
    Ok(())
}

fn ingest_gitlab(
    git: &GitRepo,
    store: &mut SqliteStore,
    repository: &ctx_core::domain::RepositoryId,
    now: &str,
    options: &IngestOptions<'_>,
) -> Result<ctx_app::ingest::IngestReport, CliError> {
    let config = GitLabConfig::load(git.root())
        .map_err(|error| CliError::InvalidGitLabConfig(error.to_string()))?;
    let client = GitLabClient::new(
        GitLabUreqTransport::new(config.base_url, config.token),
        config.project,
    );
    Ok(GitLabIngestRunner::new(&client, store).run_scoped(
        repository,
        now,
        ingest_scope(options.scope, options.related_depth),
    )?)
}

fn ingest_jira(
    git: &GitRepo,
    store: &mut SqliteStore,
    repository: &ctx_core::domain::RepositoryId,
    now: &str,
    options: &IngestOptions<'_>,
) -> Result<ctx_app::ingest::IngestReport, CliError> {
    let config = JiraConfig::load(git.root())
        .map_err(|error| CliError::InvalidJiraConfig(error.to_string()))?;
    let client = JiraClient::new(
        JiraUreqTransport::new(config.base_url.clone(), &config.email, &config.token),
        config.project,
        config.base_url,
    );
    Ok(JiraIngestRunner::new(&client, store).run_scoped(
        repository,
        now,
        ingest_scope(options.scope, options.related_depth),
    )?)
}

const fn ingest_scope(scope: IngestScopeArg, related_depth: usize) -> ArtifactIngestScope {
    match scope {
        IngestScopeArg::All => ArtifactIngestScope::All,
        IngestScopeArg::BusinessLinked => ArtifactIngestScope::BusinessLinked {
            related_jira_depth: related_depth,
        },
    }
}
