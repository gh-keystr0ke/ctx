mod research;

use std::{collections::BTreeSet, path::Path};

use clap::{Args, Subcommand};
use ctx_adapters::{coverage_store::CoverageStore, git::GitRepo};
use ctx_app::ports::{GitRepository, GraphStore, ReviewRepository};
use ctx_core::{
    coverage::{GapState, analyze_coverage, gap_report, structural_mappings},
    domain::StableKey,
    graph::GraphSnapshot,
    indexing::{FileChange, PlannedNodeAttributes},
};
use serde_json::json;

use crate::{Cli, CliError, worktree_command};

#[derive(Debug, Args)]
pub(crate) struct Options {
    /// Scope to a repository-relative file or directory.
    #[arg(long)]
    path: Option<String>,
    /// Scope to a symbol name, canonical path, or stable key.
    #[arg(long)]
    symbol: Option<String>,
    /// Scope to code mapped to a product requirement.
    #[arg(long)]
    requirement: Option<String>,
    /// Scope to source paths changed since this revision.
    #[arg(long)]
    base: Option<String>,
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Investigate selected gaps with an explicitly chosen CLI agent.
    Research(research::ResearchOptions),
}

pub(crate) fn run(cli: &Cli, git: &GitRepo, options: &Options) -> Result<(), CliError> {
    let repository = git.descriptor()?;
    let (store, freshness) = worktree_command::query_store(cli, git, &repository.id, false)?;
    let graph = store.load_graph(&repository.id)?;
    let changed = changed_paths(git, options.base.as_deref())?;
    let scoped = scope(&graph, options, changed.as_ref());
    let report = analyze_coverage(&graph, &scoped);
    let analyzed_files: BTreeSet<_> = graph
        .nodes
        .values()
        .filter_map(|node| {
            if let PlannedNodeAttributes::File { path, .. } = &node.attributes {
                Some(path.as_str())
            } else {
                None
            }
        })
        .collect();
    let unanalyzed: Vec<_> = git
        .all_source_files()?
        .into_iter()
        .filter(|path| matches_path(path, options, changed.as_ref()))
        .filter(|path| !analyzed_files.contains(path.as_str()))
        .collect();
    let records = CoverageStore::new(git.context_root()).read()?;
    let gaps = gap_report(&graph, &report.decisions, &records, &scoped);
    if let Some(Action::Research(options)) = &options.action {
        let commit = freshness
            .commit
            .as_ref()
            .filter(|commit| *commit == &freshness.head)
            .ok_or_else(|| std::io::Error::other("index HEAD before researching coverage"))?;
        return research::run(cli, git, &graph, &gaps, commit.as_str(), options);
    }
    let actionable = |state| matches!(state, GapState::Open | GapState::Reopened | GapState::Asked);
    let open_candidates = gaps.gaps.iter().filter(|gap| actionable(gap.state)).count();
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "scope": {
                    "base": options.base,
                    "path": options.path,
                    "symbol": options.symbol,
                    "requirement": options.requirement,
                },
                "index": freshness,
                "coverage": report,
                "behavioral": gaps,
                "open_candidates": open_candidates,
                "unanalyzed_paths": unanalyzed,
            }))?
        );
    } else {
        if let Some(commit) = &freshness.commit {
            println!("Revision {commit}");
        }
        let percent = report
            .structural_percent
            .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.1}%"));
        println!(
            "Structural coverage {percent} ({} mapped / {} analyzed symbols)",
            report.mapped_symbols, report.analyzed_symbols
        );
        println!(
            "Behavioral decisions {} ({} candidates for research)",
            report.decisions.len(),
            open_candidates
        );
        for gap in gaps
            .gaps
            .iter()
            .filter(|gap| actionable(gap.state))
            .take(10)
        {
            let point = gap.point;
            println!(
                "{}:{} {} ({})",
                point.file, point.range.start_line, point.condition, point.score
            );
        }
        if !unanalyzed.is_empty() {
            println!("Unanalyzed paths: {}", unanalyzed.join(", "));
        }
    }
    Ok(())
}

fn changed_paths(git: &GitRepo, base: Option<&str>) -> Result<Option<BTreeSet<String>>, CliError> {
    let Some(base) = base else {
        return Ok(None);
    };
    let paths = git.review_changes(base)?.source_changes;
    Ok(Some(
        paths
            .iter()
            .filter_map(FileChange::current_path)
            .map(str::to_owned)
            .collect(),
    ))
}

fn matches_path(path: &str, options: &Options, changed: Option<&BTreeSet<String>>) -> bool {
    options
        .path
        .as_ref()
        .is_none_or(|scope| Path::new(path).starts_with(scope))
        && changed.is_none_or(|paths| paths.contains(path))
}

fn scope(
    graph: &GraphSnapshot,
    options: &Options,
    changed: Option<&BTreeSet<String>>,
) -> BTreeSet<StableKey> {
    let mapped = options
        .requirement
        .as_ref()
        .map(|_| structural_mappings(graph));
    let roots = options.symbol.as_ref().map(|query| {
        graph
            .resolve(query)
            .into_iter()
            .map(|node| (&node.stable_key, node.identifier()))
            .collect::<Vec<_>>()
    });
    graph
        .nodes
        .values()
        .filter(|node| {
            let PlannedNodeAttributes::Symbol {
                file_path,
                canonical_path,
                ..
            } = &node.attributes
            else {
                return false;
            };
            matches_path(file_path, options, changed)
                && roots.as_ref().is_none_or(|roots| {
                    roots.iter().any(|(key, name)| {
                        *key == &node.stable_key
                            || canonical_path.strip_prefix(name).is_some_and(|suffix| {
                                suffix.starts_with('.') || suffix.starts_with("::")
                            })
                    })
                })
                && options
                    .requirement
                    .as_ref()
                    .zip(mapped.as_ref())
                    .is_none_or(|(requirement, mappings)| {
                        mappings
                            .get(&node.stable_key)
                            .is_some_and(|intents| intents.contains(requirement))
                    })
        })
        .map(|node| node.stable_key.clone())
        .collect()
}
