mod ask;
mod research;

use std::{collections::BTreeSet, path::Path};

use clap::{Args, Subcommand};
use ctx_adapters::{coverage_store::CoverageStore, git::GitRepo};
use ctx_app::{
    ports::{GitRepository, GraphStore, ReviewRepository},
    status::IndexFreshness,
};
use ctx_core::{
    coverage::{Coverage, GapReport, GapState, analyze_coverage, gap_report, structural_mappings},
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
    /// Ask the developer about unresolved decision points.
    Ask(ask::AskOptions),
}

pub(crate) fn run(cli: &Cli, git: &GitRepo, options: &Options) -> Result<(), CliError> {
    let repository = git.descriptor()?;
    let (store, freshness) = worktree_command::query_store(cli, git, &repository.id, false)?;
    let graph = store.load_graph(&repository.id)?;
    let changed = changed_paths(git, options.base.as_deref())?;
    let scoped = scope(&graph, options, changed.as_ref());
    let report = analyze_coverage(&graph, &scoped);
    let records = CoverageStore::new(git.context_root()).read()?;
    let gaps = gap_report(&graph, &report.decisions, &records, &scoped);
    if let Some(action) = &options.action {
        let commit = freshness
            .commit
            .as_ref()
            .filter(|commit| *commit == &freshness.head)
            .ok_or_else(|| std::io::Error::other("index HEAD before changing coverage state"))?;
        match action {
            Action::Research(options) => {
                return research::run(cli, git, &graph, &gaps, commit.as_str(), options);
            }
            Action::Ask(options) => {
                if let Some(agent) = &options.agent {
                    research::research_gaps(
                        cli,
                        git,
                        &graph,
                        &gaps,
                        commit.as_str(),
                        &research::ResearchOptions::for_ask(agent),
                    )?;
                }
                let records = CoverageStore::new(git.context_root()).read()?;
                let gaps = gap_report(&graph, &report.decisions, &records, &scoped);
                return ask::ask(cli, git, &graph, &gaps, commit.as_str(), options);
            }
        }
    }
    let unanalyzed = unanalyzed_paths(git, &graph, options, changed.as_ref())?;
    print_report(cli, options, &freshness, &report, &gaps, &unanalyzed)
}

fn print_report(
    cli: &Cli,
    options: &Options,
    freshness: &IndexFreshness,
    report: &Coverage,
    gaps: &GapReport<'_>,
    unanalyzed: &[String],
) -> Result<(), CliError> {
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

fn unanalyzed_paths(
    git: &GitRepo,
    graph: &GraphSnapshot,
    options: &Options,
    changed: Option<&BTreeSet<String>>,
) -> Result<Vec<String>, CliError> {
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
    Ok(git
        .all_source_files()?
        .into_iter()
        .filter(|path| matches_path(path, options, changed))
        .filter(|path| !analyzed_files.contains(path.as_str()))
        .collect())
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

pub(crate) fn explain(
    git: &GitRepo,
    store: &ctx_adapters::sqlite::SqliteStore,
    repository: &ctx_core::domain::RepositoryId,
    explanations: &[ctx_core::explain::Explanation],
) -> Result<Vec<serde_json::Value>, CliError> {
    let graph = store.load_graph(repository)?;
    let mut selected = BTreeSet::new();
    let mut documents = BTreeSet::new();
    for subject in explanations
        .iter()
        .flat_map(|explanation| &explanation.subjects)
    {
        let Ok(key) = StableKey::new(subject.stable_key.clone()) else {
            continue;
        };
        match graph.nodes.get(&key).map(|node| &node.attributes) {
            Some(PlannedNodeAttributes::Symbol { .. }) => {
                selected.insert(key);
            }
            Some(PlannedNodeAttributes::Business { anchors, id, .. }) => {
                documents.insert(id);
                selected.extend(anchors.iter().map(|anchor| anchor.symbol.clone()));
            }
            _ => {}
        }
    }
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let report = analyze_coverage(&graph, &selected);
    let records = CoverageStore::new(git.context_root()).read()?;
    let gaps = gap_report(&graph, &report.decisions, &records, &selected);
    gaps.gaps
        .into_iter()
        .filter(|gap| gap.state != GapState::SuspectedDivergence)
        .filter(|gap| {
            documents.is_empty()
                || gap
                    .document
                    .as_ref()
                    .is_some_and(|doc| documents.contains(&doc.id))
        })
        .map(|gap| serde_json::to_value(gap).map_err(CliError::from))
        .collect()
}

pub(crate) fn print_explanation(gaps: &[serde_json::Value]) {
    if gaps.is_empty() {
        return;
    }
    println!("Behavioral decisions:");
    for gap in gaps {
        let file = gap["point"]["file"].as_str().unwrap_or_default();
        let line = gap["point"]["range"]["start_line"]
            .as_u64()
            .unwrap_or_default();
        let state = gap["state"].as_str().unwrap_or_default();
        let condition = gap["point"]["condition"].as_str().unwrap_or_default();
        println!("  {file}:{line} [{state}] {condition}");
        if let Some(anchor) = gap["point"].get("anchor") {
            println!("    anchor {anchor}");
        }
    }
}
