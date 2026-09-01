use std::collections::BTreeMap;

use chrono::Utc;
use clap::{Subcommand, ValueEnum};
use ctx_adapters::{git::GitRepo, sqlite::SqliteStore};
use ctx_app::{
    artifact_prune::{ArtifactPruneOptions, ArtifactPruneReport, ArtifactPruneService},
    ports::{GitRepository, IndexStore},
};
use ctx_core::{
    artifact::ArtifactIdentity,
    artifact_scope::{ArtifactScopeDisposition, ArtifactScopeReason},
};

use super::{Cli, CliError, database_path};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(super) enum PruneScopeArg {
    #[default]
    BusinessLinked,
}

#[derive(Debug, Subcommand)]
pub(super) enum ArtifactsCommand {
    /// Plan or apply removal of artifacts outside deterministic business scope.
    Prune {
        /// Scope policy used to decide which artifacts are retained.
        #[arg(long, value_enum, default_value = "business-linked")]
        scope: PruneScopeArg,
        /// Jira `RelatedIssue` hops retained after a repository-backed issue.
        #[arg(long, default_value_t = 0)]
        related_depth: usize,
        /// Apply the plan. Without this flag the command is a dry run.
        #[arg(long)]
        apply: bool,
    },
}

pub(super) fn artifacts(
    cli: &Cli,
    git: &GitRepo,
    command: &ArtifactsCommand,
) -> Result<(), CliError> {
    match command {
        ArtifactsCommand::Prune {
            scope: PruneScopeArg::BusinessLinked,
            related_depth,
            apply,
        } => prune(cli, git, *related_depth, *apply),
    }
}

fn prune(cli: &Cli, git: &GitRepo, related_depth: usize, apply: bool) -> Result<(), CliError> {
    let mut store = SqliteStore::open(&database_path(git.root())?, git.context_root())?;
    let repository = git.descriptor()?;
    store.ensure_repository(&repository, &Utc::now().to_rfc3339())?;
    let report = ArtifactPruneService::new(&mut store).run(
        &repository.id,
        ArtifactPruneOptions {
            related_jira_depth: related_depth,
            apply,
        },
    )?;
    log_prune_decisions(&report);
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_prune_report(&report, cli.verbose);
    }
    Ok(())
}

fn log_prune_decisions(report: &ArtifactPruneReport) {
    for decision in report
        .decisions
        .iter()
        .filter(|decision| decision.disposition == ArtifactScopeDisposition::Prune)
    {
        tracing::info!(
            provider = ?decision.identity.provider,
            kind = ?decision.identity.kind,
            external_id = decision.identity.external_id,
            reason = ?decision.reason,
            applied = report.applied,
            "artifact excluded by business-linked scope"
        );
    }
}

fn print_prune_report(report: &ArtifactPruneReport, verbose: u8) {
    if report.applied {
        println!(
            "Pruned {} artifact(s); kept {} of {} scanned.",
            report.artifacts_removed.len(),
            report.artifacts_kept,
            report.artifacts_scanned
        );
    } else {
        println!(
            "Dry run: would prune {} artifact(s); keep {} of {} scanned. Use --apply to delete.",
            report.artifacts_pruned, report.artifacts_kept, report.artifacts_scanned
        );
    }
    if !report.pending_candidates_affected.is_empty() {
        println!(
            "{} pending candidate(s) cite evidence in the prune set; candidate files were not changed.",
            report.pending_candidates_affected.len()
        );
    }
    if verbose > 0 {
        for (reason, count) in prune_reason_counts(report) {
            println!("  {reason}: {count}");
        }
    }
    if verbose > 1 {
        print_pruned_identities(report);
        for candidate in &report.pending_candidates_affected {
            println!("  pending candidate: {}", candidate.fingerprint);
        }
    }
}

fn prune_reason_counts(report: &ArtifactPruneReport) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for decision in report
        .decisions
        .iter()
        .filter(|decision| decision.disposition == ArtifactScopeDisposition::Prune)
    {
        *counts.entry(reason_label(&decision.reason)).or_default() += 1;
    }
    counts
}

fn print_pruned_identities(report: &ArtifactPruneReport) {
    for decision in report
        .decisions
        .iter()
        .filter(|decision| decision.disposition == ArtifactScopeDisposition::Prune)
    {
        println!(
            "  {} — {}",
            identity_label(&decision.identity),
            reason_label(&decision.reason)
        );
    }
}

fn identity_label(identity: &ArtifactIdentity) -> String {
    format!(
        "{:?}:{:?}:{}",
        identity.provider, identity.kind, identity.external_id
    )
    .to_lowercase()
}

fn reason_label(reason: &ArtifactScopeReason) -> String {
    format!("{reason:?}")
}
