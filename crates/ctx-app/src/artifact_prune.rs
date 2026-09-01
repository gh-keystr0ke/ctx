//! Explicit, auditable pruning of artifacts outside business-linked scope.

use ctx_core::{
    artifact::ArtifactIdentity,
    artifact_scope::{
        ArtifactScopeDecision, ArtifactScopeDisposition, BusinessScopeOptions, plan_business_scope,
    },
    domain::RepositoryId,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::{
    ArtifactLinkStore, ArtifactMaintenanceStore, ArtifactRepository, KnowledgeCandidateStore,
    PortError,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArtifactPruneOptions {
    pub related_jira_depth: usize,
    pub apply: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingCandidateImpact {
    pub fingerprint: String,
    pub pruned_evidence: Vec<ArtifactIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactPruneReport {
    pub applied: bool,
    pub artifacts_scanned: usize,
    pub artifacts_kept: usize,
    pub artifacts_pruned: usize,
    pub artifacts_removed: Vec<ArtifactIdentity>,
    pub pending_candidates_affected: Vec<PendingCandidateImpact>,
    pub decisions: Vec<ArtifactScopeDecision>,
}

#[derive(Debug, Error)]
pub enum ArtifactPruneError {
    #[error("artifact state could not be read: {0}")]
    Read(PortError),
    #[error("artifact prune could not be applied: {0}")]
    Store(PortError),
}

pub struct ArtifactPruneService<'a, S> {
    store: &'a mut S,
}

impl<'a, S> ArtifactPruneService<'a, S>
where
    S: ArtifactRepository + ArtifactLinkStore + ArtifactMaintenanceStore + KnowledgeCandidateStore,
{
    pub const fn new(store: &'a mut S) -> Self {
        Self { store }
    }

    /// Plans business-linked scope and, only when explicitly requested,
    /// atomically deletes the planned prune set. Candidate files and business
    /// documents are never mutated; pending candidates that cite pruned
    /// evidence are reported for human follow-up.
    ///
    /// # Errors
    /// Returns [`ArtifactPruneError`] when stored state cannot be read or an
    /// explicitly requested deletion fails.
    pub fn run(
        &mut self,
        repository: &RepositoryId,
        options: ArtifactPruneOptions,
    ) -> Result<ArtifactPruneReport, ArtifactPruneError> {
        let artifacts = self
            .store
            .list_artifacts(repository)
            .map_err(ArtifactPruneError::Read)?;
        let links = self
            .store
            .list_links(repository)
            .map_err(ArtifactPruneError::Read)?;
        let pending = self
            .store
            .pending_candidates(repository)
            .map_err(ArtifactPruneError::Read)?;
        let plan = plan_business_scope(
            &artifacts,
            &links,
            BusinessScopeOptions {
                related_jira_depth: options.related_jira_depth,
            },
        );
        let pruned = plan.pruned_identities();
        let pending_candidates_affected = pending
            .into_iter()
            .filter_map(|candidate| {
                let mut evidence = candidate
                    .evidence
                    .into_iter()
                    .map(|reference| reference.identity)
                    .filter(|identity| pruned.contains(identity))
                    .collect::<Vec<_>>();
                evidence.sort_by(identity_order);
                evidence.dedup();
                (!evidence.is_empty()).then_some(PendingCandidateImpact {
                    fingerprint: candidate.fingerprint,
                    pruned_evidence: evidence,
                })
            })
            .collect();
        let mut identities = pruned.into_iter().collect::<Vec<_>>();
        identities.sort_by(identity_order);
        let artifacts_removed = if options.apply {
            self.store
                .delete_artifacts(repository, &identities)
                .map_err(ArtifactPruneError::Store)?
                .removed
        } else {
            Vec::new()
        };
        let artifacts_kept = plan
            .decisions
            .iter()
            .filter(|decision| decision.disposition == ArtifactScopeDisposition::Keep)
            .count();
        Ok(ArtifactPruneReport {
            applied: options.apply,
            artifacts_scanned: artifacts.len(),
            artifacts_kept,
            artifacts_pruned: identities.len(),
            artifacts_removed,
            pending_candidates_affected,
            decisions: plan.decisions,
        })
    }
}

fn identity_order(left: &ArtifactIdentity, right: &ArtifactIdentity) -> std::cmp::Ordering {
    left.provider
        .cmp(&right.provider)
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.external_id.cmp(&right.external_id))
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::HashMap};

    use ctx_core::{
        artifact::{Artifact, ArtifactKind, ArtifactLink, ArtifactProvider, ArtifactRef},
        business::BusinessKind,
        domain::{Project, Url},
        knowledge::{AgentProvenance, KnowledgeCandidate, KnowledgeDecision},
    };

    use super::*;
    use crate::ports::{ArtifactReconcileReport, PortError};

    #[derive(Default)]
    struct FakeStore {
        artifacts: Vec<Artifact>,
        links: Vec<ArtifactLink>,
        pending: Vec<KnowledgeCandidate>,
        deleted: RefCell<Vec<ArtifactIdentity>>,
    }

    impl ArtifactRepository for FakeStore {
        fn upsert_artifact(
            &mut self,
            _repository: &RepositoryId,
            _artifact: &Artifact,
            _ingested_at: &str,
            _ingest_version: &str,
        ) -> Result<(), PortError> {
            unreachable!("prune never upserts")
        }

        fn list_artifacts(&self, _repository: &RepositoryId) -> Result<Vec<Artifact>, PortError> {
            Ok(self.artifacts.clone())
        }

        fn mark_analyzed(
            &mut self,
            _repository: &RepositoryId,
            _identity: &ArtifactIdentity,
            _content_hash: &str,
            _input_fingerprint: &str,
            _analyzed_at: &str,
        ) -> Result<(), PortError> {
            unreachable!("prune never records analysis")
        }

        fn analyzed_input_fingerprints(
            &self,
            _repository: &RepositoryId,
        ) -> Result<HashMap<ArtifactIdentity, String>, PortError> {
            unreachable!("prune never reads analysis")
        }
    }

    impl ArtifactLinkStore for FakeStore {
        fn persist_links(
            &mut self,
            _repository: &RepositoryId,
            _links: &[ArtifactLink],
        ) -> Result<(), PortError> {
            unreachable!("prune never persists links")
        }

        fn list_links(&self, _repository: &RepositoryId) -> Result<Vec<ArtifactLink>, PortError> {
            Ok(self.links.clone())
        }
    }

    impl ArtifactMaintenanceStore for FakeStore {
        fn replace_outgoing_links(
            &mut self,
            _repository: &RepositoryId,
            _source: &ArtifactIdentity,
            _links: &[ArtifactLink],
        ) -> Result<(), PortError> {
            unreachable!("prune never replaces links")
        }

        fn reconcile_snapshot(
            &mut self,
            _repository: &RepositoryId,
            _provider: ArtifactProvider,
            _kinds: &[ArtifactKind],
            _current: &std::collections::HashSet<ArtifactIdentity>,
        ) -> Result<ArtifactReconcileReport, PortError> {
            unreachable!("prune never reconciles provider snapshots")
        }

        fn delete_artifacts(
            &mut self,
            _repository: &RepositoryId,
            identities: &[ArtifactIdentity],
        ) -> Result<ArtifactReconcileReport, PortError> {
            self.deleted.borrow_mut().extend_from_slice(identities);
            Ok(ArtifactReconcileReport {
                removed: identities.to_vec(),
            })
        }
    }

    impl KnowledgeCandidateStore for FakeStore {
        fn upsert_candidates(
            &mut self,
            _repository: &RepositoryId,
            _candidates: &[KnowledgeCandidate],
        ) -> Result<(), PortError> {
            unreachable!("prune never writes candidates")
        }

        fn pending_candidates(
            &self,
            _repository: &RepositoryId,
        ) -> Result<Vec<KnowledgeCandidate>, PortError> {
            Ok(self.pending.clone())
        }

        fn record_decision(
            &mut self,
            _repository: &RepositoryId,
            _fingerprint: &str,
            _decision: &KnowledgeDecision,
            _author: &str,
            _timestamp: &str,
        ) -> Result<(), PortError> {
            unreachable!("prune never records candidate decisions")
        }

        fn accepted_evidence(
            &self,
            _repository: &RepositoryId,
        ) -> Result<std::collections::BTreeMap<String, Vec<ArtifactRef>>, PortError> {
            unreachable!("prune never reads accepted evidence")
        }

        fn accepted_record_for_document(
            &self,
            _repository: &RepositoryId,
            _document_id: &str,
        ) -> Result<Option<ctx_core::knowledge::AcceptedKnowledgeRecord>, PortError> {
            unreachable!("prune never reads accepted records")
        }
    }

    fn artifact(provider: ArtifactProvider, kind: ArtifactKind, id: &str) -> Artifact {
        Artifact {
            identity: ArtifactIdentity {
                provider,
                kind,
                external_id: id.to_owned(),
            },
            project: Project("repo".to_owned()),
            title: id.to_owned(),
            body: String::new(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: Url(id.to_owned()),
            content_hash: format!("hash-{id}"),
        }
    }

    fn pending_candidate(identity: &ArtifactIdentity) -> KnowledgeCandidate {
        KnowledgeCandidate {
            fingerprint: "candidate-1".to_owned(),
            kind: BusinessKind::Requirement,
            statement: "statement".to_owned(),
            evidence: vec![ArtifactRef {
                identity: identity.clone(),
                locator: "body".to_owned(),
                excerpt: "evidence".to_owned(),
            }],
            implementation_candidates: Vec::new(),
            test_candidates: Vec::new(),
            provenance: AgentProvenance {
                producer: "test".to_owned(),
                model: None,
                input_artifact_ids: Vec::new(),
                produced_at: "2026-09-01T00:00:00Z".to_owned(),
                fingerprint: "prompt".to_owned(),
            },
        }
    }

    #[test]
    fn dry_run_reports_pruned_artifacts_and_candidate_impact_without_deleting() {
        let orphan = artifact(ArtifactProvider::Git, ArtifactKind::Commit, "orphan");
        let mut store = FakeStore {
            artifacts: vec![orphan.clone()],
            pending: vec![pending_candidate(&orphan.identity)],
            ..FakeStore::default()
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = ArtifactPruneService::new(&mut store)
            .run(&repository, ArtifactPruneOptions::default())
            .expect("dry run");

        assert!(!report.applied);
        assert_eq!(report.artifacts_pruned, 1);
        assert!(report.artifacts_removed.is_empty());
        assert!(store.deleted.borrow().is_empty());
        assert_eq!(report.pending_candidates_affected.len(), 1);
        assert_eq!(
            report.pending_candidates_affected[0].pruned_evidence,
            vec![orphan.identity]
        );
    }

    #[test]
    fn apply_deletes_exactly_the_planned_prune_set() {
        let orphan = artifact(ArtifactProvider::Git, ArtifactKind::Commit, "orphan");
        let code = artifact(
            ArtifactProvider::Code,
            ArtifactKind::CodeComment,
            "src/lib.rs:1",
        );
        let mut store = FakeStore {
            artifacts: vec![orphan.clone(), code],
            ..FakeStore::default()
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = ArtifactPruneService::new(&mut store)
            .run(
                &repository,
                ArtifactPruneOptions {
                    apply: true,
                    ..ArtifactPruneOptions::default()
                },
            )
            .expect("apply prune");

        assert!(report.applied);
        assert_eq!(report.artifacts_kept, 1);
        assert_eq!(report.artifacts_removed, vec![orphan.identity.clone()]);
        assert_eq!(*store.deleted.borrow(), vec![orphan.identity]);
    }
}
