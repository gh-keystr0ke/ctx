use ctx_core::{
    business::{BusinessDocument, BusinessKind, ExplicitSymbolLink},
    domain::RepositoryId,
    knowledge::{DecisionMethod, KnowledgeCandidate, KnowledgeDecision, ReviewVerdict},
    verification::{
        ArtifactEvidenceContext, CandidateCluster, KnowledgeIdAllocator, SemanticCandidate,
        VerificationDecision, cluster_candidates, possible_duplicate, semantic_candidates,
    },
};
use serde::Serialize;
use thiserror::Error;

use crate::ports::{
    ArtifactLinkStore, BusinessContextReader, BusinessContextWriter, CommitMetadata, GraphStore,
    KnowledgeCandidateStore, KnowledgeReviewAgent, PortError, VerificationStore,
};

#[derive(Debug, Error)]
pub enum VerificationError {
    #[error("verification candidates could not be loaded: {0}")]
    Store(PortError),
    #[error("verification candidate '{0}' was not found")]
    CandidateNotFound(String),
    #[error(
        "'{statement}' looks like a restatement of already-active {existing_id} -- attach as evidence to it instead, or pass force to create a new document anyway"
    )]
    PossibleDuplicate {
        existing_id: String,
        statement: String,
    },
}

/// What one `ctx verify --knowledge --auto` run did, for `--json` output and
/// the plain-text summary alike: how many clusters an independent review
/// agent actually looked at, and the resulting split across written
/// documents (one per accepted, non-duplicate candidate or merged cluster),
/// individually-decided accept/reject verdicts, and candidates left pending
/// because they looked like a restatement of an already-active document
/// (skipped rather than force-created, matching the human accept path's own
/// default -- REQ-INCR-002).
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct AutoVerifyReport {
    pub clusters_reviewed: usize,
    pub documents_written: usize,
    pub candidates_accepted: usize,
    pub candidates_rejected: usize,
    pub candidates_skipped_possible_duplicate: usize,
}

/// What the review agent's verdict on one [`KnowledgeCandidate`] actually
/// resulted in, once persistence ran -- distinct from [`ReviewVerdict`]
/// itself, since an `Accept` verdict can still end up
/// `SkippedPossibleDuplicate` rather than written (REQ-INCR-002's duplicate
/// check, unless `--force`). Reported per candidate so
/// [`KnowledgeVerificationService::auto_with_progress`]'s `on_result`
/// callback can show a real result -- not just "reviewing cluster..." with
/// no visible outcome, the exact gap a real `--auto` user hit right after
/// the progress-output fix landed.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum CandidateOutcome {
    Accepted { document_id: String },
    Rejected,
    SkippedPossibleDuplicate { existing_id: String },
}

/// One reviewed candidate's statement and its resulting
/// [`CandidateOutcome`], in review order -- everything a caller needs to
/// print a human-readable per-candidate result line without re-deriving it
/// from [`AutoVerifyReport`]'s aggregate counts.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReviewedCandidate {
    pub statement: String,
    pub outcome: CandidateOutcome,
}

pub struct VerificationService<'a, S> {
    store: &'a mut S,
}

impl<'a, S> VerificationService<'a, S>
where
    S: GraphStore + VerificationStore + ArtifactLinkStore + KnowledgeCandidateStore,
{
    pub const fn new(store: &'a mut S) -> Self {
        Self { store }
    }

    /// Returns deterministic, impact-prioritized semantic candidates,
    /// including the artifact-evidence signal (PR-MAP-001) for any intent
    /// that originated from an accepted AI-derived candidate.
    ///
    /// # Errors
    ///
    /// Returns [`VerificationError`] when current graph or artifact state
    /// cannot be loaded.
    pub fn candidates(
        &self,
        repository: &RepositoryId,
    ) -> Result<Vec<SemanticCandidate>, VerificationError> {
        let graph = self
            .store
            .load_graph(repository)
            .map_err(VerificationError::Store)?;
        let artifact_context = ArtifactEvidenceContext {
            links: self
                .store
                .list_links(repository)
                .map_err(VerificationError::Store)?,
            accepted_evidence: self
                .store
                .accepted_evidence(repository)
                .map_err(VerificationError::Store)?,
        };
        Ok(semantic_candidates(&graph, &artifact_context))
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

/// Human verification for AI-derived [`KnowledgeCandidate`]s
/// (`ctx verify --knowledge`, prompt3.md PR-VERIFY-001) — deliberately a
/// sibling service, not a shared queue with [`VerificationService`]:
/// accepting a heuristic `SemanticCandidate` only asserts an already-known
/// claim, while accepting a `KnowledgeCandidate` creates a brand-new
/// product-knowledge entity and needs a human-chosen stable ID, so the two
/// flows have genuinely different shapes rather than one interchangeable
/// accept/reject action.
pub struct KnowledgeVerificationService<'a, S, W> {
    store: &'a mut S,
    writer: &'a W,
}

impl<'a, S, W> KnowledgeVerificationService<'a, S, W>
where
    S: KnowledgeCandidateStore + GraphStore,
    W: BusinessContextWriter + BusinessContextReader,
{
    pub const fn new(store: &'a mut S, writer: &'a W) -> Self {
        Self { store, writer }
    }

    /// Returns every candidate still awaiting a human decision.
    ///
    /// # Errors
    /// Returns [`VerificationError`] when stored candidates cannot be read.
    pub fn candidates(
        &self,
        repository: &RepositoryId,
    ) -> Result<Vec<KnowledgeCandidate>, VerificationError> {
        self.store
            .pending_candidates(repository)
            .map_err(VerificationError::Store)
    }

    /// Accepts a pending candidate under `document_id`: writes the resulting
    /// `.context/*.yaml` file (the next `ctx index` absorbs it like any
    /// hand-authored document) and records the decision, keeping the
    /// original candidate row -- status `accepted`, pointing at this ID --
    /// rather than discarding the artifact-to-inference chain (PR-VERIFY-002).
    ///
    /// Unless `force`, refuses when the statement looks like a restatement
    /// of an already-active document of the same kind (prompt3.md §13 MUST:
    /// "restating REQ-17 must not silently become REQ-94") -- a lexical
    /// similarity check against the current graph, advisory only, never a
    /// second AI call.
    ///
    /// # Errors
    /// Returns [`VerificationError`] when `fingerprint` is not currently
    /// pending, a likely duplicate exists and `force` is false, the document
    /// file already exists, or persistence fails.
    #[allow(clippy::too_many_arguments)]
    pub fn accept(
        &mut self,
        repository: &RepositoryId,
        fingerprint: &str,
        document_id: &str,
        author: &str,
        timestamp: &str,
        force: bool,
        method: DecisionMethod,
    ) -> Result<String, VerificationError> {
        let candidate = self
            .candidates(repository)?
            .into_iter()
            .find(|candidate| candidate.fingerprint == fingerprint)
            .ok_or_else(|| VerificationError::CandidateNotFound(fingerprint.to_owned()))?;
        if !force {
            let graph = self
                .store
                .load_graph(repository)
                .map_err(VerificationError::Store)?;
            if let Some(existing_id) =
                possible_duplicate(&graph, candidate.kind, &candidate.statement)
            {
                return Err(VerificationError::PossibleDuplicate {
                    existing_id,
                    statement: candidate.statement,
                });
            }
        }
        let document = candidate_to_document(&candidate, document_id);
        let path = self
            .writer
            .write_document(&document)
            .map_err(VerificationError::Store)?;
        self.store
            .record_decision(
                repository,
                fingerprint,
                &KnowledgeDecision::Accept {
                    document_id: document_id.to_owned(),
                    method,
                },
                author,
                timestamp,
            )
            .map_err(VerificationError::Store)?;
        Ok(path)
    }

    /// Rejects a pending candidate; it is never proposed again once a future
    /// `ctx enrich` run recognizes the same fingerprint.
    ///
    /// # Errors
    /// Returns [`VerificationError`] when `fingerprint` is not currently
    /// pending or persistence fails.
    pub fn reject(
        &mut self,
        repository: &RepositoryId,
        fingerprint: &str,
        author: &str,
        timestamp: &str,
        method: DecisionMethod,
    ) -> Result<(), VerificationError> {
        self.store
            .record_decision(
                repository,
                fingerprint,
                &KnowledgeDecision::Reject { method },
                author,
                timestamp,
            )
            .map_err(VerificationError::Store)
    }

    /// Runs every pending candidate through an independent second-opinion
    /// review agent (`ctx verify --knowledge --auto`) instead of a human:
    /// clusters related candidates first ([`cluster_candidates`]), asks
    /// `agent` to accept/reject each one on its own merits and optionally
    /// name a single merged statement for a cluster whose accepted
    /// candidates genuinely restate the same knowledge, then writes one
    /// document per accepted candidate or merged cluster -- exactly the
    /// `candidate_to_document`/`write_document`/`record_decision` path
    /// [`Self::accept`] already uses, so an auto-accepted document is
    /// indistinguishable in shape from a human-accepted one; only
    /// `method: DecisionMethod::Agent` on the recorded decision marks it
    /// honestly (`INV-PROVENANCE-001`). Each stable ID is allocated by
    /// [`KnowledgeIdAllocator`] under `id_prefix` rather than typed by a
    /// human, since removing exactly that typing is the point of `--auto`.
    ///
    /// A candidate (or, for a merge, every candidate in the merged group)
    /// that looks like a restatement of an already-active document is left
    /// pending rather than force-created, unless `force` -- the same
    /// default [`Self::accept`] already applies, so a human can still review
    /// it through the ordinary interactive flow afterward.
    ///
    /// # Errors
    /// Returns [`VerificationError`] when the graph or candidates cannot be
    /// loaded, the review agent cannot be reached or returns an invalid
    /// response, or persistence fails.
    pub fn auto(
        &mut self,
        repository: &RepositoryId,
        id_prefix: &str,
        author: &str,
        timestamp: &str,
        force: bool,
        agent: &dyn KnowledgeReviewAgent,
    ) -> Result<AutoVerifyReport, VerificationError> {
        self.auto_with_progress(
            repository,
            id_prefix,
            author,
            timestamp,
            force,
            agent,
            &mut |_, _, _| {},
            &mut |_, _, _| {},
        )
    }

    /// Identical to [`Self::auto`], but calls `on_progress(position, total,
    /// cluster)` immediately before reviewing each cluster -- `position` is
    /// 1-based, `total` is the number of clusters this run will review --
    /// and `on_result(position, total, reviewed)` immediately after that
    /// cluster's decisions are all recorded, one [`ReviewedCandidate`] per
    /// candidate the agent actually returned a verdict for, in review order.
    /// Split out the same way [`crate::enrich::EnrichRunner::run_with_progress`]
    /// is: a real agent call per cluster can take tens of seconds, and with
    /// dozens of pending candidates grouped into several clusters, silence
    /// the whole time is indistinguishable from a hang (the exact bug a real
    /// user already hit once with `ctx enrich` before it got the same fix) --
    /// `on_result` closes the follow-up gap the same user hit next: the
    /// progress line alone says a cluster was reviewed but never what the
    /// agent actually decided.
    ///
    /// # Errors
    /// Returns [`VerificationError`] under the same conditions as
    /// [`Self::auto`].
    #[allow(clippy::too_many_arguments)]
    pub fn auto_with_progress(
        &mut self,
        repository: &RepositoryId,
        id_prefix: &str,
        author: &str,
        timestamp: &str,
        force: bool,
        agent: &dyn KnowledgeReviewAgent,
        on_progress: &mut dyn FnMut(usize, usize, &CandidateCluster),
        on_result: &mut dyn FnMut(usize, usize, &[ReviewedCandidate]),
    ) -> Result<AutoVerifyReport, VerificationError> {
        let pending = self.candidates(repository)?;
        let mut report = AutoVerifyReport::default();
        if pending.is_empty() {
            return Ok(report);
        }
        let graph = self
            .store
            .load_graph(repository)
            .map_err(VerificationError::Store)?;
        let mut allocator = KnowledgeIdAllocator::new(id_prefix, &graph);
        // The indexed graph can lag behind `.context/*.yaml` on disk (e.g. a
        // document written since the last `ctx index`) -- reserve those IDs
        // too, or `allocate` would hand one back out and `write_document`
        // would reject the whole run with a confusing "already exists".
        for document in self.writer.read_all().map_err(VerificationError::Store)? {
            allocator.mark_used(document.id);
        }

        let clusters = cluster_candidates(&pending);
        let total = clusters.len();
        for (index, cluster) in clusters.into_iter().enumerate() {
            on_progress(index + 1, total, &cluster);
            let outcomes = self.process_cluster(
                repository,
                &cluster,
                &pending,
                &mut allocator,
                &graph,
                author,
                timestamp,
                force,
                agent,
                &mut report,
            )?;
            on_result(index + 1, total, &outcomes);
        }
        Ok(report)
    }

    /// Reviews and decides every candidate in one cluster: records each
    /// verdict, writes any accepted (non-duplicate) document(s), and
    /// returns one [`ReviewedCandidate`] per candidate the agent returned a
    /// verdict for, in review order -- split out of
    /// [`Self::auto_with_progress`] purely to keep that loop body readable.
    #[allow(clippy::too_many_arguments)]
    fn process_cluster(
        &mut self,
        repository: &RepositoryId,
        cluster: &CandidateCluster,
        pending: &[KnowledgeCandidate],
        allocator: &mut KnowledgeIdAllocator,
        graph: &ctx_core::graph::GraphSnapshot,
        author: &str,
        timestamp: &str,
        force: bool,
        agent: &dyn KnowledgeReviewAgent,
        report: &mut AutoVerifyReport,
    ) -> Result<Vec<ReviewedCandidate>, VerificationError> {
        let members: Vec<KnowledgeCandidate> = cluster
            .fingerprints
            .iter()
            .filter_map(|fingerprint| {
                pending
                    .iter()
                    .find(|candidate| &candidate.fingerprint == fingerprint)
                    .cloned()
            })
            .collect();
        report.clusters_reviewed += 1;
        let review = agent.review(&members).map_err(VerificationError::Store)?;

        let mut outcomes: Vec<ReviewedCandidate> = Vec::new();
        let mut accepted = Vec::new();
        for decision in &review.decisions {
            match decision.verdict {
                ReviewVerdict::Reject => {
                    self.store
                        .record_decision(
                            repository,
                            &decision.fingerprint,
                            &KnowledgeDecision::Reject {
                                method: DecisionMethod::Agent,
                            },
                            author,
                            timestamp,
                        )
                        .map_err(VerificationError::Store)?;
                    report.candidates_rejected += 1;
                    if let Some(candidate) = members
                        .iter()
                        .find(|candidate| candidate.fingerprint == decision.fingerprint)
                    {
                        outcomes.push(ReviewedCandidate {
                            statement: candidate.statement.clone(),
                            outcome: CandidateOutcome::Rejected,
                        });
                    }
                }
                ReviewVerdict::Accept => {
                    if let Some(candidate) = members
                        .iter()
                        .find(|candidate| candidate.fingerprint == decision.fingerprint)
                    {
                        accepted.push(candidate.clone());
                    }
                }
            }
        }

        if !accepted.is_empty() {
            if accepted.len() >= 2
                && let Some(merged_statement) = &review.merged_statement
            {
                let outcome = self.accept_merged(
                    repository,
                    cluster.kind,
                    merged_statement,
                    &accepted,
                    allocator,
                    graph,
                    author,
                    timestamp,
                    force,
                    report,
                )?;
                for candidate in &accepted {
                    outcomes.push(ReviewedCandidate {
                        statement: candidate.statement.clone(),
                        outcome: outcome.clone(),
                    });
                }
            } else {
                for candidate in &accepted {
                    let outcome = self.accept_one_auto(
                        repository, candidate, allocator, graph, author, timestamp, force, report,
                    )?;
                    outcomes.push(ReviewedCandidate {
                        statement: candidate.statement.clone(),
                        outcome,
                    });
                }
            }
        }
        Ok(outcomes)
    }

    #[allow(clippy::too_many_arguments)]
    fn accept_one_auto(
        &mut self,
        repository: &RepositoryId,
        candidate: &KnowledgeCandidate,
        allocator: &mut KnowledgeIdAllocator,
        graph: &ctx_core::graph::GraphSnapshot,
        author: &str,
        timestamp: &str,
        force: bool,
        report: &mut AutoVerifyReport,
    ) -> Result<CandidateOutcome, VerificationError> {
        if let Some(existing_id) = (!force)
            .then(|| possible_duplicate(graph, candidate.kind, &candidate.statement))
            .flatten()
        {
            report.candidates_skipped_possible_duplicate += 1;
            return Ok(CandidateOutcome::SkippedPossibleDuplicate { existing_id });
        }
        let document_id = allocator.allocate(candidate.kind);
        let document = candidate_to_document(candidate, &document_id);
        self.writer
            .write_document(&document)
            .map_err(VerificationError::Store)?;
        self.store
            .record_decision(
                repository,
                &candidate.fingerprint,
                &KnowledgeDecision::Accept {
                    document_id: document_id.clone(),
                    method: DecisionMethod::Agent,
                },
                author,
                timestamp,
            )
            .map_err(VerificationError::Store)?;
        report.documents_written += 1;
        report.candidates_accepted += 1;
        Ok(CandidateOutcome::Accepted { document_id })
    }

    #[allow(clippy::too_many_arguments)]
    fn accept_merged(
        &mut self,
        repository: &RepositoryId,
        kind: BusinessKind,
        merged_statement: &str,
        accepted: &[KnowledgeCandidate],
        allocator: &mut KnowledgeIdAllocator,
        graph: &ctx_core::graph::GraphSnapshot,
        author: &str,
        timestamp: &str,
        force: bool,
        report: &mut AutoVerifyReport,
    ) -> Result<CandidateOutcome, VerificationError> {
        if let Some(existing_id) = (!force)
            .then(|| possible_duplicate(graph, kind, merged_statement))
            .flatten()
        {
            report.candidates_skipped_possible_duplicate += accepted.len();
            return Ok(CandidateOutcome::SkippedPossibleDuplicate { existing_id });
        }
        let merged = KnowledgeCandidate {
            fingerprint: accepted[0].fingerprint.clone(),
            kind,
            statement: merged_statement.to_owned(),
            evidence: accepted
                .iter()
                .flat_map(|candidate| candidate.evidence.clone())
                .collect(),
            implementation_candidates: accepted
                .iter()
                .flat_map(|candidate| candidate.implementation_candidates.clone())
                .collect(),
            test_candidates: accepted
                .iter()
                .flat_map(|candidate| candidate.test_candidates.clone())
                .collect(),
            provenance: accepted[0].provenance.clone(),
        };
        let document_id = allocator.allocate(kind);
        let document = candidate_to_document(&merged, &document_id);
        self.writer
            .write_document(&document)
            .map_err(VerificationError::Store)?;
        for candidate in accepted {
            self.store
                .record_decision(
                    repository,
                    &candidate.fingerprint,
                    &KnowledgeDecision::Accept {
                        document_id: document_id.clone(),
                        method: DecisionMethod::Agent,
                    },
                    author,
                    timestamp,
                )
                .map_err(VerificationError::Store)?;
            report.candidates_accepted += 1;
        }
        report.documents_written += 1;
        Ok(CandidateOutcome::Accepted { document_id })
    }
}

fn candidate_to_document(candidate: &KnowledgeCandidate, document_id: &str) -> BusinessDocument {
    let title = match candidate.kind {
        BusinessKind::Requirement | BusinessKind::Invariant => candidate.statement.clone(),
        BusinessKind::Feature | BusinessKind::Decision => candidate.derived_title(),
    };
    let to_links = |symbols: &[String]| {
        symbols
            .iter()
            .map(|symbol| ExplicitSymbolLink {
                symbol: symbol.clone(),
                locator: String::new(),
            })
            .collect()
    };
    BusinessDocument {
        id: document_id.to_owned(),
        kind: candidate.kind,
        title,
        body: candidate.statement.clone(),
        status: "active".to_owned(),
        visibility: ctx_core::business::Visibility::Private,
        feature: None,
        implementation: to_links(&candidate.implementation_candidates),
        tests: to_links(&candidate.test_candidates),
        source_uri: String::new(),
        content_hash: String::new(),
    }
}

#[cfg(test)]
mod knowledge_tests {
    use std::cell::RefCell;

    use ctx_core::{
        artifact::{ArtifactIdentity, ArtifactKind, ArtifactProvider, ArtifactRef},
        knowledge::{AgentProvenance, CandidateReviewDecision, ClusterReview},
    };

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        pending: Vec<KnowledgeCandidate>,
        decisions: RefCell<Vec<(String, KnowledgeDecision)>>,
        graph: ctx_core::graph::GraphSnapshot,
    }

    impl GraphStore for FakeStore {
        fn load_graph(
            &self,
            _repository: &RepositoryId,
        ) -> Result<ctx_core::graph::GraphSnapshot, PortError> {
            Ok(self.graph.clone())
        }
    }

    impl KnowledgeCandidateStore for FakeStore {
        fn upsert_candidates(
            &mut self,
            _repository: &RepositoryId,
            _candidates: &[KnowledgeCandidate],
        ) -> Result<(), PortError> {
            unreachable!("verification never upserts candidates")
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
            fingerprint: &str,
            decision: &KnowledgeDecision,
            _author: &str,
            _timestamp: &str,
        ) -> Result<(), PortError> {
            self.decisions
                .borrow_mut()
                .push((fingerprint.to_owned(), decision.clone()));
            Ok(())
        }

        fn accepted_evidence(
            &self,
            _repository: &RepositoryId,
        ) -> Result<std::collections::BTreeMap<String, Vec<ArtifactRef>>, PortError> {
            unreachable!("knowledge verification never reads accepted evidence")
        }

        fn accepted_record_for_document(
            &self,
            _repository: &RepositoryId,
            _document_id: &str,
        ) -> Result<Option<ctx_core::knowledge::AcceptedKnowledgeRecord>, PortError> {
            unreachable!("knowledge verification never reads accepted candidate records")
        }
    }

    #[derive(Default)]
    struct FakeWriter {
        /// Documents already present on disk that the indexed graph passed
        /// to `auto`/`auto_with_progress` doesn't know about -- lets tests
        /// simulate a stale index without a real filesystem.
        existing: Vec<BusinessDocument>,
        written: RefCell<Vec<BusinessDocument>>,
    }

    impl BusinessContextWriter for FakeWriter {
        fn write_document(&self, document: &BusinessDocument) -> Result<String, PortError> {
            if self
                .existing
                .iter()
                .chain(self.written.borrow().iter())
                .any(|written| written.id == document.id)
            {
                return Err(PortError::new(format!(
                    "a business context document already exists at '.context/fake/{}.yaml'",
                    document.id
                )));
            }
            self.written.borrow_mut().push(document.clone());
            Ok(format!(".context/fake/{}.yaml", document.id))
        }
    }

    impl BusinessContextReader for FakeWriter {
        fn read_all(&self) -> Result<Vec<BusinessDocument>, PortError> {
            Ok(self.existing.clone())
        }
    }

    struct FakeReviewAgent<F> {
        review: F,
    }

    impl<F: Fn(&[KnowledgeCandidate]) -> ClusterReview> KnowledgeReviewAgent for FakeReviewAgent<F> {
        fn review(&self, candidates: &[KnowledgeCandidate]) -> Result<ClusterReview, PortError> {
            Ok((self.review)(candidates))
        }
    }

    fn candidate(kind: BusinessKind, statement: &str) -> KnowledgeCandidate {
        KnowledgeCandidate {
            fingerprint: KnowledgeCandidate::fingerprint_for(kind, statement),
            kind,
            statement: statement.to_owned(),
            evidence: vec![ArtifactRef {
                identity: ArtifactIdentity {
                    provider: ArtifactProvider::GitLab,
                    kind: ArtifactKind::Issue,
                    external_id: "317".to_owned(),
                },
                locator: "body".to_owned(),
                excerpt: "excerpt".to_owned(),
            }],
            implementation_candidates: vec!["SubscriptionService.cancel".to_owned()],
            test_candidates: Vec::new(),
            provenance: AgentProvenance {
                producer: "test".to_owned(),
                model: None,
                input_artifact_ids: Vec::new(),
                produced_at: "2026-08-21T00:00:00Z".to_owned(),
                fingerprint: "fp".to_owned(),
            },
        }
    }

    #[test]
    fn accepting_writes_the_document_and_records_the_decision() {
        let candidate = candidate(BusinessKind::Requirement, "Cancellation preserves access.");
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let path = KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                &candidate.fingerprint,
                "REQ-SUB-014",
                "alice",
                "2026-08-21T00:00:00Z",
                false,
                DecisionMethod::Human,
            )
            .expect("accept");

        assert_eq!(path, ".context/fake/REQ-SUB-014.yaml");
        let written = writer.written.borrow();
        assert_eq!(written[0].id, "REQ-SUB-014");
        assert_eq!(written[0].title, "Cancellation preserves access.");
        assert_eq!(
            written[0].implementation[0].symbol,
            "SubscriptionService.cancel"
        );
        assert_eq!(
            store.decisions.borrow()[0],
            (
                candidate.fingerprint.clone(),
                KnowledgeDecision::Accept {
                    document_id: "REQ-SUB-014".to_owned(),
                    method: DecisionMethod::Human,
                }
            )
        );
    }

    #[test]
    fn a_decision_kind_gets_a_derived_title_distinct_from_its_body() {
        let candidate = candidate(
            BusinessKind::Decision,
            "Cancellation stays reversible until period end. Detailed reasoning follows.",
        );
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                &candidate.fingerprint,
                "ADR-SUB-002",
                "alice",
                "2026-08-21T00:00:00Z",
                false,
                DecisionMethod::Human,
            )
            .expect("accept");

        let written = writer.written.borrow();
        assert_eq!(
            written[0].title,
            "Cancellation stays reversible until period end"
        );
        assert_eq!(written[0].body, candidate.statement);
    }

    #[test]
    fn accepting_an_unknown_fingerprint_fails_clearly() {
        let mut store = FakeStore::default();
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let error = KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                "missing",
                "REQ-X",
                "alice",
                "2026-08-21T00:00:00Z",
                false,
                DecisionMethod::Human,
            )
            .expect_err("unknown fingerprint must fail");

        assert!(matches!(error, VerificationError::CandidateNotFound(_)));
        assert!(writer.written.borrow().is_empty());
    }

    #[test]
    fn accepting_a_likely_restatement_is_refused_unless_forced() {
        let candidate = candidate(
            BusinessKind::Requirement,
            "Cancellation preserves paid access until the period ends.",
        );
        let existing = ctx_core::graph::GraphNode {
            stable_key: ctx_core::domain::StableKey::new("intent:REQ-SUB-001").expect("stable key"),
            kind: ctx_core::domain::NodeKind::Requirement,
            name: "Cancellation preserves access".to_owned(),
            content_hash: "hash".to_owned(),
            attributes: ctx_core::indexing::PlannedNodeAttributes::Business {
                id: "REQ-SUB-001".to_owned(),
                status: "active".to_owned(),
                visibility: ctx_core::business::Visibility::Private,
                body: "Cancellation preserves paid access until the period ends.".to_owned(),
                feature: None,
                source_uri: "requirement.yaml".to_owned(),
            },
        };
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            graph: ctx_core::graph::GraphSnapshot {
                nodes: [(existing.stable_key.clone(), existing)]
                    .into_iter()
                    .collect(),
                edges: Vec::new(),
            },
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let error = KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                &candidate.fingerprint,
                "REQ-SUB-002",
                "alice",
                "2026-08-21T00:00:00Z",
                false,
                DecisionMethod::Human,
            )
            .expect_err("a likely restatement must be refused without force");
        assert!(matches!(
            error,
            VerificationError::PossibleDuplicate { existing_id, .. } if existing_id == "REQ-SUB-001"
        ));
        assert!(writer.written.borrow().is_empty());

        // force overrides the check.
        let path = KnowledgeVerificationService::new(&mut store, &writer)
            .accept(
                &repository,
                &candidate.fingerprint,
                "REQ-SUB-002",
                "alice",
                "2026-08-21T00:00:00Z",
                true,
                DecisionMethod::Human,
            )
            .expect("force overrides the duplicate check");
        assert_eq!(path, ".context/fake/REQ-SUB-002.yaml");
    }

    #[test]
    fn rejecting_records_a_reject_decision_and_writes_nothing() {
        let candidate = candidate(BusinessKind::Invariant, "Never delete paid history.");
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        KnowledgeVerificationService::new(&mut store, &writer)
            .reject(
                &repository,
                &candidate.fingerprint,
                "alice",
                "2026-08-21T00:00:00Z",
                DecisionMethod::Human,
            )
            .expect("reject");

        assert!(writer.written.borrow().is_empty());
        assert_eq!(
            store.decisions.borrow()[0],
            (
                candidate.fingerprint,
                KnowledgeDecision::Reject {
                    method: DecisionMethod::Human
                }
            )
        );
    }

    #[test]
    fn auto_accepts_a_single_candidate_with_an_agent_allocated_id_and_honest_method() {
        let candidate = candidate(BusinessKind::Requirement, "Cancellation preserves access.");
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let agent = FakeReviewAgent {
            review: |candidates: &[KnowledgeCandidate]| ClusterReview {
                decisions: candidates
                    .iter()
                    .map(|candidate| CandidateReviewDecision {
                        fingerprint: candidate.fingerprint.clone(),
                        verdict: ReviewVerdict::Accept,
                    })
                    .collect(),
                merged_statement: None,
            },
        };

        let report = KnowledgeVerificationService::new(&mut store, &writer)
            .auto(
                &repository,
                "SUB",
                "auto-claude",
                "2026-08-23T00:00:00Z",
                false,
                &agent,
            )
            .expect("auto run");

        assert_eq!(report.clusters_reviewed, 1);
        assert_eq!(report.documents_written, 1);
        assert_eq!(report.candidates_accepted, 1);
        assert_eq!(report.candidates_rejected, 0);
        let written = writer.written.borrow();
        assert_eq!(written[0].id, "REQ-SUB-001");
        assert_eq!(
            store.decisions.borrow()[0],
            (
                candidate.fingerprint,
                KnowledgeDecision::Accept {
                    document_id: "REQ-SUB-001".to_owned(),
                    method: DecisionMethod::Agent,
                }
            )
        );
    }

    #[test]
    fn auto_skips_an_id_already_on_disk_even_when_the_graph_has_not_indexed_it_yet() {
        // Regression: a `.context/*.yaml` document can exist on disk without
        // yet being reflected in the indexed graph (e.g. written since the
        // last `ctx index`). The allocator must not reissue that ID -- doing
        // so aborts the whole `--auto` run with a misleading "candidates
        // could not be loaded" error on what is really a write conflict.
        let candidate = candidate(BusinessKind::Requirement, "Cancellation preserves access.");
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter {
            existing: vec![candidate_to_document(&candidate, "REQ-SUB-001")],
            ..FakeWriter::default()
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let agent = FakeReviewAgent {
            review: |candidates: &[KnowledgeCandidate]| ClusterReview {
                decisions: candidates
                    .iter()
                    .map(|candidate| CandidateReviewDecision {
                        fingerprint: candidate.fingerprint.clone(),
                        verdict: ReviewVerdict::Accept,
                    })
                    .collect(),
                merged_statement: None,
            },
        };

        let report = KnowledgeVerificationService::new(&mut store, &writer)
            .auto(
                &repository,
                "SUB",
                "auto-claude",
                "2026-08-23T00:00:00Z",
                false,
                &agent,
            )
            .expect("auto run should skip the on-disk ID rather than fail");

        assert_eq!(report.documents_written, 1);
        assert_eq!(writer.written.borrow()[0].id, "REQ-SUB-002");
    }

    #[test]
    fn auto_rejects_a_candidate_the_review_agent_rejects() {
        let candidate = candidate(BusinessKind::Requirement, "Weak unsupported statement.");
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let agent = FakeReviewAgent {
            review: |candidates: &[KnowledgeCandidate]| ClusterReview {
                decisions: candidates
                    .iter()
                    .map(|candidate| CandidateReviewDecision {
                        fingerprint: candidate.fingerprint.clone(),
                        verdict: ReviewVerdict::Reject,
                    })
                    .collect(),
                merged_statement: None,
            },
        };

        let report = KnowledgeVerificationService::new(&mut store, &writer)
            .auto(
                &repository,
                "SUB",
                "auto-claude",
                "2026-08-23T00:00:00Z",
                false,
                &agent,
            )
            .expect("auto run");

        assert_eq!(report.candidates_rejected, 1);
        assert_eq!(report.documents_written, 0);
        assert!(writer.written.borrow().is_empty());
        assert_eq!(
            store.decisions.borrow()[0],
            (
                candidate.fingerprint,
                KnowledgeDecision::Reject {
                    method: DecisionMethod::Agent,
                }
            )
        );
    }

    #[test]
    fn auto_merges_two_accepted_candidates_of_one_cluster_into_a_single_document() {
        let first = candidate(BusinessKind::Requirement, "Cancellation preserves access.");
        let second = candidate(
            BusinessKind::Requirement,
            "Cancellation must preserve access.",
        );
        let mut store = FakeStore {
            pending: vec![first.clone(), second.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let agent = FakeReviewAgent {
            review: |candidates: &[KnowledgeCandidate]| ClusterReview {
                decisions: candidates
                    .iter()
                    .map(|candidate| CandidateReviewDecision {
                        fingerprint: candidate.fingerprint.clone(),
                        verdict: ReviewVerdict::Accept,
                    })
                    .collect(),
                merged_statement: Some("Cancellation preserves access.".to_owned()),
            },
        };

        let report = KnowledgeVerificationService::new(&mut store, &writer)
            .auto(
                &repository,
                "SUB",
                "auto-claude",
                "2026-08-23T00:00:00Z",
                false,
                &agent,
            )
            .expect("auto run");

        assert_eq!(report.documents_written, 1, "one merged document, not two");
        assert_eq!(report.candidates_accepted, 2);
        assert_eq!(writer.written.borrow().len(), 1);
        let decisions = store.decisions.borrow();
        let document_ids: std::collections::BTreeSet<_> = decisions
            .iter()
            .filter_map(|(_, decision)| match decision {
                KnowledgeDecision::Accept { document_id, .. } => Some(document_id.clone()),
                KnowledgeDecision::Reject { .. } => None,
            })
            .collect();
        assert_eq!(
            document_ids.len(),
            1,
            "both candidates point at the same merged document"
        );
    }

    #[test]
    fn auto_leaves_a_likely_duplicate_pending_unless_forced() {
        let candidate = candidate(
            BusinessKind::Requirement,
            "Cancellation preserves paid access until the period ends.",
        );
        let existing = ctx_core::graph::GraphNode {
            stable_key: ctx_core::domain::StableKey::new("intent:REQ-SUB-001").expect("stable key"),
            kind: ctx_core::domain::NodeKind::Requirement,
            name: "Cancellation preserves access".to_owned(),
            content_hash: "hash".to_owned(),
            attributes: ctx_core::indexing::PlannedNodeAttributes::Business {
                id: "REQ-SUB-001".to_owned(),
                status: "active".to_owned(),
                visibility: ctx_core::business::Visibility::Private,
                body: "Cancellation preserves paid access until the period ends.".to_owned(),
                feature: None,
                source_uri: "requirement.yaml".to_owned(),
            },
        };
        let mut store = FakeStore {
            pending: vec![candidate.clone()],
            graph: ctx_core::graph::GraphSnapshot {
                nodes: [(existing.stable_key.clone(), existing)]
                    .into_iter()
                    .collect(),
                edges: Vec::new(),
            },
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let agent = FakeReviewAgent {
            review: |candidates: &[KnowledgeCandidate]| ClusterReview {
                decisions: candidates
                    .iter()
                    .map(|candidate| CandidateReviewDecision {
                        fingerprint: candidate.fingerprint.clone(),
                        verdict: ReviewVerdict::Accept,
                    })
                    .collect(),
                merged_statement: None,
            },
        };

        let report = KnowledgeVerificationService::new(&mut store, &writer)
            .auto(
                &repository,
                "SUB",
                "auto-claude",
                "2026-08-23T00:00:00Z",
                false,
                &agent,
            )
            .expect("auto run");

        assert_eq!(report.candidates_skipped_possible_duplicate, 1);
        assert_eq!(report.documents_written, 0);
        assert!(store.decisions.borrow().is_empty());

        let forced_report = KnowledgeVerificationService::new(&mut store, &writer)
            .auto(
                &repository,
                "SUB",
                "auto-claude",
                "2026-08-23T00:01:00Z",
                true,
                &agent,
            )
            .expect("forced auto run");

        assert_eq!(forced_report.documents_written, 1);
        assert_eq!(forced_report.candidates_accepted, 1);
    }

    #[test]
    fn auto_with_progress_reports_position_and_total_once_per_cluster() {
        // Two unrelated statements never cluster together (no shared
        // vocabulary), so this is two clusters of one candidate each.
        let first = candidate(BusinessKind::Requirement, "Cancellation preserves access.");
        let second = candidate(BusinessKind::Invariant, "Never delete billing history.");
        let mut store = FakeStore {
            pending: vec![first.clone(), second.clone()],
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let agent = FakeReviewAgent {
            review: |candidates: &[KnowledgeCandidate]| ClusterReview {
                decisions: candidates
                    .iter()
                    .map(|candidate| CandidateReviewDecision {
                        fingerprint: candidate.fingerprint.clone(),
                        verdict: ReviewVerdict::Accept,
                    })
                    .collect(),
                merged_statement: None,
            },
        };
        let mut seen = Vec::new();

        KnowledgeVerificationService::new(&mut store, &writer)
            .auto_with_progress(
                &repository,
                "SUB",
                "auto-claude",
                "2026-08-23T00:00:00Z",
                false,
                &agent,
                &mut |position, total, cluster| {
                    seen.push((position, total, cluster.fingerprints.len()));
                },
                &mut |_, _, _| {},
            )
            .expect("auto run");

        assert_eq!(seen, vec![(1, 2, 1), (2, 2, 1)]);
    }

    #[test]
    fn auto_with_progress_reports_a_result_per_candidate_after_each_cluster() {
        // No vocabulary overlap between any two of these three statements,
        // so each lands in its own single-candidate cluster -- isolating
        // one outcome kind (accept/reject/duplicate-skip) per cluster.
        let accepted_candidate = candidate(
            BusinessKind::Requirement,
            "The premium tier grants dashboard exports.",
        );
        let rejected_candidate =
            candidate(BusinessKind::Invariant, "Never delete billing history.");
        let duplicate_candidate = candidate(
            BusinessKind::Requirement,
            "Cancellation preserves paid access until the period ends.",
        );
        let existing = ctx_core::graph::GraphNode {
            stable_key: ctx_core::domain::StableKey::new("intent:REQ-SUB-001").expect("stable key"),
            kind: ctx_core::domain::NodeKind::Requirement,
            name: "Cancellation preserves access".to_owned(),
            content_hash: "hash".to_owned(),
            attributes: ctx_core::indexing::PlannedNodeAttributes::Business {
                id: "REQ-SUB-001".to_owned(),
                status: "active".to_owned(),
                visibility: ctx_core::business::Visibility::Private,
                body: "Cancellation preserves paid access until the period ends.".to_owned(),
                feature: None,
                source_uri: "requirement.yaml".to_owned(),
            },
        };
        let mut store = FakeStore {
            pending: vec![
                accepted_candidate.clone(),
                rejected_candidate.clone(),
                duplicate_candidate.clone(),
            ],
            graph: ctx_core::graph::GraphSnapshot {
                nodes: [(existing.stable_key.clone(), existing)]
                    .into_iter()
                    .collect(),
                edges: Vec::new(),
            },
            ..FakeStore::default()
        };
        let writer = FakeWriter::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let rejected_fingerprint = rejected_candidate.fingerprint.clone();
        let agent = FakeReviewAgent {
            review: move |candidates: &[KnowledgeCandidate]| ClusterReview {
                decisions: candidates
                    .iter()
                    .map(|candidate| CandidateReviewDecision {
                        fingerprint: candidate.fingerprint.clone(),
                        verdict: if candidate.fingerprint == rejected_fingerprint {
                            ReviewVerdict::Reject
                        } else {
                            ReviewVerdict::Accept
                        },
                    })
                    .collect(),
                merged_statement: None,
            },
        };
        let mut results: Vec<ReviewedCandidate> = Vec::new();

        KnowledgeVerificationService::new(&mut store, &writer)
            .auto_with_progress(
                &repository,
                "SUB",
                "auto-claude",
                "2026-08-23T00:00:00Z",
                false,
                &agent,
                &mut |_, _, _| {},
                &mut |_, _, reviewed| results.extend_from_slice(reviewed),
            )
            .expect("auto run");

        assert_eq!(results.len(), 3);
        assert!(results.contains(&ReviewedCandidate {
            statement: accepted_candidate.statement.clone(),
            outcome: CandidateOutcome::Accepted {
                document_id: "REQ-SUB-002".to_owned(),
            },
        }));
        assert!(results.contains(&ReviewedCandidate {
            statement: rejected_candidate.statement.clone(),
            outcome: CandidateOutcome::Rejected,
        }));
        assert!(results.contains(&ReviewedCandidate {
            statement: duplicate_candidate.statement.clone(),
            outcome: CandidateOutcome::SkippedPossibleDuplicate {
                existing_id: "REQ-SUB-001".to_owned(),
            },
        }));
    }
}
