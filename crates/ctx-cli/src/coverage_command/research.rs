use std::{io, time::Duration};

use clap::Args;
use ctx_adapters::{coverage_store::CoverageStore, git::GitRepo};
use ctx_app::ports::{ResearchAgent, ReviewRepository};
use ctx_core::{
    coverage::{
        CoverageRecord, DocumentEvidence, GapReport, GapState, RankedDecision,
        research::{Evidence, EvidenceKind, JudgmentClass, ResearchInput},
        structural_mappings,
    },
    domain::{ClaimStatus, RelationKind},
    graph::{GraphNode, GraphSnapshot},
    indexing::PlannedNodeAttributes,
};
use serde_json::json;

use crate::{Cli, CliError, agent_dispatch::ConfiguredAgent};

#[derive(Debug, Args)]
pub(super) struct ResearchOptions {
    #[arg(long, default_value = "claude")]
    agent: String,
    #[arg(long)]
    model: Option<String>,
    #[arg(long, default_value_t = 5)]
    max_questions: usize,
    #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u64).range(1..))]
    timeout_ms: u64,
}

impl ResearchOptions {
    pub(super) fn for_ask(agent: &str) -> Self {
        Self {
            agent: agent.to_owned(),
            model: None,
            max_questions: 5,
            timeout_ms: 30_000,
        }
    }
}

pub(super) fn run(
    cli: &Cli,
    git: &GitRepo,
    graph: &GraphSnapshot,
    gaps: &GapReport<'_>,
    revision: &str,
    options: &ResearchOptions,
) -> Result<(), CliError> {
    let outcomes = research_gaps(cli, git, graph, gaps, revision, options)?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"results": outcomes}))?
        );
    } else {
        println!(
            "Coverage research attempted {} decision(s).",
            outcomes.len()
        );
    }
    Ok(())
}

pub(super) fn research_gaps(
    cli: &Cli,
    git: &GitRepo,
    graph: &GraphSnapshot,
    gaps: &GapReport<'_>,
    revision: &str,
    options: &ResearchOptions,
) -> Result<Vec<serde_json::Value>, CliError> {
    let agent = ConfiguredAgent::from_name(
        &options.agent,
        options.model.clone(),
        cli.verbose > 0,
        cli.pace_agent(),
    )
    .map_err(CliError::UnsupportedAgent)?
    .with_ledger(git.root())
    .with_timeout(Duration::from_millis(options.timeout_ms));
    let store = CoverageStore::new(git.context_root());
    let mut outcomes = Vec::new();
    for gap in gaps
        .gaps
        .iter()
        .filter(|gap| {
            matches!(
                gap.state,
                GapState::Open | GapState::Reopened | GapState::Asked
            ) && (gap.state == GapState::Reopened
                || gap.record.is_none_or(|record| {
                    record.research.is_none()
                        || record
                            .note
                            .as_deref()
                            .is_some_and(|note| note.starts_with("additional source: "))
                }))
        })
        .take(options.max_questions)
    {
        let mut input = evidence_for(git, graph, gap.point, revision)?;
        if let Some(reference) = gap
            .record
            .and_then(|record| record.note.as_deref())
            .and_then(|note| note.strip_prefix("additional source: "))
        {
            let excerpt = graph
                .nodes
                .values()
                .find_map(|node| match &node.attributes {
                    PlannedNodeAttributes::Business { id, body, .. } if id == reference => {
                        Some(body.clone())
                    }
                    _ => None,
                })
                .or_else(|| git.source_at(revision, reference).ok().flatten());
            add_evidence(
                &mut input.evidence,
                EvidenceKind::Developer,
                reference.to_owned(),
                excerpt.as_deref().unwrap_or(reference),
                None,
            );
        }
        let judgment = match agent.research(&input) {
            Ok(judgment) => judgment,
            Err(error) => {
                eprintln!(
                    "Coverage research unavailable for {}: {error}",
                    gap.point.anchor
                );
                outcomes.push(json!({"anchor": gap.point.anchor, "outcome": "unavailable"}));
                continue;
            }
        };
        let state = if judgment.judgment.suspected_divergence {
            GapState::SuspectedDivergence
        } else if judgment.judgment.class == JudgmentClass::Established
            && judgment.document.is_some()
        {
            GapState::AutoResolved
        } else {
            GapState::Open
        };
        let record = CoverageRecord {
            anchor: gap.point.anchor.clone(),
            state,
            revision: revision.to_owned(),
            document: judgment.document.clone(),
            citations: judgment
                .evidence
                .iter()
                .map(|item| item.source.clone())
                .collect(),
            note: None,
            research: Some(judgment),
        };
        store.record(&gap.point.file, record)?;
        outcomes.push(json!({"anchor": gap.point.anchor, "outcome": state}));
    }
    Ok(outcomes)
}

pub(super) fn evidence_for(
    git: &GitRepo,
    graph: &GraphSnapshot,
    point: &RankedDecision,
    revision: &str,
) -> Result<ResearchInput, CliError> {
    let mut evidence = Vec::new();
    if let Some(code) = git.source_at(revision, &point.file)? {
        add_evidence(
            &mut evidence,
            EvidenceKind::Code,
            format!("{}:{}", point.file, point.range.start_line),
            &snippet(
                &code,
                point.range.start_line.saturating_sub(5),
                point.range.end_line.saturating_add(5),
            ),
            None,
        );
    }
    let mapped = structural_mappings(graph);
    for node in graph
        .nodes
        .values()
        .filter(|node| {
            matches!(&node.attributes, PlannedNodeAttributes::Business { .. })
                && mapped
                    .get(&point.anchor.symbol)
                    .is_some_and(|intents| intents.contains(node.identifier()))
        })
        .take(6)
    {
        if let PlannedNodeAttributes::Business { body, id, .. } = &node.attributes {
            let assertion = Some(DocumentEvidence {
                id: id.clone(),
                fingerprint: node.content_hash.clone(),
            });
            add_evidence(
                &mut evidence,
                EvidenceKind::Context,
                id.clone(),
                body,
                assertion,
            );
        }
    }
    for node in neighbors(graph, point).into_iter().take(6) {
        if let PlannedNodeAttributes::Symbol {
            file_path, range, ..
        } = &node.attributes
            && let Some(code) = git.source_at(revision, file_path)?
        {
            let kind = if node.is_test() {
                EvidenceKind::Test
            } else {
                EvidenceKind::Code
            };
            add_evidence(
                &mut evidence,
                kind,
                format!("{file_path}:{}", range.start_line),
                &snippet(&code, range.start_line, range.end_line),
                None,
            );
        }
    }
    if let Ok(history) = git.decision_history(
        revision,
        &point.file,
        point.range.start_line,
        point.range.end_line,
    ) {
        for (commit, subject) in history.into_iter().take(3) {
            add_evidence(&mut evidence, EvidenceKind::History, commit, &subject, None);
        }
    }
    if evidence.is_empty() {
        return Err(io::Error::other("no evidence available at the indexed revision").into());
    }
    Ok(ResearchInput {
        behavior: format!(
            "{} at {}:{}: {}",
            point.symbol, point.file, point.range.start_line, point.condition
        ),
        evidence,
    })
}

fn neighbors<'a>(graph: &'a GraphSnapshot, point: &RankedDecision) -> Vec<&'a GraphNode> {
    let mut keys = std::collections::BTreeSet::new();
    for edge in &graph.edges {
        if edge.status != ClaimStatus::Active
            || !matches!(edge.kind, RelationKind::Calls | RelationKind::CoveredBy)
        {
            continue;
        }
        if edge.source == point.anchor.symbol {
            keys.insert(&edge.target);
        } else if edge.target == point.anchor.symbol {
            keys.insert(&edge.source);
        }
    }
    let mut nodes: Vec<_> = keys
        .into_iter()
        .filter_map(|key| graph.nodes.get(key))
        .collect();
    nodes.sort_by_key(|node| !node.is_test());
    nodes
}

pub(super) fn snippet(source: &str, start: usize, end: usize) -> String {
    source
        .lines()
        .skip(start.max(1) - 1)
        .take(end.saturating_sub(start).saturating_add(1).min(35))
        .collect::<Vec<_>>()
        .join("\n")
}

fn add_evidence(
    evidence: &mut Vec<Evidence>,
    kind: EvidenceKind,
    source: String,
    excerpt: &str,
    assertion: Option<DocumentEvidence>,
) {
    if excerpt.trim().is_empty() {
        return;
    }
    let excerpt: String = excerpt.chars().take(750).collect();
    evidence.push(Evidence {
        id: format!("E{}", evidence.len() + 1),
        kind,
        source,
        excerpt,
        assertion,
    });
}
