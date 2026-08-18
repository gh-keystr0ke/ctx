use ctx_core::{
    domain::CommitOid,
    schema::{SchemaDivergence, reconcile_orm_and_migrations},
    verification::{intents_without_mapping, stale_semantic_claims},
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
    /// Identifiers of active Feature/Requirement/Invariant/Decision nodes
    /// with no active implementation/test mapping (PR-MAP-003) -- computed
    /// per entity, so one freshly accepted, still-unmapped document is
    /// caught even when most other documents already have one.
    pub unmapped_intents: Vec<String>,
    /// Every stale semantic relationship as a `"source -> target"` string
    /// `ctx explain` accepts directly -- so `notices`/`suggested_actions`
    /// can name exactly what to inspect instead of only a count.
    pub stale_claims: Vec<String>,
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
        let (schema_divergences, unmapped_intents, stale_claims) =
            if knowledge.last_indexed_commit.is_some() {
                let graph = self
                    .store
                    .load_graph(&repository.id)
                    .map_err(StatusError::Storage)?;
                (
                    reconcile_orm_and_migrations(&graph),
                    intents_without_mapping(&graph),
                    stale_semantic_claims(&graph),
                )
            } else {
                (Vec::new(), Vec::new(), Vec::new())
            };
        Ok(build_report(
            repository.root_path,
            head.oid,
            self.git.source_scope(),
            dirty,
            knowledge,
            GraphAnalysis {
                schema_divergences,
                unmapped_intents,
                stale_claims,
            },
        ))
    }
}

/// The graph-derived diagnostics `inspect` only computes once an index
/// exists -- bundled so `build_report` stays under Clippy's argument limit
/// as this set grows.
struct GraphAnalysis {
    schema_divergences: Vec<SchemaDivergence>,
    unmapped_intents: Vec<String>,
    stale_claims: Vec<String>,
}

fn build_report(
    repository: String,
    head_commit: CommitOid,
    source_scope: SourceScope,
    uncommitted_index_inputs: Vec<String>,
    knowledge: RepositoryStatus,
    analysis: GraphAnalysis,
) -> StatusReport {
    let GraphAnalysis {
        schema_divergences,
        unmapped_intents,
        stale_claims,
    } = analysis;
    let index_state = match &knowledge.last_indexed_commit {
        None => IndexState::NotIndexed,
        Some(indexed) if indexed == &head_commit => IndexState::Current,
        Some(_) => IndexState::Behind,
    };
    let health = classify_health(
        index_state,
        &knowledge,
        &schema_divergences,
        &unmapped_intents,
        &stale_claims,
    );
    let (notices, suggested_actions) = diagnostics(
        index_state,
        health,
        &uncommitted_index_inputs,
        &schema_divergences,
        &unmapped_intents,
        &stale_claims,
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
        unmapped_intents,
        stale_claims,
        notices,
        suggested_actions,
    }
}

fn classify_health(
    index_state: IndexState,
    knowledge: &RepositoryStatus,
    schema_divergences: &[SchemaDivergence],
    unmapped_intents: &[String],
    stale_claims: &[String],
) -> StatusHealth {
    if index_state != IndexState::Current {
        return StatusHealth::NeedsIndex;
    }
    // Gates on the same filtered list `diagnostics` displays, not the raw
    // `stale_semantic_edges` row count: a stale edge whose source or target
    // node has since been fully retired (for example, the symbol was
    // renamed away rather than merely re-shaped) is filtered out of
    // `stale_claims` -- nothing left to act on, so it must not report
    // NeedsAttention with an empty, unactionable notice either.
    if !stale_claims.is_empty() || !schema_divergences.is_empty() {
        return StatusHealth::NeedsAttention;
    }
    if product_document_count(knowledge) == 0 {
        return StatusHealth::NeedsContext;
    }
    if !unmapped_intents.is_empty() {
        return StatusHealth::NeedsMappings;
    }
    StatusHealth::Ready
}

fn diagnostics(
    index_state: IndexState,
    health: StatusHealth,
    dirty: &[String],
    schema_divergences: &[SchemaDivergence],
    unmapped_intents: &[String],
    stale_claims: &[String],
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
            notices.push(format!(
                "{} document(s) have no active implementation/test mapping: {}.",
                unmapped_intents.len(),
                unmapped_intents.join(", ")
            ));
            actions.push("Add exact `implementation` and `tests` symbol mappings.".to_owned());
        }
        StatusHealth::NeedsAttention => {
            if !stale_claims.is_empty() {
                notices.push(format!(
                    "{} semantic relationship(s) are stale: {}.",
                    stale_claims.len(),
                    stale_claims.join("; ")
                ));
                actions.push(format!(
                    "Run `ctx explain \"{}\"` (repeat for each listed relationship) to see why and refresh its mapping.",
                    stale_claims.first().map_or("source -> target", String::as_str)
                ));
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
        report_full(knowledge, schema_divergences, Vec::new(), Vec::new())
    }

    fn report_full(
        knowledge: RepositoryStatus,
        schema_divergences: Vec<SchemaDivergence>,
        unmapped_intents: Vec<String>,
        stale_claims: Vec<String>,
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
            GraphAnalysis {
                schema_divergences,
                unmapped_intents,
                stale_claims,
            },
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
    fn an_unmapped_document_forces_needs_mappings_even_when_others_are_well_mapped() {
        // The bug this replaced: a global `active_assertions == 0` check
        // would report `Ready` here, since most documents already have
        // strong mappings -- exactly the case a freshly accepted, still-
        // unmapped `ctx verify --knowledge` document would otherwise hide
        // behind (PR-MAP-003).
        let status = report_full(
            RepositoryStatus {
                last_indexed_commit: Some(oid("aaaaaaaa")),
                features: 1,
                requirements: 2,
                invariants: 1,
                active_assertions: 4,
                ..RepositoryStatus::default()
            },
            Vec::new(),
            vec!["REQ-NEW-001".to_owned()],
            Vec::new(),
        );

        assert_eq!(status.health, StatusHealth::NeedsMappings);
        assert!(
            status
                .notices
                .iter()
                .any(|notice| notice.contains("REQ-NEW-001"))
        );
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

    #[test]
    fn a_stale_relationship_notice_names_a_runnable_ctx_explain_query() {
        // The bug this replaced: the notice/action only ever said "N
        // semantic relationship(s) are stale" with no identifiers, so `ctx
        // explain` (which requires a target) had nothing to point at -- a
        // real user hit this directly after `ctx sync`/`ctx status`.
        let status = report_full(
            RepositoryStatus {
                last_indexed_commit: Some(oid("aaaaaaaa")),
                features: 1,
                requirements: 1,
                invariants: 1,
                active_assertions: 4,
                stale_semantic_edges: 1,
                ..RepositoryStatus::default()
            },
            Vec::new(),
            Vec::new(),
            vec!["subscription.cancel -> REQ-SUB-014".to_owned()],
        );

        assert_eq!(status.health, StatusHealth::NeedsAttention);
        assert!(
            status
                .notices
                .iter()
                .any(|notice| notice.contains("subscription.cancel -> REQ-SUB-014"))
        );
        assert!(status.suggested_actions.iter().any(|action| {
            action.contains("ctx explain \"subscription.cancel -> REQ-SUB-014\"")
        }));
    }

    #[test]
    fn a_stale_edge_whose_node_was_retired_never_produces_an_empty_unactionable_notice() {
        // The bug the test above's own fix introduced: `stale_semantic_edges`
        // is a raw row count (every edge with status='stale'), while
        // `stale_claims` filters out any edge whose source/target node has
        // since been fully retired (found live: a symbol renamed away
        // entirely, not just re-shaped). classify_health used to gate on the
        // raw count while diagnostics displayed the filtered list, so this
        // exact case reported "0 semantic relationship(s) are stale: ." with
        // a placeholder `ctx explain "source -> target"` action -- nothing
        // for a human or agent to actually act on.
        let status = report_full(
            RepositoryStatus {
                last_indexed_commit: Some(oid("aaaaaaaa")),
                features: 1,
                requirements: 1,
                invariants: 1,
                active_assertions: 1,
                stale_semantic_edges: 1,
                ..RepositoryStatus::default()
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(status.health, StatusHealth::Ready);
        assert!(status.notices.is_empty());
        assert!(status.suggested_actions.is_empty());
    }
}
