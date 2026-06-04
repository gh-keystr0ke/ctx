use ctx_core::{
    domain::RepositoryId,
    verification::{SemanticCandidate, VerificationDecision, semantic_candidates},
};
use thiserror::Error;

use crate::ports::{CommitMetadata, GraphStore, PortError, VerificationStore};

#[derive(Debug, Error)]
pub enum VerificationError {
    #[error("verification candidates could not be loaded: {0}")]
    Store(PortError),
    #[error("verification candidate '{0}' was not found")]
    CandidateNotFound(String),
}

pub struct VerificationService<'a, S> {
    store: &'a mut S,
}

impl<'a, S> VerificationService<'a, S>
where
    S: GraphStore + VerificationStore,
{
    pub const fn new(store: &'a mut S) -> Self {
        Self { store }
    }

    /// Returns deterministic, impact-prioritized semantic candidates.
    ///
    /// # Errors
    ///
    /// Returns [`VerificationError`] when current graph state cannot be loaded.
    pub fn candidates(
        &self,
        repository: &RepositoryId,
    ) -> Result<Vec<SemanticCandidate>, VerificationError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(VerificationError::Store)?;
        Ok(semantic_candidates(&graph))
    }

    /// Records a decision for a current candidate.
    ///
    /// # Errors
    ///
    /// Returns [`VerificationError`] when the fingerprint is no longer a
    /// current candidate or persistence fails.
    pub fn decide(
        &mut self,
        repository: &RepositoryId,
        commit: &CommitMetadata,
        fingerprint: &str,
        decision: VerificationDecision,
        author: &str,
        timestamp: &str,
    ) -> Result<(), VerificationError> {
        let candidate = self
            .candidates(repository)?
            .into_iter()
            .find(|candidate| candidate.fingerprint == fingerprint)
            .ok_or_else(|| VerificationError::CandidateNotFound(fingerprint.to_owned()))?;
        self.store
            .record_verification(repository, commit, &candidate, decision, author, timestamp)
            .map_err(VerificationError::Store)
    }
}
