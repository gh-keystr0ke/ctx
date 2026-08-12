use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    domain::{ClaimClass, ClaimStatus, Confidence, NodeKind, RelationKind, SourceKind, StableKey},
    ir::{
        ApiEndpoint, DatabaseAccess, DatabaseAccessKind, ExternalCall, FileAnalysis,
        SchemaTableDefinition, SourceRange, SymbolDefinition, SymbolKind,
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexedSymbol {
    pub stable_key: StableKey,
    pub language: String,
    pub file_path: String,
    pub name: String,
    pub canonical_path: String,
    pub kind: SymbolKind,
    pub range: SourceRange,
    pub signature: Option<String>,
    pub body_hash: String,
    pub structural_fingerprint: String,
    pub calls: Vec<String>,
    #[serde(default)]
    pub database_accesses: Vec<DatabaseAccess>,
    #[serde(default)]
    pub schema_tables: Vec<SchemaTableDefinition>,
    #[serde(default)]
    pub api_endpoints: Vec<ApiEndpoint>,
    #[serde(default)]
    pub external_calls: Vec<ExternalCall>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexedFile {
    pub stable_key: StableKey,
    pub path: String,
    pub language: String,
    #[serde(default)]
    pub analysis_version: String,
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

/// Reconciles Git changes with the configured source boundary.
///
/// Git does not report source changes when a commit only edits configuration.
/// Comparing the current allowed paths with the stored snapshot turns boundary
/// crossings into ordinary additions and deletions for the incremental planner.
#[must_use]
pub fn reconcile_source_scope(
    changes: &[FileChange],
    indexed_paths: impl IntoIterator<Item = String>,
    current_paths: impl IntoIterator<Item = String>,
) -> Vec<FileChange> {
    let indexed = indexed_paths.into_iter().collect::<BTreeSet<_>>();
    let current = current_paths.into_iter().collect::<BTreeSet<_>>();
    let mut reconciled = changes.to_vec();
    let covered_previous = changes
        .iter()
        .filter_map(previous_path)
        .collect::<BTreeSet<_>>();
    let covered_current = changes
        .iter()
        .filter_map(FileChange::current_path)
        .collect::<BTreeSet<_>>();

    reconciled.extend(
        indexed
            .difference(&current)
            .filter(|path| !covered_previous.contains(path.as_str()))
            .map(|path| FileChange::Deleted { path: path.clone() }),
    );
    reconciled.extend(
        current
            .difference(&indexed)
            .filter(|path| !covered_current.contains(path.as_str()))
            .map(|path| FileChange::Added { path: path.clone() }),
    );
    reconciled.sort_by(|left, right| change_sort_key(left).cmp(&change_sort_key(right)));
    reconciled.dedup();
    reconciled
}

/// Adds deterministic reparses when the responsible analyzer's normalization
/// schema changed even though Git source bytes did not.
#[must_use]
pub fn reconcile_analysis_versions(
    changes: &[FileChange],
    snapshot: &RepositorySnapshot,
    expected_versions: &BTreeMap<String, String>,
) -> Vec<FileChange> {
    let mut reconciled = changes.to_vec();
    let covered_current = changes
        .iter()
        .filter_map(FileChange::current_path)
        .collect::<BTreeSet<_>>();
    reconciled.extend(expected_versions.iter().filter_map(|(path, expected)| {
        let indexed = snapshot.files.get(path)?;
        (indexed.analysis_version != *expected && !covered_current.contains(path.as_str()))
            .then(|| FileChange::Modified { path: path.clone() })
    }));
    reconciled.sort_by(|left, right| change_sort_key(left).cmp(&change_sort_key(right)));
    reconciled.dedup();
    reconciled
}

fn previous_path(change: &FileChange) -> Option<&str> {
    match change {
        FileChange::Modified { path } | FileChange::Deleted { path } => Some(path),
        FileChange::Renamed { old_path, .. } => Some(old_path),
        FileChange::Added { .. } => None,
    }
}

fn change_sort_key(change: &FileChange) -> (u8, &str, &str) {
    match change {
        FileChange::Added { path } => (0, path, ""),
        FileChange::Modified { path } => (1, path, ""),
        FileChange::Deleted { path } => (2, path, ""),
        FileChange::Renamed { old_path, new_path } => (3, old_path, new_path),
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
        #[serde(default)]
        analysis_version: String,
    },
    Symbol {
        file_path: String,
        canonical_path: String,
        symbol_kind: SymbolKind,
        range: SourceRange,
        signature: Option<String>,
        structural_fingerprint: String,
        calls: Vec<String>,
        #[serde(default)]
        database_accesses: Vec<DatabaseAccess>,
        #[serde(default)]
        schema_tables: Vec<SchemaTableDefinition>,
        #[serde(default)]
        api_endpoints: Vec<ApiEndpoint>,
        #[serde(default)]
        external_calls: Vec<ExternalCall>,
    },
    ApiEndpoint {
        endpoint: ApiEndpoint,
    },
    ExternalCall {
        call: ExternalCall,
    },
    Interaction {
        identifier: String,
    },
    Business {
        id: String,
        status: String,
        #[serde(default)]
        visibility: crate::business::Visibility,
        #[serde(default = "default_implementation_expected")]
        implementation_expected: bool,
        body: String,
        feature: Option<String>,
        source_uri: String,
    },
}

const fn default_implementation_expected() -> bool {
    true
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
    pub evidence_locator: Option<String>,
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
    #[error("index plan contains more than one write for stable key '{0}'")]
    DuplicateNodeWrite(String),
}

/// Calculates all persistence effects for changed files without performing IO.
///
/// Symbol matching deliberately uses a conservative sequence: canonical path,
/// name/signature within the prior file, then a unique name plus structural
/// fingerprint anywhere in the repository (a cross-file move). The name is
/// required even for the fingerprint tier: a repository can easily contain
/// two unrelated, differently named one-liners with an identical
/// whitespace-stripped body, and matching on shape alone would silently
/// merge them into one identity instead of treating the second as new.
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
    let historical_symbols = all_symbols(snapshot);
    let historical_database_entities = database_entities(&historical_symbols);
    let mut current_symbols = historical_symbols.clone();
    // Shared across every file in this transition: once one file's symbol
    // claims a prior identity (typically via its own unchanged canonical
    // path), that identity is spoken for and must not also be handed to an
    // unrelated symbol in a different file via the same-shape fallback.
    // Scoping this per file instead of per transition is exactly how two
    // trivial, differently-scoped one-line helpers with an identical
    // whitespace-stripped body ended up silently sharing one node identity.
    let mut used = BTreeSet::new();

    // Reserve every unchanged symbol's own exact-canonical-path identity
    // before any file's same-shape cross-file fallback (tier 3 in
    // `match_symbols`) runs. Without this pre-pass, processing order alone
    // decided outcomes: a new file whose helper happened to share another
    // *unprocessed* file's name and whitespace-stripped shape could steal
    // that file's still-unclaimed historical identity via the fallback: the
    // rightful owner, matched later by its own unchanged canonical path,
    // would then find its own key already `used` and fall through every
    // tier back to the very same canonical-path-derived key, producing two
    // writes for one stable key instead of one correct match plus one
    // correctly distinct new identity. Reserving first makes the outcome
    // independent of `changes`' order.
    for change in changes {
        let Some(path) = change.current_path() else {
            continue;
        };
        if let Some(file_analysis) = analyses.get(path) {
            reserve_exact_canonical_matches(
                &file_analysis.language,
                &file_analysis.symbols,
                &historical_symbols,
                &mut used,
            );
        }
    }

    // Tier 3 of `match_symbols` (the same-shape cross-file fallback) must
    // only be allowed to claim a historical identity that this transition is
    // actually vacating — i.e. a symbol belonging to a file being modified,
    // renamed, or deleted here. Without this restriction, a brand-new file
    // defining a symbol with the same name/kind/structural-fingerprint as
    // some *unrelated, unchanged* symbol elsewhere in the codebase (e.g. two
    // files independently declaring an identically-shaped private test
    // helper) would "steal" that unrelated symbol's stable key: the fallback
    // is meant to detect a genuine move, and there is no move when the old
    // location isn't changing at all.
    let changed_paths: BTreeSet<&str> = changes
        .iter()
        .filter_map(|change| replaced_path(snapshot, change))
        .collect();
    let same_shape_prior: Vec<IndexedSymbol> = historical_symbols
        .iter()
        .filter(|symbol| changed_paths.contains(symbol.file_path.as_str()))
        .cloned()
        .collect();
    let pools = SymbolMatchPools {
        historical_symbols: &historical_symbols,
        same_shape_prior: &same_shape_prior,
    };

    for change in changes {
        let replaced = replaced_path(snapshot, change);
        if let Some(path) = replaced {
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
            &mut RetirementLedger {
                plan: &mut plan,
                retired: &mut retired,
            },
            &pools,
            &mut current_symbols,
            &mut used,
        )?;
    }

    current_symbols.retain(|symbol| !retired.contains(&symbol.stable_key));
    add_resolved_calls(&mut plan, &current_symbols);
    add_database_facts(&mut plan, &historical_database_entities, &current_symbols)?;
    add_http_facts(&mut plan, &historical_symbols, &current_symbols)?;
    normalize_plan(&mut plan)?;
    Ok(plan)
}

/// The prior path whose structural identity `change` replaces, if any: the
/// file a `Modified`/`Deleted` change's own path names, an `Added` change's
/// path when it happens to reuse an already-known path, or a `Renamed`
/// change's `old_path`. Shared by the `structural_sources_to_close` pass and
/// the same-shape-fallback eligibility pass so both agree on exactly which
/// files this transition is vacating.
fn replaced_path<'a>(snapshot: &RepositorySnapshot, change: &'a FileChange) -> Option<&'a str> {
    match change {
        FileChange::Added { path } if snapshot.files.contains_key(path) => Some(path.as_str()),
        FileChange::Added { .. } => None,
        FileChange::Modified { path } | FileChange::Deleted { path } => Some(path.as_str()),
        FileChange::Renamed { old_path, .. } => Some(old_path.as_str()),
    }
}

/// The plan being assembled and the retired-key ledger that keeps
/// `retire_symbol`/`retire_file_identity` idempotent across one transition.
/// Grouped so the changed-file planning functions stay under the workspace's
/// argument-count lint without losing either accumulator's own identity.
struct RetirementLedger<'a> {
    plan: &'a mut IndexPlan,
    retired: &'a mut BTreeSet<StableKey>,
}

/// The two historical-symbol pools `match_symbols` draws candidates from:
/// `historical_symbols` (every symbol ever indexed, for the exact-canonical-
/// path tier) and `same_shape_prior` (only symbols belonging to a file this
/// transition is actually vacating, for the same-shape fallback tier — see
/// [`replaced_path`]). Grouped so the changed-file planning functions stay
/// under the workspace's argument-count lint without losing either pool's
/// own identity.
struct SymbolMatchPools<'a> {
    historical_symbols: &'a [IndexedSymbol],
    same_shape_prior: &'a [IndexedSymbol],
}

fn plan_changed_file(
    snapshot: &RepositorySnapshot,
    analyses: &BTreeMap<String, FileAnalysis>,
    change: &FileChange,
    ledger: &mut RetirementLedger<'_>,
    pools: &SymbolMatchPools<'_>,
    current_symbols: &mut Vec<IndexedSymbol>,
    used: &mut BTreeSet<StableKey>,
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
    ledger.plan.nodes_to_write.push(PlannedNode {
        stable_key: file_key.clone(),
        kind: NodeKind::File,
        name: path.to_owned(),
        content_hash: file_analysis.content_hash.clone(),
        attributes: PlannedNodeAttributes::File {
            path: path.to_owned(),
            language: file_analysis.language.clone(),
            analysis_version: file_analysis.analysis_version.clone(),
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
    let matched = match_symbols(
        &file_analysis.language,
        &file_analysis.symbols,
        prior_symbols,
        pools.historical_symbols,
        pools.same_shape_prior,
        used,
    )?;
    let matched_keys: BTreeSet<_> = matched.iter().map(|(_, key)| key.clone()).collect();
    for prior in prior_symbols {
        if !matched_keys.contains(&prior.stable_key) {
            retire_symbol(prior, ledger.plan, ledger.retired);
        }
    }
    if prior_path != path {
        retire_file_identity(snapshot, prior_path, ledger.plan, ledger.retired);
    }
    for (definition, stable_key) in matched {
        write_symbol(
            &definition,
            &stable_key,
            &file_key,
            path,
            file_analysis,
            current_symbols,
            ledger.plan,
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
            database_accesses: definition.database_accesses.clone(),
            schema_tables: definition.schema_tables.clone(),
            api_endpoints: definition.api_endpoints.clone(),
            external_calls: definition.external_calls.clone(),
        },
        mutation,
    });
    current_symbols.retain(|symbol| &symbol.stable_key != stable_key);
    current_symbols.push(IndexedSymbol {
        stable_key: stable_key.clone(),
        language: file_analysis.language.clone(),
        file_path: path.to_owned(),
        name: definition.name.clone(),
        canonical_path: definition.canonical_path.clone(),
        kind: definition.kind,
        range: definition.range,
        signature: definition.signature.clone(),
        body_hash: definition.body_hash.clone(),
        structural_fingerprint: definition.structural_fingerprint.clone(),
        calls,
        database_accesses: definition.database_accesses.clone(),
        schema_tables: definition.schema_tables.clone(),
        api_endpoints: definition.api_endpoints.clone(),
        external_calls: definition.external_calls.clone(),
    });
    plan.edges_to_create.push(structural_edge(
        file_key,
        stable_key,
        RelationKind::Contains,
        path,
        &file_analysis.content_hash,
        &file_analysis.language,
        None,
    ));
}

/// Claims every definition's exact-canonical-path historical identity, if it
/// uniquely exists, before any file's same-shape cross-file fallback can run.
/// See the call site in `plan_incremental_index` for why this must happen as
/// a pre-pass across the whole transition rather than per file.
fn reserve_exact_canonical_matches(
    language: &str,
    definitions: &[SymbolDefinition],
    all_prior: &[IndexedSymbol],
    used: &mut BTreeSet<StableKey>,
) {
    for definition in definitions {
        if let Some(key) = exact_canonical_match(all_prior, language, definition) {
            used.insert(key.clone());
        }
    }
}

fn exact_canonical_match<'a>(
    all_prior: &'a [IndexedSymbol],
    language: &str,
    definition: &SymbolDefinition,
) -> Option<&'a StableKey> {
    unique_match(all_prior, |symbol| {
        symbol.language == language
            && symbol.canonical_path == definition.canonical_path
            && symbol.kind == definition.kind
    })
}

fn match_symbols(
    language: &str,
    definitions: &[SymbolDefinition],
    prior_file: &[IndexedSymbol],
    all_prior: &[IndexedSymbol],
    same_shape_prior: &[IndexedSymbol],
    used: &mut BTreeSet<StableKey>,
) -> Result<Vec<(SymbolDefinition, StableKey)>, IndexPlanError> {
    definitions
        .iter()
        .map(|definition| {
            let candidates = [
                exact_canonical_match(all_prior, language, definition),
                unique_match(prior_file, |symbol| {
                    symbol.name == definition.name
                        && symbol.kind == definition.kind
                        && symbol.signature == definition.signature
                }),
                unique_match(same_shape_prior, |symbol| {
                    symbol.language == language
                        && symbol.kind == definition.kind
                        && symbol.name == definition.name
                        && symbol.structural_fingerprint == definition.structural_fingerprint
                }),
            ];
            // A candidate already in `used` isn't necessarily taken by
            // someone else: `reserve_exact_canonical_matches` pre-reserves
            // this very definition's own exact-canonical-path key before
            // `match_symbols` ever runs, purely to stop an unrelated
            // definition's looser same-shape fallback from stealing it. So
            // when every candidate is filtered out by `used`, recomputing
            // `symbol_key` (as `has_historical_match` guards below) safely
            // reclaims that reservation rather than treating it as a
            // collision, because the same deterministic formula reproduces
            // it exactly.
            let has_historical_match = candidates.iter().any(Option::is_some);
            let existing = candidates
                .into_iter()
                .flatten()
                .find(|key| !used.contains(*key));
            let stable_key = match existing {
                Some(key) => key.clone(),
                None if has_historical_match => symbol_key(language, definition)?,
                None => disambiguated_symbol_key(language, definition, used)?,
            };
            used.insert(stable_key.clone());
            Ok((definition.clone(), stable_key))
        })
        .collect()
}

/// Only reached when a definition has no historical grounding at all (every
/// candidate tier in `match_symbols` came back `None`), so `symbol_key`
/// alone is not guaranteed unique: two symbols that legitimately share a
/// canonical path — Rust `#[cfg(...)]`-gated platform variants of the same
/// function name, or Go's blank identifier `_` used for repeated
/// interface-conformance checks — can both be "new" in the same transition.
/// Appending a stable ordinal to every collision beyond the first keeps each
/// write distinct instead of `plan_incremental_index` producing two writes
/// for one key, and stays deterministic run-to-run because `definitions` is
/// already sorted by canonical path then source position before
/// `match_symbols` sees it, so colliding definitions are always visited in
/// the same order.
fn disambiguated_symbol_key(
    language: &str,
    definition: &SymbolDefinition,
    used: &BTreeSet<StableKey>,
) -> Result<StableKey, IndexPlanError> {
    let base = symbol_key(language, definition)?;
    if !used.contains(&base) {
        return Ok(base);
    }
    let mut ordinal = 2u32;
    loop {
        let candidate = StableKey::new(format!("{}#{ordinal}", base.as_str()))
            .map_err(|error| IndexPlanError::InvalidStableKey(error.to_string()))?;
        if !used.contains(&candidate) {
            return Ok(candidate);
        }
        ordinal += 1;
    }
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

fn symbol_key(language: &str, definition: &SymbolDefinition) -> Result<StableKey, IndexPlanError> {
    StableKey::new(format!(
        "symbol:{language}:{}:{:?}",
        definition.canonical_path, definition.kind,
    ))
    .map_err(|error| IndexPlanError::InvalidStableKey(error.to_string()))
}

fn structural_edge(
    source: &StableKey,
    target: &StableKey,
    kind: RelationKind,
    source_uri: &str,
    input_fingerprint: &str,
    language: &str,
    evidence_locator: Option<String>,
) -> PlannedEdge {
    PlannedEdge {
        source: source.clone(),
        target: target.clone(),
        kind,
        claim_class: ClaimClass::Fact,
        source_kind: SourceKind::StaticAnalysis,
        confidence: Confidence::CERTAIN,
        status: ClaimStatus::Active,
        producer: producer_name(language),
        fingerprint: format!("{}:{kind:?}:{}", source.as_str(), target.as_str()),
        source_uri: source_uri.to_owned(),
        input_fingerprint: input_fingerprint.to_owned(),
        evidence_locator,
    }
}

/// Names the deterministic extraction technique behind a structural fact.
/// Every Tree-sitter-backed module keeps the historical `<language>_tree_sitter`
/// form; goose migrations are recognized with a dedicated DDL-statement
/// reader rather than a grammar, so its provenance says so instead of
/// implying a parser that was never involved.
fn producer_name(language: &str) -> String {
    if language == "goose" {
        "goose_ddl_recognizer".to_owned()
    } else {
        format!("{language}_tree_sitter")
    }
}

fn add_resolved_calls(plan: &mut IndexPlan, symbols: &[IndexedSymbol]) {
    let mut names: BTreeMap<(&str, &str), Vec<&IndexedSymbol>> = BTreeMap::new();
    for symbol in symbols.iter().filter(|symbol| is_callable(symbol.kind)) {
        names
            .entry((&symbol.language, &symbol.name))
            .or_default()
            .push(symbol);
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
            let Some(candidates) = names.get(&(caller.language.as_str(), callee_name.as_str()))
            else {
                continue;
            };
            if let [callee] = candidates.as_slice() {
                plan.edges_to_create.push(structural_edge(
                    &caller.stable_key,
                    &callee.stable_key,
                    RelationKind::Calls,
                    &caller.file_path,
                    &caller.body_hash,
                    &caller.language,
                    None,
                ));
            }
        }
    }
}

fn database_entities(symbols: &[IndexedSymbol]) -> BTreeSet<String> {
    symbols
        .iter()
        .flat_map(|symbol| {
            let accesses = symbol.database_accesses.iter().map(|access| &access.entity);
            let schemas = symbol.schema_tables.iter().map(|table| &table.entity);
            accesses.chain(schemas).cloned()
        })
        .collect()
}

fn add_database_facts(
    plan: &mut IndexPlan,
    historical_entities: &BTreeSet<String>,
    symbols: &[IndexedSymbol],
) -> Result<(), IndexPlanError> {
    let current_entities = database_entities(symbols);
    for entity in current_entities.difference(historical_entities) {
        let stable_key = database_entity_key(entity)?;
        plan.nodes_to_write.push(PlannedNode {
            stable_key,
            kind: NodeKind::DbEntity,
            name: entity.clone(),
            content_hash: format!("db-entity:{entity}"),
            attributes: PlannedNodeAttributes::Interaction {
                identifier: entity.clone(),
            },
            mutation: NodeMutationKind::Create,
        });
    }
    for entity in historical_entities.difference(&current_entities) {
        plan.nodes_to_retire.push(database_entity_key(entity)?);
    }

    let changed = plan
        .nodes_to_write
        .iter()
        .filter(|node| node.kind == NodeKind::CodeSymbol)
        .map(|node| node.stable_key.clone())
        .collect::<BTreeSet<_>>();
    for symbol in symbols
        .iter()
        .filter(|symbol| changed.contains(&symbol.stable_key))
    {
        add_database_access_edges(plan, symbol)?;
        add_schema_definition_edges(plan, symbol)?;
    }
    Ok(())
}

fn add_database_access_edges(
    plan: &mut IndexPlan,
    symbol: &IndexedSymbol,
) -> Result<(), IndexPlanError> {
    let mut grouped = BTreeMap::<(DatabaseAccessKind, &str), Vec<&DatabaseAccess>>::new();
    for access in &symbol.database_accesses {
        grouped
            .entry((access.kind, access.entity.as_str()))
            .or_default()
            .push(access);
    }
    for ((kind, entity), accesses) in grouped {
        let target = database_entity_key(entity)?;
        let relation = match kind {
            DatabaseAccessKind::Read => RelationKind::ReadsFrom,
            DatabaseAccessKind::Write => RelationKind::WritesTo,
        };
        let mut statement_hashes = accesses
            .iter()
            .map(|access| access.statement_hash.as_str())
            .collect::<Vec<_>>();
        statement_hashes.sort_unstable();
        statement_hashes.dedup();
        let mut lines = accesses
            .iter()
            .map(|access| access.range.start_line)
            .collect::<Vec<_>>();
        lines.sort_unstable();
        lines.dedup();
        let mut columns = accesses
            .iter()
            .flat_map(|access| access.columns.iter().cloned())
            .collect::<Vec<_>>();
        columns.sort_unstable();
        columns.dedup();
        let locator = if columns.is_empty() {
            format!(
                "lines:{}",
                lines
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        } else {
            format!(
                "lines:{} columns:{}",
                lines
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                columns.join(",")
            )
        };
        plan.edges_to_create.push(structural_edge(
            &symbol.stable_key,
            &target,
            relation,
            &symbol.file_path,
            &statement_hashes.join(":"),
            &symbol.language,
            Some(locator),
        ));
    }
    Ok(())
}

fn add_schema_definition_edges(
    plan: &mut IndexPlan,
    symbol: &IndexedSymbol,
) -> Result<(), IndexPlanError> {
    let mut tables_by_entity = BTreeMap::<&str, Vec<&SchemaTableDefinition>>::new();
    for table in &symbol.schema_tables {
        tables_by_entity
            .entry(table.entity.as_str())
            .or_default()
            .push(table);
    }
    for (entity, tables) in tables_by_entity {
        let target = database_entity_key(entity)?;
        let mut columns = tables
            .iter()
            .flat_map(|table| &table.columns)
            .map(|column| format!("{}:{}", column.name, column.data_type))
            .collect::<Vec<_>>();
        columns.sort_unstable();
        columns.dedup();
        let mut lines = tables
            .iter()
            .map(|table| table.range.start_line)
            .collect::<Vec<_>>();
        lines.sort_unstable();
        lines.dedup();
        plan.edges_to_create.push(structural_edge(
            &symbol.stable_key,
            &target,
            RelationKind::DefinesSchema,
            &symbol.file_path,
            &format!(
                "lines:{}",
                lines
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            &symbol.language,
            Some(format!("columns:{}", columns.join(","))),
        ));
    }
    Ok(())
}

fn database_entity_key(entity: &str) -> Result<StableKey, IndexPlanError> {
    StableKey::new(format!("db:{entity}"))
        .map_err(|error| IndexPlanError::InvalidStableKey(error.to_string()))
}

fn add_http_facts(
    plan: &mut IndexPlan,
    historical_symbols: &[IndexedSymbol],
    symbols: &[IndexedSymbol],
) -> Result<(), IndexPlanError> {
    let historical_endpoints = indexed_api_endpoints(historical_symbols);
    let current_endpoints = indexed_api_endpoints(symbols);
    reconcile_api_endpoint_nodes(plan, &historical_endpoints, &current_endpoints)?;

    let historical_calls = indexed_external_calls(historical_symbols);
    let current_calls = indexed_external_calls(symbols);
    reconcile_external_call_nodes(plan, &historical_calls, &current_calls)?;

    let changed = plan
        .nodes_to_write
        .iter()
        .filter(|node| node.kind == NodeKind::CodeSymbol)
        .map(|node| node.stable_key.clone())
        .collect::<BTreeSet<_>>();
    for symbol in symbols
        .iter()
        .filter(|symbol| changed.contains(&symbol.stable_key))
    {
        add_api_endpoint_edges(plan, symbol)?;
        add_external_call_edges(plan, symbol)?;
    }
    Ok(())
}

fn indexed_api_endpoints(symbols: &[IndexedSymbol]) -> BTreeMap<String, ApiEndpoint> {
    let mut endpoints = BTreeMap::new();
    for endpoint in symbols.iter().flat_map(|symbol| &symbol.api_endpoints) {
        endpoints
            .entry(api_endpoint_identifier(endpoint))
            .or_insert_with(|| endpoint.clone());
    }
    endpoints
}

fn indexed_external_calls(symbols: &[IndexedSymbol]) -> BTreeMap<String, ExternalCall> {
    let mut calls = BTreeMap::new();
    for call in symbols.iter().flat_map(|symbol| &symbol.external_calls) {
        calls
            .entry(external_call_identifier(call))
            .or_insert_with(|| call.clone());
    }
    calls
}

fn reconcile_api_endpoint_nodes(
    plan: &mut IndexPlan,
    historical: &BTreeMap<String, ApiEndpoint>,
    current: &BTreeMap<String, ApiEndpoint>,
) -> Result<(), IndexPlanError> {
    for (identifier, endpoint) in current {
        let mutation = match historical.get(identifier) {
            Some(previous) if previous == endpoint => continue,
            Some(_) => NodeMutationKind::Version,
            None => NodeMutationKind::Create,
        };
        plan.nodes_to_write.push(PlannedNode {
            stable_key: api_endpoint_key(endpoint)?,
            kind: NodeKind::ApiEndpoint,
            name: identifier.clone(),
            content_hash: normalized_hash(endpoint),
            attributes: PlannedNodeAttributes::ApiEndpoint {
                endpoint: endpoint.clone(),
            },
            mutation,
        });
    }
    for endpoint in historical
        .iter()
        .filter(|(identifier, _)| !current.contains_key(*identifier))
        .map(|(_, endpoint)| endpoint)
    {
        plan.nodes_to_retire.push(api_endpoint_key(endpoint)?);
    }
    Ok(())
}

fn reconcile_external_call_nodes(
    plan: &mut IndexPlan,
    historical: &BTreeMap<String, ExternalCall>,
    current: &BTreeMap<String, ExternalCall>,
) -> Result<(), IndexPlanError> {
    for (identifier, call) in current {
        let mutation = match historical.get(identifier) {
            Some(previous) if previous == call => continue,
            Some(_) => NodeMutationKind::Version,
            None => NodeMutationKind::Create,
        };
        plan.nodes_to_write.push(PlannedNode {
            stable_key: external_call_key(call)?,
            kind: NodeKind::ExternalSystem,
            name: identifier.clone(),
            content_hash: normalized_hash(call),
            attributes: PlannedNodeAttributes::ExternalCall { call: call.clone() },
            mutation,
        });
    }
    for call in historical
        .iter()
        .filter(|(identifier, _)| !current.contains_key(*identifier))
        .map(|(_, call)| call)
    {
        plan.nodes_to_retire.push(external_call_key(call)?);
    }
    Ok(())
}

fn add_api_endpoint_edges(
    plan: &mut IndexPlan,
    symbol: &IndexedSymbol,
) -> Result<(), IndexPlanError> {
    for endpoint in &symbol.api_endpoints {
        let target = api_endpoint_key(endpoint)?;
        plan.edges_to_create.push(structural_edge(
            &symbol.stable_key,
            &target,
            RelationKind::Exposes,
            &symbol.file_path,
            &normalized_hash(endpoint),
            &symbol.language,
            Some(format!(
                "line:{} decorator:{} {}",
                endpoint.range.start_line,
                endpoint.method.as_str(),
                endpoint.path
            )),
        ));
    }
    Ok(())
}

fn add_external_call_edges(
    plan: &mut IndexPlan,
    symbol: &IndexedSymbol,
) -> Result<(), IndexPlanError> {
    let mut grouped = BTreeMap::<String, Vec<&ExternalCall>>::new();
    for call in &symbol.external_calls {
        grouped
            .entry(external_call_identifier(call))
            .or_default()
            .push(call);
    }
    for calls in grouped.into_values() {
        let call = calls[0];
        let target = external_call_key(call)?;
        let lines = calls
            .iter()
            .map(|call| call.range.start_line.to_string())
            .collect::<Vec<_>>()
            .join(",");
        plan.edges_to_create.push(structural_edge(
            &symbol.stable_key,
            &target,
            RelationKind::CallsExternal,
            &symbol.file_path,
            &normalized_hash(call),
            &symbol.language,
            Some(format!(
                "lines:{lines} {} {}",
                call.method.as_str(),
                call.url
            )),
        ));
    }
    Ok(())
}

fn api_endpoint_identifier(endpoint: &ApiEndpoint) -> String {
    format!("{} {}", endpoint.method.as_str(), endpoint.path)
}

fn external_call_identifier(call: &ExternalCall) -> String {
    format!("{} {}", call.method.as_str(), call.url)
}

fn api_endpoint_key(endpoint: &ApiEndpoint) -> Result<StableKey, IndexPlanError> {
    StableKey::new(format!(
        "api_endpoint:{}:{}",
        endpoint.method.as_str(),
        endpoint.path
    ))
    .map_err(|error| IndexPlanError::InvalidStableKey(error.to_string()))
}

fn external_call_key(call: &ExternalCall) -> Result<StableKey, IndexPlanError> {
    StableKey::new(format!(
        "external_http:{}:{}",
        call.method.as_str(),
        call.url
    ))
    .map_err(|error| IndexPlanError::InvalidStableKey(error.to_string()))
}

fn normalized_hash(value: &impl std::fmt::Debug) -> String {
    format!("{value:?}")
}

const fn is_callable(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Class
            | SymbolKind::Struct
            | SymbolKind::Test
    )
}

fn normalize_plan(plan: &mut IndexPlan) -> Result<(), IndexPlanError> {
    plan.nodes_to_write
        .sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    if let Some(duplicate) = plan
        .nodes_to_write
        .windows(2)
        .find(|nodes| nodes[0].stable_key == nodes[1].stable_key)
    {
        return Err(IndexPlanError::DuplicateNodeWrite(
            duplicate[0].stable_key.to_string(),
        ));
    }
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CallSite, DatabaseAccess};

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
            database_accesses: Vec::new(),
            schema_tables: Vec::new(),
            api_endpoints: Vec::new(),
            external_calls: Vec::new(),
        }
    }

    fn indexed_file(path: &str, definition: &SymbolDefinition) -> IndexedFile {
        IndexedFile {
            stable_key: file_key(path).expect("file key"),
            path: path.to_owned(),
            language: "python".to_owned(),
            analysis_version: "python-tree-sitter-v1".to_owned(),
            content_hash: "old-file".to_owned(),
            symbols: vec![IndexedSymbol {
                stable_key: symbol_key("python", definition).expect("symbol key"),
                language: "python".to_owned(),
                file_path: path.to_owned(),
                name: definition.name.clone(),
                canonical_path: definition.canonical_path.clone(),
                kind: definition.kind,
                range: definition.range,
                signature: definition.signature.clone(),
                body_hash: definition.body_hash.clone(),
                structural_fingerprint: definition.structural_fingerprint.clone(),
                calls: Vec::new(),
                database_accesses: definition.database_accesses.clone(),
                schema_tables: definition.schema_tables.clone(),
                api_endpoints: definition.api_endpoints.clone(),
                external_calls: definition.external_calls.clone(),
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
                analysis_version: "python-tree-sitter-v1".to_owned(),
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
                analysis_version: "python-tree-sitter-v1".to_owned(),
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
                analysis_version: "python-tree-sitter-v1".to_owned(),
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

    #[test]
    fn api_and_external_http_facts_follow_the_full_index_lifecycle() {
        let mut handler = definition("cancel", "billing.cancel", "body-v1", "shape-v1");
        handler.api_endpoints.push(ApiEndpoint {
            path: "/subscriptions/{subscription_id}".to_owned(),
            method: crate::ir::HttpMethod::Delete,
            params: vec![crate::ir::ApiParam {
                name: "subscription_id".to_owned(),
                type_hint: Some("str".to_owned()),
                source: crate::ir::ParamSource::Path,
                required: true,
            }],
            return_type: Some("Subscription".to_owned()),
            framework: "python_http_framework".to_owned(),
            range: range(),
        });
        handler.external_calls.push(ExternalCall {
            method: crate::ir::HttpMethod::Post,
            url: "https://audit.internal/events".to_owned(),
            range: range(),
        });
        let analyses = BTreeMap::from([(
            "billing.py".to_owned(),
            FileAnalysis {
                path: "billing.py".to_owned(),
                language: "python".to_owned(),
                analysis_version: "python-tree-sitter-v4".to_owned(),
                content_hash: "file-v1".to_owned(),
                symbols: vec![handler.clone()],
            },
        )]);
        let appeared = plan_incremental_index(
            &RepositorySnapshot::default(),
            &analyses,
            &[FileChange::Added {
                path: "billing.py".to_owned(),
            }],
        )
        .expect("HTTP facts appear");
        assert!(appeared.nodes_to_write.iter().any(|node| {
            node.kind == NodeKind::ApiEndpoint
                && node.stable_key.as_str()
                    == "api_endpoint:DELETE:/subscriptions/{subscription_id}"
        }));
        assert!(
            appeared
                .edges_to_create
                .iter()
                .any(|edge| edge.kind == RelationKind::Exposes)
        );
        assert!(
            appeared
                .edges_to_create
                .iter()
                .any(|edge| edge.kind == RelationKind::CallsExternal)
        );

        let snapshot = RepositorySnapshot {
            files: BTreeMap::from([(
                "billing.py".to_owned(),
                indexed_file("billing.py", &handler),
            )]),
        };
        let without_contracts = definition("cancel", "billing.cancel", "body-v2", "shape-v2");
        let analyses = BTreeMap::from([(
            "billing.py".to_owned(),
            FileAnalysis {
                path: "billing.py".to_owned(),
                language: "python".to_owned(),
                analysis_version: "python-tree-sitter-v4".to_owned(),
                content_hash: "file-v2".to_owned(),
                symbols: vec![without_contracts],
            },
        )]);
        let retired = plan_incremental_index(
            &snapshot,
            &analyses,
            &[FileChange::Modified {
                path: "billing.py".to_owned(),
            }],
        )
        .expect("HTTP facts retire");
        assert!(
            retired.nodes_to_retire.iter().any(|key| {
                key.as_str() == "api_endpoint:DELETE:/subscriptions/{subscription_id}"
            })
        );
        assert!(
            retired
                .nodes_to_retire
                .iter()
                .any(|key| { key.as_str() == "external_http:POST:https://audit.internal/events" })
        );
    }

    #[test]
    fn database_entities_and_facts_follow_the_current_symbol_snapshot() {
        let mut old = definition("persist", "billing.persist", "body-v1", "shape-v1");
        old.database_accesses.push(DatabaseAccess {
            entity: "subscriptions".to_owned(),
            kind: DatabaseAccessKind::Write,
            range: range(),
            statement_hash: "sql-v1".to_owned(),
            columns: Vec::new(),
        });
        let mut snapshot = RepositorySnapshot::default();
        snapshot
            .files
            .insert("billing.py".to_owned(), indexed_file("billing.py", &old));

        let mut new = definition("persist", "billing.persist", "body-v2", "shape-v2");
        new.database_accesses.push(DatabaseAccess {
            entity: "subscription_archive".to_owned(),
            kind: DatabaseAccessKind::Write,
            range: range(),
            statement_hash: "sql-v2".to_owned(),
            columns: Vec::new(),
        });
        let analyses = BTreeMap::from([(
            "billing.py".to_owned(),
            FileAnalysis {
                path: "billing.py".to_owned(),
                language: "python".to_owned(),
                analysis_version: "python-tree-sitter-v2".to_owned(),
                content_hash: "file-v2".to_owned(),
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
        .expect("database transition");

        assert!(
            plan.nodes_to_retire
                .iter()
                .any(|key| key.as_str() == "db:subscriptions")
        );
        assert!(plan.nodes_to_write.iter().any(|node| {
            node.kind == NodeKind::DbEntity && node.stable_key.as_str() == "db:subscription_archive"
        }));
        let edge = plan
            .edges_to_create
            .iter()
            .find(|edge| edge.kind == RelationKind::WritesTo)
            .expect("write fact");
        assert_eq!(edge.target.as_str(), "db:subscription_archive");
        assert_eq!(edge.claim_class, ClaimClass::Fact);
        assert_eq!(edge.evidence_locator.as_deref(), Some("lines:1"));
    }

    #[test]
    fn column_level_write_access_is_unioned_into_edge_evidence() {
        let mut symbol = definition("cancel", "billing.cancel", "body", "shape");
        symbol.database_accesses.push(DatabaseAccess {
            entity: "subscriptions".to_owned(),
            kind: DatabaseAccessKind::Write,
            range: SourceRange {
                start_line: 4,
                ..range()
            },
            statement_hash: "sql-a".to_owned(),
            columns: vec!["status".to_owned()],
        });
        symbol.database_accesses.push(DatabaseAccess {
            entity: "subscriptions".to_owned(),
            kind: DatabaseAccessKind::Write,
            range: SourceRange {
                start_line: 5,
                ..range()
            },
            statement_hash: "sql-b".to_owned(),
            columns: vec!["paid_until".to_owned(), "status".to_owned()],
        });
        let analyses = BTreeMap::from([(
            "billing.py".to_owned(),
            FileAnalysis {
                path: "billing.py".to_owned(),
                language: "python".to_owned(),
                analysis_version: "python-tree-sitter-v2".to_owned(),
                content_hash: "file-v1".to_owned(),
                symbols: vec![symbol],
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

        let edge = plan
            .edges_to_create
            .iter()
            .find(|edge| edge.kind == RelationKind::WritesTo)
            .expect("write fact");
        assert_eq!(
            edge.evidence_locator.as_deref(),
            Some("lines:4,5 columns:paid_until,status")
        );
    }

    #[test]
    fn schema_migrations_and_code_database_accesses_share_one_db_entity() {
        let mut migration = definition(
            "migrations.001_create_subscriptions",
            "migrations.001_create_subscriptions",
            "migration-body",
            "migration-shape",
        );
        migration.kind = SymbolKind::SchemaMigration;
        migration.schema_tables.push(SchemaTableDefinition {
            entity: "subscriptions".to_owned(),
            columns: vec![
                crate::ir::SchemaColumn {
                    name: "id".to_owned(),
                    data_type: "UUID".to_owned(),
                    ..crate::ir::SchemaColumn::default()
                },
                crate::ir::SchemaColumn {
                    name: "status".to_owned(),
                    data_type: "VARCHAR(50)".to_owned(),
                    ..crate::ir::SchemaColumn::default()
                },
            ],
            range: range(),
            ..SchemaTableDefinition::default()
        });
        let mut reader = definition("cancel", "billing.cancel", "reader-body", "reader-shape");
        reader.database_accesses.push(DatabaseAccess {
            entity: "subscriptions".to_owned(),
            kind: DatabaseAccessKind::Write,
            range: range(),
            statement_hash: "sql".to_owned(),
            columns: Vec::new(),
        });
        let analyses = BTreeMap::from([
            (
                "migrations/001.sql".to_owned(),
                FileAnalysis {
                    path: "migrations/001.sql".to_owned(),
                    language: "goose".to_owned(),
                    analysis_version: "goose-migration-v1".to_owned(),
                    content_hash: "migration-file".to_owned(),
                    symbols: vec![migration],
                },
            ),
            (
                "billing.py".to_owned(),
                FileAnalysis {
                    path: "billing.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v2".to_owned(),
                    content_hash: "billing-file".to_owned(),
                    symbols: vec![reader],
                },
            ),
        ]);

        let plan = plan_incremental_index(
            &RepositorySnapshot::default(),
            &analyses,
            &[
                FileChange::Added {
                    path: "migrations/001.sql".to_owned(),
                },
                FileChange::Added {
                    path: "billing.py".to_owned(),
                },
            ],
        )
        .expect("schema and access plan");

        let db_entity_nodes = plan
            .nodes_to_write
            .iter()
            .filter(|node| {
                node.kind == NodeKind::DbEntity && node.stable_key.as_str() == "db:subscriptions"
            })
            .count();
        assert_eq!(db_entity_nodes, 1, "one DbEntity, not one per fact source");

        let schema_edge = plan
            .edges_to_create
            .iter()
            .find(|edge| edge.kind == RelationKind::DefinesSchema)
            .expect("defines-schema fact");
        assert_eq!(schema_edge.target.as_str(), "db:subscriptions");
        assert_eq!(
            schema_edge.evidence_locator.as_deref(),
            Some("columns:id:UUID,status:VARCHAR(50)")
        );

        assert!(
            plan.edges_to_create
                .iter()
                .any(|edge| edge.kind == RelationKind::WritesTo
                    && edge.target.as_str() == "db:subscriptions")
        );
    }

    #[test]
    fn calls_do_not_resolve_to_non_callable_type_aliases() {
        let mut caller = definition("parse", "app.parse", "caller", "caller-shape");
        caller.calls.push(CallSite {
            callee: "Err".to_owned(),
            range: range(),
        });
        let mut associated_type = definition("Err", "Parser.Err", "type", "type-shape");
        associated_type.kind = SymbolKind::TypeAlias;
        let analyses = BTreeMap::from([(
            "app.rs".to_owned(),
            FileAnalysis {
                path: "app.rs".to_owned(),
                language: "rust".to_owned(),
                analysis_version: "rust-tree-sitter-v2".to_owned(),
                content_hash: "file".to_owned(),
                symbols: vec![caller, associated_type],
            },
        )]);

        let plan = plan_incremental_index(
            &RepositorySnapshot::default(),
            &analyses,
            &[FileChange::Added {
                path: "app.rs".to_owned(),
            }],
        )
        .expect("plan");

        assert!(!plan.edges_to_create.iter().any(|edge| {
            edge.kind == RelationKind::Calls && edge.target.as_str().contains("Parser.Err")
        }));
    }

    #[test]
    fn identical_symbols_in_different_languages_keep_distinct_identities() {
        let definition = definition("run", "app.run", "same-body", "same-shape");
        let analyses = BTreeMap::from([
            (
                "app.py".to_owned(),
                FileAnalysis {
                    path: "app.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v1".to_owned(),
                    content_hash: "python-file".to_owned(),
                    symbols: vec![definition.clone()],
                },
            ),
            (
                "app.rs".to_owned(),
                FileAnalysis {
                    path: "app.rs".to_owned(),
                    language: "rust".to_owned(),
                    analysis_version: "rust-tree-sitter-v2".to_owned(),
                    content_hash: "rust-file".to_owned(),
                    symbols: vec![definition],
                },
            ),
        ]);

        let plan = plan_incremental_index(
            &RepositorySnapshot::default(),
            &analyses,
            &[
                FileChange::Added {
                    path: "app.py".to_owned(),
                },
                FileChange::Added {
                    path: "app.rs".to_owned(),
                },
            ],
        )
        .expect("plan");
        let symbol_keys = plan
            .nodes_to_write
            .iter()
            .filter(|node| node.kind == NodeKind::CodeSymbol)
            .map(|node| node.stable_key.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(symbol_keys.len(), 2);
        assert!(symbol_keys.contains("symbol:python:app.run:Function"));
        assert!(symbol_keys.contains("symbol:rust:app.run:Function"));
    }

    #[test]
    fn duplicate_file_transitions_fail_before_storage() {
        let definition = definition("run", "app.run", "body", "shape");
        let analyses = BTreeMap::from([(
            "app.py".to_owned(),
            FileAnalysis {
                path: "app.py".to_owned(),
                language: "python".to_owned(),
                analysis_version: "python-tree-sitter-v1".to_owned(),
                content_hash: "file".to_owned(),
                symbols: vec![definition],
            },
        )]);

        let error = plan_incremental_index(
            &RepositorySnapshot::default(),
            &analyses,
            &[
                FileChange::Added {
                    path: "app.py".to_owned(),
                },
                FileChange::Modified {
                    path: "app.py".to_owned(),
                },
            ],
        )
        .expect_err("duplicate plan must fail");

        assert_eq!(
            error,
            IndexPlanError::DuplicateNodeWrite("file:app.py".to_owned())
        );
    }

    #[test]
    fn structurally_equal_symbols_added_together_do_not_impersonate_history() {
        let first = definition("new", "alpha.Reader.new", "same-body", "same-shape");
        let second = definition("new", "beta.Reader.new", "same-body", "same-shape");
        let analyses = BTreeMap::from([
            (
                "alpha.py".to_owned(),
                FileAnalysis {
                    path: "alpha.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v1".to_owned(),
                    content_hash: "alpha-file".to_owned(),
                    symbols: vec![first],
                },
            ),
            (
                "beta.py".to_owned(),
                FileAnalysis {
                    path: "beta.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v1".to_owned(),
                    content_hash: "beta-file".to_owned(),
                    symbols: vec![second],
                },
            ),
        ]);

        let plan = plan_incremental_index(
            &RepositorySnapshot::default(),
            &analyses,
            &[
                FileChange::Added {
                    path: "alpha.py".to_owned(),
                },
                FileChange::Added {
                    path: "beta.py".to_owned(),
                },
            ],
        )
        .expect("independent additions");
        let symbol_keys = plan
            .nodes_to_write
            .iter()
            .filter(|node| node.kind == NodeKind::CodeSymbol)
            .map(|node| node.stable_key.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(symbol_keys.len(), 2);
        assert!(symbol_keys.contains("symbol:python:alpha.Reader.new:Function"));
        assert!(symbol_keys.contains("symbol:python:beta.Reader.new:Function"));
    }

    /// A tiny, trivial function body ("return a shared shape") is common
    /// enough that two unrelated files can define differently named
    /// functions with an identical whitespace-stripped body. The
    /// cross-repository fingerprint fallback must not merge a brand-new
    /// function into an unrelated prior symbol just because their shapes
    /// coincide; requiring the name to also match is what tells them apart.
    #[test]
    fn differently_named_symbols_with_the_same_shape_do_not_merge_across_files() {
        let existing = definition("touches", "context_pack.touches", "same-body", "same-shape");
        let existing_file = indexed_file("context_pack.py", &existing);
        let existing_key = existing_file.symbols[0].stable_key.clone();
        let snapshot = RepositorySnapshot {
            files: BTreeMap::from([("context_pack.py".to_owned(), existing_file)]),
        };
        let new_symbol = definition("checks", "impact.checks", "same-body", "same-shape");
        let analyses = BTreeMap::from([(
            "impact.py".to_owned(),
            FileAnalysis {
                path: "impact.py".to_owned(),
                language: "python".to_owned(),
                analysis_version: "python-tree-sitter-v1".to_owned(),
                content_hash: "impact-file".to_owned(),
                symbols: vec![new_symbol],
            },
        )]);

        let plan = plan_incremental_index(
            &snapshot,
            &analyses,
            &[FileChange::Added {
                path: "impact.py".to_owned(),
            }],
        )
        .expect("independent addition alongside a same-shaped prior symbol");

        let symbol_keys = plan
            .nodes_to_write
            .iter()
            .filter(|node| node.kind == NodeKind::CodeSymbol)
            .map(|node| node.stable_key.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            symbol_keys.len(),
            1,
            "must not also rewrite the unrelated prior symbol"
        );
        assert!(symbol_keys.contains("symbol:python:impact.checks:Function"));
        assert!(!symbol_keys.contains(existing_key.as_str()));
    }

    /// Reproduces the exact defect dogfooding Phase 4 of the external-artifact
    /// ingestion work found: adding a brand-new file whose helper shares a
    /// name, kind, and structural fingerprint with an *unrelated, unchanged*
    /// file's symbol (that file is not part of `changes` at all -- e.g. two
    /// files each defining a same-shaped private `edge` test helper) must not
    /// let the same-shape fallback steal the unrelated file's still-live
    /// identity. There is no move to detect: the old file isn't changing.
    #[test]
    fn a_new_files_same_shaped_symbol_does_not_steal_an_unrelated_unchanged_files_identity() {
        let existing = definition("edge", "verification.tests.edge", "same-body", "same-shape");
        let existing_file = indexed_file("verification.py", &existing);
        let existing_key = existing_file.symbols[0].stable_key.clone();
        let snapshot = RepositorySnapshot {
            files: BTreeMap::from([("verification.py".to_owned(), existing_file)]),
        };
        let new_symbol = definition("edge", "neighborhood.tests.edge", "same-body", "same-shape");
        let analyses = BTreeMap::from([(
            "neighborhood.py".to_owned(),
            FileAnalysis {
                path: "neighborhood.py".to_owned(),
                language: "python".to_owned(),
                analysis_version: "python-tree-sitter-v1".to_owned(),
                content_hash: "neighborhood-file".to_owned(),
                symbols: vec![new_symbol],
            },
        )]);

        let plan = plan_incremental_index(
            &snapshot,
            &analyses,
            &[FileChange::Added {
                path: "neighborhood.py".to_owned(),
            }],
        )
        .expect("independent addition alongside a same-shaped, same-named prior symbol");

        let symbol_keys = plan
            .nodes_to_write
            .iter()
            .filter(|node| node.kind == NodeKind::CodeSymbol)
            .map(|node| node.stable_key.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            symbol_keys.len(),
            1,
            "must not also rewrite the unrelated, unchanged prior symbol"
        );
        assert!(symbol_keys.contains("symbol:python:neighborhood.tests.edge:Function"));
        assert!(
            !symbol_keys.contains(existing_key.as_str()),
            "the new file's symbol must not claim the unrelated unchanged file's identity"
        );
        assert!(
            plan.nodes_to_retire.is_empty(),
            "the unrelated file was never touched, so nothing about it should retire"
        );
    }

    /// Reproduces the exact defect dogfooding this repository found:
    /// `impact.rs` and `context_pack.rs` each define a same-named, one-line
    /// `touches` helper with an identical whitespace-stripped body. Only
    /// `context_pack.touches` has ever been stored under its own canonical
    /// path (`impact.touches` was historically mismatched away by the same
    /// fingerprint fallback, elsewhere fixed by requiring name equality).
    /// Once both files are modified in the same transition, the same-shape
    /// fallback must not hand `impact.touches` the identity that
    /// `context_pack.touches` already claimed via its own exact canonical
    /// path in this same pass, even though both share a name.
    #[test]
    fn same_named_same_shaped_symbols_in_different_files_claim_distinct_identities() {
        let context_pack_definition =
            definition("touches", "context_pack.touches", "same-body", "same-shape");
        let context_pack_file = indexed_file("context_pack.py", &context_pack_definition);
        let context_pack_key = context_pack_file.symbols[0].stable_key.clone();
        let impact_file_without_its_own_identity = IndexedFile {
            stable_key: file_key("impact.py").expect("file key"),
            path: "impact.py".to_owned(),
            language: "python".to_owned(),
            analysis_version: "python-tree-sitter-v1".to_owned(),
            content_hash: "impact-file".to_owned(),
            symbols: Vec::new(),
        };
        let snapshot = RepositorySnapshot {
            files: BTreeMap::from([
                ("context_pack.py".to_owned(), context_pack_file),
                ("impact.py".to_owned(), impact_file_without_its_own_identity),
            ]),
        };
        let impact_definition = definition("touches", "impact.touches", "same-body", "same-shape");
        let analyses = BTreeMap::from([
            (
                "context_pack.py".to_owned(),
                FileAnalysis {
                    path: "context_pack.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v1".to_owned(),
                    content_hash: "context-pack-file-v2".to_owned(),
                    symbols: vec![context_pack_definition],
                },
            ),
            (
                "impact.py".to_owned(),
                FileAnalysis {
                    path: "impact.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v1".to_owned(),
                    content_hash: "impact-file-v2".to_owned(),
                    symbols: vec![impact_definition],
                },
            ),
        ]);

        let plan = plan_incremental_index(
            &snapshot,
            &analyses,
            &[
                FileChange::Modified {
                    path: "context_pack.py".to_owned(),
                },
                FileChange::Modified {
                    path: "impact.py".to_owned(),
                },
            ],
        )
        .expect("both files claim distinct identities");

        let symbol_keys = plan
            .nodes_to_write
            .iter()
            .filter(|node| node.kind == NodeKind::CodeSymbol)
            .map(|node| node.stable_key.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            symbol_keys.len(),
            2,
            "each file's touches must keep its own identity"
        );
        assert!(symbol_keys.contains(context_pack_key.as_str()));
        assert!(symbol_keys.contains("symbol:python:impact.touches:Function"));
    }

    /// Found dogfooding this fix on Zed: `#[cfg(target_os = "windows")]` /
    /// `#[cfg(not(target_os = "windows"))]` variants of the same Rust
    /// function name, and Go's blank identifier `_` reused for repeated
    /// interface-conformance checks, both leave tree-sitter with two
    /// distinct symbols that legitimately share one canonical path. Neither
    /// has any prior indexed identity to match, so both must become "new"
    /// in the same transition without colliding on one generated key.
    #[test]
    fn two_new_symbols_sharing_one_canonical_path_claim_distinct_identities() {
        let first = definition("_", "main._", "first-body", "first-shape");
        let second = definition("_", "main._", "second-body", "second-shape");
        let snapshot = RepositorySnapshot::default();
        let analyses = BTreeMap::from([(
            "main.go".to_owned(),
            FileAnalysis {
                path: "main.go".to_owned(),
                language: "go".to_owned(),
                analysis_version: "go-tree-sitter-v1".to_owned(),
                content_hash: "main-file".to_owned(),
                symbols: vec![first, second],
            },
        )]);

        let plan = plan_incremental_index(
            &snapshot,
            &analyses,
            &[FileChange::Added {
                path: "main.go".to_owned(),
            }],
        )
        .expect("colliding canonical paths must not abort the transition");

        let symbol_keys = plan
            .nodes_to_write
            .iter()
            .filter(|node| node.kind == NodeKind::CodeSymbol)
            .map(|node| node.stable_key.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            symbol_keys.len(),
            2,
            "both blank-identifier symbols must be written"
        );
        assert_eq!(
            symbol_keys.iter().collect::<BTreeSet<_>>().len(),
            2,
            "colliding new symbols must not share one write"
        );
        assert!(symbol_keys.contains(&"symbol:go:main._:Function"));
        assert!(symbol_keys.contains(&"symbol:go:main._:Function#2"));
    }

    /// The same defect class as
    /// `same_named_same_shaped_symbols_in_different_files_claim_distinct_identities`,
    /// but with the two files in the opposite order. Dogfooding this
    /// repository found that order matters without a transition-wide
    /// reservation pass: processing the file that needs the same-shape
    /// fallback *before* the file that owns the identity by its own
    /// unchanged exact canonical path let the fallback claim that identity
    /// first (it was not `used` yet). Its rightful owner, matched afterward
    /// by its own canonical path, then found that key already `used`, fell
    /// through every tier, and its final `symbol_key` fallback deterministically
    /// recomputed the very same already-claimed key — two writes for one
    /// stable key, caught only by the pre-storage duplicate-write guard.
    #[test]
    fn same_named_same_shaped_symbols_claim_distinct_identities_regardless_of_file_order() {
        let context_pack_definition =
            definition("touches", "context_pack.touches", "same-body", "same-shape");
        let context_pack_file = indexed_file("context_pack.py", &context_pack_definition);
        let context_pack_key = context_pack_file.symbols[0].stable_key.clone();
        let impact_file_without_its_own_identity = IndexedFile {
            stable_key: file_key("impact.py").expect("file key"),
            path: "impact.py".to_owned(),
            language: "python".to_owned(),
            analysis_version: "python-tree-sitter-v1".to_owned(),
            content_hash: "impact-file".to_owned(),
            symbols: Vec::new(),
        };
        let snapshot = RepositorySnapshot {
            files: BTreeMap::from([
                ("context_pack.py".to_owned(), context_pack_file),
                ("impact.py".to_owned(), impact_file_without_its_own_identity),
            ]),
        };
        let impact_definition = definition("touches", "impact.touches", "same-body", "same-shape");
        let analyses = BTreeMap::from([
            (
                "context_pack.py".to_owned(),
                FileAnalysis {
                    path: "context_pack.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v1".to_owned(),
                    content_hash: "context-pack-file-v2".to_owned(),
                    symbols: vec![context_pack_definition],
                },
            ),
            (
                "impact.py".to_owned(),
                FileAnalysis {
                    path: "impact.py".to_owned(),
                    language: "python".to_owned(),
                    analysis_version: "python-tree-sitter-v1".to_owned(),
                    content_hash: "impact-file-v2".to_owned(),
                    symbols: vec![impact_definition],
                },
            ),
        ]);

        // Reversed order versus the sibling test above: the file that needs
        // the same-shape fallback (`impact.py`) is planned *before* the file
        // that owns the identity by its own unchanged canonical path
        // (`context_pack.py`).
        let plan = plan_incremental_index(
            &snapshot,
            &analyses,
            &[
                FileChange::Modified {
                    path: "impact.py".to_owned(),
                },
                FileChange::Modified {
                    path: "context_pack.py".to_owned(),
                },
            ],
        )
        .expect("both files claim distinct identities regardless of processing order");

        let symbol_keys = plan
            .nodes_to_write
            .iter()
            .filter(|node| node.kind == NodeKind::CodeSymbol)
            .map(|node| node.stable_key.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            symbol_keys.len(),
            2,
            "each file's touches must keep its own identity regardless of order"
        );
        assert!(symbol_keys.contains(context_pack_key.as_str()));
        assert!(symbol_keys.contains("symbol:python:impact.touches:Function"));
    }

    #[test]
    fn scope_reconciliation_retires_files_excluded_by_config() {
        let changes = reconcile_source_scope(
            &[],
            ["src/keep.py".to_owned(), "legacy/drop.py".to_owned()],
            ["src/keep.py".to_owned()],
        );

        assert_eq!(
            changes,
            vec![FileChange::Deleted {
                path: "legacy/drop.py".to_owned()
            }]
        );
    }

    #[test]
    fn scope_reconciliation_adds_newly_included_files_without_duplicates() {
        let added = FileChange::Added {
            path: "app/new.py".to_owned(),
        };
        let changes = reconcile_source_scope(
            std::slice::from_ref(&added),
            std::iter::empty(),
            ["app/new.py".to_owned()],
        );

        assert_eq!(changes, vec![added]);
    }

    #[test]
    fn analyzer_version_change_reparses_an_unchanged_file_once() {
        let definition = definition("run", "app.run", "body", "shape");
        let snapshot = RepositorySnapshot {
            files: BTreeMap::from([("app.py".to_owned(), indexed_file("app.py", &definition))]),
        };
        let expected = BTreeMap::from([("app.py".to_owned(), "python-tree-sitter-v2".to_owned())]);

        let changes = reconcile_analysis_versions(&[], &snapshot, &expected);
        let already_changed = reconcile_analysis_versions(&changes, &snapshot, &expected);

        assert_eq!(
            changes,
            vec![FileChange::Modified {
                path: "app.py".to_owned()
            }]
        );
        assert_eq!(already_changed, changes);
    }

    #[test]
    fn repeated_add_for_an_existing_snapshot_closes_prior_structural_facts() {
        let old = definition("cancel", "billing.cancel", "body", "shape");
        let snapshot = RepositorySnapshot {
            files: BTreeMap::from([("billing.py".to_owned(), indexed_file("billing.py", &old))]),
        };
        let analyses = BTreeMap::from([(
            "billing.py".to_owned(),
            FileAnalysis {
                path: "billing.py".to_owned(),
                language: "python".to_owned(),
                analysis_version: "python-tree-sitter-v1".to_owned(),
                content_hash: "same-file".to_owned(),
                symbols: vec![old],
            },
        )]);

        let plan = plan_incremental_index(
            &snapshot,
            &analyses,
            &[FileChange::Added {
                path: "billing.py".to_owned(),
            }],
        )
        .expect("plan");

        assert_eq!(plan.structural_sources_to_close, ["billing.py"]);
    }
}
