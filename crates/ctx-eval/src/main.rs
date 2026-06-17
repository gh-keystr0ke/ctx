//! Runs the full `ctx` evaluation corpus and prints a machine-readable
//! summary. Exits non-zero when any case fails or errors, so this doubles as
//! a regression gate (`cargo run -p ctx-eval` or `cargo test -p ctx-eval`).

use std::process::ExitCode;

use ctx_eval::{report, runner};
use serde_json::json;

fn main() -> ExitCode {
    let reports = runner::run_corpus();
    let summary = report::summarize(&reports);
    let healthy = summary.errored_cases == 0 && summary.passed_cases == summary.total_cases;
    let output = json!({
        "summary": summary,
        "cases": reports,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).expect("evaluation report serializes")
    );
    if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
