use ctx_core::{
    context_pack::{ContextCompileError, ContextPack, ContextRequest, compile_context_pack},
    domain::RepositoryId,
    explain::{ExplainError, Explanation, explain},
    impact::{ImpactError, ImpactReport, analyze_impact},
};
use thiserror::Error;

use crate::ports::{GraphStore, PortError};

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("graph could not be loaded: {0}")]
    Store(PortError),
    #[error(transparent)]
    Impact(#[from] ImpactError),
    #[error(transparent)]
    Explain(#[from] ExplainError),
    #[error(transparent)]
    Context(#[from] ContextCompileError),
}

pub struct QueryService<'a, S> {
    store: &'a S,
}

impl<'a, S> QueryService<'a, S>
where
    S: GraphStore,
{
    pub const fn new(store: &'a S) -> Self {
        Self { store }
    }

    /// Returns bounded product and implementation impact for one seed.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph loading or seed resolution fails.
    pub fn impact(
        &self,
        repository: &RepositoryId,
        target: &str,
    ) -> Result<ImpactReport, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        analyze_impact(target, &graph).map_err(QueryError::from)
    }

    /// Explains one node or directed relationship from persisted evidence.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph loading or claim resolution fails.
    pub fn explain(
        &self,
        repository: &RepositoryId,
        target: &str,
    ) -> Result<Explanation, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        explain(target, &graph).map_err(QueryError::from)
    }

    /// Compiles a bounded context pack from task and explicit seeds.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph loading or context compilation fails.
    pub fn context(
        &self,
        repository: &RepositoryId,
        request: &ContextRequest,
    ) -> Result<ContextPack, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        compile_context_pack(&graph, request).map_err(QueryError::from)
    }
}
