use std::collections::BTreeMap;

use ctx_core::indexing::{
    IndexStats, plan_incremental_index, reconcile_analysis_versions, reconcile_source_scope,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::{GitRepository, IndexStore, LanguageAnalyzer, PortError};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexReport {
    pub commit: String,
    pub already_current: bool,
    pub stats: IndexStats,
}

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("index inputs have uncommitted changes: {paths}; commit them before indexing")]
    UncommittedInputs { paths: String },
    #[error("repository operation failed: {0}")]
    Git(PortError),
    #[error("source analysis failed: {0}")]
    Analysis(PortError),
    #[error("storage operation failed: {0}")]
    Storage(PortError),
    #[error("could not plan incremental index: {0}")]
    Planning(#[from] ctx_core::indexing::IndexPlanError),
}

pub struct IndexRunner<'a, G, A, S> {
    git: &'a G,
    analyzer: &'a A,
    store: &'a mut S,
}

impl<'a, G, A, S> IndexRunner<'a, G, A, S>
where
    G: GitRepository,
    A: LanguageAnalyzer,
    S: IndexStore,
{
    pub const fn new(git: &'a G, analyzer: &'a A, store: &'a mut S) -> Self {
        Self {
            git,
            analyzer,
            store,
        }
    }

    /// Indexes only source files changed since the last recorded commit.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] when repository inspection, parsing, planning, or
    /// the atomic persistence operation fails.
    pub fn run(&mut self, now: &str) -> Result<IndexReport, IndexError> {
        let uncommitted = self
            .git
            .uncommitted_index_inputs()
            .map_err(IndexError::Git)?;
        if !uncommitted.is_empty() {
            return Err(IndexError::UncommittedInputs {
                paths: uncommitted.join(", "),
            });
        }
        let repository = self.git.descriptor().map_err(IndexError::Git)?;
        self.store
            .ensure_repository(&repository, now)
            .map_err(IndexError::Storage)?;
        let head = self.git.head().map_err(IndexError::Git)?;
        let previous = self
            .store
            .latest_commit(&repository.id)
            .map_err(IndexError::Storage)?;
        let current_paths = self.git.all_source_files().map_err(IndexError::Git)?;
        let changes = previous
            .as_ref()
            .map_or_else(
                || {
                    Ok(current_paths
                        .iter()
                        .cloned()
                        .map(|path| ctx_core::indexing::FileChange::Added { path })
                        .collect())
                },
                |commit| self.git.changes_since(&commit.oid),
            )
            .map_err(IndexError::Git)?;
        let snapshot = self
            .store
            .load_snapshot(&repository.id)
            .map_err(IndexError::Storage)?;
        let changes = reconcile_source_scope(
            &changes,
            snapshot.files.keys().cloned(),
            current_paths.iter().cloned(),
        );
        let expected_versions = current_paths
            .iter()
            .map(|path| {
                self.analyzer
                    .analysis_version(path)
                    .map(|version| (path.clone(), version))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(IndexError::Analysis)?;
        let changes = reconcile_analysis_versions(&changes, &snapshot, &expected_versions);
        if changes.is_empty()
            && previous
                .as_ref()
                .is_some_and(|commit| commit.oid == head.oid)
        {
            return Ok(IndexReport {
                commit: head.oid.to_string(),
                already_current: true,
                stats: IndexStats::default(),
            });
        }
        let mut analyses = BTreeMap::new();
        for path in changes.iter().filter_map(|change| change.current_path()) {
            let file_ir = self.analyzer.analyze(path).map_err(IndexError::Analysis)?;
            analyses.insert(path.to_owned(), file_ir);
        }
        let plan = plan_incremental_index(&snapshot, &analyses, &changes)?;
        self.store
            .apply_index(&repository.id, &head, now, &plan)
            .map_err(IndexError::Storage)?;

        Ok(IndexReport {
            commit: head.oid.to_string(),
            already_current: false,
            stats: plan.stats,
        })
    }
}
