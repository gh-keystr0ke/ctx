use ctx_core::domain::CommitOid;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::{GitRepository, IndexStore, PortError, RepositoryStatus, SourceScope};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    NotIndexed,
    Behind,
    Current,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusHealth {
    Ready,
    NeedsIndex,
    NeedsContext,
    NeedsMappings,
    NeedsAttention,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StatusReport {
    pub repository: String,
    pub head_commit: CommitOid,
    pub index_state: IndexState,
    pub health: StatusHealth,
    pub source_scope: SourceScope,
    pub uncommitted_index_inputs: Vec<String>,
    pub knowledge: RepositoryStatus,
    pub notices: Vec<String>,
    pub suggested_actions: Vec<String>,
}

#[derive(Debug, Error)]
pub enum StatusError {
    #[error("repository operation failed: {0}")]
    Git(PortError),
    #[error("storage operation failed: {0}")]
    Storage(PortError),
}

pub struct StatusService<'a, G, S> {
    git: &'a G,
    store: &'a S,
}

impl<'a, G, S> StatusService<'a, G, S>
where
    G: GitRepository,
    S: IndexStore,
{
    pub const fn new(git: &'a G, store: &'a S) -> Self {
        Self { git, store }
    }

    /// Inspects index freshness and whether the graph contains actionable
    /// product context rather than reporting node/edge vanity metrics alone.
    ///
    /// # Errors
    ///
    /// Returns [`StatusError`] when Git or the local store cannot be read.
    pub fn inspect(&self) -> Result<StatusReport, StatusError> {
        let repository = self.git.descriptor().map_err(StatusError::Git)?;
        let head = self.git.head().map_err(StatusError::Git)?;
        let dirty = self
            .git
            .uncommitted_index_inputs()
            .map_err(StatusError::Git)?;
        let knowledge = self
            .store
            .status(&repository.id)
            .map_err(StatusError::Storage)?;
        Ok(build_report(
            repository.root_path,
            head.oid,
            self.git.source_scope(),
            dirty,
            knowledge,
        ))
    }
}

fn build_report(
    repository: String,
    head_commit: CommitOid,
    source_scope: SourceScope,
    uncommitted_index_inputs: Vec<String>,
    knowledge: RepositoryStatus,
) -> StatusReport {
    let index_state = match &knowledge.last_indexed_commit {
        None => IndexState::NotIndexed,
        Some(indexed) if indexed == &head_commit => IndexState::Current,
        Some(_) => IndexState::Behind,
    };
    let health = classify_health(index_state, &knowledge);
    let (notices, suggested_actions) =
        diagnostics(index_state, health, &knowledge, &uncommitted_index_inputs);
    StatusReport {
        repository,
        head_commit,
        index_state,
        health,
        source_scope,
        uncommitted_index_inputs,
        knowledge,
        notices,
        suggested_actions,
    }
}

fn classify_health(index_state: IndexState, knowledge: &RepositoryStatus) -> StatusHealth {
    if index_state != IndexState::Current {
        return StatusHealth::NeedsIndex;
    }
    if knowledge.stale_semantic_edges > 0 {
        return StatusHealth::NeedsAttention;
    }
    if product_document_count(knowledge) == 0 {
        return StatusHealth::NeedsContext;
    }
    if knowledge.active_assertions == 0 {
        return StatusHealth::NeedsMappings;
    }
    StatusHealth::Ready
}

fn diagnostics(
    index_state: IndexState,
    health: StatusHealth,
    knowledge: &RepositoryStatus,
    dirty: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut notices = Vec::new();
    let mut actions = Vec::new();
    match index_state {
        IndexState::NotIndexed => {
            notices.push("No commit has been indexed.".to_owned());
            actions.push("Run `ctx index` after committing index inputs.".to_owned());
        }
        IndexState::Behind => {
            notices.push("The graph is behind repository HEAD.".to_owned());
            actions.push("Run `ctx index` to apply committed changes.".to_owned());
        }
        IndexState::Current => {}
    }
    if !dirty.is_empty() {
        notices.push(format!(
            "{} index input(s) differ from HEAD; the stored graph still describes the committed state.",
            dirty.len()
        ));
        actions.push("Run `ctx review --base HEAD` before committing the working diff.".to_owned());
    }
    match health {
        StatusHealth::NeedsContext => {
            notices.push(
                "No Feature, Requirement, Invariant, or Decision documents are indexed.".to_owned(),
            );
            actions.push("Add a few high-value documents under `.context/`.".to_owned());
        }
        StatusHealth::NeedsMappings => {
            notices.push(
                "Product documents exist, but no active assertions map them to code or tests."
                    .to_owned(),
            );
            actions.push("Add exact `implementation` and `tests` symbol mappings.".to_owned());
        }
        StatusHealth::NeedsAttention => {
            notices.push(format!(
                "{} semantic relationship(s) are stale.",
                knowledge.stale_semantic_edges
            ));
            actions.push(
                "Inspect stale claims with `ctx explain` and refresh their mappings.".to_owned(),
            );
        }
        StatusHealth::Ready | StatusHealth::NeedsIndex => {}
    }
    (notices, actions)
}

const fn product_document_count(status: &RepositoryStatus) -> usize {
    status.features + status.requirements + status.invariants + status.decisions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(value: &str) -> CommitOid {
        CommitOid::new(value).expect("commit OID")
    }

    fn report(knowledge: RepositoryStatus) -> StatusReport {
        build_report(
            "/repo".to_owned(),
            oid("aaaaaaaa"),
            SourceScope {
                language: "python".to_owned(),
                include: vec!["src".to_owned()],
                exclude: Vec::new(),
            },
            Vec::new(),
            knowledge,
        )
    }

    #[test]
    fn structural_only_graph_does_not_report_ready() {
        let status = report(RepositoryStatus {
            last_indexed_commit: Some(oid("aaaaaaaa")),
            files: 2,
            symbols: 6,
            structural_facts: 11,
            active_edges: 11,
            ..RepositoryStatus::default()
        });

        assert_eq!(status.index_state, IndexState::Current);
        assert_eq!(status.health, StatusHealth::NeedsContext);
        assert!(status.notices[0].contains("No Feature"));
    }

    #[test]
    fn strong_current_assertions_make_the_graph_ready() {
        let status = report(RepositoryStatus {
            last_indexed_commit: Some(oid("aaaaaaaa")),
            features: 1,
            requirements: 1,
            invariants: 1,
            active_assertions: 4,
            ..RepositoryStatus::default()
        });

        assert_eq!(status.health, StatusHealth::Ready);
        assert!(status.notices.is_empty());
    }
}
