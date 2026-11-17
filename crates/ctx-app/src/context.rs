use ctx_core::business::{BusinessDocument, ContextImportStats};
use thiserror::Error;

use crate::ports::{
    BusinessContextReader, BusinessContextStore, CommitMetadata, PortError, RepositoryDescriptor,
};

#[derive(Debug, Error)]
pub enum ContextImportError {
    #[error("business context could not be read: {0}")]
    Read(PortError),
    #[error("business context could not be stored: {0}")]
    Store(PortError),
}

pub struct ContextImporter<'a, R, S> {
    reader: &'a R,
    store: &'a mut S,
}

impl<'a, R, S> ContextImporter<'a, R, S>
where
    R: BusinessContextReader,
    S: BusinessContextStore,
{
    pub const fn new(reader: &'a R, store: &'a mut S) -> Self {
        Self { reader, store }
    }

    /// Imports the full repository-owned business-context snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ContextImportError`] when reading or the atomic store update
    /// fails.
    pub fn run(
        &mut self,
        repository: &RepositoryDescriptor,
        commit: &CommitMetadata,
        now: &str,
    ) -> Result<ContextImportStats, ContextImportError> {
        let documents = self.reader.read_all().map_err(ContextImportError::Read)?;
        self.import(repository, commit, now, &documents)
    }

    /// Imports an already validated snapshot.
    ///
    /// # Errors
    /// Returns [`ContextImportError`] when the store update fails.
    pub fn import(
        &mut self,
        repository: &RepositoryDescriptor,
        commit: &CommitMetadata,
        now: &str,
        documents: &[BusinessDocument],
    ) -> Result<ContextImportStats, ContextImportError> {
        self.store
            .sync_context(&repository.id, commit, now, documents)
            .map_err(ContextImportError::Store)
    }
}
