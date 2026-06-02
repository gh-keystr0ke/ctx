use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    domain::{ClaimClass, ClaimStatus, Confidence, NodeKind, RelationKind, SourceKind, StableKey},
    ir::{FileAnalysis, SourceRange, SymbolDefinition, SymbolKind},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexedSymbol {
    pub stable_key: StableKey,
    pub file_path: String,
    pub name: String,
    pub canonical_path: String,
    pub kind: SymbolKind,
    pub range: SourceRange,
    pub signature: Option<String>,
    pub body_hash: String,
    pub structural_fingerprint: String,
    pub calls: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexedFile {
    pub stable_key: StableKey,
    pub path: String,
    pub language: String,
    pub content_hash: String,
    pub symbols: Vec<IndexedSymbol>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepositorySnapshot {
    pub files: BTreeMap<String, IndexedFile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileChange {
    Added { path: String },
    Modified { path: String },
    Deleted { path: String },
    Renamed { old_path: String, new_path: String },
}

impl FileChange {
    pub fn current_path(&self) -> Option<&str> {
        match self {
            Self::Added { path } | Self::Modified { path } => Some(path),
            Self::Deleted { .. } => None,
            Self::Renamed { new_path, .. } => Some(new_path),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeMutationKind {
    Create,
    Version,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlannedNodeAttributes {
    File {
        path: String,
        language: String,
    },
    Symbol {
        file_path: String,
        canonical_path: String,
        symbol_kind: SymbolKind,
        range: SourceRange,
        signature: Option<String>,
        structural_fingerprint: String,
        calls: Vec<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlannedNode {
    pub stable_key: StableKey,
    pub kind: NodeKind,
    pub name: String,
    pub content_hash: String,
    pub attributes: PlannedNodeAttributes,
    pub mutation: NodeMutationKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlannedEdge {
    pub source: StableKey,
    pub target: StableKey,
    pub kind: RelationKind,
    pub claim_class: ClaimClass,
    pub source_kind: SourceKind,
    pub confidence: Confidence,
    pub status: ClaimStatus,
    pub producer: String,
    pub fingerprint: String,
    pub source_uri: String,
    pub input_fingerprint: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexStats {
    pub files_reparsed: usize,
    pub nodes_created: usize,
    pub nodes_versioned: usize,
    pub nodes_retired: usize,
    pub edges_recomputed: usize,
    pub semantic_links_marked_stale: usize,
}

/// A deterministic set of storage effects for one repository transition.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexPlan {
    pub nodes_to_write: Vec<PlannedNode>,
    pub nodes_to_retire: Vec<StableKey>,
    pub structural_sources_to_close: Vec<String>,
    pub edges_to_create: Vec<PlannedEdge>,
    pub semantic_sources_to_mark_stale: Vec<StableKey>,
    pub stats: IndexStats,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum IndexPlanError {
    #[error("analysis for changed source file '{0}' is missing")]
    MissingAnalysis(String),
    #[error("invalid generated stable key: {0}")]
    InvalidStableKey(String),
}

/// Calculates all persistence effects for changed files without performing IO.
///
/// Symbol matching deliberately uses a conservative sequence: canonical path,
/// name/signature within the prior file, then a unique structural fingerprint.
/// Ambiguous matches create a new identity instead of silently conflating code.
///
/// # Errors
///
/// Returns [`IndexPlanError`] if a non-deleted change lacks an analysis or a
/// generated key violates domain constraints.
pub fn plan_incremental_index(
    snapshot: &RepositorySnapshot,
    analyses: &BTreeMap<String, FileAnalysis>,
    changes: &[FileChange],
) -> Result<IndexPlan, IndexPlanError> {
    let mut plan = IndexPlan::default();
    let mut retired = BTreeSet::new();
    let mut current_symbols = all_symbols(snapshot);

    for change in changes {
        let replaced_path = match change {
            FileChange::Added { .. } => None,
            FileChange::Modified { path } | FileChange::Deleted { path } => Some(path.as_str()),
            FileChange::Renamed { old_path, .. } => Some(old_path.as_str()),
        };
        if let Some(path) = replaced_path {
            plan.structural_sources_to_close.push(path.to_owned());
        }

        if let FileChange::Deleted { path } = change {
            retire_file(snapshot, path, &mut plan, &mut retired);
            continue;
        }
        plan_changed_file(
            snapshot,
            analyses,
            change,
            &mut plan,
            &mut retired,
            &mut current_symbols,
        )?;
    }

    current_symbols.retain(|symbol| !retired.contains(&symbol.stable_key));
    add_resolved_calls(&mut plan, &current_symbols);
    normalize_plan(&mut plan);
    Ok(plan)
}

fn plan_changed_file(
    snapshot: &RepositorySnapshot,
    analyses: &BTreeMap<String, FileAnalysis>,
    change: &FileChange,
    plan: &mut IndexPlan,
    retired: &mut BTreeSet<StableKey>,
    current_symbols: &mut Vec<IndexedSymbol>,
) -> Result<(), IndexPlanError> {
    let Some(path) = change.current_path() else {
        return Ok(());
    };
    let file_analysis = analyses
        .get(path)
        .ok_or_else(|| IndexPlanError::MissingAnalysis(path.to_owned()))?;
    let prior_path = match change {
        FileChange::Renamed { old_path, .. } => old_path.as_str(),
        _ => path,
    };
    let file_key = file_key(path)?;
    plan.nodes_to_write.push(PlannedNode {
        stable_key: file_key.clone(),
        kind: NodeKind::File,
        name: path.to_owned(),
        content_hash: file_analysis.content_hash.clone(),
        attributes: PlannedNodeAttributes::File {
            path: path.to_owned(),
            language: file_analysis.language.clone(),
        },
        mutation: if snapshot.files.contains_key(path) {
            NodeMutationKind::Version
        } else {
            NodeMutationKind::Create
        },
    });

    let prior_symbols = snapshot
        .files
        .get(prior_path)
        .map_or(&[][..], |file| file.symbols.as_slice());
    let matched = match_symbols(&file_analysis.symbols, prior_symbols, current_symbols)?;
    let matched_keys: BTreeSet<_> = matched.iter().map(|(_, key)| key.clone()).collect();
    for prior in prior_symbols {
        if !matched_keys.contains(&prior.stable_key) {
            retire_symbol(prior, plan, retired);
        }
    }
    if prior_path != path {
        retire_file_identity(snapshot, prior_path, plan, retired);
    }
    for (definition, stable_key) in matched {
        write_symbol(
            &definition,
            &stable_key,
            &file_key,
            path,
            file_analysis,
            current_symbols,
            plan,
        );
    }
    Ok(())
}

fn write_symbol(
    definition: &SymbolDefinition,
    stable_key: &StableKey,
    file_key: &StableKey,
    path: &str,
    file_analysis: &FileAnalysis,
    current_symbols: &mut Vec<IndexedSymbol>,
    plan: &mut IndexPlan,
) {
    let prior = current_symbols
        .iter()
        .find(|symbol| &symbol.stable_key == stable_key);
    let mutation = if prior.is_some() {
        NodeMutationKind::Version
    } else {
        NodeMutationKind::Create
    };
    if prior.is_some_and(|symbol| symbol.body_hash != definition.body_hash) {
        plan.semantic_sources_to_mark_stale.push(stable_key.clone());
    }
    let calls = definition
        .calls
        .iter()
        .map(|call| call.callee.clone())
        .collect::<Vec<_>>();
    plan.nodes_to_write.push(PlannedNode {
        stable_key: stable_key.clone(),
        kind: NodeKind::CodeSymbol,
        name: definition.name.clone(),
        content_hash: definition.body_hash.clone(),
        attributes: PlannedNodeAttributes::Symbol {
            file_path: path.to_owned(),
            canonical_path: definition.canonical_path.clone(),
            symbol_kind: definition.kind,
            range: definition.range,
            signature: definition.signature.clone(),
            structural_fingerprint: definition.structural_fingerprint.clone(),
            calls: calls.clone(),
        },
        mutation,
    });
    current_symbols.retain(|symbol| &symbol.stable_key != stable_key);
    current_symbols.push(IndexedSymbol {
        stable_key: stable_key.clone(),
        file_path: path.to_owned(),
        name: definition.name.clone(),
        canonical_path: definition.canonical_path.clone(),
        kind: definition.kind,
        range: definition.range,
        signature: definition.signature.clone(),
        body_hash: definition.body_hash.clone(),
        structural_fingerprint: definition.structural_fingerprint.clone(),
        calls,
    });
    plan.edges_to_create.push(structural_edge(
        file_key,
        stable_key,
        RelationKind::Contains,
        path,
        &file_analysis.content_hash,
    ));
}

fn match_symbols(
    definitions: &[SymbolDefinition],
    prior_file: &[IndexedSymbol],
    all_prior: &[IndexedSymbol],
) -> Result<Vec<(SymbolDefinition, StableKey)>, IndexPlanError> {
    let mut used = BTreeSet::new();
    definitions
        .iter()
        .map(|definition| {
            let candidates = [
                unique_match(all_prior, |symbol| {
                    symbol.canonical_path == definition.canonical_path
                        && symbol.kind == definition.kind
                }),
                unique_match(prior_file, |symbol| {
                    symbol.name == definition.name
                        && symbol.kind == definition.kind
                        && symbol.signature == definition.signature
                }),
                unique_match(all_prior, |symbol| {
                    symbol.kind == definition.kind
                        && symbol.structural_fingerprint == definition.structural_fingerprint
                }),
            ];
            let existing = candidates
                .into_iter()
                .flatten()
                .find(|key| !used.contains(*key));
            let stable_key = existing
                .cloned()
                .map_or_else(|| symbol_key(definition), Ok)?;
            used.insert(stable_key.clone());
            Ok((definition.clone(), stable_key))
        })
        .collect()
}

fn unique_match<F>(symbols: &[IndexedSymbol], predicate: F) -> Option<&StableKey>
where
    F: Fn(&IndexedSymbol) -> bool,
{
    let mut matches = symbols.iter().filter(|symbol| predicate(symbol));
    let first = matches.next()?;
    matches.next().is_none().then_some(&first.stable_key)
}

fn retire_file(
    snapshot: &RepositorySnapshot,
    path: &str,
    plan: &mut IndexPlan,
    retired: &mut BTreeSet<StableKey>,
) {
    retire_file_identity(snapshot, path, plan, retired);
    let Some(file) = snapshot.files.get(path) else {
        return;
    };
    for symbol in &file.symbols {
        retire_symbol(symbol, plan, retired);
    }
}

fn retire_file_identity(
    snapshot: &RepositorySnapshot,
    path: &str,
    plan: &mut IndexPlan,
    retired: &mut BTreeSet<StableKey>,
) {
    let Some(file) = snapshot.files.get(path) else {
        return;
    };
    if retired.insert(file.stable_key.clone()) {
        plan.nodes_to_retire.push(file.stable_key.clone());
    }
}

fn retire_symbol(symbol: &IndexedSymbol, plan: &mut IndexPlan, retired: &mut BTreeSet<StableKey>) {
    if retired.insert(symbol.stable_key.clone()) {
        plan.nodes_to_retire.push(symbol.stable_key.clone());
        plan.semantic_sources_to_mark_stale
            .push(symbol.stable_key.clone());
    }
}

fn all_symbols(snapshot: &RepositorySnapshot) -> Vec<IndexedSymbol> {
    snapshot
        .files
        .values()
        .flat_map(|file| file.symbols.iter().cloned())
        .collect()
}

fn file_key(path: &str) -> Result<StableKey, IndexPlanError> {
    StableKey::new(format!("file:{path}"))
        .map_err(|error| IndexPlanError::InvalidStableKey(error.to_string()))
}

fn symbol_key(definition: &SymbolDefinition) -> Result<StableKey, IndexPlanError> {
    StableKey::new(format!(
        "symbol:python:{}:{:?}",
        definition.canonical_path, definition.kind
    ))
    .map_err(|error| IndexPlanError::InvalidStableKey(error.to_string()))
}

fn structural_edge(
    source: &StableKey,
    target: &StableKey,
    kind: RelationKind,
    source_uri: &str,
    input_fingerprint: &str,
) -> PlannedEdge {
    PlannedEdge {
        source: source.clone(),
        target: target.clone(),
        kind,
        claim_class: ClaimClass::Fact,
        source_kind: SourceKind::StaticAnalysis,
        confidence: Confidence::CERTAIN,
        status: ClaimStatus::Active,
        producer: "python_tree_sitter".to_owned(),
        fingerprint: format!("{}:{kind:?}:{}", source.as_str(), target.as_str()),
        source_uri: source_uri.to_owned(),
        input_fingerprint: input_fingerprint.to_owned(),
    }
}

fn add_resolved_calls(plan: &mut IndexPlan, symbols: &[IndexedSymbol]) {
    let mut names: BTreeMap<&str, Vec<&IndexedSymbol>> = BTreeMap::new();
    for symbol in symbols {
        names.entry(&symbol.name).or_default().push(symbol);
    }
    let changed: BTreeSet<_> = plan
        .nodes_to_write
        .iter()
        .filter(|node| node.kind == NodeKind::CodeSymbol)
        .map(|node| node.stable_key.clone())
        .collect();
    for caller in symbols
        .iter()
        .filter(|symbol| changed.contains(&symbol.stable_key))
    {
        for callee_name in &caller.calls {
            let Some(candidates) = names.get(callee_name.as_str()) else {
                continue;
            };
            if let [callee] = candidates.as_slice() {
                plan.edges_to_create.push(structural_edge(
                    &caller.stable_key,
                    &callee.stable_key,
                    RelationKind::Calls,
                    &caller.file_path,
                    &caller.body_hash,
                ));
            }
        }
    }
}

fn normalize_plan(plan: &mut IndexPlan) {
    plan.nodes_to_write
        .sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    plan.nodes_to_retire.sort();
    plan.nodes_to_retire.dedup();
    plan.structural_sources_to_close.sort();
    plan.structural_sources_to_close.dedup();
    plan.edges_to_create
        .sort_by(|left, right| left.fingerprint.cmp(&right.fingerprint));
    plan.edges_to_create
        .dedup_by(|left, right| left.fingerprint == right.fingerprint);
    plan.semantic_sources_to_mark_stale.sort();
    plan.semantic_sources_to_mark_stale.dedup();
    plan.stats = IndexStats {
        files_reparsed: plan
            .nodes_to_write
            .iter()
            .filter(|node| node.kind == NodeKind::File)
            .count(),
        nodes_created: plan
            .nodes_to_write
            .iter()
            .filter(|node| node.mutation == NodeMutationKind::Create)
            .count(),
        nodes_versioned: plan
            .nodes_to_write
            .iter()
            .filter(|node| node.mutation == NodeMutationKind::Version)
            .count(),
        nodes_retired: plan.nodes_to_retire.len(),
        edges_recomputed: plan.edges_to_create.len(),
        semantic_links_marked_stale: plan.semantic_sources_to_mark_stale.len(),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::CallSite;

    fn range() -> SourceRange {
        SourceRange {
            start_byte: 0,
            end_byte: 20,
            start_line: 1,
            end_line: 2,
        }
    }

    fn definition(name: &str, canonical: &str, body: &str, fingerprint: &str) -> SymbolDefinition {
        SymbolDefinition {
            name: name.to_owned(),
            canonical_path: canonical.to_owned(),
            kind: SymbolKind::Function,
            range: range(),
            signature: Some("()".to_owned()),
            body_hash: body.to_owned(),
            structural_fingerprint: fingerprint.to_owned(),
            calls: Vec::new(),
        }
    }

    fn indexed_file(path: &str, definition: &SymbolDefinition) -> IndexedFile {
        IndexedFile {
            stable_key: file_key(path).expect("file key"),
            path: path.to_owned(),
            language: "python".to_owned(),
            content_hash: "old-file".to_owned(),
            symbols: vec![IndexedSymbol {
                stable_key: symbol_key(definition).expect("symbol key"),
                file_path: path.to_owned(),
                name: definition.name.clone(),
                canonical_path: definition.canonical_path.clone(),
                kind: definition.kind,
                range: definition.range,
                signature: definition.signature.clone(),
                body_hash: definition.body_hash.clone(),
                structural_fingerprint: definition.structural_fingerprint.clone(),
                calls: Vec::new(),
            }],
        }
    }

    #[test]
    fn changed_body_versions_symbol_and_marks_semantics_stale() {
        let old = definition("cancel", "billing.cancel", "body-v1", "shape-v1");
        let mut snapshot = RepositorySnapshot::default();
        snapshot
            .files
            .insert("billing.py".to_owned(), indexed_file("billing.py", &old));
        let new = definition("cancel", "billing.cancel", "body-v2", "shape-v2");
        let analyses = BTreeMap::from([(
            "billing.py".to_owned(),
            FileAnalysis {
                path: "billing.py".to_owned(),
                language: "python".to_owned(),
                content_hash: "new-file".to_owned(),
                symbols: vec![new],
            },
        )]);

        let plan = plan_incremental_index(
            &snapshot,
            &analyses,
            &[FileChange::Modified {
                path: "billing.py".to_owned(),
            }],
        )
        .expect("plan");

        assert_eq!(plan.stats.files_reparsed, 1);
        assert_eq!(plan.stats.nodes_versioned, 2);
        assert_eq!(plan.stats.semantic_links_marked_stale, 1);
        assert!(plan.nodes_to_retire.is_empty());
    }

    #[test]
    fn file_rename_preserves_symbol_identity_by_structure() {
        let old = definition("cancel", "old.cancel", "same-body", "same-shape");
        let old_file = indexed_file("old.py", &old);
        let old_symbol_key = old_file.symbols[0].stable_key.clone();
        let snapshot = RepositorySnapshot {
            files: BTreeMap::from([("old.py".to_owned(), old_file)]),
        };
        let new = definition("cancel", "new.cancel", "same-body", "same-shape");
        let analyses = BTreeMap::from([(
            "new.py".to_owned(),
            FileAnalysis {
                path: "new.py".to_owned(),
                language: "python".to_owned(),
                content_hash: "new-file".to_owned(),
                symbols: vec![new],
            },
        )]);

        let plan = plan_incremental_index(
            &snapshot,
            &analyses,
            &[FileChange::Renamed {
                old_path: "old.py".to_owned(),
                new_path: "new.py".to_owned(),
            }],
        )
        .expect("plan");

        assert!(plan.nodes_to_write.iter().any(|node| {
            node.stable_key == old_symbol_key && node.mutation == NodeMutationKind::Version
        }));
        assert!(!plan.nodes_to_retire.contains(&old_symbol_key));
    }

    #[test]
    fn unique_calls_create_deterministic_fact_edges() {
        let mut caller = definition("cancel", "billing.cancel", "caller-body", "caller-shape");
        caller.calls.push(CallSite {
            callee: "revoke".to_owned(),
            range: range(),
        });
        let target_definition =
            definition("revoke", "billing.revoke", "callee-body", "callee-shape");
        let analyses = BTreeMap::from([(
            "billing.py".to_owned(),
            FileAnalysis {
                path: "billing.py".to_owned(),
                language: "python".to_owned(),
                content_hash: "file".to_owned(),
                symbols: vec![caller, target_definition],
            },
        )]);

        let plan = plan_incremental_index(
            &RepositorySnapshot::default(),
            &analyses,
            &[FileChange::Added {
                path: "billing.py".to_owned(),
            }],
        )
        .expect("plan");

        assert_eq!(
            plan.edges_to_create
                .iter()
                .filter(|edge| edge.kind == RelationKind::Calls)
                .count(),
            1
        );
    }
}
