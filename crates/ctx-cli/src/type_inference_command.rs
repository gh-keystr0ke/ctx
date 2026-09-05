use std::{
    path::Path,
    time::{Duration, Instant},
};

use chrono::Utc;
use ctx_adapters::{
    git::GitRepo, pyright::PyrightTypeServer, python::PythonAnalyzer, sqlite::SqliteStore,
};
use ctx_app::type_inference::{InferTypesReport, InferTypesRunner};
use ctx_core::domain::Confidence;
use serde::Serialize;
use serde_json::json;

use crate::{Cli, CliError, database_path};

#[derive(Serialize)]
struct CliInferTypesReport<'a> {
    ok: bool,
    status: &'static str,
    pyright_startup_ms: u128,
    #[serde(flatten)]
    inference: &'a InferTypesReport,
}

pub(super) fn infer_types(
    cli: &Cli,
    git: &GitRepo,
    pyright: &Path,
    confidence: f32,
    timeout_ms: u64,
) -> Result<(), CliError> {
    let startup = Instant::now();
    let mut oracle =
        match PyrightTypeServer::start(pyright, git.root(), Duration::from_millis(timeout_ms)) {
            Ok(oracle) => oracle,
            Err(error) if error.is_not_found() => {
                print_missing_pyright(cli, pyright);
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
    let pyright_startup_ms = startup.elapsed().as_millis();
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let candidates = PythonAnalyzer::new(git.root().to_path_buf());
    let confidence = Confidence::new(confidence).expect("confidence is validated by clap");
    let result = InferTypesRunner::new(git, &candidates, &mut oracle, &mut store, confidence)
        .run(&Utc::now().to_rfc3339());
    let shutdown = oracle.shutdown();
    let report = result?;
    shutdown?;

    print_report(cli, &report, pyright_startup_ms)
}

fn print_missing_pyright(cli: &Cli, pyright: &Path) {
    if cli.json {
        println!(
            "{}",
            json!({
                "ok": true,
                "status": "skipped",
                "reason": "pyright_typeserver_not_found",
                "executable": pyright,
                "message": "Pyright Type Server was not found; rerun install.sh with Node.js 18.12+ and npm available or pass --pyright <path>",
            })
        );
    } else {
        println!(
            "Type inference skipped: '{}' was not found. Rerun install.sh with Node.js 18.12+ and npm available or pass --pyright <path>.",
            pyright.display()
        );
    }
}

fn print_report(
    cli: &Cli,
    report: &InferTypesReport,
    pyright_startup_ms: u128,
) -> Result<(), CliError> {
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&CliInferTypesReport {
                ok: true,
                status: "completed",
                pyright_startup_ms,
                inference: report,
            })?
        );
        return Ok(());
    }

    println!("Pyright type inference completed");
    println!(
        "Candidates: {}; type queries: {}; import queries: {}; resolved: {}",
        report.candidate_sites,
        report.type_queries,
        report.import_queries,
        report.resolved_model_candidates
    );
    println!(
        "Inferences created: {}; updated: {}; removed: {}",
        report.inferences_created, report.inferences_updated, report.inferences_removed
    );
    println!(
        "Unknown: {}; ambiguous: {}; unsupported: {}; suppressed by Fact: {}",
        report.dropped_unknown,
        report.dropped_ambiguous,
        report.dropped_unsupported,
        report.suppressed_by_fact
    );
    println!(
        "Failed type queries: {}; candidate extraction failures: {}",
        report.pyright_failures, report.extraction_failures
    );
    println!(
        "Timing: Pyright startup {} ms; workspace analysis/type queries {} ms; inference phase {} ms",
        pyright_startup_ms, report.pyright_query_ms, report.duration_ms
    );
    if cli.verbose > 1 {
        for diagnostic in &report.diagnostics {
            println!(
                "  {}:{} {:?} probe={} type={} model={} table={} drop={:?}: {}",
                diagnostic.file,
                diagnostic.line,
                diagnostic.form,
                diagnostic.probe.as_deref().unwrap_or("-"),
                diagnostic.inferred_type.as_deref().unwrap_or("-"),
                diagnostic.model_symbol.as_deref().unwrap_or("-"),
                diagnostic.table.as_deref().unwrap_or("-"),
                diagnostic.reason,
                diagnostic.detail
            );
        }
    }
    Ok(())
}

pub(super) fn parse_inference_confidence(value: &str) -> Result<f32, String> {
    let value = value
        .parse::<f32>()
        .map_err(|_| "confidence must be a number between 0 and 1".to_owned())?;
    if value.is_finite() && (0.0..1.0).contains(&value) {
        Ok(value)
    } else {
        Err("inference confidence must be finite, at least 0, and less than 1".to_owned())
    }
}
