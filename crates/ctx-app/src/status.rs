use ctx_core::{
    domain::CommitOid,
    schema::{SchemaDivergence, reconcile_orm_and_migrations},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::{
    GitRepository, GraphStore, IndexStore, PortError, RepositoryStatus, SourceScope,
};

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
    /// Best-effort `SQLAlchemy` model vs goose migration-history divergences
    /// on tables declared by both sources. See
    /// [`ctx_core::schema::reconcile_orm_and_migrations`] for exactly what
    /// this does and does not prove.
    pub schema_divergences: Vec<SchemaDivergence>,
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
    S: IndexStore + GraphStore,
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
        let schema_divergences = if knowledge.last_indexed_commit.is_some() {
            let graph = self
                .store
                .load_graph(&repository.id)
                .map_err(StatusError::Storage)?;
            reconcile_orm_and_migrations(&graph)
        } else {
            Vec::new()
        };
        Ok(build_report(
            repository.root_path,
            head.oid,
            self.git.source_scope(),
            dirty,
            knowledge,
            schema_divergences,
        ))
    }
}

fn build_report(
    repository: String,
    head_commit: CommitOid,
    source_scope: SourceScope,
    uncommitted_index_inputs: Vec<String>,
    knowledge: RepositoryStatus,
    schema_divergences: Vec<SchemaDivergence>,
) -> StatusReport {
    let index_state = match &knowledge.last_indexed_commit {
        None => IndexState::NotIndexed,
        Some(indexed) if indexed == &head_commit => IndexState::Current,
        Some(_) => IndexState::Behind,
    };
    let health = classify_health(index_state, &knowledge, &schema_divergences);
    let (notices, suggested_actions) = diagnostics(
        index_state,
        health,
        &knowledge,
        &uncommitted_index_inputs,
        &schema_divergences,
    );
    StatusReport {
        repository,
        head_commit,
        index_state,
        health,
        source_scope,
        uncommitted_index_inputs,
        knowledge,
        schema_divergences,
        notices,
        suggested_actions,
    }
}

fn classify_health(
    index_state: IndexState,
    knowledge: &RepositoryStatus,
    schema_divergences: &[SchemaDivergence],
) -> StatusHealth {
    if index_state != IndexState::Current {
        return StatusHealth::NeedsIndex;
    }
    if knowledge.stale_semantic_edges > 0 || !schema_divergences.is_empty() {
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
    schema_divergences: &[SchemaDivergence],
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
            if knowledge.stale_semantic_edges > 0 {
                notices.push(format!(
                    "{} semantic relationship(s) are stale.",
                    knowledge.stale_semantic_edges
                ));
                actions.push(
                    "Inspect stale claims with `ctx explain` and refresh their mappings."
                        .to_owned(),
                );
            }
            if !schema_divergences.is_empty() {
                notices.push(format!(
                    "{} SQLAlchemy/migration schema divergence(s) found (best-effort; see `ctx status --json`).",
                    schema_divergences.len()
                ));
                actions.push(
                    "Reconcile ORM model fields against migration history for the listed tables."
                        .to_owned(),
                );
            }
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
        report_with_divergences(knowledge, Vec::new())
    }

    fn report_with_divergences(
        knowledge: RepositoryStatus,
        schema_divergences: Vec<SchemaDivergence>,
    ) -> StatusReport {
        build_report(
            "/repo".to_owned(),
            oid("aaaaaaaa"),
            SourceScope {
                languages: vec!["python".to_owned()],
                include: vec!["src".to_owned()],
                exclude: Vec::new(),
            },
            Vec::new(),
            knowledge,
            schema_divergences,
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

    #[test]
    fn schema_divergences_prevent_a_ready_report_even_with_strong_assertions() {
        let status = report_with_divergences(
            RepositoryStatus {
                last_indexed_commit: Some(oid("aaaaaaaa")),
                features: 1,
                requirements: 1,
                invariants: 1,
                active_assertions: 4,
                ..RepositoryStatus::default()
            },
            vec![SchemaDivergence {
                entity: "users".to_owned(),
                column: "email".to_owned(),
                kind: ctx_core::schema::DivergenceKind::ExpectedByOrmOnly,
            }],
        );

        assert_eq!(status.health, StatusHealth::NeedsAttention);
        assert!(
            status
                .notices
                .iter()
                .any(|notice| notice.contains("divergence"))
        );
    }
}
