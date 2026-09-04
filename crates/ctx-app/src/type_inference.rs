//! Type-backed semantic enrichment for Python ORM writes. Candidate syntax,
//! external type identity, ORM model matching, and graph persistence remain
//! separate stages so no type-derived result can enter deterministic indexing.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Instant,
};

use ctx_core::{
    domain::{ClaimClass, ClaimStatus, Confidence, NodeKind, RelationKind, StableKey},
    graph::GraphSnapshot,
    indexing::PlannedNodeAttributes,
    ir::{SourceRange, SymbolKind},
    type_inference::{
        PythonClassType, PythonFunctionType, PythonType, TypeInferenceEdge,
        TypeInferencePersistenceStats, TypeWriteCandidate, TypeWriteForm,
    },
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::{
    GitRepository, GraphStore, IndexStore, PortError, PythonTypeCandidateExtractor,
    PythonTypeOracle, TypeInferenceStore,
};

pub const DEFAULT_TYPE_INFERENCE_CONFIDENCE: f32 = 0.90;
pub const PYRIGHT_PRODUCER: &str = "pyright";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeInferenceDropReason {
    UnknownType,
    AmbiguousType,
    UnsupportedOperation,
    MissingSourceOwner,
    TypeQueryFailed,
    CandidateExtractionFailed,
    SuppressedByFact,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TypeInferenceDiagnostic {
    pub file: String,
    pub line: usize,
    pub form: Option<TypeWriteForm>,
    pub probe: Option<String>,
    pub inferred_type: Option<String>,
    pub model_symbol: Option<String>,
    pub table: Option<String>,
    pub reason: TypeInferenceDropReason,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InferTypesReport {
    pub candidate_sites: usize,
    pub type_queries: usize,
    pub resolved_model_candidates: usize,
    pub inferences_created: usize,
    pub inferences_updated: usize,
    pub inferences_removed: usize,
    pub dropped_unknown: usize,
    pub dropped_ambiguous: usize,
    pub dropped_unsupported: usize,
    pub suppressed_by_fact: usize,
    pub pyright_failures: usize,
    pub extraction_failures: usize,
    pub duration_ms: u128,
    pub persistence: TypeInferencePersistenceStats,
    pub diagnostics: Vec<TypeInferenceDiagnostic>,
}

#[derive(Debug, Error)]
pub enum InferTypesError {
    #[error("type inference inputs have uncommitted changes: {paths}; commit and index them first")]
    UncommittedInputs { paths: String },
    #[error("ctx index must complete before type inference")]
    IndexRequired,
    #[error("the ctx index is at {indexed}, but the checkout is at {head}; run `ctx index` first")]
    IndexOutOfDate { indexed: String, head: String },
    #[error("repository operation failed: {0}")]
    Git(PortError),
    #[error("storage operation failed: {0}")]
    Storage(PortError),
    #[error("the Python type oracle failed catastrophically: {0}")]
    OracleFatal(PortError),
}

pub struct InferTypesRunner<'a, G, C, O, S> {
    git: &'a G,
    candidates: &'a C,
    oracle: &'a mut O,
    store: &'a mut S,
    confidence: Confidence,
}

impl<'a, G, C, O, S> InferTypesRunner<'a, G, C, O, S>
where
    G: GitRepository,
    C: PythonTypeCandidateExtractor,
    O: PythonTypeOracle,
    S: GraphStore + IndexStore + TypeInferenceStore,
{
    pub const fn new(
        git: &'a G,
        candidates: &'a C,
        oracle: &'a mut O,
        store: &'a mut S,
        confidence: Confidence,
    ) -> Self {
        Self {
            git,
            candidates,
            oracle,
            store,
            confidence,
        }
    }

    /// Fully recomputes Pyright's inference layer for the current indexed
    /// commit, persisting only after every candidate query has completed.
    ///
    /// # Errors
    /// Returns [`InferTypesError`] for dirty/stale index state, repository or
    /// storage failures, and fatal oracle process failures.
    pub fn run(&mut self, now: &str) -> Result<InferTypesReport, InferTypesError> {
        let started = Instant::now();
        let dirty = self
            .git
            .uncommitted_index_inputs()
            .map_err(InferTypesError::Git)?;
        if !dirty.is_empty() {
            return Err(InferTypesError::UncommittedInputs {
                paths: dirty.join(", "),
            });
        }
        let repository = self.git.descriptor().map_err(InferTypesError::Git)?;
        let head = self.git.head().map_err(InferTypesError::Git)?;
        let indexed = self
            .store
            .latest_commit(&repository.id)
            .map_err(InferTypesError::Storage)?
            .ok_or(InferTypesError::IndexRequired)?;
        if indexed.oid != head.oid {
            return Err(InferTypesError::IndexOutOfDate {
                indexed: indexed.oid.to_string(),
                head: head.oid.to_string(),
            });
        }
        let graph = self
            .store
            .load_graph(&repository.id)
            .map_err(InferTypesError::Storage)?;
        let models = ModelIndex::new(Path::new(&repository.root_path), &graph);
        let fact_writes = active_fact_writes(&graph);
        let mut report = InferTypesReport::default();
        let mut groups = BTreeMap::<(StableKey, StableKey), InferenceGroup>::new();
        let paths = self.git.all_source_files().map_err(InferTypesError::Git)?;
        for path in paths
            .into_iter()
            .filter(|path| Path::new(path).extension().is_some_and(|ext| ext == "py"))
        {
            let extracted = match self.candidates.candidates(&path) {
                Ok(extracted) => extracted,
                Err(error) => {
                    report.extraction_failures += 1;
                    report.diagnostics.push(TypeInferenceDiagnostic {
                        file: path,
                        line: 0,
                        form: None,
                        probe: None,
                        inferred_type: None,
                        model_symbol: None,
                        table: None,
                        reason: TypeInferenceDropReason::CandidateExtractionFailed,
                        detail: error.to_string(),
                    });
                    continue;
                }
            };
            report.candidate_sites += extracted.len();
            let absolute = Path::new(&repository.root_path).join(&path);
            let mut session_uris = None;
            for candidate in extracted {
                let Some(owner) = containing_symbol(&graph, &candidate) else {
                    report.dropped_unsupported += 1;
                    report.diagnostics.push(diagnostic(
                        &candidate,
                        TypeInferenceDropReason::MissingSourceOwner,
                        "no indexed containing function or method",
                        None,
                        None,
                    ));
                    continue;
                };
                report.type_queries += 1;
                let inferred = match self.oracle.inferred_type(&absolute, &candidate.probe) {
                    Ok(inferred) => inferred,
                    Err(error) => {
                        self.record_oracle_failure(&candidate, &mut report, &error)?;
                        continue;
                    }
                };
                let resolved = match models.resolve(&inferred) {
                    Ok(resolved) => resolved,
                    Err(ModelResolutionError::Unknown(detail)) => {
                        report.dropped_unknown += 1;
                        report.diagnostics.push(diagnostic(
                            &candidate,
                            TypeInferenceDropReason::UnknownType,
                            &detail,
                            Some(&inferred),
                            None,
                        ));
                        continue;
                    }
                    Err(ModelResolutionError::Ambiguous(detail)) => {
                        report.dropped_ambiguous += 1;
                        report.diagnostics.push(diagnostic(
                            &candidate,
                            TypeInferenceDropReason::AmbiguousType,
                            &detail,
                            Some(&inferred),
                            None,
                        ));
                        continue;
                    }
                };
                report.resolved_model_candidates += 1;

                if candidate.form == TypeWriteForm::AttrAssign {
                    let column = candidate.column.as_deref().unwrap_or_default();
                    if !resolved.columns.contains(column) {
                        report.dropped_unsupported += 1;
                        report.diagnostics.push(diagnostic(
                            &candidate,
                            TypeInferenceDropReason::UnsupportedOperation,
                            "attribute is not a statically known mapped column",
                            Some(&inferred),
                            Some(&resolved),
                        ));
                        continue;
                    }
                } else {
                    if session_uris.is_none() {
                        session_uris =
                            Some(self.resolve_session_uris(&absolute, &candidate, &mut report)?);
                    }
                    let Some(method_probe) = candidate.method_probe.as_ref() else {
                        report.dropped_unsupported += 1;
                        continue;
                    };
                    report.type_queries += 1;
                    let method_type = match self.oracle.inferred_type(&absolute, method_probe) {
                        Ok(method_type) => method_type,
                        Err(error) => {
                            self.record_oracle_failure(&candidate, &mut report, &error)?;
                            continue;
                        }
                    };
                    if !sqlalchemy_session_method(
                        candidate.form,
                        &method_type,
                        session_uris.as_ref().expect("initialized above"),
                    ) {
                        report.dropped_unsupported += 1;
                        report.diagnostics.push(diagnostic(
                            &candidate,
                            TypeInferenceDropReason::UnsupportedOperation,
                            "method is not a bound SQLAlchemy Session/AsyncSession API",
                            Some(&method_type),
                            Some(&resolved),
                        ));
                        continue;
                    }
                }

                if fact_writes.contains(&(owner.clone(), resolved.target.clone())) {
                    report.suppressed_by_fact += 1;
                    report.diagnostics.push(diagnostic(
                        &candidate,
                        TypeInferenceDropReason::SuppressedByFact,
                        "an active Fact already proves this symbol-to-table write",
                        Some(&inferred),
                        Some(&resolved),
                    ));
                    continue;
                }
                groups
                    .entry((owner.clone(), resolved.target.clone()))
                    .or_insert_with(|| InferenceGroup::new(owner, resolved.clone(), &path))
                    .sites
                    .push(InferenceSite::new(&candidate, &inferred, &resolved));
            }
        }
        let edges = groups
            .into_values()
            .map(|group| group.into_edge(self.confidence))
            .collect::<Vec<_>>();
        let persistence = self
            .store
            .replace_type_inferences(&repository.id, &head, now, PYRIGHT_PRODUCER, &edges)
            .map_err(InferTypesError::Storage)?;
        report.inferences_created = persistence.created;
        report.inferences_updated = persistence.updated;
        report.inferences_removed = persistence.removed;
        report.persistence = persistence;
        report.duration_ms = started.elapsed().as_millis();
        report.diagnostics.sort_by(|left, right| {
            left.file
                .cmp(&right.file)
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| left.probe.cmp(&right.probe))
        });
        Ok(report)
    }

    fn resolve_session_uris(
        &mut self,
        file: &Path,
        candidate: &TypeWriteCandidate,
        report: &mut InferTypesReport,
    ) -> Result<BTreeSet<String>, InferTypesError> {
        let mut uris = BTreeSet::new();
        for module in ["sqlalchemy.orm.session", "sqlalchemy.ext.asyncio.session"] {
            match self.oracle.resolve_import(file, module) {
                Ok(Some(uri)) => {
                    uris.insert(uri);
                }
                Ok(None) => {}
                Err(error) => {
                    report.pyright_failures += 1;
                    report.diagnostics.push(diagnostic(
                        candidate,
                        TypeInferenceDropReason::TypeQueryFailed,
                        &format!("could not resolve {module}: {error}"),
                        None,
                        None,
                    ));
                    if !self.oracle.is_healthy() {
                        return Err(InferTypesError::OracleFatal(error));
                    }
                }
            }
        }
        Ok(uris)
    }

    fn record_oracle_failure(
        &mut self,
        candidate: &TypeWriteCandidate,
        report: &mut InferTypesReport,
        error: &PortError,
    ) -> Result<(), InferTypesError> {
        report.pyright_failures += 1;
        report.diagnostics.push(diagnostic(
            candidate,
            TypeInferenceDropReason::TypeQueryFailed,
            &error.to_string(),
            None,
            None,
        ));
        if !self.oracle.is_healthy() {
            return Err(InferTypesError::OracleFatal(error.clone()));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct ModelRecord {
    symbol: StableKey,
    canonical_path: String,
    source_path: PathBuf,
    name: String,
    range: SourceRange,
    target: StableKey,
    table: String,
    columns: BTreeSet<String>,
}

struct ModelIndex {
    models: Vec<ModelRecord>,
}

impl ModelIndex {
    fn new(root: &Path, graph: &GraphSnapshot) -> Self {
        let mut models = Vec::new();
        for node in graph.nodes.values() {
            let PlannedNodeAttributes::Symbol {
                file_path,
                canonical_path,
                symbol_kind,
                range,
                schema_tables,
                ..
            } = &node.attributes
            else {
                continue;
            };
            if *symbol_kind != SymbolKind::Class {
                continue;
            }
            let mut entities = schema_tables
                .iter()
                .map(|table| table.entity.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter();
            let Some(table) = entities.next() else {
                continue;
            };
            if entities.next().is_some() {
                continue;
            }
            let Ok(target) = StableKey::new(format!("db:{table}")) else {
                continue;
            };
            if !graph
                .nodes
                .get(&target)
                .is_some_and(|node| node.kind == NodeKind::DbEntity)
            {
                continue;
            }
            models.push(ModelRecord {
                symbol: node.stable_key.clone(),
                canonical_path: canonical_path.clone(),
                source_path: normalized_path(&root.join(file_path)),
                name: node.name.clone(),
                range: *range,
                target,
                table: table.to_owned(),
                columns: schema_tables
                    .iter()
                    .filter(|schema| schema.entity == *table)
                    .flat_map(|schema| schema.columns.iter().map(|column| column.name.clone()))
                    .collect(),
            });
        }
        models.sort_by(|left, right| left.symbol.cmp(&right.symbol));
        Self { models }
    }

    fn resolve(&self, inferred: &PythonType) -> Result<ResolvedModel, ModelResolutionError> {
        match inferred {
            PythonType::Any | PythonType::Unknown => Err(ModelResolutionError::Unknown(
                "type checker returned Any or Unknown".to_owned(),
            )),
            PythonType::Class(class) => self.resolve_class(class).ok_or_else(|| {
                ModelResolutionError::Unknown(
                    "type does not resolve to an indexed ORM model declaration".to_owned(),
                )
            }),
            PythonType::Union { members } => {
                if members.is_empty() {
                    return Err(ModelResolutionError::Ambiguous(
                        "empty union type".to_owned(),
                    ));
                }
                let resolved = members
                    .iter()
                    .map(|member| match member {
                        PythonType::Class(class) => self.resolve_class(class),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        ModelResolutionError::Ambiguous(
                            "union retains a non-model, None, Any, or Unknown alternative"
                                .to_owned(),
                        )
                    })?;
                let tables = resolved
                    .iter()
                    .map(|model| model.target.clone())
                    .collect::<BTreeSet<_>>();
                if tables.len() != 1 {
                    return Err(ModelResolutionError::Ambiguous(
                        "union model alternatives map to different database entities".to_owned(),
                    ));
                }
                Ok(ResolvedModel::merge(resolved))
            }
            _ => Err(ModelResolutionError::Unknown(
                "type is not a direct class instance".to_owned(),
            )),
        }
    }

    fn resolve_class(&self, class: &PythonClassType) -> Option<ResolvedModel> {
        if !class.is_instance || class.declaration.category != Some(6) {
            return None;
        }
        let declaration_path = normalized_path(Path::new(class.declaration.path.as_deref()?));
        let declaration_line = class.declaration.range?.0.line + 1;
        let matches = self
            .models
            .iter()
            .filter(|model| {
                model.source_path == declaration_path
                    && class.declaration.name.as_deref() == Some(model.name.as_str())
                    && model.range.start_line <= declaration_line
                    && declaration_line <= model.range.end_line
            })
            .collect::<Vec<_>>();
        let [model] = matches.as_slice() else {
            return None;
        };
        Some(ResolvedModel {
            target: model.target.clone(),
            table: model.table.clone(),
            model_symbols: BTreeSet::from([model.symbol.clone()]),
            model_paths: BTreeSet::from([model.canonical_path.clone()]),
            columns: model.columns.clone(),
        })
    }
}

#[derive(Clone)]
struct ResolvedModel {
    target: StableKey,
    table: String,
    model_symbols: BTreeSet<StableKey>,
    model_paths: BTreeSet<String>,
    columns: BTreeSet<String>,
}

impl ResolvedModel {
    fn merge(models: Vec<Self>) -> Self {
        let mut models = models.into_iter();
        let mut merged = models.next().expect("non-empty union resolution");
        for model in models {
            merged.model_symbols.extend(model.model_symbols);
            merged.model_paths.extend(model.model_paths);
            merged.columns.extend(model.columns);
        }
        merged
    }
}

enum ModelResolutionError {
    Unknown(String),
    Ambiguous(String),
}

struct InferenceSite {
    line: usize,
    form: TypeWriteForm,
    probe: String,
    inferred_type: String,
    models: Vec<String>,
    column: Option<String>,
    statement_hash: String,
}

impl InferenceSite {
    fn new(
        candidate: &TypeWriteCandidate,
        inferred: &PythonType,
        resolved: &ResolvedModel,
    ) -> Self {
        Self {
            line: candidate.write_range.start_line,
            form: candidate.form,
            probe: candidate.probe.expression.clone(),
            inferred_type: inferred.diagnostic_name(),
            models: resolved.model_paths.iter().cloned().collect(),
            column: candidate.column.clone(),
            statement_hash: candidate.statement_hash.clone(),
        }
    }
}

struct InferenceGroup {
    source: StableKey,
    target: StableKey,
    file: String,
    table: String,
    sites: Vec<InferenceSite>,
}

impl InferenceGroup {
    fn new(source: StableKey, model: ResolvedModel, file: &str) -> Self {
        Self {
            source,
            target: model.target,
            file: file.to_owned(),
            table: model.table,
            sites: Vec::new(),
        }
    }

    fn into_edge(mut self, confidence: Confidence) -> TypeInferenceEdge {
        self.sites.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| left.form.cmp(&right.form))
                .then_with(|| left.probe.cmp(&right.probe))
        });
        let lines = joined(self.sites.iter().map(|site| site.line.to_string()));
        let forms = joined(self.sites.iter().map(|site| format!("{:?}", site.form)));
        let probes = joined(self.sites.iter().map(|site| site.probe.clone()));
        let types = joined(self.sites.iter().map(|site| site.inferred_type.clone()));
        let models = joined(self.sites.iter().flat_map(|site| site.models.clone()));
        let columns = joined(self.sites.iter().filter_map(|site| site.column.clone()));
        let input = joined(self.sites.iter().map(|site| {
            format!(
                "{}:{:?}:{}:{}",
                site.statement_hash,
                site.form,
                site.models.join(","),
                site.column.as_deref().unwrap_or("")
            )
        }));
        let fingerprint = format!(
            "type_inference:{PYRIGHT_PRODUCER}:{}:{:?}:{}",
            self.source,
            RelationKind::WritesTo,
            self.target
        );
        TypeInferenceEdge {
            source: self.source,
            target: self.target,
            relation: RelationKind::WritesTo,
            confidence,
            producer: PYRIGHT_PRODUCER.to_owned(),
            fingerprint,
            source_uri: self.file,
            input_fingerprint: blake3::hash(input.as_bytes()).to_hex().to_string(),
            evidence_locator: format!(
                "lines:{lines} forms:{forms} probes:{probes} resolved_types:{types} models:{models} table:{} columns:{columns}",
                self.table
            ),
        }
    }
}

fn joined(values: impl IntoIterator<Item = String>) -> String {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values.join(",")
}

fn containing_symbol(graph: &GraphSnapshot, candidate: &TypeWriteCandidate) -> Option<StableKey> {
    graph
        .nodes
        .values()
        .filter_map(|node| {
            let PlannedNodeAttributes::Symbol {
                file_path,
                symbol_kind,
                range,
                ..
            } = &node.attributes
            else {
                return None;
            };
            if file_path != &candidate.file_path
                || !matches!(
                    symbol_kind,
                    SymbolKind::Function | SymbolKind::Method | SymbolKind::Test
                )
                || range.start_byte > candidate.write_range.start_byte
                || range.end_byte < candidate.write_range.end_byte
            {
                return None;
            }
            Some((range.end_byte - range.start_byte, node.stable_key.clone()))
        })
        .min_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)))
        .map(|(_, stable_key)| stable_key)
}

fn active_fact_writes(graph: &GraphSnapshot) -> BTreeSet<(StableKey, StableKey)> {
    graph
        .edges
        .iter()
        .filter(|edge| {
            edge.kind == RelationKind::WritesTo
                && edge.claim_class == ClaimClass::Fact
                && edge.status == ClaimStatus::Active
        })
        .map(|edge| (edge.source.clone(), edge.target.clone()))
        .collect()
}

fn sqlalchemy_session_method(
    form: TypeWriteForm,
    inferred: &PythonType,
    session_uris: &BTreeSet<String>,
) -> bool {
    let PythonType::Function(PythonFunctionType {
        declaration,
        bound_to: Some(bound_to),
    }) = inferred
    else {
        return false;
    };
    let expected = match form {
        TypeWriteForm::Add => "add",
        TypeWriteForm::AddAll => "add_all",
        TypeWriteForm::Merge => "merge",
        TypeWriteForm::Delete => "delete",
        TypeWriteForm::AttrAssign => return false,
    };
    declaration.name.as_deref() == Some(expected)
        && declaration.range.is_some()
        && session_uris.contains(&declaration.uri)
        && matches!(bound_to.as_ref(), PythonType::Class(class) if class.is_instance)
}

fn normalized_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn diagnostic(
    candidate: &TypeWriteCandidate,
    reason: TypeInferenceDropReason,
    detail: &str,
    inferred: Option<&PythonType>,
    model: Option<&ResolvedModel>,
) -> TypeInferenceDiagnostic {
    TypeInferenceDiagnostic {
        file: candidate.file_path.clone(),
        line: candidate.write_range.start_line,
        form: Some(candidate.form),
        probe: Some(candidate.probe.expression.clone()),
        inferred_type: inferred.map(PythonType::diagnostic_name),
        model_symbol: model.map(|model| {
            model
                .model_paths
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        }),
        table: model.map(|model| model.table.clone()),
        reason,
        detail: detail.to_owned(),
    }
}
