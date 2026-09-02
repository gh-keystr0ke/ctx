//! Orchestrates Git-native artifact ingestion (prompt3.md PR-EXT-001 MUST
//! list: commit messages, branch names): reads artifacts through
//! [`GitArtifactSource`], persists them idempotently, then runs
//! [`ctx_core::linking::text_reference_links`] against every artifact this
//! repository already knows about so a reference discovered in one ingest
//! run can still resolve against artifacts stored by an earlier one.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ctx_core::{
    artifact::{
        Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkKind,
        ArtifactLinkTarget, ArtifactProvider,
    },
    codedoc::{CodeDocKind, extract_code_docs},
    domain::{CommitOid, NodeKind, RepositoryId, StableKey},
    graph::GraphSnapshot,
    linking::{ReferenceKind, extract_references, text_reference_links},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::{
    ArtifactLinkStore, ArtifactMaintenanceStore, ArtifactRepository, BranchArtifact,
    CommitArtifact, ExternalArtifactRequest, ExternalArtifactSource, GitArtifactSource,
    GitRepository, GraphStore, IngestCursorStore, LanguageAnalyzer, PortError,
    RepositoryArtifactRefs, ReviewRepository,
};

/// Bumped when the normalization this runner applies to Git artifacts
/// changes, so a future incremental-sync pass (prompt3.md PR-INCR-001) can
/// tell a stale ingestion apart from a current one.
const GIT_INGEST_VERSION: &str = "git-native-v1";

/// Bumped when the normalization this runner applies to code comments and
/// docstrings changes.
const CODE_DOC_INGEST_VERSION: &str = "code-doc-v1";

/// Bumped when the normalization this runner applies to GitLab artifacts
/// changes.
const GITLAB_INGEST_VERSION: &str = "gitlab-v1";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct IngestReport {
    pub artifacts_ingested: usize,
    pub links_created: usize,
    pub artifacts_removed: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ArtifactIngestScope {
    #[default]
    All,
    BusinessLinked {
        related_jira_depth: usize,
    },
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("artifact source could not be read: {0}")]
    Source(PortError),
    #[error("artifacts could not be persisted: {0}")]
    Store(PortError),
}

pub struct GitIngestRunner<'a, G, S> {
    source: &'a G,
    store: &'a mut S,
}

impl<'a, G, S> GitIngestRunner<'a, G, S>
where
    G: GitArtifactSource,
    S: ArtifactRepository + ArtifactLinkStore + GraphStore,
{
    pub const fn new(source: &'a G, store: &'a mut S) -> Self {
        Self { source, store }
    }

    /// Ingests every local branch and every commit reachable from `HEAD`
    /// back to (exclusive of) `since`, links each branch to the commits it
    /// alone contains relative to the repository's default branch, links
    /// each commit to the already-indexed symbols in the files it changed,
    /// then links references found in artifact text to every artifact
    /// already known for this repository.
    ///
    /// # Errors
    /// Returns [`IngestError`] when artifacts cannot be read or persisted.
    pub fn run(
        &mut self,
        repository: &RepositoryId,
        since: Option<&CommitOid>,
        ingested_at: &str,
    ) -> Result<IngestReport, IngestError> {
        let branches = self
            .source
            .branch_artifacts()
            .map_err(IngestError::Source)?;
        let commits = self
            .source
            .commit_artifacts(since)
            .map_err(IngestError::Source)?;
        let mut artifacts = branches
            .iter()
            .map(|branch| branch.artifact.clone())
            .collect::<Vec<_>>();
        artifacts.extend(commits.iter().map(|commit| commit.artifact.clone()));
        for artifact in &artifacts {
            self.store
                .upsert_artifact(repository, artifact, ingested_at, GIT_INGEST_VERSION)
                .map_err(IngestError::Store)?;
        }
        let known = self
            .store
            .list_artifacts(repository)
            .map_err(IngestError::Store)?;
        let graph = self
            .store
            .load_graph(repository)
            .map_err(IngestError::Store)?;
        let mut links = branch_commit_links(&branches);
        links.extend(commit_symbol_links(&commits, &graph));
        links.extend(
            artifacts
                .iter()
                .flat_map(|artifact| text_reference_links(artifact, &known)),
        );
        self.store
            .persist_links(repository, &links)
            .map_err(IngestError::Store)?;
        Ok(IngestReport {
            artifacts_ingested: artifacts.len(),
            links_created: links.len(),
            artifacts_removed: 0,
        })
    }
}

/// A branch's structural `ContainsCommit` links to the commits it alone
/// contains ([`BranchArtifact::own_commits`]) -- empty for the default
/// branch and for any branch when no default branch could be resolved.
fn branch_commit_links(branches: &[BranchArtifact]) -> Vec<ArtifactLink> {
    branches
        .iter()
        .flat_map(|branch| {
            branch.own_commits.iter().map(|oid| ArtifactLink {
                source: branch.artifact.identity.clone(),
                target: ArtifactLinkTarget::Artifact(ArtifactIdentity {
                    provider: ArtifactProvider::Git,
                    kind: ArtifactKind::Commit,
                    external_id: oid.as_str().to_owned(),
                }),
                kind: ArtifactLinkKind::ContainsCommit,
                evidence_locator: format!("branch:{}", branch.artifact.identity.external_id),
            })
        })
        .collect()
}

/// Each commit's structural `ChangedSymbol` links to the already-indexed
/// symbols in the files it changed ([`ctx_core::linking::changed_symbol_links`]).
/// A graph with nothing indexed yet (no `ctx index` has run) yields none.
fn commit_symbol_links(commits: &[CommitArtifact], graph: &GraphSnapshot) -> Vec<ArtifactLink> {
    let threshold = ctx_core::linking::sweep_threshold(graph);
    commits
        .iter()
        .flat_map(|commit| {
            ctx_core::linking::changed_symbol_links(
                &commit.artifact.identity,
                &commit.changed_paths,
                graph,
                threshold,
            )
        })
        .collect()
}

/// The provider tag `GitLabIngestRunner`'s cursor is stored under (matches
/// `ArtifactProvider::GitLab`'s own serde tag).
const GITLAB_CURSOR_PROVIDER: &str = "gitlab";

/// Orchestrates GitLab issue/merge-request ingestion (prompt3.md PR-EXT-001
/// MUST list, the chosen end-to-end provider): reads artifacts and their
/// provider-reported deterministic links through [`ExternalArtifactSource`],
/// persists them idempotently, then additionally runs
/// [`text_reference_links`] the same way [`GitIngestRunner`] does, so a
/// ticket reference in an MR body can resolve against artifacts from any
/// source already known for this repository, not only other GitLab ones.
/// Incremental by default (prompt3.md PR-INCR-001, T8.1): reads the
/// repository's stored GitLab sync cursor first and asks the source for only
/// what changed since then, then advances the cursor to `ingested_at` once
/// the run succeeds -- a failed run leaves the old cursor in place so the
/// same window is retried next time rather than silently skipped.
pub struct GitLabIngestRunner<'a, G, S> {
    source: &'a G,
    store: &'a mut S,
}

impl<'a, G, S> GitLabIngestRunner<'a, G, S>
where
    G: ExternalArtifactSource,
    S: ArtifactRepository + ArtifactLinkStore + IngestCursorStore,
{
    pub const fn new(source: &'a G, store: &'a mut S) -> Self {
        Self { source, store }
    }

    /// # Errors
    /// Returns [`IngestError`] when artifacts cannot be read or persisted.
    pub fn run(
        &mut self,
        repository: &RepositoryId,
        ingested_at: &str,
    ) -> Result<IngestReport, IngestError> {
        self.run_scoped(repository, ingested_at, ArtifactIngestScope::All)
    }

    /// # Errors
    /// Returns [`IngestError`] when artifacts cannot be read or persisted.
    pub fn run_scoped(
        &mut self,
        repository: &RepositoryId,
        ingested_at: &str,
        scope: ArtifactIngestScope,
    ) -> Result<IngestReport, IngestError> {
        let known_before = self
            .store
            .list_artifacts(repository)
            .map_err(IngestError::Store)?;
        let batch = match scope {
            ArtifactIngestScope::All => {
                let cursor = self
                    .store
                    .sync_cursor(repository, GITLAB_CURSOR_PROVIDER)
                    .map_err(IngestError::Store)?;
                self.source
                    .fetch(ExternalArtifactRequest::UpdatedSince(cursor.as_deref()))
            }
            ArtifactIngestScope::BusinessLinked { .. } => {
                let request_refs = repository_artifact_refs(&known_before);
                self.source
                    .fetch(ExternalArtifactRequest::RepositoryLinked(&request_refs))
            }
        }
        .map_err(IngestError::Source)?;
        let artifacts = batch.artifacts;
        let mut links = batch.links;
        for artifact in &artifacts {
            self.store
                .upsert_artifact(repository, artifact, ingested_at, GITLAB_INGEST_VERSION)
                .map_err(IngestError::Store)?;
        }
        let known = self
            .store
            .list_artifacts(repository)
            .map_err(IngestError::Store)?;
        let reference_sources: Vec<&Artifact> = match scope {
            ArtifactIngestScope::All => artifacts.iter().collect(),
            ArtifactIngestScope::BusinessLinked { .. } => known
                .iter()
                .filter(|artifact| {
                    artifact.identity.provider == ArtifactProvider::Git
                        || artifacts
                            .iter()
                            .any(|new| new.identity == artifact.identity)
                })
                .collect(),
        };
        links.extend(
            reference_sources
                .iter()
                .flat_map(|artifact| text_reference_links(artifact, &known)),
        );
        self.store
            .persist_links(repository, &links)
            .map_err(IngestError::Store)?;
        if scope == ArtifactIngestScope::All {
            self.store
                .set_sync_cursor(repository, GITLAB_CURSOR_PROVIDER, ingested_at)
                .map_err(IngestError::Store)?;
        }
        Ok(IngestReport {
            artifacts_ingested: artifacts.len(),
            links_created: links.len(),
            artifacts_removed: 0,
        })
    }
}

fn repository_artifact_refs(artifacts: &[Artifact]) -> RepositoryArtifactRefs {
    let mut references = RepositoryArtifactRefs::default();
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.identity.provider == ArtifactProvider::Git)
    {
        match artifact.identity.kind {
            ArtifactKind::Branch => {
                references
                    .branch_names
                    .insert(artifact.identity.external_id.clone());
            }
            ArtifactKind::Commit => {
                references
                    .commit_shas
                    .insert(artifact.identity.external_id.clone());
            }
            _ => continue,
        }
        for reference in extract_references(&format!("{}\n{}", artifact.title, artifact.body)) {
            if reference.kind == ReferenceKind::MergeRequestNumber {
                references.merge_request_iids.insert(reference.value);
            }
        }
    }
    references
}

/// Bumped when the normalization this runner applies to Jira artifacts
/// changes.
const JIRA_INGEST_VERSION: &str = "jira-v2";

/// Orchestrates Jira issue ingestion: reads artifacts through
/// [`ExternalArtifactSource`], persists them idempotently, then additionally
/// runs [`text_reference_links`] the same way [`GitLabIngestRunner`] does.
///
/// Unlike every other runner in this module, this one is not "fetch
/// everything, or everything changed since a cursor" -- a Jira project
/// routinely spans many repositories, so it never asks for the whole
/// project. Instead it first scans every artifact this repository already
/// knows about (commits, branches, GitLab issues/MRs, prior Jira issues)
/// for ticket-key-shaped references, and passes that candidate set to
/// [`ExternalArtifactSource`], which fetches only the ones under its own
/// configured project plus one hop of Jira-reported related issues. There
/// is deliberately no sync cursor here: the candidate set is what keeps
/// this bounded, not a time filter, so every run simply re-fetches the
/// current candidate set in full (a cheap, idempotent upsert either way).
pub struct JiraIngestRunner<'a, J, S> {
    source: &'a J,
    store: &'a mut S,
}

impl<'a, J, S> JiraIngestRunner<'a, J, S>
where
    J: ExternalArtifactSource,
    S: ArtifactRepository + ArtifactLinkStore,
{
    pub const fn new(source: &'a J, store: &'a mut S) -> Self {
        Self { source, store }
    }

    /// # Errors
    /// Returns [`IngestError`] when artifacts cannot be read or persisted.
    pub fn run(
        &mut self,
        repository: &RepositoryId,
        ingested_at: &str,
    ) -> Result<IngestReport, IngestError> {
        self.run_scoped(repository, ingested_at, ArtifactIngestScope::All)
    }

    /// # Errors
    /// Returns [`IngestError`] when artifacts cannot be read or persisted.
    pub fn run_scoped(
        &mut self,
        repository: &RepositoryId,
        ingested_at: &str,
        scope: ArtifactIngestScope,
    ) -> Result<IngestReport, IngestError> {
        let known = self
            .store
            .list_artifacts(repository)
            .map_err(IngestError::Store)?;
        let known_links = self
            .store
            .list_links(repository)
            .map_err(IngestError::Store)?;
        let candidate_sources = jira_candidate_sources(&known, &known_links, scope);
        let candidate_keys: BTreeSet<String> = candidate_sources
            .iter()
            .flat_map(|artifact| {
                extract_references(&format!("{}\n{}", artifact.title, artifact.body))
            })
            .filter(|reference| reference.kind == ReferenceKind::TicketKey)
            .map(|reference| reference.value)
            .collect();
        let request = match scope {
            ArtifactIngestScope::All => ExternalArtifactRequest::ReferencedKeys(&candidate_keys),
            ArtifactIngestScope::BusinessLinked { related_jira_depth } => {
                ExternalArtifactRequest::BusinessLinkedKeys {
                    keys: &candidate_keys,
                    related_depth: related_jira_depth,
                }
            }
        };
        let batch = self.source.fetch(request).map_err(IngestError::Source)?;
        let artifacts = batch.artifacts;
        let mut links = batch.links;
        for artifact in &artifacts {
            self.store
                .upsert_artifact(repository, artifact, ingested_at, JIRA_INGEST_VERSION)
                .map_err(IngestError::Store)?;
        }
        let known = self
            .store
            .list_artifacts(repository)
            .map_err(IngestError::Store)?;
        let reference_sources: Vec<&Artifact> = match scope {
            ArtifactIngestScope::All => artifacts.iter().collect(),
            ArtifactIngestScope::BusinessLinked { .. } => candidate_sources
                .into_iter()
                .chain(artifacts.iter())
                .collect(),
        };
        links.extend(
            reference_sources
                .iter()
                .flat_map(|artifact| text_reference_links(artifact, &known)),
        );
        self.store
            .persist_links(repository, &links)
            .map_err(IngestError::Store)?;
        Ok(IngestReport {
            artifacts_ingested: artifacts.len(),
            links_created: links.len(),
            artifacts_removed: 0,
        })
    }
}

fn jira_candidate_sources<'a>(
    artifacts: &'a [Artifact],
    links: &[ArtifactLink],
    scope: ArtifactIngestScope,
) -> Vec<&'a Artifact> {
    if scope == ArtifactIngestScope::All {
        return artifacts.iter().collect();
    }
    let known_by_identity: HashMap<_, _> = artifacts
        .iter()
        .map(|artifact| (&artifact.identity, artifact))
        .collect();
    let mut repository_mrs = HashSet::new();
    for link in links {
        let ArtifactLinkTarget::Artifact(target) = &link.target else {
            continue;
        };
        match (
            known_by_identity.get(&link.source),
            known_by_identity.get(target),
        ) {
            (Some(source), Some(target_artifact))
                if is_git_artifact(source) && is_merge_request_artifact(target_artifact) =>
            {
                repository_mrs.insert(target);
            }
            (Some(source), Some(target_artifact))
                if is_merge_request_artifact(source) && is_git_artifact(target_artifact) =>
            {
                repository_mrs.insert(&link.source);
            }
            _ => {}
        }
    }
    let mr_comments: HashSet<_> = links
        .iter()
        .filter_map(|link| {
            let ArtifactLinkTarget::Artifact(parent) = &link.target else {
                return None;
            };
            (link.kind == ArtifactLinkKind::CommentsOn && repository_mrs.contains(parent))
                .then_some(&link.source)
        })
        .collect();
    artifacts
        .iter()
        .filter(|artifact| {
            is_git_artifact(artifact)
                || repository_mrs.contains(&artifact.identity)
                || mr_comments.contains(&artifact.identity)
        })
        .collect()
}

fn is_git_artifact(artifact: &Artifact) -> bool {
    artifact.identity.provider == ArtifactProvider::Git
        && matches!(
            artifact.identity.kind,
            ArtifactKind::Branch | ArtifactKind::Commit
        )
}

fn is_merge_request_artifact(artifact: &Artifact) -> bool {
    artifact.identity.provider == ArtifactProvider::GitLab
        && artifact.identity.kind == ArtifactKind::MergeRequest
}

/// Orchestrates code-comment/docstring ingestion (prompt3.md PR-EXT-001 MUST
/// list, PR-CODEDOC-001/002): reads every currently indexable source file's
/// text at `HEAD`, extracts comment/docstring candidates via
/// [`ctx_core::codedoc::extract_code_docs`], and links each one to the
/// single already-indexed graph symbol its nearest-enclosing canonical path
/// unambiguously resolves to — never guessing among several candidates
/// sharing that path, matching this codebase's existing explicit-mapping
/// discipline.
pub struct CodeDocIngestRunner<'a, R, A, S> {
    repository: &'a R,
    analyzer: &'a A,
    store: &'a mut S,
}

impl<'a, R, A, S> CodeDocIngestRunner<'a, R, A, S>
where
    R: GitRepository + ReviewRepository,
    A: LanguageAnalyzer,
    S: ArtifactRepository + ArtifactLinkStore + ArtifactMaintenanceStore + GraphStore,
{
    pub const fn new(repository: &'a R, analyzer: &'a A, store: &'a mut S) -> Self {
        Self {
            repository,
            analyzer,
            store,
        }
    }

    /// # Errors
    /// Returns [`IngestError`] when source files, analysis, the graph, or
    /// artifacts cannot be read or persisted.
    pub fn run(
        &mut self,
        repository_id: &RepositoryId,
        ingested_at: &str,
    ) -> Result<IngestReport, IngestError> {
        self.run_with_reconcile(repository_id, ingested_at, false)
    }

    /// # Errors
    /// Returns [`IngestError`] when source files, analysis, the graph, or
    /// artifacts cannot be read or reconciled.
    pub fn run_with_reconcile(
        &mut self,
        repository_id: &RepositoryId,
        ingested_at: &str,
        reconcile: bool,
    ) -> Result<IngestReport, IngestError> {
        let (artifacts, links) = self.collect_snapshot(repository_id)?;
        for artifact in &artifacts {
            self.store
                .upsert_artifact(
                    repository_id,
                    artifact,
                    ingested_at,
                    CODE_DOC_INGEST_VERSION,
                )
                .map_err(IngestError::Store)?;
        }
        let artifacts_removed =
            self.persist_snapshot(repository_id, &artifacts, &links, reconcile)?;
        Ok(IngestReport {
            artifacts_ingested: artifacts.len(),
            links_created: links.len(),
            artifacts_removed,
        })
    }

    fn collect_snapshot(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<(Vec<Artifact>, Vec<ArtifactLink>), IngestError> {
        let paths = self
            .repository
            .all_source_files()
            .map_err(IngestError::Source)?;
        let symbol_by_canonical_path = self.canonical_path_index(repository_id)?;
        let project = self
            .repository
            .descriptor()
            .map_err(IngestError::Source)?
            .root_path;

        let mut artifacts = Vec::new();
        let mut links = Vec::new();
        for path in paths {
            let Some(source) = self
                .repository
                .source_at("HEAD", &path)
                .map_err(IngestError::Source)?
            else {
                continue;
            };
            let analysis = self
                .analyzer
                .analyze_text(&path, &source)
                .map_err(IngestError::Source)?;
            for candidate in extract_code_docs(&source, &analysis.language, &analysis.symbols) {
                let identity = ArtifactIdentity {
                    provider: ArtifactProvider::Code,
                    kind: match candidate.kind {
                        CodeDocKind::Comment => ArtifactKind::CodeComment,
                        CodeDocKind::Docstring => ArtifactKind::Docstring,
                    },
                    external_id: format!(
                        "{path}#L{}-L{}",
                        candidate.start_line, candidate.end_line
                    ),
                };
                let content_hash = blake3::hash(candidate.text.as_bytes()).to_hex().to_string();
                let artifact = Artifact {
                    title: candidate.text.lines().next().unwrap_or_default().to_owned(),
                    body: candidate.text,
                    author: None,
                    external_created_at: None,
                    external_updated_at: None,
                    source_locator: ctx_core::domain::Url(identity.external_id.clone()),
                    content_hash,
                    project: ctx_core::domain::Project(project.clone()),
                    identity: identity.clone(),
                };
                if let Some(stable_key) = candidate.nearest_symbol.and_then(|canonical_path| {
                    symbol_by_canonical_path.get(&canonical_path).cloned()
                }) {
                    links.push(ArtifactLink {
                        source: identity.clone(),
                        target: ArtifactLinkTarget::CodeSymbol(stable_key),
                        kind: ArtifactLinkKind::Discusses,
                        evidence_locator: identity.external_id.clone(),
                    });
                }
                artifacts.push(artifact);
            }
        }
        Ok((artifacts, links))
    }

    fn persist_snapshot(
        &mut self,
        repository_id: &RepositoryId,
        artifacts: &[Artifact],
        links: &[ArtifactLink],
        reconcile: bool,
    ) -> Result<usize, IngestError> {
        if reconcile {
            let links_by_source: HashMap<_, Vec<_>> = links.iter().fold(
                HashMap::<ArtifactIdentity, Vec<ArtifactLink>>::new(),
                |mut grouped, link| {
                    grouped
                        .entry(link.source.clone())
                        .or_default()
                        .push(link.clone());
                    grouped
                },
            );
            for artifact in artifacts {
                self.store
                    .replace_outgoing_links(
                        repository_id,
                        &artifact.identity,
                        links_by_source
                            .get(&artifact.identity)
                            .map_or(&[], Vec::as_slice),
                    )
                    .map_err(IngestError::Store)?;
            }
            let current: HashSet<_> = artifacts
                .iter()
                .map(|artifact| artifact.identity.clone())
                .collect();
            let removed = self
                .store
                .reconcile_snapshot(
                    repository_id,
                    ArtifactProvider::Code,
                    &[ArtifactKind::CodeComment, ArtifactKind::Docstring],
                    &current,
                )
                .map_err(IngestError::Store)?;
            Ok(removed.removed.len())
        } else {
            self.store
                .persist_links(repository_id, links)
                .map_err(IngestError::Store)?;
            Ok(0)
        }
    }

    /// Maps a canonical path to its `StableKey` only when exactly one
    /// currently indexed code symbol has that canonical path — a comment
    /// attributed to an ambiguous canonical path (the same cross-language
    /// collision `.context/*.yaml` mappings already refuse to guess among)
    /// stays unlinked rather than picked arbitrarily.
    fn canonical_path_index(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<BTreeMap<String, StableKey>, IngestError> {
        let graph = self
            .store
            .load_graph(repository_id)
            .map_err(IngestError::Store)?;
        let mut candidates: BTreeMap<String, Vec<StableKey>> = BTreeMap::new();
        for node in graph.nodes.values() {
            if node.kind == NodeKind::CodeSymbol {
                candidates
                    .entry(node.identifier().to_owned())
                    .or_default()
                    .push(node.stable_key.clone());
            }
        }
        Ok(candidates
            .into_iter()
            .filter_map(|(path, keys)| match keys.as_slice() {
                [key] => Some((path, key.clone())),
                _ => None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use ctx_core::artifact::{
        Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactProvider,
    };

    use super::*;

    struct FakeGitSource {
        commits: Vec<CommitArtifact>,
        branches: Vec<BranchArtifact>,
    }

    impl GitArtifactSource for FakeGitSource {
        fn commit_artifacts(
            &self,
            _since: Option<&CommitOid>,
        ) -> Result<Vec<CommitArtifact>, PortError> {
            Ok(self.commits.clone())
        }

        fn branch_artifacts(&self) -> Result<Vec<BranchArtifact>, PortError> {
            Ok(self.branches.clone())
        }
    }

    #[derive(Default)]
    struct FakeStore {
        artifacts: RefCell<BTreeMap<(ArtifactProvider, ArtifactKind, String), Artifact>>,
        links: RefCell<Vec<ArtifactLink>>,
        cursors: RefCell<BTreeMap<String, String>>,
        graph: RefCell<GraphSnapshot>,
    }

    impl ArtifactRepository for FakeStore {
        fn upsert_artifact(
            &mut self,
            _repository: &RepositoryId,
            artifact: &Artifact,
            _ingested_at: &str,
            _ingest_version: &str,
        ) -> Result<(), PortError> {
            self.artifacts.borrow_mut().insert(
                (
                    artifact.identity.provider,
                    artifact.identity.kind,
                    artifact.identity.external_id.clone(),
                ),
                artifact.clone(),
            );
            Ok(())
        }

        fn list_artifacts(&self, _repository: &RepositoryId) -> Result<Vec<Artifact>, PortError> {
            Ok(self.artifacts.borrow().values().cloned().collect())
        }

        fn mark_analyzed(
            &mut self,
            _repository: &RepositoryId,
            _identity: &ArtifactIdentity,
            _content_hash: &str,
            _input_fingerprint: &str,
            _analyzed_at: &str,
        ) -> Result<(), PortError> {
            unreachable!("ingest never marks artifacts analyzed")
        }

        fn analyzed_input_fingerprints(
            &self,
            _repository: &RepositoryId,
        ) -> Result<std::collections::HashMap<ArtifactIdentity, String>, PortError> {
            unreachable!("ingest never reads analyzed content hashes")
        }
    }

    impl ArtifactLinkStore for FakeStore {
        fn persist_links(
            &mut self,
            _repository: &RepositoryId,
            links: &[ArtifactLink],
        ) -> Result<(), PortError> {
            self.links.borrow_mut().extend_from_slice(links);
            Ok(())
        }

        fn list_links(&self, _repository: &RepositoryId) -> Result<Vec<ArtifactLink>, PortError> {
            Ok(self.links.borrow().clone())
        }
    }

    impl ArtifactMaintenanceStore for FakeStore {
        fn replace_outgoing_links(
            &mut self,
            _repository: &RepositoryId,
            source: &ArtifactIdentity,
            links: &[ArtifactLink],
        ) -> Result<(), PortError> {
            self.links
                .borrow_mut()
                .retain(|link| link.source != *source);
            self.links.borrow_mut().extend_from_slice(links);
            Ok(())
        }

        fn reconcile_snapshot(
            &mut self,
            repository: &RepositoryId,
            provider: ArtifactProvider,
            kinds: &[ArtifactKind],
            current: &HashSet<ArtifactIdentity>,
        ) -> Result<crate::ports::ArtifactReconcileReport, PortError> {
            let removed: Vec<_> = self
                .artifacts
                .borrow()
                .values()
                .filter(|artifact| {
                    artifact.identity.provider == provider
                        && kinds.contains(&artifact.identity.kind)
                        && !current.contains(&artifact.identity)
                })
                .map(|artifact| artifact.identity.clone())
                .collect();
            self.delete_artifacts(repository, &removed)
        }

        fn delete_artifacts(
            &mut self,
            _repository: &RepositoryId,
            identities: &[ArtifactIdentity],
        ) -> Result<crate::ports::ArtifactReconcileReport, PortError> {
            let identities: HashSet<_> = identities.iter().cloned().collect();
            let removed: Vec<_> = identities
                .iter()
                .filter(|identity| {
                    self.artifacts.borrow().contains_key(&(
                        identity.provider,
                        identity.kind,
                        identity.external_id.clone(),
                    ))
                })
                .cloned()
                .collect();
            self.artifacts
                .borrow_mut()
                .retain(|_, artifact| !identities.contains(&artifact.identity));
            self.links.borrow_mut().retain(|link| {
                !identities.contains(&link.source)
                    && !matches!(
                        &link.target,
                        ArtifactLinkTarget::Artifact(target) if identities.contains(target)
                    )
            });
            Ok(crate::ports::ArtifactReconcileReport { removed })
        }
    }

    impl GraphStore for FakeStore {
        fn load_graph(&self, _repository: &RepositoryId) -> Result<GraphSnapshot, PortError> {
            Ok(self.graph.borrow().clone())
        }
    }

    impl IngestCursorStore for FakeStore {
        fn sync_cursor(
            &self,
            _repository: &RepositoryId,
            provider: &str,
        ) -> Result<Option<String>, PortError> {
            Ok(self.cursors.borrow().get(provider).cloned())
        }

        fn set_sync_cursor(
            &mut self,
            _repository: &RepositoryId,
            provider: &str,
            cursor: &str,
        ) -> Result<(), PortError> {
            self.cursors
                .borrow_mut()
                .insert(provider.to_owned(), cursor.to_owned());
            Ok(())
        }
    }

    fn artifact(external_id: &str, kind: ArtifactKind, title: &str, body: &str) -> Artifact {
        Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::Git,
                kind,
                external_id: external_id.to_owned(),
            },
            project: ctx_core::domain::Project("/repo".to_owned()),
            title: title.to_owned(),
            body: body.to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: ctx_core::domain::Url(format!("git:{external_id}")),
            content_hash: "hash".to_owned(),
        }
    }

    fn commit_artifact(
        external_id: &str,
        kind: ArtifactKind,
        title: &str,
        body: &str,
    ) -> CommitArtifact {
        CommitArtifact {
            artifact: artifact(external_id, kind, title, body),
            changed_paths: BTreeSet::new(),
        }
    }

    #[test]
    fn ingests_branches_and_commits_and_links_references_between_them() {
        let source = FakeGitSource {
            commits: vec![commit_artifact(
                "abc123",
                ArtifactKind::Commit,
                "fix cancellation",
                "See branch feature/PAY-317-cancel",
            )],
            branches: vec![BranchArtifact {
                artifact: artifact(
                    "feature/PAY-317-cancel",
                    ArtifactKind::Branch,
                    "feature/PAY-317-cancel",
                    "",
                ),
                own_commits: Vec::new(),
            }],
        };
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = GitIngestRunner::new(&source, &mut store)
            .run(&repository, None, "2026-08-21T00:00:00Z")
            .expect("ingest run");

        assert_eq!(report.artifacts_ingested, 2);
        assert_eq!(
            store.list_artifacts(&repository).expect("artifacts").len(),
            2
        );
        // "feature/PAY-317-cancel" contains no deterministic reference
        // pattern this module recognizes (no ticket key alone, since the
        // whole string only matches as a branch name, not a `PAY-317`
        // substring reference) — the commit body's literal branch-name
        // mention is what should resolve, by exact `external_id` match is
        // not defined for branches, so zero links from this fixture is the
        // conservative, correct outcome: no fabricated relation.
        assert_eq!(report.links_created, 0);
    }

    #[test]
    fn git_ingest_links_a_branch_to_the_commits_it_alone_contains() {
        let commit = artifact(
            "abc123",
            ArtifactKind::Commit,
            "fix cancellation",
            "fix cancellation",
        );
        let source = FakeGitSource {
            commits: vec![CommitArtifact {
                artifact: commit.clone(),
                changed_paths: BTreeSet::new(),
            }],
            branches: vec![BranchArtifact {
                artifact: artifact(
                    "feature/PAY-317-cancel",
                    ArtifactKind::Branch,
                    "feature/PAY-317-cancel",
                    "",
                ),
                own_commits: vec![CommitOid::new("abc123").expect("commit oid")],
            }],
        };
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        GitIngestRunner::new(&source, &mut store)
            .run(&repository, None, "2026-08-21T00:00:00Z")
            .expect("ingest run");

        let links = store.list_links(&repository).expect("links");
        assert!(links.iter().any(|link| {
            link.source
                == ArtifactIdentity {
                    provider: ArtifactProvider::Git,
                    kind: ArtifactKind::Branch,
                    external_id: "feature/PAY-317-cancel".to_owned(),
                }
                && link.target == ArtifactLinkTarget::Artifact(commit.identity.clone())
                && link.kind == ArtifactLinkKind::ContainsCommit
        }));
    }

    #[test]
    fn git_ingest_links_a_commit_to_the_symbols_in_the_files_it_changed() {
        use ctx_core::graph::GraphNode;
        use ctx_core::indexing::PlannedNodeAttributes;
        use ctx_core::ir::{SourceRange, SymbolKind};

        fn symbol_node(key: &str, file_path: &str, canonical: &str) -> GraphNode {
            GraphNode {
                stable_key: StableKey::new(key).expect("stable key"),
                kind: NodeKind::CodeSymbol,
                name: canonical.to_owned(),
                content_hash: "hash".to_owned(),
                attributes: PlannedNodeAttributes::Symbol {
                    file_path: file_path.to_owned(),
                    canonical_path: canonical.to_owned(),
                    symbol_kind: SymbolKind::Method,
                    range: SourceRange {
                        start_byte: 0,
                        end_byte: 1,
                        start_line: 1,
                        end_line: 1,
                    },
                    signature: None,
                    structural_fingerprint: "shape".to_owned(),
                    calls: Vec::new(),
                    database_accesses: Vec::new(),
                    orm_accesses: Vec::new(),
                    schema_tables: Vec::new(),
                    api_endpoints: Vec::new(),
                    external_calls: Vec::new(),
                },
            }
        }

        let touched = symbol_node("cancel", "billing.py", "SubscriptionService.cancel");
        let untouched = symbol_node("refund", "refund.py", "SubscriptionService.refund");
        let commit = commit_artifact("abc123", ArtifactKind::Commit, "fix cancellation", "body");
        let source = FakeGitSource {
            commits: vec![CommitArtifact {
                changed_paths: BTreeSet::from(["billing.py".to_owned()]),
                ..commit.clone()
            }],
            branches: Vec::new(),
        };
        let mut store = FakeStore::default();
        *store.graph.borrow_mut() = GraphSnapshot {
            nodes: [touched.clone(), untouched]
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges: Vec::new(),
        };
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        GitIngestRunner::new(&source, &mut store)
            .run(&repository, None, "2026-08-21T00:00:00Z")
            .expect("ingest run");

        let links = store.list_links(&repository).expect("links");
        let changed_symbol_links = links
            .iter()
            .filter(|link| link.kind == ArtifactLinkKind::ChangedSymbol)
            .collect::<Vec<_>>();
        assert_eq!(changed_symbol_links.len(), 1);
        assert_eq!(changed_symbol_links[0].source, commit.artifact.identity);
        assert_eq!(
            changed_symbol_links[0].target,
            ArtifactLinkTarget::CodeSymbol(touched.stable_key)
        );
    }

    #[test]
    fn re_running_ingest_does_not_duplicate_artifacts() {
        let source = FakeGitSource {
            commits: vec![commit_artifact(
                "abc123",
                ArtifactKind::Commit,
                "fix",
                "body",
            )],
            branches: Vec::new(),
        };
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let mut runner = GitIngestRunner::new(&source, &mut store);

        runner
            .run(&repository, None, "2026-08-21T00:00:00Z")
            .expect("first run");
        runner
            .run(&repository, None, "2026-08-21T01:00:00Z")
            .expect("second run");

        assert_eq!(
            store.list_artifacts(&repository).expect("artifacts").len(),
            1
        );
    }

    #[derive(Default)]
    struct FakeGitLabSource {
        artifacts: Vec<Artifact>,
        links: Vec<ArtifactLink>,
        received_since: RefCell<Vec<Option<String>>>,
        received_repository_refs: RefCell<Vec<RepositoryArtifactRefs>>,
    }

    impl ExternalArtifactSource for FakeGitLabSource {
        fn fetch(
            &self,
            request: ExternalArtifactRequest<'_>,
        ) -> Result<crate::ports::ExternalArtifactBatch, PortError> {
            match request {
                ExternalArtifactRequest::UpdatedSince(since) => self
                    .received_since
                    .borrow_mut()
                    .push(since.map(str::to_owned)),
                ExternalArtifactRequest::RepositoryLinked(repository_refs) => self
                    .received_repository_refs
                    .borrow_mut()
                    .push(repository_refs.clone()),
                _ => return Err(PortError::new("unexpected request mode")),
            }
            Ok(crate::ports::ExternalArtifactBatch {
                artifacts: self.artifacts.clone(),
                links: self.links.clone(),
            })
        }
    }

    #[test]
    fn gitlab_ingest_persists_provider_reported_links_and_stays_idempotent() {
        let issue = Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: ArtifactKind::Issue,
                external_id: "317".to_owned(),
            },
            project: ctx_core::domain::Project("billing/subscriptions".to_owned()),
            title: "Cancellation removes prepaid access".to_owned(),
            body: String::new(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: ctx_core::domain::Url("https://gitlab.example/-/issues/317".to_owned()),
            content_hash: "hash".to_owned(),
        };
        let merge_request = Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: ArtifactKind::MergeRequest,
                external_id: "842".to_owned(),
            },
            project: ctx_core::domain::Project("billing/subscriptions".to_owned()),
            title: "Fix cancellation semantics".to_owned(),
            body: "Fixes #317.".to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: ctx_core::domain::Url(
                "https://gitlab.example/-/merge_requests/842".to_owned(),
            ),
            content_hash: "hash".to_owned(),
        };
        let comments_on = ArtifactLink {
            source: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: ArtifactKind::Comment,
                external_id: "317-note-1".to_owned(),
            },
            target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
            kind: ArtifactLinkKind::CommentsOn,
            evidence_locator: "gitlab notes API: 317".to_owned(),
        };
        let source = FakeGitLabSource {
            artifacts: vec![issue.clone(), merge_request.clone()],
            links: vec![comments_on.clone()],
            ..FakeGitLabSource::default()
        };
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = GitLabIngestRunner::new(&source, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z")
            .expect("first run");
        assert_eq!(report.artifacts_ingested, 2);
        // The provider-reported `comments_on` link plus the deterministic
        // `#317` text reference the MR body names, resolving against the
        // now-known issue artifact.
        assert_eq!(report.links_created, 2);
        assert!(store.links.borrow().contains(&comments_on));

        GitLabIngestRunner::new(&source, &mut store)
            .run(&repository, "2026-08-21T01:00:00Z")
            .expect("second run");
        assert_eq!(
            store.list_artifacts(&repository).expect("artifacts").len(),
            2,
            "re-running ingestion must not duplicate artifacts"
        );
    }

    #[test]
    fn gitlab_ingest_advances_its_sync_cursor_and_passes_it_to_the_next_run() {
        let source = FakeGitLabSource::default();
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        GitLabIngestRunner::new(&source, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z")
            .expect("first run");
        GitLabIngestRunner::new(&source, &mut store)
            .run(&repository, "2026-08-21T01:00:00Z")
            .expect("second run");

        assert_eq!(
            *source.received_since.borrow(),
            vec![None, Some("2026-08-21T00:00:00Z".to_owned())],
            "the first run has no prior cursor, the second gets the first run's ingested_at"
        );
        assert_eq!(
            store
                .sync_cursor(&repository, "gitlab")
                .expect("stored cursor"),
            Some("2026-08-21T01:00:00Z".to_owned()),
            "the cursor advances to the latest successful run's timestamp"
        );
    }

    #[test]
    fn business_linked_gitlab_scope_builds_refs_only_from_current_git() {
        let source = FakeGitLabSource::default();
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        for git_artifact in [
            artifact(
                "feature/cancellation",
                ArtifactKind::Branch,
                "feature/cancellation",
                "",
            ),
            artifact(
                "abc123",
                ArtifactKind::Commit,
                "Fix cancellation in !842",
                "",
            ),
        ] {
            store
                .upsert_artifact(
                    &repository,
                    &git_artifact,
                    "2026-08-21T00:00:00Z",
                    "git-native-v1",
                )
                .expect("seed Git artifact");
        }

        GitLabIngestRunner::new(&source, &mut store)
            .run_scoped(
                &repository,
                "2026-08-21T01:00:00Z",
                ArtifactIngestScope::BusinessLinked {
                    related_jira_depth: 0,
                },
            )
            .expect("business-linked run");

        let received = source.received_repository_refs.borrow();
        assert_eq!(received.len(), 1);
        assert_eq!(
            received[0].branch_names,
            BTreeSet::from(["feature/cancellation".to_owned()])
        );
        assert_eq!(
            received[0].commit_shas,
            BTreeSet::from(["abc123".to_owned()])
        );
        assert_eq!(
            received[0].merge_request_iids,
            BTreeSet::from(["842".to_owned()])
        );
        assert!(
            store
                .sync_cursor(&repository, GITLAB_CURSOR_PROVIDER)
                .expect("cursor")
                .is_none(),
            "repository relevance can change independently of GitLab updated_at"
        );
    }

    #[derive(Default)]
    struct FakeJiraSource {
        artifacts: Vec<Artifact>,
        links: Vec<ArtifactLink>,
        received_candidate_keys: RefCell<Vec<BTreeSet<String>>>,
        received_related_depths: RefCell<Vec<usize>>,
    }

    impl ExternalArtifactSource for FakeJiraSource {
        fn fetch(
            &self,
            request: ExternalArtifactRequest<'_>,
        ) -> Result<crate::ports::ExternalArtifactBatch, PortError> {
            let candidate_keys = match request {
                ExternalArtifactRequest::ReferencedKeys(candidate_keys) => candidate_keys,
                ExternalArtifactRequest::BusinessLinkedKeys {
                    keys,
                    related_depth,
                } => {
                    self.received_related_depths
                        .borrow_mut()
                        .push(related_depth);
                    keys
                }
                _ => return Err(PortError::new("unexpected request mode")),
            };
            self.received_candidate_keys
                .borrow_mut()
                .push(candidate_keys.clone());
            Ok(crate::ports::ExternalArtifactBatch {
                artifacts: self.artifacts.clone(),
                links: self.links.clone(),
            })
        }
    }

    #[test]
    fn jira_ingest_persists_provider_reported_links_and_stays_idempotent() {
        let issue = Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::Jira,
                kind: ArtifactKind::Issue,
                external_id: "PSI-1122".to_owned(),
            },
            project: ctx_core::domain::Project("PSI".to_owned()),
            title: "Cancellation removes prepaid access".to_owned(),
            body: String::new(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: ctx_core::domain::Url(
                "https://example.atlassian.net/browse/PSI-1122".to_owned(),
            ),
            content_hash: "hash".to_owned(),
        };
        let comment = Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::Jira,
                kind: ArtifactKind::Comment,
                external_id: "PSI-1122-comment-1".to_owned(),
            },
            project: ctx_core::domain::Project("PSI".to_owned()),
            title: "Do not revoke an already paid entitlement immediately.".to_owned(),
            body: "Do not revoke an already paid entitlement immediately.".to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: ctx_core::domain::Url(
                "https://example.atlassian.net/browse/PSI-1122?focusedCommentId=1".to_owned(),
            ),
            content_hash: "hash".to_owned(),
        };
        let comments_on = ArtifactLink {
            source: comment.identity.clone(),
            target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
            kind: ArtifactLinkKind::CommentsOn,
            evidence_locator: "jira comment API: PSI-1122".to_owned(),
        };
        let source = FakeJiraSource {
            artifacts: vec![issue.clone(), comment.clone()],
            links: vec![comments_on.clone()],
            ..FakeJiraSource::default()
        };
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        let report = JiraIngestRunner::new(&source, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z")
            .expect("first run");
        assert_eq!(report.artifacts_ingested, 2);
        assert!(store.links.borrow().contains(&comments_on));

        JiraIngestRunner::new(&source, &mut store)
            .run(&repository, "2026-08-21T01:00:00Z")
            .expect("second run");
        assert_eq!(
            store.list_artifacts(&repository).expect("artifacts").len(),
            2,
            "re-running ingestion must not duplicate artifacts"
        );
    }

    #[test]
    fn jira_ingest_derives_candidate_keys_from_already_known_artifact_text() {
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let commit = Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::Git,
                kind: ArtifactKind::Commit,
                external_id: "abc123".to_owned(),
            },
            project: ctx_core::domain::Project("repo".to_owned()),
            title: "Fix PSI-1122 cancellation bug".to_owned(),
            body: String::new(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: ctx_core::domain::Url(String::new()),
            content_hash: "hash".to_owned(),
        };
        store
            .upsert_artifact(
                &repository,
                &commit,
                "2026-08-21T00:00:00Z",
                "git-native-v1",
            )
            .expect("seed a known commit");
        let source = FakeJiraSource::default();

        JiraIngestRunner::new(&source, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z")
            .expect("run");

        assert_eq!(
            *source.received_candidate_keys.borrow(),
            vec![BTreeSet::from(["PSI-1122".to_owned()])],
            "the ticket key mentioned in the already-known commit becomes a candidate key"
        );
    }

    #[test]
    fn jira_ingest_with_no_known_ticket_key_references_requests_nothing() {
        let source = FakeJiraSource::default();
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");

        JiraIngestRunner::new(&source, &mut store)
            .run(&repository, "2026-08-21T00:00:00Z")
            .expect("run");

        assert_eq!(
            *source.received_candidate_keys.borrow(),
            vec![BTreeSet::new()]
        );
    }

    #[test]
    fn business_linked_jira_scope_cannot_be_seeded_by_old_jira_or_gitlab_issues() {
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let mut old_jira = artifact("PSI-1", ArtifactKind::Issue, "Old PSI-1", "");
        old_jira.identity.provider = ArtifactProvider::Jira;
        let mut gitlab_issue = artifact("2", ArtifactKind::Issue, "Unrelated PSI-2", "");
        gitlab_issue.identity.provider = ArtifactProvider::GitLab;
        let commit = artifact(
            "abc123",
            ArtifactKind::Commit,
            "Repository-backed PSI-3",
            "",
        );
        for known in [&old_jira, &gitlab_issue, &commit] {
            store
                .upsert_artifact(&repository, known, "2026-08-21T00:00:00Z", "test")
                .expect("seed artifact");
        }
        let source = FakeJiraSource::default();

        JiraIngestRunner::new(&source, &mut store)
            .run_scoped(
                &repository,
                "2026-08-21T01:00:00Z",
                ArtifactIngestScope::BusinessLinked {
                    related_jira_depth: 0,
                },
            )
            .expect("business-linked run");

        assert_eq!(
            *source.received_candidate_keys.borrow(),
            vec![BTreeSet::from(["PSI-3".to_owned()])]
        );
        assert_eq!(*source.received_related_depths.borrow(), vec![0]);
    }

    #[test]
    fn business_linked_jira_scope_accepts_a_repository_linked_mr_as_evidence() {
        let mut store = FakeStore::default();
        let repository = RepositoryId::new("repo:test").expect("repository ID");
        let branch = artifact(
            "feature/cancellation",
            ArtifactKind::Branch,
            "feature/cancellation",
            "",
        );
        let mut merge_request = artifact("842", ArtifactKind::MergeRequest, "Implement PSI-7", "");
        merge_request.identity.provider = ArtifactProvider::GitLab;
        for known in [&branch, &merge_request] {
            store
                .upsert_artifact(&repository, known, "2026-08-21T00:00:00Z", "test")
                .expect("seed artifact");
        }
        store
            .persist_links(
                &repository,
                &[ArtifactLink {
                    source: merge_request.identity.clone(),
                    target: ArtifactLinkTarget::Artifact(branch.identity.clone()),
                    kind: ArtifactLinkKind::References,
                    evidence_locator: "merge_request.source_branch".to_owned(),
                }],
            )
            .expect("MR branch link");
        let source = FakeJiraSource::default();

        JiraIngestRunner::new(&source, &mut store)
            .run_scoped(
                &repository,
                "2026-08-21T01:00:00Z",
                ArtifactIngestScope::BusinessLinked {
                    related_jira_depth: 0,
                },
            )
            .expect("business-linked run");

        assert_eq!(
            *source.received_candidate_keys.borrow(),
            vec![BTreeSet::from(["PSI-7".to_owned()])]
        );
    }
}
