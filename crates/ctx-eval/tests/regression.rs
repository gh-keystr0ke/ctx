//! Regression gate for the evaluation corpus: every case must still run
//! cleanly and every one of its ground-truth checks must still pass.

use ctx_eval::{report::CaseReport, runner};

#[test]
fn full_corpus_passes_every_ground_truth_check() {
    let reports = runner::run_corpus();
    let mut failures = Vec::new();
    for report in &reports {
        match report {
            CaseReport::Errored { id, error, .. } => {
                failures.push(format!("{id}: harness error: {error}"));
            }
            CaseReport::Completed { id, checks, .. } => {
                for outcome in checks {
                    if !outcome.passed {
                        failures.push(format!(
                            "{id}: {} ({})",
                            outcome.description, outcome.detail
                        ));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "evaluation regressions:\n{}",
        failures.join("\n")
    );
}
