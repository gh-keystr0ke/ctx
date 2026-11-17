use std::fs::{File, OpenOptions};

use chrono::Utc;
use ctx_app::{
    context::{ContextImportError, ContextImporter},
    index::{IndexError, IndexReport, IndexRunner},
    ports::{BusinessContextReader, GitRepository, PortError},
    status::{FreshnessState, IndexFreshness, StatusError, index_freshness},
};
use ctx_core::{business::ContextImportStats, domain::RepositoryId};
use serde::Serialize;
use thiserror::Error;

use crate::{
    analyzer::AnalyzerRegistry, business_context::YamlBusinessContextReader, git::GitRepo,
    sqlite::SqliteStore,
};

#[derive(Serialize)]
pub struct LocalIndexReport {
    #[serde(flatten)]
    pub code: IndexReport,
    pub business_context: ContextImportStats,
}

#[derive(Debug, Error)]
pub enum LocalIndexError {
    #[error(transparent)]
    Git(#[from] PortError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Context(#[from] ContextImportError),
    #[error("index transaction failed: {0}")]
    Transaction(#[from] rusqlite::Error),
    #[error("indexing is already in progress")]
    Locked,
    #[error("could not lock the local index: {0}")]
    LockIo(#[from] std::io::Error),
}

/// Takes the advisory lock shared by all local index writers.
///
/// # Errors
/// Returns an error if a writer is already active or the lock cannot be opened.
pub fn lock_index(git: &GitRepo) -> Result<File, LocalIndexError> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(git.root().join(".ctx/index.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(LocalIndexError::Locked),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Indexes committed inputs and synchronizes their context documents.
///
/// # Errors
/// Returns an error if the index is busy, inputs are dirty or cannot be indexed.
pub fn index_local(
    git: &GitRepo,
    store: &mut SqliteStore,
) -> Result<LocalIndexReport, LocalIndexError> {
    let _lock = lock_index(git)?;
    index_locked(git, store)
}

fn index_locked(
    git: &GitRepo,
    store: &mut SqliteStore,
) -> Result<LocalIndexReport, LocalIndexError> {
    let dirty = git.uncommitted_index_inputs()?;
    if !dirty.is_empty() {
        return Err(IndexError::UncommittedInputs {
            paths: dirty.join(", "),
        }
        .into());
    }
    let analyzer = AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?;
    let now = Utc::now().to_rfc3339();
    let reader = YamlBusinessContextReader::new(git.context_root().to_path_buf());
    // Validate documents before advancing the code index.
    let documents = reader.read_all().map_err(ContextImportError::Read)?;
    let repository = git.descriptor()?;
    let head = git.head()?;
    index_transaction(store, |store| {
        let code = IndexRunner::new(git, &analyzer, store).run(&now)?;
        if code.commit != head.oid.as_str() || git.head_oid()? != head.oid {
            return Err(PortError::new("HEAD changed during indexing; retry the command").into());
        }
        let business_context =
            ContextImporter::new(&reader, store).import(&repository, &head, &now, &documents)?;
        Ok(LocalIndexReport {
            code,
            business_context,
        })
    })
}

fn index_transaction<T>(
    store: &mut SqliteStore,
    index: impl FnOnce(&mut SqliteStore) -> Result<T, LocalIndexError>,
) -> Result<T, LocalIndexError> {
    store.connection().execute_batch("BEGIN IMMEDIATE")?;
    let result = index(store).and_then(|report| {
        store.connection().execute_batch("COMMIT")?;
        Ok(report)
    });
    if result.is_err() {
        store.connection().execute_batch("ROLLBACK")?;
    }
    result
}

/// Refreshes an outdated index when policy allows writes and inputs are clean.
///
/// # Errors
/// Returns an error if the current index freshness cannot be determined.
pub fn ensure_fresh(
    git: &GitRepo,
    store: &mut SqliteStore,
    repository: &RepositoryId,
    refresh: bool,
) -> Result<IndexFreshness, StatusError> {
    let mut freshness = index_freshness(git, store, repository)?;
    if matches!(
        freshness.state,
        FreshnessState::Behind | FreshnessState::NotIndexed
    ) && freshness.reason.is_none()
    {
        if refresh {
            match index_local(git, store) {
                Ok(_) => freshness = index_freshness(git, store, repository)?,
                Err(error) => freshness.reason = Some(format!("indexing failed: {error}")),
            }
        } else {
            freshness.reason = Some("automatic refresh is disabled".to_owned());
        }
    }
    Ok(freshness)
}

/// Gives MCP a bounded wait for refresh while the writer completes in the background.
///
/// # Errors
/// Returns an error if Git or the local index cannot be inspected.
pub fn refresh_in_background(
    git: &GitRepo,
    store: &SqliteStore,
    repository: &RepositoryId,
    refresh: bool,
) -> Result<IndexFreshness, StatusError> {
    let mut freshness = index_freshness(git, store, repository)?;
    if !matches!(
        freshness.state,
        FreshnessState::Behind | FreshnessState::NotIndexed
    ) || freshness.reason.is_some()
    {
        return Ok(freshness);
    }
    if !refresh {
        freshness.reason = Some("automatic refresh is disabled for this query".to_owned());
        return Ok(freshness);
    }
    let lock = match lock_index(git) {
        Ok(lock) => lock,
        Err(error) => {
            freshness.reason = Some(error.to_string());
            return Ok(freshness);
        }
    };
    let worker_git = git.clone();
    let (send, received) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("ctx-index".to_owned())
        .spawn(move || {
            let _lock = lock;
            let result = SqliteStore::open(
                &worker_git.root().join(".ctx/ctx.db"),
                worker_git.context_root(),
            )
            .map_err(|error| error.to_string())
            .and_then(|mut store| {
                index_locked(&worker_git, &mut store).map_err(|error| error.to_string())
            });
            let _ = send.send(result);
        });
    freshness.reason = match worker {
        Err(error) => Some(format!("could not start refresh: {error}")),
        Ok(_) => match received.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(_)) => return index_freshness(git, store, repository),
            Ok(Err(error)) => Some(format!("indexing failed: {error}")),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Some("refresh is still running".to_owned())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Some("refresh worker stopped".to_owned())
            }
        },
    };
    Ok(freshness)
}
