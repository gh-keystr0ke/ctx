//! Drives the full evaluation corpus: builds each case's repository, records
//! its use-case results, and scores them against ground truth.

use crate::{
    cases::{self, EvaluationCase},
    harness,
    report::{self, CaseReport},
};

/// Runs one case end to end and scores it. A harness failure (Git, storage,
/// or a use-case error) is reported as [`CaseReport::Errored`] rather than
/// propagated, so one broken case does not hide the rest of the corpus.
#[must_use]
pub fn evaluate_case(case: &EvaluationCase) -> CaseReport {
    match harness::run_case(case) {
        Ok(run) => {
            let checks = case
                .checks
                .iter()
                .map(|check| report::evaluate(&run, check))
                .collect();
            let mut result = CaseReport::completed(case.id, case.description, checks);
            if let CaseReport::Completed {
                context_symbol_tokens,
                ..
            } = &mut result
                && let Some(pack) = run.context
            {
                *context_symbol_tokens = Some(
                    pack.items
                        .iter()
                        .filter(|item| item.kind == ctx_core::domain::NodeKind::CodeSymbol)
                        .fold(
                            report::ContextSymbolTokens::default(),
                            |mut totals, item| {
                                totals.symbols += 1;
                                totals.estimated_tokens += item.content.chars().count().div_ceil(4);
                                totals
                            },
                        ),
                );
            }
            result
        }
        Err(error) => CaseReport::Errored {
            id: case.id,
            description: case.description,
            error: error.to_string(),
        },
    }
}

/// Runs the full corpus in deterministic order.
#[must_use]
pub fn run_corpus() -> Vec<CaseReport> {
    cases::corpus().iter().map(evaluate_case).collect()
}
