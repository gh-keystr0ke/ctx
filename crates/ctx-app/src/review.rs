use std::collections::BTreeMap;

use ctx_core::{
    domain::RepositoryId,
    indexing::FileChange,
    review::{ReviewInput, ReviewReport, build_review_findings},
};
use thiserror::Error;

use crate::ports::{GraphStore, LanguageAnalyzer, PortError, ReviewRepository};

#[derive(Debug, Error)]
pub enum ReviewError {
    #[error("Git diff could not be read: {0}")]
    Git(PortError),
    #[error("changed source could not be analyzed: {0}")]
    Analysis(PortError),
    #[error("context graph could not be loaded: {0}")]
    Store(PortError),
}

pub struct ReviewRunner<'a, G, A, S> {
    git: &'a G,
    analyzer: &'a A,
    store: &'a S,
}

impl<'a, G, A, S> ReviewRunner<'a, G, A, S>
where
    G: ReviewRepository,
    A: LanguageAnalyzer,
    S: GraphStore,
{
    pub const fn new(git: &'a G, analyzer: &'a A, store: &'a S) -> Self {
        Self {
            git,
            analyzer,
            store,
        }
    }

    /// Reviews a Git diff using only current, stored semantic claims.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError`] when the diff, either source version, or graph
    /// cannot be loaded.
    pub fn run(
        &self,
        repository: &RepositoryId,
        base: &str,
        verbose: bool,
    ) -> Result<ReviewReport, ReviewError> {
        let changes = self.git.review_changes(base).map_err(ReviewError::Git)?;
        let mut before = BTreeMap::new();
        let mut after = BTreeMap::new();
        for change in &changes.source_changes {
            let (old_path, new_path) = change_paths(change);
            if let Some(path) = old_path
                && let Some(source) = self.git.source_at(base, path).map_err(ReviewError::Git)?
            {
                let analysis = self
                    .analyzer
                    .analyze_text(path, &source)
                    .map_err(ReviewError::Analysis)?;
                before.insert(path.to_owned(), analysis);
            }
            if let Some(path) = new_path {
                let analysis = self.analyzer.analyze(path).map_err(ReviewError::Analysis)?;
                after.insert(path.to_owned(), analysis);
            }
        }
        let graph = self
            .store
            .load_graph(repository)
            .map_err(ReviewError::Store)?;
        let input = ReviewInput {
            base: base.to_owned(),
            changes: changes.source_changes,
            before,
            after,
            changed_context_files: changes.changed_context_files.into_iter().collect(),
            verbose,
        };
        Ok(build_review_findings(&graph, &input))
    }
}

fn change_paths(change: &FileChange) -> (Option<&str>, Option<&str>) {
    match change {
        FileChange::Added { path } => (None, Some(path)),
        FileChange::Modified { path } => (Some(path), Some(path)),
        FileChange::Deleted { path } => (Some(path), None),
        FileChange::Renamed { old_path, new_path } => (Some(old_path), Some(new_path)),
    }
}
