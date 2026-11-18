use std::{collections::BTreeSet, path::Path};

use ctx_adapters::{coverage_store::CoverageStore, sqlite::SqliteStore};
use ctx_app::ports::GraphStore;
use ctx_core::{
    coverage::{GapState, analyze_coverage, gap_report},
    domain::RepositoryId,
    indexing::PlannedNodeAttributes,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) const TOOLS: [&str; 3] = ["get_coverage", "list_gaps", "explain_gap"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    path: Option<String>,
    symbol: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GapArguments {
    anchor: String,
}

pub(super) fn execute(
    name: &str,
    args: Value,
    store: &SqliteStore,
    repository: &RepositoryId,
) -> Result<Value, String> {
    let (scope, anchor) = if name == "explain_gap" {
        let args = super::decode::<GapArguments>(args)?;
        (
            Scope {
                path: None,
                symbol: None,
            },
            Some(args.anchor),
        )
    } else {
        (super::decode::<Scope>(args)?, None)
    };
    let graph = store
        .load_graph(repository)
        .map_err(|error| error.to_string())?;
    let selected: BTreeSet<_> = graph
        .nodes
        .values()
        .filter(|node| {
            let PlannedNodeAttributes::Symbol { file_path, .. } = &node.attributes else {
                return false;
            };
            scope
                .path
                .as_ref()
                .is_none_or(|path| Path::new(file_path).starts_with(path))
                && scope.symbol.as_ref().is_none_or(|symbol| {
                    node.identifier() == symbol || node.stable_key.as_str() == symbol
                })
        })
        .map(|node| node.stable_key.clone())
        .collect();
    let coverage = analyze_coverage(&graph, &selected);
    let records = CoverageStore::new(store.root())
        .read()
        .map_err(|error| error.to_string())?;
    let report = gap_report(&graph, &coverage.decisions, &records, &selected);
    match name {
        "get_coverage" => Ok(json!({
            "mapped_symbols": coverage.mapped_symbols,
            "analyzed_symbols": coverage.analyzed_symbols,
            "structural_percent": coverage.structural_percent,
            "decisions": coverage.decisions.len(),
            "counts": report.counts,
            "stale_anchors": report.stale_anchors,
            "suspected_divergences": report.gaps.iter().filter(|gap| gap.state == GapState::SuspectedDivergence).take(25).collect::<Vec<_>>(),
        })),
        "list_gaps" => Ok(json!({"gaps": report.gaps.iter().filter(|gap| matches!(
            gap.state, GapState::Open | GapState::Reopened | GapState::Asked | GapState::Ambiguous
        )).take(25).collect::<Vec<_>>() })),
        "explain_gap" => report
            .gaps
            .iter()
            .find(|gap| {
                gap.state != GapState::SuspectedDivergence
                    && anchor.as_deref() == Some(&gap.point.anchor.to_string())
            })
            .ok_or_else(|| "coverage gap not found".to_owned())
            .and_then(|gap| serde_json::to_value(gap).map_err(|error| error.to_string())),
        _ => Err(format!("unknown coverage tool {name}")),
    }
}

pub(super) fn definitions() -> [Value; 3] {
    let scope = json!({
        "type": "object", "additionalProperties": false,
        "properties": {"path": {"type": "string"}, "symbol": {"type": "string"}}
    });
    [
        tool(
            "get_coverage",
            "Read structural coverage, gap counts, and potential divergence evidence",
            scope.clone(),
        ),
        tool(
            "list_gaps",
            "List up to 25 ranked actionable knowledge gaps",
            scope,
        ),
        tool(
            "explain_gap",
            "Read one behavioral point and its stored coverage evidence",
            super::object_schema("anchor", "Stable decision anchor"),
        ),
    ]
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
        "annotations": {"readOnlyHint": true, "openWorldHint": false}
    })
}
