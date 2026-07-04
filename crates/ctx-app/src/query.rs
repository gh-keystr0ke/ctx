use ctx_core::{
    context_pack::{ContextCompileError, ContextPack, ContextRequest, compile_context_pack},
    domain::RepositoryId,
    explain::{ExplainError, Explanation, explain},
    graph::{NodeSummary, SymbolMatch, find_requirements, find_symbols},
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

    /// Returns bounded product and implementation impact for every distinct
    /// node the seed resolves to (several exact matches are not an error;
    /// each gets its own independent report — PR-LOOKUP-002/003).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph loading fails or the seed resolves
    /// to nothing.
    pub fn impact(
        &self,
        repository: &RepositoryId,
        target: &str,
    ) -> Result<Vec<ImpactReport>, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        analyze_impact(target, &graph).map_err(QueryError::from)
    }

    /// Explains every node the query resolves to (or the single directed
    /// relationship it names) from persisted evidence. Several exact matches
    /// are not an error; each gets its own independent explanation
    /// (PR-LOOKUP-002/003).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph loading fails or the query resolves
    /// to nothing.
    pub fn explain(
        &self,
        repository: &RepositoryId,
        target: &str,
    ) -> Result<Vec<Explanation>, QueryError> {
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

    /// Finds product requirements by ID or terms.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph state cannot be loaded.
    pub fn find_requirements(
        &self,
        repository: &RepositoryId,
        query: &str,
    ) -> Result<Vec<NodeSummary>, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        Ok(find_requirements(query, &graph))
    }

    /// Discovery lookup for a short or exact name: every distinct match,
    /// with no ambiguity error (PR-LOOKUP-007).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] when graph state cannot be loaded.
    pub fn find(
        &self,
        repository: &RepositoryId,
        query: &str,
    ) -> Result<Vec<SymbolMatch>, QueryError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(QueryError::Store)?;
        Ok(find_symbols(query, &graph))
    }
}
