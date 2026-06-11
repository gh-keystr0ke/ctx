use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    domain::{ClaimStatus, NodeKind, RelationKind, StableKey},
    graph::{GraphEdge, GraphNode, GraphSnapshot, NodeSummary},
    indexing::{FileChange, PlannedNodeAttributes},
    ir::{FileAnalysis, SymbolDefinition},
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReviewReport {
    pub base: String,
    pub changed_entities: Vec<ChangedEntity>,
    pub findings: Vec<ReviewFinding>,
    pub stale_relationships: Vec<String>,
    pub suppressed_non_behavioral_changes: usize,
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
    let mut findings = Vec::new();
    let mut stale_relationships = Vec::new();
    let mut suppressed = 0;
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
                entity,
                edge,
                confidence,
                &changed_paths,
                input.changed_context_files.is_empty(),
            ) {
                findings.push(finding);
            }
        }
    }
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
        stale_relationships,
        suppressed_non_behavioral_changes: suppressed,
    }
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
    entities.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.after.cmp(&right.after))
            .then_with(|| left.before.cmp(&right.before))
    });
    entities
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
    (ChangeKind::BehaviorPotentiallyChanged, signals)
}

fn call_names(symbol: &SymbolDefinition) -> BTreeSet<&str> {
    symbol
        .calls
        .iter()
        .map(|call| call.callee.as_str())
        .collect()
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
    entity: &ChangedEntity,
    edge: &GraphEdge,
    confidence: f32,
    changed_paths: &BTreeSet<String>,
    context_unchanged: bool,
) -> Option<ReviewFinding> {
    let intent = graph.nodes.get(&edge.target)?;
    let related_tests = related_tests(graph, &intent.stable_key);
    let tests_modified = related_tests.iter().any(|test| {
        graph
            .nodes
            .values()
            .find(|node| node.stable_key.as_str() == test.stable_key)
            .is_some_and(|node| node_file_changed(node, changed_paths))
    });
    let changed_entity = entity
        .after
        .as_ref()
        .or(entity.before.as_ref())
        .cloned()
        .unwrap_or_else(|| entity.file_path.clone());
    let possible_requirement_drift =
        context_unchanged && matches!(intent.kind, NodeKind::Requirement | NodeKind::Invariant);
    Some(ReviewFinding {
        severity: severity(intent.kind, entity.change_kind),
        confidence,
        changed_entity,
        change_kind: entity.change_kind,
        affected_intent: NodeSummary::from(intent),
        reason: format!(
            "The changed symbol has a {:?} {:?} claim from {:?}.",
            edge.claim_class, edge.kind, edge.source_kind
        ),
        evidence: edge
            .evidence
            .iter()
            .map(|item| format!("{}#{}", item.source_uri, item.locator))
            .collect(),
        related_tests,
        tests_modified,
        possible_requirement_drift,
        uncertainty: None,
        suggested_action: suggested_action(intent.kind, tests_modified).to_owned(),
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
        ir::{CallSite, SourceRange, SymbolKind},
    };

    use super::*;

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
