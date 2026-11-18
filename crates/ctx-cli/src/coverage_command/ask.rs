use std::io::{self, BufRead};

use clap::Args;
use ctx_adapters::{coverage_store::CoverageStore, git::GitRepo, sqlite::SqliteStore};
use ctx_app::ports::{GitRepository, KnowledgeCandidateStore};
use ctx_core::{
    artifact::{ArtifactIdentity, ArtifactKind, ArtifactProvider, ArtifactRef},
    business::BusinessKind,
    coverage::{CoverageRecord, Gap, GapReport, GapState},
    graph::GraphSnapshot,
    knowledge::{AgentProvenance, KnowledgeCandidate},
};
use serde_json::json;

use crate::{Cli, CliError, database_path};

#[derive(Debug, Args)]
pub(super) struct AskOptions {
    #[arg(long, default_value_t = 5)]
    max_questions: usize,
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=2))]
    max_follow_ups: u8,
    /// Research unresolved gaps first with this agent.
    #[arg(long)]
    pub agent: Option<String>,
}

pub(super) fn ask(
    cli: &Cli,
    git: &GitRepo,
    graph: &GraphSnapshot,
    report: &GapReport<'_>,
    revision: &str,
    options: &AskOptions,
) -> Result<(), CliError> {
    let map = CoverageStore::new(git.context_root());
    let repository = git.descriptor()?;
    let mut store = SqliteStore::open(&database_path(git.root())?, git.context_root())?;
    let mut answers = Vec::new();
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    for gap in report
        .gaps
        .iter()
        .filter(|gap| {
            matches!(gap.state, GapState::Open | GapState::Reopened)
                || (gap.state == GapState::Asked
                    && gap.record.is_none_or(|record| record.note.is_none()))
        })
        .take(options.max_questions)
    {
        let input = super::research::evidence_for(git, graph, gap.point, revision)?;
        show_question(gap, &input);
        let Some(answer) = read_answer(&mut stdin, gap, options.max_follow_ups)? else {
            break;
        };
        let (state, note) = match answer {
            Answer::Knowledge(statement) => {
                let candidate = knowledge_candidate(gap, statement, revision);
                let fingerprint = candidate.fingerprint.clone();
                store.upsert_candidates(&repository.id, &[candidate])?;
                eprintln!(
                    "Candidate {fingerprint} saved. Use ctx verify --knowledge to confirm it."
                );
                (GapState::Asked, Some(fingerprint))
            }
            Answer::Source(source) => (
                GapState::Asked,
                Some(format!("additional source: {source}")),
            ),
            Answer::Disposition(state, note) => (state, Some(note.to_owned())),
        };
        map.record(
            &gap.point.file,
            CoverageRecord {
                anchor: gap.point.anchor.clone(),
                state,
                revision: revision.to_owned(),
                document: None,
                citations: gap
                    .record
                    .map_or_else(Vec::new, |record| record.citations.clone()),
                note,
                research: gap.record.and_then(|record| record.research.clone()),
            },
        )?;
        answers.push(json!({"anchor": gap.point.anchor, "state": state}));
    }
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"answers": answers}))?
        );
    } else {
        println!("Recorded {} answer(s).", answers.len());
    }
    Ok(())
}

fn show_question(gap: &Gap<'_>, input: &ctx_core::coverage::research::ResearchInput) {
    eprintln!("{}\nWhat explains this behavior?", input.behavior);
    eprintln!("Ranking contributions: {:?}", gap.point.contributions);
    if let Some(evidence) = input.evidence.first() {
        eprintln!("{}", evidence.excerpt);
    }
    if let Some(research) = gap.record.and_then(|record| record.research.as_ref()) {
        if !research.judgment.explanation.is_empty() {
            eprintln!(
                "Agent finding ({:?}): {}",
                research.judgment.class, research.judgment.explanation
            );
        }
        for (index, hypothesis) in research.judgment.hypotheses.iter().enumerate() {
            eprintln!("  {}. {hypothesis}", index + 1);
        }
        for evidence in &research.evidence {
            eprintln!("  Evidence {}: {}", evidence.source, evidence.excerpt);
        }
    }
    eprintln!("answer <text> | choose <number> | correct <text> | source <reference>");
    eprintln!("unknown | defer | dismiss | reject | quit");
}

enum Answer {
    Knowledge(String),
    Source(String),
    Disposition(GapState, &'static str),
}

fn read_answer(
    input: &mut impl BufRead,
    gap: &Gap<'_>,
    follow_ups: u8,
) -> io::Result<Option<Answer>> {
    for attempt in 0..=follow_ups {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim();
        let (command, argument) = line
            .split_once(' ')
            .map_or((line, ""), |(command, argument)| (command, argument.trim()));
        let answer = match command {
            "quit" => return Ok(None),
            "answer" | "correct" if !argument.is_empty() => {
                Some(Answer::Knowledge(argument.to_owned()))
            }
            "choose" => hypothesis(gap, argument).map(Answer::Knowledge),
            "source" if !argument.is_empty() => Some(Answer::Source(argument.to_owned())),
            "unknown" => Some(Answer::Disposition(
                GapState::Unknown,
                "developer does not know",
            )),
            "defer" => Some(Answer::Disposition(
                GapState::Deferred,
                "developer deferred the question",
            )),
            "dismiss" => Some(Answer::Disposition(
                GapState::Dismissed,
                "developer marked the decision insignificant",
            )),
            "reject" => Some(Answer::Disposition(
                GapState::Dismissed,
                "developer rejected the wording",
            )),
            _ if !line.is_empty()
                && !matches!(command, "answer" | "correct" | "source" | "choose") =>
            {
                Some(Answer::Knowledge(line.to_owned()))
            }
            _ => None,
        };
        if answer.is_some() {
            return Ok(answer);
        }
        if attempt < follow_ups {
            eprintln!(
                "Enter the reason in one sentence, choose a listed hypothesis, or select a disposition."
            );
        }
    }
    Ok(None)
}

fn hypothesis(gap: &Gap<'_>, argument: &str) -> Option<String> {
    gap.record?
        .research
        .as_ref()?
        .judgment
        .hypotheses
        .get(argument.parse::<usize>().ok()?.checked_sub(1)?)
        .cloned()
}

fn knowledge_candidate(gap: &Gap<'_>, statement: String, revision: &str) -> KnowledgeCandidate {
    let kind = BusinessKind::Invariant;
    let source = format!("{}:{}", gap.point.file, gap.point.range.start_line);
    let fingerprint = format!(
        "{}@{}",
        KnowledgeCandidate::fingerprint_for(kind, &statement),
        gap.point.anchor
    );
    KnowledgeCandidate {
        fingerprint: fingerprint.clone(),
        kind,
        statement: statement.clone(),
        evidence: vec![ArtifactRef {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::Code,
                kind: ArtifactKind::Documentation,
                external_id: format!("coverage:{}@{revision}", gap.point.anchor),
            },
            locator: source,
            excerpt: statement,
        }],
        implementation_candidates: vec![gap.point.anchor.symbol.as_str().to_owned()],
        test_candidates: Vec::new(),
        anchors: vec![gap.point.anchor.clone()],
        provenance: AgentProvenance {
            producer: "developer:coverage-ask".to_owned(),
            model: None,
            input_artifact_ids: vec![gap.point.anchor.to_string()],
            produced_at: chrono::Utc::now().to_rfc3339(),
            fingerprint,
        },
    }
}
