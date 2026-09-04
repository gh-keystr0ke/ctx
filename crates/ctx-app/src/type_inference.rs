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
    pub import_queries: usize,
    pub pyright_query_ms: u128,
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
            let mut context = FileInferenceContext {
                graph: &graph,
                models: &models,
                fact_writes: &fact_writes,
                relative_path: &path,
                absolute_path: Path::new(&repository.root_path).join(&path),
                session_uris: None,
                report: &mut report,
                groups: &mut groups,
            };
            for candidate in extracted {
                self.process_candidate(&candidate, &mut context)?;
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

    fn process_candidate(
        &mut self,
        candidate: &TypeWriteCandidate,
        context: &mut FileInferenceContext<'_>,
    ) -> Result<(), InferTypesError> {
        let Some(owner) = containing_symbol(context.graph, candidate) else {
            context.report.dropped_unsupported += 1;
            context.report.diagnostics.push(diagnostic(
                candidate,
                TypeInferenceDropReason::MissingSourceOwner,
                "no indexed containing function or method",
                None,
                None,
            ));
            return Ok(());
        };
        let Some(inferred) = self.query_type(
            &context.absolute_path,
            &candidate.probe,
            candidate,
            context.report,
        )?
        else {
            return Ok(());
        };
        let Some(resolved) =
            resolve_candidate_model(context.models, candidate, &inferred, context.report)
        else {
            return Ok(());
        };
        if !self.operation_is_supported(candidate, &inferred, &resolved, context)? {
            return Ok(());
        }
        if context
            .fact_writes
            .contains(&(owner.clone(), resolved.target.clone()))
        {
            context.report.suppressed_by_fact += 1;
            context.report.diagnostics.push(diagnostic(
                candidate,
                TypeInferenceDropReason::SuppressedByFact,
                "an active Fact already proves this symbol-to-table write",
                Some(&inferred),
                Some(&resolved),
            ));
            return Ok(());
        }
        context
            .groups
            .entry((owner.clone(), resolved.target.clone()))
            .or_insert_with(|| InferenceGroup::new(owner, resolved.clone(), context.relative_path))
            .sites
            .push(InferenceSite::new(candidate, &inferred, &resolved));
        Ok(())
    }

    fn operation_is_supported(
        &mut self,
        candidate: &TypeWriteCandidate,
        inferred: &PythonType,
        resolved: &ResolvedModel,
        context: &mut FileInferenceContext<'_>,
    ) -> Result<bool, InferTypesError> {
        if candidate.form == TypeWriteForm::AttrAssign {
            let column = candidate.column.as_deref().unwrap_or_default();
            if resolved.columns.contains(column) {
                return Ok(true);
            }
            context.report.dropped_unsupported += 1;
            context.report.diagnostics.push(diagnostic(
                candidate,
                TypeInferenceDropReason::UnsupportedOperation,
                "attribute is not a statically known mapped column",
                Some(inferred),
                Some(resolved),
            ));
            return Ok(false);
        }

        if context.session_uris.is_none() {
            context.session_uris = Some(self.resolve_session_uris(
                &context.absolute_path,
                candidate,
                context.report,
            )?);
        }
        let Some(method_probe) = candidate.method_probe.as_ref() else {
            context.report.dropped_unsupported += 1;
            context.report.diagnostics.push(diagnostic(
                candidate,
                TypeInferenceDropReason::UnsupportedOperation,
                "unit-of-work candidate is missing its method probe",
                None,
                Some(resolved),
            ));
            return Ok(false);
        };
        let Some(method_type) = self.query_type(
            &context.absolute_path,
            method_probe,
            candidate,
            context.report,
        )?
        else {
            return Ok(false);
        };
        let supported = context
            .session_uris
            .as_ref()
            .is_some_and(|uris| sqlalchemy_session_method(candidate.form, &method_type, uris));
        if !supported {
            let session_uris = context
                .session_uris
                .as_ref()
                .map(|uris| uris.iter().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            context.report.dropped_unsupported += 1;
            context.report.diagnostics.push(diagnostic(
                candidate,
                TypeInferenceDropReason::UnsupportedOperation,
                &format!(
                    "method declaration does not match resolved SQLAlchemy Session API modules [{session_uris}]"
                ),
                Some(&method_type),
                Some(resolved),
            ));
        }
        Ok(supported)
    }

    fn query_type(
        &mut self,
        file: &Path,
        probe: &ctx_core::type_inference::TypeProbe,
        candidate: &TypeWriteCandidate,
        report: &mut InferTypesReport,
    ) -> Result<Option<PythonType>, InferTypesError> {
        report.type_queries += 1;
        let started = Instant::now();
        let result = self.oracle.inferred_type(file, probe);
        report.pyright_query_ms += started.elapsed().as_millis();
        match result {
            Ok(inferred) => Ok(Some(inferred)),
            Err(error) => {
                self.record_oracle_failure(candidate, report, &error)?;
                Ok(None)
            }
        }
    }

    fn resolve_session_uris(
        &mut self,
        file: &Path,
        candidate: &TypeWriteCandidate,
        report: &mut InferTypesReport,
    ) -> Result<BTreeSet<String>, InferTypesError> {
        let mut uris = BTreeSet::new();
        for module in ["sqlalchemy.orm.session", "sqlalchemy.ext.asyncio.session"] {
            report.import_queries += 1;
            let started = Instant::now();
            let result = self.oracle.resolve_import(file, module);
            report.pyright_query_ms += started.elapsed().as_millis();
            match result {
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

struct FileInferenceContext<'a> {
    graph: &'a GraphSnapshot,
    models: &'a ModelIndex,
    fact_writes: &'a BTreeSet<(StableKey, StableKey)>,
    relative_path: &'a str,
    absolute_path: PathBuf,
    session_uris: Option<BTreeSet<String>>,
    report: &'a mut InferTypesReport,
    groups: &'a mut BTreeMap<(StableKey, StableKey), InferenceGroup>,
}

fn resolve_candidate_model(
    models: &ModelIndex,
    candidate: &TypeWriteCandidate,
    inferred: &PythonType,
    report: &mut InferTypesReport,
) -> Option<ResolvedModel> {
    match models.resolve(inferred) {
        Ok(resolved) => {
            report.resolved_model_candidates += 1;
            Some(resolved)
        }
        Err(ModelResolutionError::Unknown(detail)) => {
            report.dropped_unknown += 1;
            report.diagnostics.push(diagnostic(
                candidate,
                TypeInferenceDropReason::UnknownType,
                &detail,
                Some(inferred),
                None,
            ));
            None
        }
        Err(ModelResolutionError::Ambiguous(detail)) => {
            report.dropped_ambiguous += 1;
            report.diagnostics.push(diagnostic(
                candidate,
                TypeInferenceDropReason::AmbiguousType,
                &detail,
                Some(inferred),
                None,
            ));
            None
        }
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
            merged
                .columns
                .retain(|column| model.columns.contains(column));
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
    let PythonType::Function(PythonFunctionType { declaration, .. }) = inferred else {
        return false;
    };
    let expected = match form {
        TypeWriteForm::Add => "add",
        TypeWriteForm::AddAll => "add_all",
        TypeWriteForm::Merge => "merge",
        TypeWriteForm::Delete => "delete",
        TypeWriteForm::AttrAssign => return false,
    };
    declaration.name.as_deref() == Some(expected) && session_uris.contains(&declaration.uri)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_core::{
        domain::{CommitOid, RepositoryId, SourceKind},
        graph::{GraphEdge, GraphNode},
        indexing::{IndexPlan, RepositorySnapshot},
        ir::{SchemaColumn, SchemaTableDefinition},
        type_inference::PythonDeclaration,
    };

    use crate::ports::{CommitMetadata, RepositoryDescriptor, RepositoryStatus, SourceScope};

    #[test]
    fn typed_columns_emit_while_any_unions_and_properties_drop() {
        let candidates = vec![
            candidate(TypeWriteForm::AttrAssign, "row", 101, Some("status")),
            candidate(TypeWriteForm::AttrAssign, "row", 102, Some("some_property")),
            candidate(TypeWriteForm::AttrAssign, "any", 103, Some("status")),
            candidate(TypeWriteForm::AttrAssign, "union", 104, Some("status")),
        ];
        let mut graph = graph_fixture();
        add_model(&mut graph, "BarDB", "models.py", "bars", &["status"]);
        let mut oracle = FakeOracle::default();
        oracle
            .types
            .insert("row".to_owned(), model_type("Model", "models.py", 1));
        oracle.types.insert("any".to_owned(), PythonType::Any);
        oracle.types.insert(
            "union".to_owned(),
            PythonType::Union {
                members: vec![
                    model_type("Model", "models.py", 1),
                    model_type("BarDB", "models.py", 1),
                ],
            },
        );
        let (report, edges) = run(candidates, graph, oracle).expect("inference run");

        assert_eq!(report.candidate_sites, 4);
        assert_eq!(report.resolved_model_candidates, 2);
        assert_eq!(report.dropped_unknown, 1);
        assert_eq!(report.dropped_ambiguous, 1);
        assert_eq!(report.dropped_unsupported, 1);
        assert_eq!(report.inferences_created, 1);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].source.as_str(),
            "symbol:python:service.update:Function"
        );
        assert_eq!(edges[0].target.as_str(), "db:models");
        assert!(edges[0].evidence_locator.contains("columns:status"));
    }

    #[test]
    fn same_table_model_union_is_allowed_only_for_columns_common_to_all_models() {
        let mut graph = graph_fixture();
        add_model(
            &mut graph,
            "CompatibleModel",
            "compatible.py",
            "models",
            &["status"],
        );
        let candidates = vec![candidate(
            TypeWriteForm::AttrAssign,
            "compatible_union",
            101,
            Some("status"),
        )];
        let mut oracle = FakeOracle::default();
        oracle.types.insert(
            "compatible_union".to_owned(),
            PythonType::Union {
                members: vec![
                    model_type("Model", "models.py", 1),
                    model_type("CompatibleModel", "compatible.py", 1),
                ],
            },
        );
        let (report, edges) = run(candidates, graph, oracle).expect("same-table union");

        assert_eq!(report.dropped_ambiguous, 0);
        assert_eq!(edges.len(), 1);
        assert!(edges[0].evidence_locator.contains("CompatibleModel"));
        assert!(edges[0].evidence_locator.contains("Model"));
    }

    #[test]
    fn unit_of_work_requires_sqlalchemy_method_identity_and_model_argument() {
        let candidates = vec![
            call_candidate(TypeWriteForm::Add, "model", "session.add", 101),
            call_candidate(TypeWriteForm::Add, "model", "legacy_session.add", 102),
            call_candidate(TypeWriteForm::Add, "model", "collection.add", 103),
            call_candidate(TypeWriteForm::Add, "name", "session.add", 104),
        ];
        let mut oracle = FakeOracle::default();
        oracle
            .types
            .insert("model".to_owned(), model_type("Model", "models.py", 1));
        oracle.types.insert(
            "name".to_owned(),
            PythonType::Other {
                oracle_kind: "str".to_owned(),
            },
        );
        oracle.types.insert(
            "session.add".to_owned(),
            method_type("add", "file:///site/sqlalchemy/orm/session.py", true),
        );
        oracle.types.insert(
            "legacy_session.add".to_owned(),
            method_type("add", "file:///site/sqlalchemy/orm/session.py", false),
        );
        oracle.types.insert(
            "collection.add".to_owned(),
            method_type("add", "file:///stdlib/collections.pyi", false),
        );
        let (report, edges) = run(candidates, graph_fixture(), oracle).expect("inference run");

        assert_eq!(report.candidate_sites, 4);
        assert_eq!(report.dropped_unknown, 1);
        assert_eq!(report.dropped_unsupported, 1);
        assert_eq!(edges.len(), 1);
        assert!(edges[0].evidence_locator.contains("forms:Add"));
    }

    #[test]
    fn active_fact_suppresses_the_same_symbol_table_inference() {
        let mut graph = graph_fixture();
        graph.edges.push(GraphEdge {
            source: key("symbol:python:service.update:Function"),
            target: key("db:models"),
            kind: RelationKind::WritesTo,
            claim_class: ClaimClass::Fact,
            source_kind: SourceKind::StaticAnalysis,
            confidence: Confidence::CERTAIN,
            status: ClaimStatus::Active,
            valid_from: "deadbeef".to_owned(),
            valid_to: None,
            producer: "python_tree_sitter".to_owned(),
            fingerprint: "fact-write".to_owned(),
            stale_reason: None,
            evidence: Vec::new(),
        });
        let candidates = vec![candidate(
            TypeWriteForm::AttrAssign,
            "row",
            101,
            Some("status"),
        )];
        let mut oracle = FakeOracle::default();
        oracle
            .types
            .insert("row".to_owned(), model_type("Model", "models.py", 1));
        let (report, edges) = run(candidates, graph, oracle).expect("inference run");

        assert_eq!(report.suppressed_by_fact, 1);
        assert!(edges.is_empty());
    }

    #[test]
    fn catastrophic_oracle_failure_never_replaces_the_stored_layer() {
        let candidates = vec![candidate(
            TypeWriteForm::AttrAssign,
            "crash",
            101,
            Some("status"),
        )];
        let oracle = FakeOracle {
            failed_probe: Some("crash".to_owned()),
            healthy: false,
            ..FakeOracle::default()
        };
        let git = FakeGit;
        let source = FakeCandidates { candidates };
        let mut oracle = oracle;
        let mut store = FakeStore::new(graph_fixture());
        let confidence = Confidence::new(DEFAULT_TYPE_INFERENCE_CONFIDENCE).expect("confidence");
        let error = InferTypesRunner::new(&git, &source, &mut oracle, &mut store, confidence)
            .run("2026-09-04T00:00:01Z")
            .expect_err("fatal oracle failure");

        assert!(matches!(error, InferTypesError::OracleFatal(_)));
        assert_eq!(store.replacements, 0);
    }

    fn run(
        candidates: Vec<TypeWriteCandidate>,
        graph: GraphSnapshot,
        mut oracle: FakeOracle,
    ) -> Result<(InferTypesReport, Vec<TypeInferenceEdge>), InferTypesError> {
        let git = FakeGit;
        let source = FakeCandidates { candidates };
        let mut store = FakeStore::new(graph);
        let confidence = Confidence::new(DEFAULT_TYPE_INFERENCE_CONFIDENCE).expect("confidence");
        let report = InferTypesRunner::new(&git, &source, &mut oracle, &mut store, confidence)
            .run("2026-09-04T00:00:01Z")?;
        Ok((report, store.persisted))
    }

    fn graph_fixture() -> GraphSnapshot {
        let mut graph = GraphSnapshot::default();
        graph.nodes.insert(
            key("symbol:python:service.update:Function"),
            symbol_node(
                "symbol:python:service.update:Function",
                "update",
                "service.py",
                "service.update",
                SymbolKind::Function,
                source_range(100, 900, 10, 90),
                Vec::new(),
            ),
        );
        add_model(
            &mut graph,
            "Model",
            "models.py",
            "models",
            &["id", "status"],
        );
        graph
    }

    fn add_model(graph: &mut GraphSnapshot, name: &str, file: &str, table: &str, columns: &[&str]) {
        let model_key = key(&format!("symbol:python:{name}:Class"));
        graph.nodes.insert(
            model_key.clone(),
            symbol_node(
                model_key.as_str(),
                name,
                file,
                name,
                SymbolKind::Class,
                source_range(0, 90, 1, 9),
                vec![SchemaTableDefinition {
                    entity: table.to_owned(),
                    columns: columns
                        .iter()
                        .map(|column| SchemaColumn {
                            name: (*column).to_owned(),
                            ..SchemaColumn::default()
                        })
                        .collect(),
                    ..SchemaTableDefinition::default()
                }],
            ),
        );
        let database_key = key(&format!("db:{table}"));
        graph.nodes.insert(
            database_key.clone(),
            GraphNode {
                stable_key: database_key,
                kind: NodeKind::DbEntity,
                name: table.to_owned(),
                content_hash: format!("db-entity:{table}"),
                attributes: PlannedNodeAttributes::Interaction {
                    identifier: table.to_owned(),
                },
            },
        );
    }

    fn symbol_node(
        stable_key: &str,
        name: &str,
        file: &str,
        canonical_path: &str,
        symbol_kind: SymbolKind,
        range: SourceRange,
        schema_tables: Vec<SchemaTableDefinition>,
    ) -> GraphNode {
        GraphNode {
            stable_key: key(stable_key),
            kind: NodeKind::CodeSymbol,
            name: name.to_owned(),
            content_hash: "body".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: file.to_owned(),
                canonical_path: canonical_path.to_owned(),
                symbol_kind,
                range,
                signature: None,
                structural_fingerprint: "shape".to_owned(),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                orm_accesses: Vec::new(),
                schema_tables,
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            },
        }
    }

    fn candidate(
        form: TypeWriteForm,
        expression: &str,
        byte: usize,
        column: Option<&str>,
    ) -> TypeWriteCandidate {
        TypeWriteCandidate {
            file_path: "service.py".to_owned(),
            form,
            probe: probe(expression),
            method_probe: None,
            column: column.map(str::to_owned),
            write_range: source_range(byte, byte + 10, byte, byte),
            statement_hash: format!("hash-{byte}"),
        }
    }

    fn call_candidate(
        form: TypeWriteForm,
        expression: &str,
        method: &str,
        byte: usize,
    ) -> TypeWriteCandidate {
        TypeWriteCandidate {
            method_probe: Some(probe(method)),
            ..candidate(form, expression, byte, None)
        }
    }

    fn probe(expression: &str) -> ctx_core::type_inference::TypeProbe {
        ctx_core::type_inference::TypeProbe {
            expression: expression.to_owned(),
            range: source_range(0, expression.len(), 1, 1),
            start: ctx_core::type_inference::TypePosition {
                line: 0,
                character: 0,
            },
            end: ctx_core::type_inference::TypePosition {
                line: 0,
                character: expression.len(),
            },
        }
    }

    fn model_type(name: &str, file: &str, line: usize) -> PythonType {
        PythonType::Class(PythonClassType {
            declaration: declaration(name, file, line, 6),
            is_instance: true,
            type_arguments: Vec::new(),
        })
    }

    fn method_type(name: &str, uri: &str, bound: bool) -> PythonType {
        PythonType::Function(PythonFunctionType {
            declaration: PythonDeclaration {
                uri: uri.to_owned(),
                path: None,
                name: Some(name.to_owned()),
                range: bound.then_some((
                    ctx_core::type_inference::TypePosition {
                        line: 10,
                        character: 4,
                    },
                    ctx_core::type_inference::TypePosition {
                        line: 10,
                        character: 7,
                    },
                )),
                category: Some(5),
            },
            bound_to: bound.then(|| {
                Box::new(PythonType::Class(PythonClassType {
                    declaration: declaration("Session", "session.py", 1, 6),
                    is_instance: true,
                    type_arguments: Vec::new(),
                }))
            }),
        })
    }

    fn declaration(name: &str, file: &str, line: usize, category: u8) -> PythonDeclaration {
        PythonDeclaration {
            uri: format!("file:///repo/{file}"),
            path: Some(format!("/repo/{file}")),
            name: Some(name.to_owned()),
            range: Some((
                ctx_core::type_inference::TypePosition {
                    line: line - 1,
                    character: 0,
                },
                ctx_core::type_inference::TypePosition {
                    line: line - 1,
                    character: name.len(),
                },
            )),
            category: Some(category),
        }
    }

    const fn source_range(
        start_byte: usize,
        end_byte: usize,
        start_line: usize,
        end_line: usize,
    ) -> SourceRange {
        SourceRange {
            start_byte,
            end_byte,
            start_line,
            end_line,
        }
    }

    fn key(value: &str) -> StableKey {
        StableKey::new(value).expect("stable key")
    }

    struct FakeGit;

    impl GitRepository for FakeGit {
        fn descriptor(&self) -> Result<RepositoryDescriptor, PortError> {
            Ok(RepositoryDescriptor {
                id: RepositoryId::new("repo:test").expect("repository"),
                root_path: "/repo".to_owned(),
                remote_url: None,
            })
        }

        fn head(&self) -> Result<CommitMetadata, PortError> {
            Ok(commit())
        }

        fn all_source_files(&self) -> Result<Vec<String>, PortError> {
            Ok(vec!["service.py".to_owned()])
        }

        fn changes_since(
            &self,
            _oid: &CommitOid,
        ) -> Result<Vec<ctx_core::indexing::FileChange>, PortError> {
            Ok(Vec::new())
        }

        fn uncommitted_index_inputs(&self) -> Result<Vec<String>, PortError> {
            Ok(Vec::new())
        }

        fn source_scope(&self) -> SourceScope {
            SourceScope::default()
        }
    }

    struct FakeCandidates {
        candidates: Vec<TypeWriteCandidate>,
    }

    impl PythonTypeCandidateExtractor for FakeCandidates {
        fn candidates(&self, _relative_path: &str) -> Result<Vec<TypeWriteCandidate>, PortError> {
            Ok(self.candidates.clone())
        }
    }

    #[derive(Default)]
    struct FakeOracle {
        types: BTreeMap<String, PythonType>,
        failed_probe: Option<String>,
        healthy: bool,
    }

    impl PythonTypeOracle for FakeOracle {
        fn inferred_type(
            &mut self,
            _file: &Path,
            probe: &ctx_core::type_inference::TypeProbe,
        ) -> Result<PythonType, PortError> {
            if self.failed_probe.as_deref() == Some(probe.expression.as_str()) {
                return Err(PortError::new("oracle query failed"));
            }
            self.types
                .get(&probe.expression)
                .cloned()
                .ok_or_else(|| PortError::new(format!("no type for {}", probe.expression)))
        }

        fn resolve_import(
            &mut self,
            _from_file: &Path,
            module: &str,
        ) -> Result<Option<String>, PortError> {
            Ok(match module {
                "sqlalchemy.orm.session" => {
                    Some("file:///site/sqlalchemy/orm/session.py".to_owned())
                }
                "sqlalchemy.ext.asyncio.session" => {
                    Some("file:///site/sqlalchemy/ext/asyncio/session.py".to_owned())
                }
                _ => None,
            })
        }

        fn is_healthy(&mut self) -> bool {
            self.healthy || self.failed_probe.is_none()
        }
    }

    struct FakeStore {
        graph: GraphSnapshot,
        persisted: Vec<TypeInferenceEdge>,
        replacements: usize,
    }

    impl FakeStore {
        fn new(graph: GraphSnapshot) -> Self {
            Self {
                graph,
                persisted: Vec::new(),
                replacements: 0,
            }
        }
    }

    impl GraphStore for FakeStore {
        fn load_graph(&self, _repository: &RepositoryId) -> Result<GraphSnapshot, PortError> {
            Ok(self.graph.clone())
        }
    }

    impl IndexStore for FakeStore {
        fn ensure_repository(
            &mut self,
            _repository: &RepositoryDescriptor,
            _created_at: &str,
        ) -> Result<(), PortError> {
            Ok(())
        }

        fn latest_commit(
            &self,
            _repository: &RepositoryId,
        ) -> Result<Option<CommitMetadata>, PortError> {
            Ok(Some(commit()))
        }

        fn load_snapshot(
            &self,
            _repository: &RepositoryId,
        ) -> Result<RepositorySnapshot, PortError> {
            Ok(RepositorySnapshot::default())
        }

        fn apply_index(
            &mut self,
            _repository: &RepositoryId,
            _commit: &CommitMetadata,
            _indexed_at: &str,
            _plan: &IndexPlan,
        ) -> Result<(), PortError> {
            Ok(())
        }

        fn status(&self, _repository: &RepositoryId) -> Result<RepositoryStatus, PortError> {
            Ok(RepositoryStatus::default())
        }
    }

    impl TypeInferenceStore for FakeStore {
        fn replace_type_inferences(
            &mut self,
            _repository: &RepositoryId,
            _commit: &CommitMetadata,
            _inferred_at: &str,
            _producer: &str,
            edges: &[TypeInferenceEdge],
        ) -> Result<TypeInferencePersistenceStats, PortError> {
            self.replacements += 1;
            self.persisted = edges.to_vec();
            Ok(TypeInferencePersistenceStats {
                created: edges.len(),
                updated: 0,
                removed: 0,
            })
        }
    }

    fn commit() -> CommitMetadata {
        CommitMetadata {
            oid: CommitOid::new("deadbeef").expect("commit"),
            parent_oid: None,
            authored_at: "2026-09-04T00:00:00Z".to_owned(),
        }
    }
}
