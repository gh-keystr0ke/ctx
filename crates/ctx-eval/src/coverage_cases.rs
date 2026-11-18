//! Deterministic release checks for behavioral coverage and research trust.

use std::collections::BTreeMap;

use ctx_adapters::{
    agent_contract::{self, AgentContractError, AgentTransport},
    python::PythonAnalyzer,
};
use ctx_core::{
    coverage::{
        Coverage, CoverageRecord, DocumentEvidence, GapState, analyze_coverage, gap_report,
        research::{Evidence, EvidenceKind, JudgmentClass, ResearchInput, ResearchJudgment},
    },
    graph::GraphSnapshot,
    indexing::FileChange,
    review::{ReviewInput, build_review_findings},
};

use crate::{
    harness,
    report::{CaseReport, CheckKind, CheckOutcome},
};

const SOURCE: &str =
    "def retry(attempt):\n    if attempt == 0:\n        return None\n    return 1\n";
const UPDATED: &str = "def retry(attempt):\n    if attempt == 0:\n        return None\n    if attempt >= 3:\n        return 8\n    return 1\n";

struct Fake<'a>(&'a str);

impl AgentTransport for Fake<'_> {
    fn run(&self, _: &str) -> Result<String, AgentContractError> {
        if self.0.is_empty() {
            return Err(AgentContractError::Spawn("agent unavailable".to_owned()));
        }
        Ok(self.0.to_owned())
    }
}

pub(crate) fn run() -> Vec<CaseReport> {
    let graph = match harness::coverage_graph(SOURCE) {
        Ok(graph) => graph,
        Err(error) => {
            return vec![CaseReport::Errored {
                id: "coverage-fixture",
                description: "Index the behavioral fixture",
                error: error.to_string(),
            }];
        }
    };
    let scope = graph.nodes.keys().cloned().collect();
    let coverage = analyze_coverage(&graph, &scope);
    let evidence = sample_evidence(&graph);
    let reports = [
        (
            "coverage-uc-001",
            "Resolve a gap from a current assertion",
            establish(&coverage, &evidence),
        ),
        (
            "coverage-uc-002",
            "Keep competing interpretations explicit",
            ambiguous(&evidence),
        ),
        (
            "coverage-uc-003",
            "Treat test-only explanations as inferences",
            downgrade(&evidence),
        ),
        (
            "coverage-uc-004",
            "Suppress a routine empty-value guard",
            routine_guard(),
        ),
        (
            "coverage-uc-005",
            "Require both sides of suspected divergence",
            divergence(&evidence),
        ),
        (
            "coverage-uc-006",
            "Reopen a gap when its evidence changes",
            state_check(&graph, &coverage, GapState::AutoResolved),
        ),
        (
            "coverage-uc-007",
            "Order the research queue reproducibly",
            reproducible(&graph, &coverage),
        ),
        (
            "coverage-uc-008",
            "Keep an unknown answer unresolved",
            state_check(&graph, &coverage, GapState::Unknown),
        ),
        (
            "coverage-uc-009",
            "Keep a dismissal for the same anchor",
            state_check(&graph, &coverage, GapState::Dismissed),
        ),
        (
            "coverage-uc-010",
            "Report a new unexplained decision in verbose review",
            changed(&graph),
        ),
        (
            "coverage-uc-011",
            "Keep deterministic gaps when a CLI agent fails",
            offline(&coverage, &evidence),
        ),
        (
            "coverage-uc-012",
            "Preserve conflicting evidence without resolving the gap",
            conflicting(&evidence),
        ),
    ];
    reports
        .into_iter()
        .map(|(id, description, passed)| {
            CaseReport::completed(
                id,
                description,
                vec![CheckOutcome {
                    description: description.to_owned(),
                    kind: CheckKind::Classification,
                    passed,
                    detail: if passed {
                        "expected state observed"
                    } else {
                        "expected state missing"
                    }
                    .to_owned(),
                }],
            )
        })
        .collect()
}

fn sample_evidence(graph: &GraphSnapshot) -> ResearchInput {
    let assertion = graph
        .nodes
        .values()
        .find(|node| node.identifier() == "INV-RETRY")
        .map(|node| DocumentEvidence {
            id: "INV-RETRY".to_owned(),
            fingerprint: node.content_hash.clone(),
        });
    ResearchInput {
        behavior: "First retry".to_owned(),
        evidence: vec![
            Evidence {
                id: "E1".to_owned(),
                kind: EvidenceKind::Code,
                source: "src/app.py".to_owned(),
                excerpt: "return None".to_owned(),
                assertion: None,
            },
            Evidence {
                id: "E2".to_owned(),
                kind: EvidenceKind::Context,
                source: "INV-RETRY".to_owned(),
                excerpt: "First retry skips delay.".to_owned(),
                assertion,
            },
            Evidence {
                id: "E3".to_owned(),
                kind: EvidenceKind::Test,
                source: "tests/test_app.py".to_owned(),
                excerpt: "assert retry(0) is None".to_owned(),
                assertion: None,
            },
        ],
    }
}

fn run_agent(
    input: &ResearchInput,
    class: &str,
    citations: &serde_json::Value,
    suspected: bool,
) -> Option<ResearchJudgment> {
    let response = serde_json::json!({
        "class": class, "explanation": "Observed retry behavior", "citations": citations,
        "suspected_divergence": suspected, "hypotheses": ["zero delay", "no retry"],
    })
    .to_string();
    agent_contract::research(&Fake(&response), input, None).ok()
}

fn establish(coverage: &Coverage, input: &ResearchInput) -> bool {
    coverage.mapped_symbols == 1
        && coverage.decisions.iter().any(|point| point.ask)
        && run_agent(
            input,
            "established",
            &serde_json::json!([{"id": "E2", "quote": "First retry skips delay."}]),
            false,
        )
        .is_some_and(|answer| {
            answer.document.is_some() && answer.judgment.class == JudgmentClass::Established
        })
        && run_agent(
            input,
            "established",
            &serde_json::json!([{"id": "invented", "quote": "First retry skips delay."}]),
            false,
        )
        .is_none()
}

fn ambiguous(input: &ResearchInput) -> bool {
    run_agent(
        input,
        "ambiguous",
        &serde_json::json!([{"id": "E1", "quote": "return None"}]),
        false,
    )
    .is_some_and(|answer| {
        answer.document.is_none()
            && answer.judgment.hypotheses.len() == 2
            && answer.judgment.class == JudgmentClass::Ambiguous
    })
}

fn downgrade(input: &ResearchInput) -> bool {
    run_agent(
        input,
        "established",
        &serde_json::json!([{"id": "E3", "quote": "assert retry(0) is None"}]),
        false,
    )
    .is_some_and(|answer| {
        answer.document.is_none() && answer.judgment.class == JudgmentClass::Inferred
    })
}

fn routine_guard() -> bool {
    let source = "def retry(retry_count):\n    if not retry_count:\n        return\n    return 1\n";
    harness::coverage_graph(source).ok().is_some_and(|graph| {
        let report = analyze_coverage(&graph, &graph.nodes.keys().cloned().collect());
        report
            .decisions
            .iter()
            .any(|point| point.condition == "not retry_count")
            && !report.decisions.iter().any(|point| point.ask)
    })
}

fn divergence(input: &ResearchInput) -> bool {
    run_agent(input, "conflicting", &serde_json::json!([{"id": "E1", "quote": "return None"}, {"id": "E2", "quote": "First retry skips delay."}]), true)
        .is_some_and(|answer| answer.judgment.suspected_divergence && answer.document.is_none())
        && run_agent(input, "conflicting", &serde_json::json!([{"id": "E1", "quote": "return None"}]), true)
            .is_some_and(|answer| !answer.judgment.suspected_divergence)
}

fn state_check(graph: &GraphSnapshot, coverage: &Coverage, state: GapState) -> bool {
    let Some(point) = coverage.decisions.iter().find(|point| point.ask) else {
        return false;
    };
    let anchor = &point.anchor;
    let record = CoverageRecord {
        anchor: anchor.clone(),
        state,
        revision: "test".to_owned(),
        document: (state == GapState::AutoResolved).then(|| DocumentEvidence {
            id: "INV-RETRY".to_owned(),
            fingerprint: "old".to_owned(),
        }),
        citations: Vec::new(),
        note: None,
        research: None,
        context_documents: None,
    };
    let scope = graph.nodes.keys().cloned().collect();
    let records = BTreeMap::from([(anchor.clone(), record)]);
    let result = gap_report(graph, &coverage.decisions, &records, &scope);
    let expected = if state == GapState::AutoResolved {
        GapState::Reopened
    } else {
        state
    };
    result
        .gaps
        .iter()
        .any(|gap| gap.point.anchor == *anchor && gap.state == expected)
}

fn reproducible(graph: &GraphSnapshot, coverage: &Coverage) -> bool {
    let scope = graph.nodes.keys().cloned().collect();
    let another = analyze_coverage(graph, &scope);
    !coverage.decisions.is_empty()
        && serde_json::to_vec(coverage).ok() == serde_json::to_vec(&another).ok()
}

fn changed(graph: &GraphSnapshot) -> bool {
    let (Ok(before), Ok(after)) = (
        PythonAnalyzer::analyze_source("src/app.py", SOURCE),
        PythonAnalyzer::analyze_source("src/app.py", UPDATED),
    ) else {
        return false;
    };
    let report = build_review_findings(
        graph,
        &ReviewInput {
            before: BTreeMap::from([(before.path.clone(), before)]),
            after: BTreeMap::from([(after.path.clone(), after)]),
            changes: vec![FileChange::Modified {
                path: "src/app.py".to_owned(),
            }],
            verbose: true,
            ..ReviewInput::default()
        },
    );
    report
        .new_behavioral_decisions
        .iter()
        .any(|decision| decision.condition == "attempt >= 3")
}

fn offline(coverage: &Coverage, input: &ResearchInput) -> bool {
    agent_contract::research(&Fake(""), input, None).is_err()
        && coverage.decisions.iter().any(|point| point.ask)
}

fn conflicting(input: &ResearchInput) -> bool {
    run_agent(input, "conflicting", &serde_json::json!([{"id": "E1", "quote": "return None"}, {"id": "E2", "quote": "First retry skips delay."}]), false)
        .is_some_and(|answer| answer.judgment.class == JudgmentClass::Conflicting && answer.document.is_none() && answer.evidence.len() == 2)
}
