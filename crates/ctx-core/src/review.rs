use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    domain::{ClaimStatus, NodeKind, RelationKind, StableKey},
    graph::{GraphEdge, GraphNode, GraphSnapshot, NodeSummary},
    indexing::{FileChange, PlannedNodeAttributes},
    ir::{ApiEndpoint, DatabaseAccessKind, FileAnalysis, HttpMethod, SymbolDefinition, SymbolKind},
    schema::{SchemaChange, declared_schema_changes, diff_schema_tables},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    FormattingOnly,
    Rename,
    RefactorLikely,
    BehaviorPotentiallyChanged,
    ContractChanged,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangedEntity {
    pub stable_key: Option<StableKey>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub file_path: String,
    pub change_kind: ChangeKind,
    pub signals: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub severity: Severity,
    pub confidence: f32,
    pub changed_entity: String,
    pub change_kind: ChangeKind,
    pub affected_intent: NodeSummary,
    pub reason: String,
    pub evidence: Vec<String>,
    pub related_tests: Vec<NodeSummary>,
    pub tests_modified: bool,
    pub possible_requirement_drift: bool,
    pub uncertainty: Option<String>,
    pub suggested_action: String,
}

/// A deterministic schema change observed directly in a diff's migration or
/// ORM model files, kept structurally separate from [`ReviewFinding`]: this
/// is an *observed schema change*, not a *proven requirement violation*.
/// `related_intents`/`related_tests` are a bounded, best-effort advisory
/// neighborhood (the table's directly connected code, and that code's own
/// mapped intent/tests) — empty means "no known product mapping was found",
/// not "this schema change is unrelated to the product".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaFinding {
    pub source_symbol: String,
    pub file_path: String,
    pub destructive: bool,
    pub changes: Vec<SchemaChange>,
    pub related_intents: Vec<NodeSummary>,
    pub related_tests: Vec<NodeSummary>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiChangeKind {
    Added,
    Removed,
    ContractModified,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApiChange {
    pub kind: ApiChangeKind,
    pub method: HttpMethod,
    pub path: String,
    pub destructive: bool,
    pub details: Vec<String>,
}

impl ApiChange {
    #[must_use]
    pub fn description(&self) -> String {
        let action = match self.kind {
            ApiChangeKind::Added => "added",
            ApiChangeKind::Removed => "removed",
            ApiChangeKind::ContractModified => "changed",
        };
        if self.details.is_empty() {
            format!("{action} {} {}", self.method.as_str(), self.path)
        } else {
            format!(
                "{action} {} {} ({})",
                self.method.as_str(),
                self.path,
                self.details.join("; ")
            )
        }
    }
}

/// A deterministic API contract change, separate from a product-impact
/// finding. Its bounded intent/test neighborhood is advisory: an empty
/// neighborhood means no mapping is known, never that the change is safe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApiFinding {
    pub source_symbol: String,
    pub file_path: String,
    pub destructive: bool,
    pub changes: Vec<ApiChange>,
    pub related_intents: Vec<NodeSummary>,
    pub related_tests: Vec<NodeSummary>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReviewReport {
    pub base: String,
    pub changed_entities: Vec<ChangedEntity>,
    pub findings: Vec<ReviewFinding>,
    pub schema_findings: Vec<SchemaFinding>,
    pub api_findings: Vec<ApiFinding>,
    pub stale_relationships: Vec<String>,
    pub suppressed_non_behavioral_changes: usize,
}

/// Shared, cheaply-copyable context threaded through finding construction so
/// `finding_for_edge` and its callers stay under Clippy's argument-count
/// gate instead of growing an eighth positional parameter.
#[derive(Clone, Copy)]
struct ReviewContext<'a> {
    changed_paths: &'a BTreeSet<String>,
    context_unchanged: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ReviewInput {
    pub base: String,
    pub changes: Vec<FileChange>,
    pub before: BTreeMap<String, FileAnalysis>,
    pub after: BTreeMap<String, FileAnalysis>,
    pub changed_context_files: BTreeSet<String>,
    pub verbose: bool,
}

/// Builds conservative, evidence-backed review findings for a source diff.
pub fn build_review_findings(graph: &GraphSnapshot, input: &ReviewInput) -> ReviewReport {
    let changed_entities = resolve_changed_entities(graph, input);
    let threshold = if input.verbose { 0.65 } else { 0.85 };
    let changed_paths = changed_path_set(&input.changes);
    let review_context = ReviewContext {
        changed_paths: &changed_paths,
        context_unchanged: input.changed_context_files.is_empty(),
    };
    let mut findings = Vec::new();
    let mut stale_relationships = Vec::new();
    let mut suppressed = 0;
    let mut direct_findings: BTreeMap<StableKey, BTreeSet<StableKey>> = BTreeMap::new();
    for entity in &changed_entities {
        if matches!(
            entity.change_kind,
            ChangeKind::FormattingOnly | ChangeKind::Rename | ChangeKind::RefactorLikely
        ) {
            suppressed += 1;
            continue;
        }
        let Some(stable_key) = &entity.stable_key else {
            continue;
        };
        let changed_entity_label = entity_label(entity);
        for edge in implementation_claims(graph, stable_key) {
            let claim = format_claim(edge, graph);
            if edge.status != ClaimStatus::Active {
                stale_relationships.push(claim);
                continue;
            }
            let confidence = behavior_confidence(entity.change_kind)
                .min(edge.confidence.get())
                .min(evidence_confidence(edge));
            if confidence < threshold {
                continue;
            }
            if let Some(finding) = finding_for_edge(
                graph,
                &changed_entity_label,
                entity.change_kind,
                confidence,
                edge,
                review_context,
                None,
            ) {
                direct_findings
                    .entry(stable_key.clone())
                    .or_default()
                    .insert(edge.target.clone());
                findings.push(finding);
            }
        }
    }
    findings.extend(indirect_call_findings(
        graph,
        &changed_entities,
        review_context,
        &direct_findings,
        threshold,
    ));
    findings.sort_by(|left, right| {
        right
            .severity
            .cmp(&left.severity)
            .then_with(|| left.changed_entity.cmp(&right.changed_entity))
            .then_with(|| {
                left.affected_intent
                    .identifier
                    .cmp(&right.affected_intent.identifier)
            })
    });
    stale_relationships.sort();
    stale_relationships.dedup();
    ReviewReport {
        base: input.base.clone(),
        changed_entities,
        findings,
        schema_findings: schema_change_findings(graph, input),
        api_findings: api_change_findings(graph, input),
        stale_relationships,
        suppressed_non_behavioral_changes: suppressed,
    }
}

fn api_change_findings(graph: &GraphSnapshot, input: &ReviewInput) -> Vec<ApiFinding> {
    let mut findings = Vec::new();
    for change in &input.changes {
        let (old_path, new_path) = change_paths(change);
        let before = old_path
            .and_then(|path| input.before.get(path))
            .map_or(&[][..], |analysis| analysis.symbols.as_slice());
        let after = new_path
            .and_then(|path| input.after.get(path))
            .map_or(&[][..], |analysis| analysis.symbols.as_slice());
        let symbols = before
            .iter()
            .map(|symbol| symbol.canonical_path.as_str())
            .chain(after.iter().map(|symbol| symbol.canonical_path.as_str()))
            .collect::<BTreeSet<_>>();
        for canonical_path in symbols {
            let before_symbol = before
                .iter()
                .find(|symbol| symbol.canonical_path == canonical_path);
            let after_symbol = after
                .iter()
                .find(|symbol| symbol.canonical_path == canonical_path);
            let changes = diff_api_endpoints(
                before_symbol.map_or(&[][..], |symbol| symbol.api_endpoints.as_slice()),
                after_symbol.map_or(&[][..], |symbol| symbol.api_endpoints.as_slice()),
            );
            if changes.is_empty() {
                continue;
            }
            let destructive = changes.iter().any(|change| change.destructive);
            let (related_intents, related_tests) = api_symbol_neighborhood(graph, canonical_path);
            findings.push(ApiFinding {
                source_symbol: canonical_path.to_owned(),
                file_path: new_path.or(old_path).unwrap_or("unknown").to_owned(),
                destructive,
                changes,
                related_intents,
                related_tests,
            });
        }
    }
    findings.sort_by(|left, right| left.source_symbol.cmp(&right.source_symbol));
    findings
}

/// Structurally compares endpoint contracts without attempting rename
/// inference. A method/path identity that disappears is destructive; a new
/// identity is informational. Parameter/return changes are compared only
/// after method and path match exactly.
#[must_use]
pub fn diff_api_endpoints(before: &[ApiEndpoint], after: &[ApiEndpoint]) -> Vec<ApiChange> {
    let before = before
        .iter()
        .map(|endpoint| ((endpoint.method, endpoint.path.as_str()), endpoint))
        .collect::<BTreeMap<_, _>>();
    let after = after
        .iter()
        .map(|endpoint| ((endpoint.method, endpoint.path.as_str()), endpoint))
        .collect::<BTreeMap<_, _>>();
    let mut changes = Vec::new();
    for ((method, path), endpoint) in &before {
        let Some(current) = after.get(&(*method, *path)) else {
            changes.push(ApiChange {
                kind: ApiChangeKind::Removed,
                method: *method,
                path: (*path).to_owned(),
                destructive: true,
                details: Vec::new(),
            });
            continue;
        };
        let (details, destructive) = changed_api_contract(endpoint, current);
        if !details.is_empty() {
            changes.push(ApiChange {
                kind: ApiChangeKind::ContractModified,
                method: *method,
                path: (*path).to_owned(),
                destructive,
                details,
            });
        }
    }
    for ((method, path), _) in after
        .iter()
        .filter(|(identity, _)| !before.contains_key(identity))
    {
        changes.push(ApiChange {
            kind: ApiChangeKind::Added,
            method: *method,
            path: (*path).to_owned(),
            destructive: false,
            details: Vec::new(),
        });
    }
    changes.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.method.cmp(&right.method))
            .then_with(|| change_kind_order(left.kind).cmp(&change_kind_order(right.kind)))
    });
    changes
}

fn changed_api_contract(before: &ApiEndpoint, after: &ApiEndpoint) -> (Vec<String>, bool) {
    let mut details = Vec::new();
    let mut destructive = false;
    let before_params = before
        .params
        .iter()
        .map(|parameter| (parameter.name.as_str(), parameter))
        .collect::<BTreeMap<_, _>>();
    let after_params = after
        .params
        .iter()
        .map(|parameter| (parameter.name.as_str(), parameter))
        .collect::<BTreeMap<_, _>>();
    for (name, parameter) in &before_params {
        match after_params.get(name) {
            None => {
                details.push(format!("removed parameter {name}"));
                destructive = true;
            }
            Some(current) if *parameter != *current => {
                details.push(format!("changed parameter {name}"));
                destructive = true;
            }
            Some(_) => {}
        }
    }
    for (name, parameter) in after_params
        .iter()
        .filter(|(name, _)| !before_params.contains_key(*name))
    {
        details.push(format!(
            "added {} parameter {name}",
            if parameter.required {
                destructive = true;
                "required"
            } else {
                "optional"
            }
        ));
    }
    if before.return_type != after.return_type {
        details.push(format!(
            "return type changed from {} to {}",
            before.return_type.as_deref().unwrap_or("unknown"),
            after.return_type.as_deref().unwrap_or("unknown")
        ));
        destructive |= before.return_type.is_some();
    }
    if before.framework != after.framework {
        details.push(format!(
            "framework changed from {} to {}",
            before.framework, after.framework
        ));
    }
    (details, destructive)
}

const fn change_kind_order(kind: ApiChangeKind) -> u8 {
    match kind {
        ApiChangeKind::Removed => 0,
        ApiChangeKind::ContractModified => 1,
        ApiChangeKind::Added => 2,
    }
}

fn api_symbol_neighborhood(
    graph: &GraphSnapshot,
    canonical_path: &str,
) -> (Vec<NodeSummary>, Vec<NodeSummary>) {
    let symbol_keys = graph
        .resolve(canonical_path)
        .into_iter()
        .filter(|node| node.kind == NodeKind::CodeSymbol)
        .map(|node| node.stable_key.clone())
        .collect::<BTreeSet<_>>();
    let mut intents = Vec::new();
    let mut tests = Vec::new();
    for edge in graph
        .edges
        .iter()
        .filter(|edge| symbol_keys.contains(&edge.source) && edge.status == ClaimStatus::Active)
    {
        match edge.kind {
            RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies => {
                if let Some(node) = graph.nodes.get(&edge.target) {
                    intents.push(NodeSummary::from(node));
                }
            }
            RelationKind::CoveredBy => {
                if let Some(node) = graph.nodes.get(&edge.target).filter(|node| node.is_test()) {
                    tests.push(NodeSummary::from(node));
                }
            }
            _ => {}
        }
    }
    (dedup_summaries(intents), dedup_summaries(tests))
}

/// Surfaces deterministic schema changes directly from a diff's migration
/// and ORM model files. A migration's own declared operations
/// (`declared_schema_changes`) are always used for `SchemaMigration` symbols
/// — goose migrations are historical, append-only records, so what a
/// statement declares matters regardless of whether the file is new or
/// edited. Every other schema-declaring symbol kind (currently only
/// `SQLAlchemy`'s declarative `Class`) is structurally diffed against its
/// matched prior version when one exists, since an ORM model genuinely is
/// edited over its lifetime.
fn schema_change_findings(graph: &GraphSnapshot, input: &ReviewInput) -> Vec<SchemaFinding> {
    let mut findings = Vec::new();
    for change in &input.changes {
        let (old_path, new_path) = change_paths(change);
        let before_symbols = old_path
            .and_then(|path| input.before.get(path))
            .map_or(&[][..], |analysis| analysis.symbols.as_slice());
        let after_symbols = new_path
            .and_then(|path| input.after.get(path))
            .map_or(&[][..], |analysis| analysis.symbols.as_slice());

        for after_symbol in after_symbols {
            if after_symbol.schema_tables.is_empty() {
                continue;
            }
            let before_symbol = (after_symbol.kind != SymbolKind::SchemaMigration)
                .then(|| {
                    before_symbols
                        .iter()
                        .find(|symbol| symbol.canonical_path == after_symbol.canonical_path)
                })
                .flatten();
            let changes = schema_symbol_changes(before_symbol, after_symbol);
            if changes.is_empty() {
                continue;
            }
            let destructive = changes.iter().any(|change| change.destructive);
            let (related_intents, related_tests) = changes
                .iter()
                .map(|change| change.entity.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .fold(
                    (Vec::new(), Vec::new()),
                    |(mut intents, mut tests), entity| {
                        let (entity_intents, entity_tests) = db_entity_neighborhood(graph, entity);
                        intents.extend(entity_intents);
                        tests.extend(entity_tests);
                        (intents, tests)
                    },
                );
            findings.push(SchemaFinding {
                source_symbol: after_symbol.canonical_path.clone(),
                file_path: new_path.unwrap_or("unknown").to_owned(),
                destructive,
                changes,
                related_intents: dedup_summaries(related_intents),
                related_tests: dedup_summaries(related_tests),
            });
        }
    }
    findings.sort_by(|left, right| left.source_symbol.cmp(&right.source_symbol));
    findings
}

fn schema_symbol_changes(
    before: Option<&SymbolDefinition>,
    after: &SymbolDefinition,
) -> Vec<SchemaChange> {
    after
        .schema_tables
        .iter()
        .flat_map(|after_table| {
            let matched_before = before.and_then(|before| {
                before
                    .schema_tables
                    .iter()
                    .find(|table| table.entity == after_table.entity)
            });
            match matched_before {
                Some(before_table) => diff_schema_tables(before_table, after_table),
                None => declared_schema_changes(after_table),
            }
        })
        .collect()
}

/// A bounded, one-hop-then-one-hop advisory lookup from a `DbEntity` stable
/// key: the code that directly reads/writes/declares it, and that code's own
/// mapped intent/tests. Deliberately shallow (no further semantic
/// expansion) so a widely used table cannot bridge into unrelated product
/// areas the way a fuller traversal policy must otherwise guard against.
fn db_entity_neighborhood(
    graph: &GraphSnapshot,
    entity: &str,
) -> (Vec<NodeSummary>, Vec<NodeSummary>) {
    let Ok(db_key) = StableKey::new(format!("db:{entity}")) else {
        return (Vec::new(), Vec::new());
    };
    if !graph.nodes.contains_key(&db_key) {
        return (Vec::new(), Vec::new());
    }
    let code_symbols = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.target == db_key
                && edge.status == ClaimStatus::Active
                && matches!(
                    edge.kind,
                    RelationKind::ReadsFrom | RelationKind::WritesTo | RelationKind::DefinesSchema
                )
        })
        .map(|edge| edge.source.clone())
        .collect::<BTreeSet<_>>();
    let mut intents = Vec::new();
    let mut tests = Vec::new();
    for symbol_key in &code_symbols {
        for edge in &graph.edges {
            if edge.source != *symbol_key || edge.status != ClaimStatus::Active {
                continue;
            }
            match edge.kind {
                RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies => {
                    if let Some(node) = graph.nodes.get(&edge.target) {
                        intents.push(NodeSummary::from(node));
                    }
                }
                RelationKind::CoveredBy => {
                    if let Some(node) = graph.nodes.get(&edge.target).filter(|node| node.is_test())
                    {
                        tests.push(NodeSummary::from(node));
                    }
                }
                _ => {}
            }
        }
        if let Some(node) = graph.nodes.get(symbol_key).filter(|node| node.is_test()) {
            tests.push(NodeSummary::from(node));
        }
    }
    (intents, tests)
}

fn dedup_summaries(mut summaries: Vec<NodeSummary>) -> Vec<NodeSummary> {
    summaries.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    summaries.dedup_by(|left, right| left.stable_key == right.stable_key);
    summaries
}

pub fn resolve_changed_entities(graph: &GraphSnapshot, input: &ReviewInput) -> Vec<ChangedEntity> {
    let mut entities = Vec::new();
    for change in &input.changes {
        let (old_path, new_path) = change_paths(change);
        let before_analysis = old_path.and_then(|path| input.before.get(path));
        let after_analysis = new_path.and_then(|path| input.after.get(path));
        let before = before_analysis.map_or(&[][..], |analysis| analysis.symbols.as_slice());
        let after = after_analysis.map_or(&[][..], |analysis| analysis.symbols.as_slice());
        pair_symbols(
            graph,
            AnalyzedSymbols {
                path: old_path,
                language: before_analysis.map(|analysis| analysis.language.as_str()),
                symbols: before,
            },
            AnalyzedSymbols {
                path: new_path,
                language: after_analysis.map(|analysis| analysis.language.as_str()),
                symbols: after,
            },
            &mut entities,
        );
    }
    let mut entities = merge_cross_file_moves(entities);
    entities.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.after.cmp(&right.after))
            .then_with(|| left.before.cmp(&right.before))
    });
    entities
}

/// `pair_symbols` only ever sees one [`FileChange`] at a time, so a symbol
/// moved between two files it already owns (Git reports this as a deletion
/// from the old file plus an addition in the new one, never a rename) comes
/// out as two independent `BehaviorPotentiallyChanged` entities even though
/// they resolve to the identical stored identity via `graph_symbol_key`'s
/// fingerprint fallback. Left unmerged, both sides can independently surface
/// findings against the same intent, doubling every finding. Collapse a
/// delete/add pair that shares one stable key back into the single `Rename`
/// they already represent at the graph level.
fn merge_cross_file_moves(entities: Vec<ChangedEntity>) -> Vec<ChangedEntity> {
    let mut by_key: BTreeMap<StableKey, Vec<usize>> = BTreeMap::new();
    for (index, entity) in entities.iter().enumerate() {
        if let Some(key) = &entity.stable_key {
            by_key.entry(key.clone()).or_default().push(index);
        }
    }
    let mut removed = BTreeSet::new();
    let mut merged = Vec::new();
    for indices in by_key.values() {
        let [first, second] = indices.as_slice() else {
            continue;
        };
        let (one, other) = (&entities[*first], &entities[*second]);
        let Some((deleted, added)) = (if one.after.is_none() && other.before.is_none() {
            Some((one, other))
        } else if other.after.is_none() && one.before.is_none() {
            Some((other, one))
        } else {
            None
        }) else {
            continue;
        };
        removed.insert(*first);
        removed.insert(*second);
        merged.push(ChangedEntity {
            stable_key: deleted.stable_key.clone(),
            before: deleted.before.clone(),
            after: added.after.clone(),
            file_path: added.file_path.clone(),
            change_kind: ChangeKind::Rename,
            signals: vec!["symbol renamed or moved".to_owned()],
        });
    }
    entities
        .into_iter()
        .enumerate()
        .filter_map(|(index, entity)| (!removed.contains(&index)).then_some(entity))
        .chain(merged)
        .collect()
}

#[derive(Clone, Copy)]
struct AnalyzedSymbols<'a> {
    path: Option<&'a str>,
    language: Option<&'a str>,
    symbols: &'a [SymbolDefinition],
}

fn pair_symbols(
    graph: &GraphSnapshot,
    before: AnalyzedSymbols<'_>,
    after: AnalyzedSymbols<'_>,
    entities: &mut Vec<ChangedEntity>,
) {
    let mut paired_after = BTreeSet::new();
    for old in before.symbols {
        let matched = after.symbols.iter().enumerate().find(|(index, new)| {
            !paired_after.contains(index)
                && (new.canonical_path == old.canonical_path
                    || (new.kind == old.kind
                        && new.structural_fingerprint == old.structural_fingerprint))
        });
        if let Some((index, new)) = matched {
            paired_after.insert(index);
            if old.body_hash != new.body_hash
                || old.signature != new.signature
                || old.canonical_path != new.canonical_path
            {
                entities.push(changed_entity(graph, before, after, Some(old), Some(new)));
            }
        } else {
            entities.push(changed_entity(graph, before, after, Some(old), None));
        }
    }
    for (index, new) in after.symbols.iter().enumerate() {
        if !paired_after.contains(&index) {
            entities.push(changed_entity(graph, before, after, None, Some(new)));
        }
    }
}

fn changed_entity(
    graph: &GraphSnapshot,
    before_analysis: AnalyzedSymbols<'_>,
    after_analysis: AnalyzedSymbols<'_>,
    before: Option<&SymbolDefinition>,
    after: Option<&SymbolDefinition>,
) -> ChangedEntity {
    let (change_kind, signals) = classify_behavior_change(before, after);
    let stable_key = before
        .and_then(|symbol| graph_symbol_key(graph, symbol, before_analysis.language))
        .or_else(|| {
            after.and_then(|symbol| graph_symbol_key(graph, symbol, after_analysis.language))
        });
    ChangedEntity {
        stable_key,
        before: before.map(|symbol| symbol.canonical_path.clone()),
        after: after.map(|symbol| symbol.canonical_path.clone()),
        file_path: after_analysis
            .path
            .or(before_analysis.path)
            .unwrap_or("unknown")
            .to_owned(),
        change_kind,
        signals,
    }
}

/// Note on `ChangeKind::RefactorLikely`: the branch below is unreachable from
/// `pair_symbols`, which only calls this function on a pair where at least
/// one of `body_hash`, `signature`, or `canonical_path` differs. Whenever
/// `body_hash` matches, the underlying bytes are identical, so
/// `structural_fingerprint` (a hash of the same bytes with whitespace
/// stripped) matches too — meaning any `canonical_path` change already took
/// the `Rename` branch above, and an unchanged `canonical_path` plus matching
/// `signature`/`body_hash` would never have produced a pair to classify in
/// the first place. This is left as-is rather than wired up with a weaker
/// signal (e.g. "body changed but the call set didn't"): that would suppress
/// exactly the failure class `cancellation-behavior-change` in the eval
/// corpus exists to catch — a guard condition removed while the surrounding
/// calls stay the same — and `eng_conclu.md` §38 explicitly rules out trying
/// to prove semantic equivalence. Kept as a documented, tested gap rather
/// than silently dead code; see `refactor_likely_is_intentionally_unreachable`.
pub fn classify_behavior_change(
    before: Option<&SymbolDefinition>,
    after: Option<&SymbolDefinition>,
) -> (ChangeKind, Vec<String>) {
    let (Some(before), Some(after)) = (before, after) else {
        return (
            ChangeKind::BehaviorPotentiallyChanged,
            vec![if before.is_some() {
                "symbol deleted".to_owned()
            } else {
                "symbol added".to_owned()
            }],
        );
    };
    if before.signature != after.signature {
        return (
            ChangeKind::ContractChanged,
            vec!["public signature changed".to_owned()],
        );
    }
    if before.canonical_path != after.canonical_path
        && before.structural_fingerprint == after.structural_fingerprint
    {
        return (
            ChangeKind::Rename,
            vec!["symbol renamed or moved".to_owned()],
        );
    }
    if before.body_hash == after.body_hash {
        return (
            ChangeKind::RefactorLikely,
            vec!["body unchanged".to_owned()],
        );
    }
    if before.structural_fingerprint == after.structural_fingerprint {
        return (
            ChangeKind::FormattingOnly,
            vec!["only whitespace changed in the symbol body".to_owned()],
        );
    }
    let before_calls = call_names(before);
    let after_calls = call_names(after);
    let mut signals = vec!["symbol body changed".to_owned()];
    if before_calls != after_calls {
        signals.push("called functions changed".to_owned());
    }
    push_database_change_signal(
        &mut signals,
        "reads",
        &database_entities(before, DatabaseAccessKind::Read),
        &database_entities(after, DatabaseAccessKind::Read),
    );
    push_database_change_signal(
        &mut signals,
        "writes",
        &database_entities(before, DatabaseAccessKind::Write),
        &database_entities(after, DatabaseAccessKind::Write),
    );
    (ChangeKind::BehaviorPotentiallyChanged, signals)
}

fn call_names(symbol: &SymbolDefinition) -> BTreeSet<&str> {
    symbol
        .calls
        .iter()
        .map(|call| call.callee.as_str())
        .collect()
}

fn database_entities(symbol: &SymbolDefinition, kind: DatabaseAccessKind) -> BTreeSet<&str> {
    symbol
        .database_accesses
        .iter()
        .filter(|access| access.kind == kind)
        .map(|access| access.entity.as_str())
        .collect()
}

fn push_database_change_signal(
    signals: &mut Vec<String>,
    operation: &str,
    before: &BTreeSet<&str>,
    after: &BTreeSet<&str>,
) {
    if before == after {
        return;
    }
    let render = |entities: &BTreeSet<&str>| {
        if entities.is_empty() {
            "none".to_owned()
        } else {
            entities.iter().copied().collect::<Vec<_>>().join(", ")
        }
    };
    signals.push(format!(
        "database {operation} changed: {} -> {}",
        render(before),
        render(after)
    ));
}

fn graph_symbol_key(
    graph: &GraphSnapshot,
    symbol: &SymbolDefinition,
    language: Option<&str>,
) -> Option<StableKey> {
    let exact = graph.nodes.values().find(|node| {
        language_matches(node, language)
            && matches!(
                &node.attributes,
                PlannedNodeAttributes::Symbol { canonical_path, .. }
                    if canonical_path == &symbol.canonical_path
            )
    });
    exact
        .or_else(|| {
            let matches = graph
                .nodes
                .values()
                .filter(|node| {
                    language_matches(node, language) && matches_fingerprint(node, symbol)
                })
                .collect::<Vec<_>>();
            (matches.len() == 1).then(|| matches[0])
        })
        .map(|node| node.stable_key.clone())
}

fn language_matches(node: &GraphNode, language: Option<&str>) -> bool {
    language.is_none_or(|language| {
        !node.stable_key.as_str().starts_with("symbol:")
            || node
                .stable_key
                .as_str()
                .starts_with(&format!("symbol:{language}:"))
    })
}

fn matches_fingerprint(node: &GraphNode, symbol: &SymbolDefinition) -> bool {
    matches!(
        &node.attributes,
        PlannedNodeAttributes::Symbol {
            symbol_kind,
            structural_fingerprint,
            ..
        } if *symbol_kind == symbol.kind
            && structural_fingerprint == &symbol.structural_fingerprint
    )
}

fn implementation_claims<'a>(
    graph: &'a GraphSnapshot,
    stable_key: &StableKey,
) -> impl Iterator<Item = &'a GraphEdge> {
    graph.edges.iter().filter(move |edge| {
        edge.source == *stable_key
            && matches!(
                edge.kind,
                RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
            )
    })
}

fn finding_for_edge(
    graph: &GraphSnapshot,
    changed_entity: &str,
    change_kind: ChangeKind,
    confidence: f32,
    edge: &GraphEdge,
    context: ReviewContext<'_>,
    indirect_via: Option<&str>,
) -> Option<ReviewFinding> {
    let intent = graph.nodes.get(&edge.target)?;
    let related_tests = related_tests(graph, &intent.stable_key);
    let tests_modified = related_tests.iter().any(|test| {
        graph
            .nodes
            .values()
            .find(|node| node.stable_key.as_str() == test.stable_key)
            .is_some_and(|node| node_file_changed(node, context.changed_paths))
    });
    let possible_requirement_drift = context.context_unchanged
        && matches!(intent.kind, NodeKind::Requirement | NodeKind::Invariant);
    let reason = indirect_via.map_or_else(
        || {
            format!(
                "The changed symbol has a {:?} {:?} claim from {:?}.",
                edge.claim_class, edge.kind, edge.source_kind
            )
        },
        |helper| {
            format!(
                "This symbol has a {:?} {:?} claim from {:?}, and it calls `{helper}`, whose body changed in this diff.",
                edge.claim_class, edge.kind, edge.source_kind
            )
        },
    );
    Some(ReviewFinding {
        severity: severity(intent.kind, change_kind),
        confidence,
        changed_entity: changed_entity.to_owned(),
        change_kind,
        affected_intent: NodeSummary::from(intent),
        reason,
        evidence: edge
            .evidence
            .iter()
            .map(|item| format!("{}#{}", item.source_uri, item.locator))
            .collect(),
        related_tests,
        tests_modified,
        possible_requirement_drift,
        uncertainty: indirect_via.map(|helper| format!(
            "Indirect signal: this symbol's own body did not change in this diff; it calls `{helper}`, whose body did (one call hop, not chased further)."
        )),
        suggested_action: suggested_action(intent.kind, tests_modified).to_owned(),
    })
}

fn entity_label(entity: &ChangedEntity) -> String {
    entity
        .after
        .as_ref()
        .or(entity.before.as_ref())
        .cloned()
        .unwrap_or_else(|| entity.file_path.clone())
}

/// Closes the recall gap the self-corpus historical-PR pilot found: a
/// changed-symbol pass only ever inspects claims on the exact symbol whose
/// own body or signature differs in the diff, so a real behavior change
/// hiding in a private helper a mapped public entry point calls produced no
/// finding at all, even though the mapped entry point's own claim is exactly
/// what a reviewer needs to re-verify. Mirrors the bounded one-hop
/// caller/callee exemption `impact.rs`'s `expand_semantics` already applies
/// (deliberately not a general call-graph walk): only a helper's direct,
/// structurally proven callers are considered, and the walk stops there — no
/// second hop, so a caller-of-a-caller can never be reached this way. A
/// caller already carrying its own direct finding for the same intent (its
/// own body changed too) is not flagged again through this weaker,
/// indirection-only signal.
///
/// Only a genuinely callable, executable-body kind (`Function`/`Method`) can
/// be an escalation source. A container symbol (a Python class, for example)
/// can independently become a `ChangedEntity` purely because a method it
/// textually contains changed — its own `body_hash` is an aggregate of its
/// children's text, not a fact about its own behavior — and excluding it
/// here is what keeps a pure rename of a nested method silent instead of
/// leaking a false "the class's callers might be affected" signal (the class
/// itself gets no signature/body change of its own; the nested method, which
/// does, is separately classified `Rename` and already suppressed above it).
///
/// Deduped by `(caller, intent)` across every changed entity in one pass, not
/// per entity: a real behavior change can still be reachable from a caller
/// through more than one call edge (for example both a constructor call and
/// a direct method call on the same line), which would otherwise earn two
/// findings for the one real underlying change. When more than one changed
/// entity explains the same `(caller, intent)` pair, the most specific one
/// (the longest canonical path) wins, deterministically.
fn indirect_call_findings(
    graph: &GraphSnapshot,
    changed_entities: &[ChangedEntity],
    context: ReviewContext<'_>,
    direct_findings: &BTreeMap<StableKey, BTreeSet<StableKey>>,
    threshold: f32,
) -> Vec<ReviewFinding> {
    let mut best: BTreeMap<(StableKey, StableKey), (usize, ReviewFinding)> = BTreeMap::new();
    for entity in changed_entities {
        if matches!(
            entity.change_kind,
            ChangeKind::FormattingOnly | ChangeKind::Rename | ChangeKind::RefactorLikely
        ) {
            continue;
        }
        let Some(helper_key) = &entity.stable_key else {
            continue;
        };
        if !is_callable_behavior_source(graph, helper_key) {
            continue;
        }
        let helper_label = entity_label(entity);
        let specificity = helper_label.len();
        for caller_key in callers_of(graph, helper_key) {
            let Some(caller_node) = graph.nodes.get(&caller_key) else {
                continue;
            };
            if caller_node.is_test() {
                continue;
            }
            let already_direct = direct_findings.get(&caller_key);
            for edge in implementation_claims(graph, &caller_key) {
                if edge.status != ClaimStatus::Active {
                    continue;
                }
                if already_direct.is_some_and(|targets| targets.contains(&edge.target)) {
                    continue;
                }
                let confidence = behavior_confidence(entity.change_kind)
                    .min(edge.confidence.get())
                    .min(evidence_confidence(edge));
                if confidence < threshold {
                    continue;
                }
                let Some(finding) = finding_for_edge(
                    graph,
                    caller_node.identifier(),
                    entity.change_kind,
                    confidence,
                    edge,
                    context,
                    Some(&helper_label),
                ) else {
                    continue;
                };
                let dedup_key = (caller_key.clone(), edge.target.clone());
                best.entry(dedup_key)
                    .and_modify(|(best_specificity, best_finding)| {
                        if specificity > *best_specificity {
                            *best_specificity = specificity;
                            *best_finding = finding.clone();
                        }
                    })
                    .or_insert((specificity, finding));
            }
        }
    }
    best.into_values().map(|(_, finding)| finding).collect()
}

/// The direct, structurally proven callers of `key` — one hop only, deduped.
fn callers_of(graph: &GraphSnapshot, key: &StableKey) -> BTreeSet<StableKey> {
    graph
        .edges
        .iter()
        .filter(|edge| {
            edge.target == *key
                && edge.kind == RelationKind::Calls
                && edge.status == ClaimStatus::Active
        })
        .map(|edge| edge.source.clone())
        .collect()
}

fn is_callable_behavior_source(graph: &GraphSnapshot, key: &StableKey) -> bool {
    graph.nodes.get(key).is_some_and(|node| {
        matches!(
            &node.attributes,
            PlannedNodeAttributes::Symbol { symbol_kind, .. }
                if matches!(symbol_kind, SymbolKind::Function | SymbolKind::Method)
        )
    })
}

fn related_tests(graph: &GraphSnapshot, intent: &StableKey) -> Vec<NodeSummary> {
    let mut tests = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.source == *intent
                && edge.kind == RelationKind::CoveredBy
                && edge.status == ClaimStatus::Active
        })
        .filter_map(|edge| graph.nodes.get(&edge.target))
        .filter(|node| node.is_test())
        .map(NodeSummary::from)
        .collect::<Vec<_>>();
    tests.sort_by(|left, right| left.identifier.cmp(&right.identifier));
    tests
}

fn node_file_changed(node: &GraphNode, changed_paths: &BTreeSet<String>) -> bool {
    matches!(
        &node.attributes,
        PlannedNodeAttributes::Symbol { file_path, .. } if changed_paths.contains(file_path)
    )
}

fn changed_path_set(changes: &[FileChange]) -> BTreeSet<String> {
    changes
        .iter()
        .flat_map(|change| match change {
            FileChange::Added { path }
            | FileChange::Modified { path }
            | FileChange::Deleted { path } => vec![path.clone()],
            FileChange::Renamed { old_path, new_path } => {
                vec![old_path.clone(), new_path.clone()]
            }
        })
        .collect()
}

fn change_paths_for_one(change: &FileChange) -> (Option<&str>, Option<&str>) {
    match change {
        FileChange::Added { path } => (None, Some(path)),
        FileChange::Modified { path } => (Some(path), Some(path)),
        FileChange::Deleted { path } => (Some(path), None),
        FileChange::Renamed { old_path, new_path } => (Some(old_path), Some(new_path)),
    }
}

fn format_claim(edge: &GraphEdge, graph: &GraphSnapshot) -> String {
    let source = graph.nodes.get(&edge.source).map_or_else(
        || edge.source.to_string(),
        |node| node.identifier().to_owned(),
    );
    let target = graph.nodes.get(&edge.target).map_or_else(
        || edge.target.to_string(),
        |node| node.identifier().to_owned(),
    );
    format!("{source} {:?} {target}", edge.kind)
}

const fn behavior_confidence(kind: ChangeKind) -> f32 {
    match kind {
        ChangeKind::FormattingOnly | ChangeKind::Rename | ChangeKind::ContractChanged => 1.0,
        ChangeKind::RefactorLikely => 0.75,
        ChangeKind::BehaviorPotentiallyChanged => 0.9,
        ChangeKind::Unknown => 0.65,
    }
}

fn evidence_confidence(edge: &GraphEdge) -> f32 {
    if edge.evidence.is_empty() && edge.kind.is_semantic() {
        0.8
    } else {
        edge.evidence
            .iter()
            .map(|evidence| evidence.strength.get())
            .fold(1.0, f32::min)
    }
}

const fn severity(kind: NodeKind, change: ChangeKind) -> Severity {
    match (kind, change) {
        (NodeKind::Invariant | NodeKind::Requirement, ChangeKind::ContractChanged) => {
            Severity::High
        }
        (NodeKind::Invariant | NodeKind::Requirement, _) => Severity::High,
        (NodeKind::Feature | NodeKind::Decision, _) => Severity::Medium,
        _ => Severity::Low,
    }
}

const fn suggested_action(kind: NodeKind, tests_modified: bool) -> &'static str {
    match (kind, tests_modified) {
        (NodeKind::Invariant, false) => {
            "Verify the invariant still holds and review the unchanged related tests."
        }
        (NodeKind::Requirement, false) => {
            "Verify the requirement still holds and review the unchanged related tests."
        }
        (NodeKind::Invariant, true) => "Verify the changed tests still protect this invariant.",
        (NodeKind::Requirement, true) => "Verify the changed tests still protect this requirement.",
        _ => "Verify the product intent remains accurate for this behavior change.",
    }
}

fn change_paths(change: &FileChange) -> (Option<&str>, Option<&str>) {
    change_paths_for_one(change)
}

#[cfg(test)]
mod tests {
    use crate::{
        domain::{ClaimClass, Confidence, SourceKind},
        graph::{GraphEvidence, GraphNode},
        ir::{CallSite, DatabaseAccess, SourceRange, SymbolKind},
    };

    use super::*;

    fn endpoint(method: HttpMethod, path: &str, params: Vec<crate::ir::ApiParam>) -> ApiEndpoint {
        ApiEndpoint {
            method,
            path: path.to_owned(),
            params,
            return_type: Some("Subscription".to_owned()),
            framework: "python_http_framework".to_owned(),
            range: source_range(),
        }
    }

    #[test]
    fn api_diff_separates_additions_from_destructive_contract_changes() {
        let before = vec![
            endpoint(HttpMethod::Delete, "/subscriptions/{id}", Vec::new()),
            endpoint(HttpMethod::Get, "/subscriptions", Vec::new()),
        ];
        let after = vec![
            endpoint(HttpMethod::Post, "/subscriptions/{id}", Vec::new()),
            endpoint(
                HttpMethod::Get,
                "/subscriptions",
                vec![crate::ir::ApiParam {
                    name: "expand".to_owned(),
                    type_hint: Some("bool".to_owned()),
                    source: crate::ir::ParamSource::Query,
                    required: false,
                }],
            ),
        ];

        let changes = diff_api_endpoints(&before, &after);
        assert_eq!(changes.len(), 3);
        assert!(changes.iter().any(|change| {
            change.kind == ApiChangeKind::Removed
                && change.method == HttpMethod::Delete
                && change.destructive
        }));
        assert!(changes.iter().any(|change| {
            change.kind == ApiChangeKind::Added
                && change.method == HttpMethod::Post
                && !change.destructive
        }));
        assert!(changes.iter().any(|change| {
            change.kind == ApiChangeKind::ContractModified
                && change.method == HttpMethod::Get
                && !change.destructive
                && change.details == vec!["added optional parameter expand"]
        }));
    }

    #[test]
    fn removing_or_changing_a_parameter_is_destructive() {
        let before = endpoint(
            HttpMethod::Get,
            "/subscriptions/{id}",
            vec![crate::ir::ApiParam {
                name: "id".to_owned(),
                type_hint: Some("str".to_owned()),
                source: crate::ir::ParamSource::Path,
                required: true,
            }],
        );
        let mut changed = before.clone();
        changed.params[0].type_hint = Some("UUID".to_owned());
        assert!(diff_api_endpoints(std::slice::from_ref(&before), &[changed])[0].destructive);

        let removed = endpoint(HttpMethod::Get, "/subscriptions/{id}", Vec::new());
        assert!(diff_api_endpoints(&[before], &[removed])[0].destructive);
    }

    #[test]
    fn classifies_contract_and_formatting_changes_without_guessing_equivalence() {
        let before = symbol("body-a", "shape", "(value)");
        let formatted = symbol("body-b", "shape", "(value)");
        let contract = symbol("body-c", "shape-c", "(value, force=False)");

        assert_eq!(
            classify_behavior_change(Some(&before), Some(&formatted)).0,
            ChangeKind::FormattingOnly
        );
        assert_eq!(
            classify_behavior_change(Some(&before), Some(&contract)).0,
            ChangeKind::ContractChanged
        );
    }

    #[test]
    fn reports_changed_database_reads_and_writes_as_behavior_signals() {
        let mut before = symbol("body-a", "shape-a", "(value)");
        before.database_accesses = vec![DatabaseAccess {
            entity: "subscriptions".to_owned(),
            kind: DatabaseAccessKind::Write,
            range: source_range(),
            statement_hash: "sql-a".to_owned(),
            columns: Vec::new(),
        }];
        let mut after = symbol("body-b", "shape-b", "(value)");
        after.database_accesses = vec![DatabaseAccess {
            entity: "subscription_archive".to_owned(),
            kind: DatabaseAccessKind::Write,
            range: source_range(),
            statement_hash: "sql-b".to_owned(),
            columns: Vec::new(),
        }];

        let (kind, signals) = classify_behavior_change(Some(&before), Some(&after));

        assert_eq!(kind, ChangeKind::BehaviorPotentiallyChanged);
        assert!(signals.iter().any(|signal| {
            signal == "database writes changed: subscriptions -> subscription_archive"
        }));
    }

    #[test]
    fn behavior_change_with_documented_requirement_creates_one_precise_finding() {
        let code_key = StableKey::new("code").expect("code key");
        let requirement_key = StableKey::new("requirement").expect("requirement key");
        let test_key = StableKey::new("test").expect("test key");
        let code = code_node(
            &code_key,
            "billing.cancel",
            SymbolKind::Method,
            "src/billing.py",
        );
        let requirement = intent_node(&requirement_key, NodeKind::Requirement, "REQ-SUB-014");
        let test = code_node(
            &test_key,
            "tests.test_cancel",
            SymbolKind::Test,
            "tests/test.py",
        );
        let nodes = [code, requirement, test]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect();
        let edges = vec![
            semantic_edge(&code_key, &requirement_key, RelationKind::Implements, true),
            semantic_edge(&requirement_key, &test_key, RelationKind::CoveredBy, true),
        ];
        let before_symbol = symbol("old-body", "old-shape", "(value)");
        let after_symbol = symbol("new-body", "new-shape", "(value)");
        let input = ReviewInput {
            base: "HEAD".to_owned(),
            changes: vec![FileChange::Modified {
                path: "src/billing.py".to_owned(),
            }],
            before: BTreeMap::from([(
                "src/billing.py".to_owned(),
                analysis("src/billing.py", before_symbol),
            )]),
            after: BTreeMap::from([(
                "src/billing.py".to_owned(),
                analysis("src/billing.py", after_symbol),
            )]),
            changed_context_files: BTreeSet::new(),
            verbose: false,
        };

        let report = build_review_findings(&GraphSnapshot { nodes, edges }, &input);

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::High);
        assert_eq!(report.findings[0].related_tests.len(), 1);
        assert!(!report.findings[0].tests_modified);
        assert!(report.findings[0].possible_requirement_drift);
    }

    #[test]
    fn cross_file_move_merges_into_one_silent_rename() {
        let code_key = StableKey::new("code").expect("code key");
        let requirement_key = StableKey::new("requirement").expect("requirement key");
        let code = code_node(
            &code_key,
            "billing.subscription.SubscriptionService.cancel",
            SymbolKind::Method,
            "src/billing/subscription.py",
        );
        let requirement = intent_node(&requirement_key, NodeKind::Requirement, "REQ-SUB-014");
        let nodes = [code, requirement]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect();
        let edges = vec![semantic_edge(
            &code_key,
            &requirement_key,
            RelationKind::Implements,
            true,
        )];
        let moved_out = SymbolDefinition {
            name: "cancel".to_owned(),
            canonical_path: "billing.subscription.SubscriptionService.cancel".to_owned(),
            kind: SymbolKind::Method,
            range: source_range(),
            signature: Some("(self, subscription, now)".to_owned()),
            body_hash: "identical-body".to_owned(),
            structural_fingerprint: "shape".to_owned(),
            calls: Vec::new(),
            database_accesses: Vec::new(),
            schema_tables: Vec::new(),
            api_endpoints: Vec::new(),
            external_calls: Vec::new(),
        };
        let moved_in = SymbolDefinition {
            canonical_path: "billing.cancellation.SubscriptionService.cancel".to_owned(),
            ..moved_out.clone()
        };
        let input = ReviewInput {
            base: "HEAD".to_owned(),
            changes: vec![
                FileChange::Modified {
                    path: "src/billing/subscription.py".to_owned(),
                },
                FileChange::Added {
                    path: "src/billing/cancellation.py".to_owned(),
                },
            ],
            before: BTreeMap::from([(
                "src/billing/subscription.py".to_owned(),
                analysis("src/billing/subscription.py", moved_out),
            )]),
            after: BTreeMap::from([
                (
                    "src/billing/subscription.py".to_owned(),
                    FileAnalysis {
                        path: "src/billing/subscription.py".to_owned(),
                        language: "python".to_owned(),
                        analysis_version: "python-tree-sitter-v1".to_owned(),
                        content_hash: "after-subscription".to_owned(),
                        symbols: Vec::new(),
                    },
                ),
                (
                    "src/billing/cancellation.py".to_owned(),
                    analysis("src/billing/cancellation.py", moved_in),
                ),
            ]),
            changed_context_files: BTreeSet::new(),
            verbose: false,
        };

        let report = build_review_findings(&GraphSnapshot { nodes, edges }, &input);

        assert_eq!(report.changed_entities.len(), 1);
        assert_eq!(report.changed_entities[0].change_kind, ChangeKind::Rename);
        assert!(report.findings.is_empty());
        assert_eq!(report.suppressed_non_behavioral_changes, 1);
    }

    /// The self-corpus historical-PR pilot's real recall gap: a mapped public
    /// entry point's own body/signature never changes, so it never becomes a
    /// `ChangedEntity` at all, but a private, unmapped helper it directly
    /// calls does change behaviorally. The caller's own `Implements` claim
    /// must still surface a finding, attributed to the caller (what the
    /// reviewer mapped), explaining it is indirect and naming the helper —
    /// and the bound must be exactly one call hop: a *caller of the caller*
    /// two hops from the changed helper must not be reached this way.
    #[test]
    fn indirect_call_finds_the_mapped_caller_of_a_changed_private_helper() {
        let helper_key = StableKey::new("helper").expect("helper key");
        let caller_key = StableKey::new("caller").expect("caller key");
        let grandparent_key = StableKey::new("grandparent").expect("grandparent key");
        let requirement_key = StableKey::new("requirement").expect("requirement key");
        let decision_key = StableKey::new("decision").expect("decision key");

        let helper = code_node(
            &helper_key,
            "billing.subscription.SubscriptionService._entitlement_status",
            SymbolKind::Method,
            "src/billing/subscription.py",
        );
        let caller = code_node(
            &caller_key,
            "billing.subscription.SubscriptionService.cancel",
            SymbolKind::Method,
            "src/billing/subscription.py",
        );
        let grandparent = code_node(
            &grandparent_key,
            "billing.subscription.StripeWebhookHandler.handle_subscription_update",
            SymbolKind::Method,
            "src/billing/subscription.py",
        );
        let requirement = intent_node(&requirement_key, NodeKind::Requirement, "REQ-SUB-014");
        let decision = intent_node(&decision_key, NodeKind::Decision, "ADR-SUB-001");
        let nodes = [helper, caller, grandparent, requirement, decision]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect();
        let edges = vec![
            semantic_edge(&caller_key, &helper_key, RelationKind::Calls, false),
            semantic_edge(&grandparent_key, &caller_key, RelationKind::Calls, false),
            semantic_edge(
                &caller_key,
                &requirement_key,
                RelationKind::Implements,
                true,
            ),
            semantic_edge(
                &grandparent_key,
                &decision_key,
                RelationKind::Implements,
                true,
            ),
        ];

        let (before_helper, after_helper) = entitlement_helper_symbols();
        let input = ReviewInput {
            base: "HEAD".to_owned(),
            changes: vec![FileChange::Modified {
                path: "src/billing/subscription.py".to_owned(),
            }],
            before: BTreeMap::from([(
                "src/billing/subscription.py".to_owned(),
                analysis("src/billing/subscription.py", before_helper),
            )]),
            after: BTreeMap::from([(
                "src/billing/subscription.py".to_owned(),
                analysis("src/billing/subscription.py", after_helper),
            )]),
            changed_context_files: BTreeSet::new(),
            verbose: false,
        };

        let report = build_review_findings(&GraphSnapshot { nodes, edges }, &input);

        assert_eq!(report.changed_entities.len(), 1);
        assert_eq!(
            report.changed_entities[0].change_kind,
            ChangeKind::BehaviorPotentiallyChanged
        );
        assert_eq!(report.findings.len(), 1);
        let finding = &report.findings[0];
        assert_eq!(finding.affected_intent.identifier, "REQ-SUB-014");
        assert_eq!(
            finding.changed_entity,
            "billing.subscription.SubscriptionService.cancel"
        );
        assert!(
            finding
                .uncertainty
                .as_deref()
                .is_some_and(|text| text.contains("_entitlement_status"))
        );
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.affected_intent.identifier == "ADR-SUB-001")
        );
    }

    fn entitlement_helper_symbols() -> (SymbolDefinition, SymbolDefinition) {
        let before = SymbolDefinition {
            name: "_entitlement_status".to_owned(),
            canonical_path: "billing.subscription.SubscriptionService._entitlement_status"
                .to_owned(),
            kind: SymbolKind::Method,
            range: source_range(),
            signature: Some("(self, subscription, now)".to_owned()),
            body_hash: "old-body".to_owned(),
            structural_fingerprint: "old-shape".to_owned(),
            calls: Vec::new(),
            database_accesses: Vec::new(),
            schema_tables: Vec::new(),
            api_endpoints: Vec::new(),
            external_calls: Vec::new(),
        };
        let after = SymbolDefinition {
            body_hash: "new-body".to_owned(),
            structural_fingerprint: "new-shape".to_owned(),
            ..before.clone()
        };
        (before, after)
    }

    /// When the caller already earns its own direct finding for an intent
    /// (its own body changed too), the weaker indirect call-graph signal for
    /// the same intent must not double it.
    #[test]
    fn indirect_call_signal_does_not_duplicate_a_caller_s_own_direct_finding() {
        let helper_key = StableKey::new("helper").expect("helper key");
        let caller_key = StableKey::new("caller").expect("caller key");
        let requirement_key = StableKey::new("requirement").expect("requirement key");

        let helper = code_node(
            &helper_key,
            "billing.subscription.SubscriptionService._entitlement_status",
            SymbolKind::Method,
            "src/billing/subscription.py",
        );
        let caller = code_node(
            &caller_key,
            "billing.subscription.SubscriptionService.cancel",
            SymbolKind::Method,
            "src/billing/subscription.py",
        );
        let requirement = intent_node(&requirement_key, NodeKind::Requirement, "REQ-SUB-014");
        let nodes = [helper, caller, requirement]
            .into_iter()
            .map(|node| (node.stable_key.clone(), node))
            .collect();
        let edges = vec![
            semantic_edge(&caller_key, &helper_key, RelationKind::Calls, false),
            semantic_edge(
                &caller_key,
                &requirement_key,
                RelationKind::Implements,
                true,
            ),
        ];

        let (before_helper, after_helper) = entitlement_helper_symbols();
        let before_caller = SymbolDefinition {
            canonical_path: "billing.subscription.SubscriptionService.cancel".to_owned(),
            ..symbol("caller-old-body", "caller-old-shape", "(value)")
        };
        let after_caller = SymbolDefinition {
            body_hash: "caller-new-body".to_owned(),
            structural_fingerprint: "caller-new-shape".to_owned(),
            ..before_caller.clone()
        };
        let input = ReviewInput {
            base: "HEAD".to_owned(),
            changes: vec![FileChange::Modified {
                path: "src/billing/subscription.py".to_owned(),
            }],
            before: BTreeMap::from([(
                "src/billing/subscription.py".to_owned(),
                FileAnalysis {
                    path: "src/billing/subscription.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v1".to_owned(),
                    content_hash: "before".to_owned(),
                    symbols: vec![before_helper, before_caller],
                },
            )]),
            after: BTreeMap::from([(
                "src/billing/subscription.py".to_owned(),
                FileAnalysis {
                    path: "src/billing/subscription.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v1".to_owned(),
                    content_hash: "after".to_owned(),
                    symbols: vec![after_helper, after_caller],
                },
            )]),
            changed_context_files: BTreeSet::new(),
            verbose: false,
        };

        let report = build_review_findings(&GraphSnapshot { nodes, edges }, &input);

        assert_eq!(report.changed_entities.len(), 2);
        let matching = report
            .findings
            .iter()
            .filter(|finding| finding.affected_intent.identifier == "REQ-SUB-014")
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert!(matching[0].uncertainty.is_none());
    }

    /// Documents (and guards) the gap explained on `classify_behavior_change`:
    /// no pair `pair_symbols` actually assembles into a `ChangedEntity` can
    /// classify as `RefactorLikely`, across every boundary its own matching
    /// and entity-creation gate distinguish. If this starts failing, either
    /// the gate changed or a real distinguishing signal was added — either
    /// way, `RefactorLikely`'s "silently suppressed" review treatment needs a
    /// fresh look before this assertion is updated.
    #[test]
    fn refactor_likely_is_unreachable_through_pair_symbols() {
        let empty_graph = GraphSnapshot {
            nodes: BTreeMap::new(),
            edges: Vec::new(),
        };
        let run = |before: SymbolDefinition, after: SymbolDefinition| {
            let mut entities = Vec::new();
            pair_symbols(
                &empty_graph,
                AnalyzedSymbols {
                    path: Some("a.py"),
                    language: Some("python"),
                    symbols: &[before],
                },
                AnalyzedSymbols {
                    path: Some("a.py"),
                    language: Some("python"),
                    symbols: &[after],
                },
                &mut entities,
            );
            entities
        };
        let base = symbol("body-a", "shape-a", "(value)");

        let real_change = run(
            base.clone(),
            SymbolDefinition {
                body_hash: "body-b".to_owned(),
                structural_fingerprint: "shape-b".to_owned(),
                ..base.clone()
            },
        );
        assert_eq!(real_change.len(), 1);
        assert_ne!(real_change[0].change_kind, ChangeKind::RefactorLikely);

        let formatting_only = run(
            base.clone(),
            SymbolDefinition {
                body_hash: "body-b".to_owned(),
                ..base.clone()
            },
        );
        assert_eq!(formatting_only.len(), 1);
        assert_eq!(formatting_only[0].change_kind, ChangeKind::FormattingOnly);

        let moved = run(
            base.clone(),
            SymbolDefinition {
                canonical_path: "billing.cancel_v2".to_owned(),
                ..base.clone()
            },
        );
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].change_kind, ChangeKind::Rename);

        let contract_changed = run(
            base.clone(),
            SymbolDefinition {
                signature: Some("(value, force=False)".to_owned()),
                ..base.clone()
            },
        );
        assert_eq!(contract_changed.len(), 1);
        assert_eq!(contract_changed[0].change_kind, ChangeKind::ContractChanged);
    }

    fn symbol(body_hash: &str, fingerprint: &str, signature: &str) -> SymbolDefinition {
        SymbolDefinition {
            name: "cancel".to_owned(),
            canonical_path: "billing.cancel".to_owned(),
            kind: SymbolKind::Method,
            range: source_range(),
            signature: Some(signature.to_owned()),
            body_hash: body_hash.to_owned(),
            structural_fingerprint: fingerprint.to_owned(),
            calls: vec![CallSite {
                callee: "persist".to_owned(),
                range: source_range(),
            }],
            database_accesses: Vec::new(),
            schema_tables: Vec::new(),
            api_endpoints: Vec::new(),
            external_calls: Vec::new(),
        }
    }

    fn analysis(path: &str, symbol: SymbolDefinition) -> FileAnalysis {
        FileAnalysis {
            path: path.to_owned(),
            language: "python".to_owned(),
            analysis_version: "python-tree-sitter-v1".to_owned(),
            content_hash: symbol.body_hash.clone(),
            symbols: vec![symbol],
        }
    }

    fn source_range() -> SourceRange {
        SourceRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            end_line: 2,
        }
    }

    fn code_node(key: &StableKey, canonical: &str, kind: SymbolKind, file_path: &str) -> GraphNode {
        GraphNode {
            stable_key: key.clone(),
            kind: NodeKind::CodeSymbol,
            name: canonical.rsplit('.').next().unwrap_or(canonical).to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: file_path.to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind: kind,
                range: source_range(),
                signature: None,
                structural_fingerprint: "shape".to_owned(),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            },
        }
    }

    fn intent_node(key: &StableKey, kind: NodeKind, id: &str) -> GraphNode {
        GraphNode {
            stable_key: key.clone(),
            kind,
            name: "Keep paid access".to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: id.to_owned(),
                status: "active".to_owned(),
                visibility: crate::business::Visibility::Private,
                body: "Keep paid access".to_owned(),
                feature: None,
                source_uri: "requirement.yaml".to_owned(),
            },
        }
    }

    fn semantic_edge(
        source: &StableKey,
        target: &StableKey,
        kind: RelationKind,
        with_evidence: bool,
    ) -> GraphEdge {
        GraphEdge {
            source: source.clone(),
            target: target.clone(),
            kind,
            claim_class: ClaimClass::Assertion,
            source_kind: SourceKind::Documentation,
            confidence: Confidence::CERTAIN,
            status: ClaimStatus::Active,
            valid_from: "commit".to_owned(),
            valid_to: None,
            producer: "explicit".to_owned(),
            fingerprint: format!("{source}:{kind:?}:{target}"),
            stale_reason: None,
            evidence: with_evidence
                .then(|| GraphEvidence {
                    source_kind: SourceKind::Documentation,
                    source_uri: "requirement.yaml".to_owned(),
                    commit: Some("commit".to_owned()),
                    author: None,
                    timestamp: "now".to_owned(),
                    locator: "implementation[0]".to_owned(),
                    strength: Confidence::CERTAIN,
                })
                .into_iter()
                .collect(),
        }
    }
}
