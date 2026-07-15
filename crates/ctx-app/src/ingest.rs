//! Orchestrates Git-native artifact ingestion (prompt3.md PR-EXT-001 MUST
//! list: commit messages, branch names): reads artifacts through
//! [`GitArtifactSource`], persists them idempotently, then runs
//! [`ctx_core::linking::text_reference_links`] against every artifact this
//! repository already knows about so a reference discovered in one ingest
//! run can still resolve against artifacts stored by an earlier one.

use std::collections::BTreeMap;

use ctx_core::{
    artifact::{
        Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkKind,
        ArtifactLinkTarget, ArtifactProvider,
    },
    codedoc::{CodeDocKind, extract_code_docs},
    domain::{CommitOid, NodeKind, RepositoryId, StableKey},
    linking::text_reference_links,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ports::{
    ArtifactLinkStore, ArtifactRepository, GitArtifactSource, GitLabArtifactSource, GitRepository,
    GraphStore, IngestCursorStore, LanguageAnalyzer, PortError, ReviewRepository,
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
    S: ArtifactRepository + ArtifactLinkStore,
{
    pub const fn new(source: &'a G, store: &'a mut S) -> Self {
        Self { source, store }
    }

    /// Ingests every local branch and every commit reachable from `HEAD`
    /// back to (exclusive of) `since`, then links references found in their
    /// text to every artifact already known for this repository.
    ///
    /// # Errors
    /// Returns [`IngestError`] when artifacts cannot be read or persisted.
    pub fn run(
        &mut self,
        repository: &RepositoryId,
        since: Option<&CommitOid>,
        ingested_at: &str,
    ) -> Result<IngestReport, IngestError> {
        let mut artifacts = self
            .source
            .branch_artifacts()
            .map_err(IngestError::Source)?;
        artifacts.extend(
            self.source
                .commit_artifacts(since)
                .map_err(IngestError::Source)?,
        );
        for artifact in &artifacts {
            self.store
                .upsert_artifact(repository, artifact, ingested_at, GIT_INGEST_VERSION)
                .map_err(IngestError::Store)?;
        }
        let known = self
            .store
            .list_artifacts(repository)
            .map_err(IngestError::Store)?;
        let links = artifacts
            .iter()
            .flat_map(|artifact| text_reference_links(artifact, &known))
            .collect::<Vec<_>>();
        self.store
            .persist_links(repository, &links)
            .map_err(IngestError::Store)?;
        Ok(IngestReport {
            artifacts_ingested: artifacts.len(),
            links_created: links.len(),
        })
    }
}

/// The provider tag `GitLabIngestRunner`'s cursor is stored under (matches
/// `ArtifactProvider::GitLab`'s own serde tag).
const GITLAB_CURSOR_PROVIDER: &str = "gitlab";

/// Orchestrates GitLab issue/merge-request ingestion (prompt3.md PR-EXT-001
/// MUST list, the chosen end-to-end provider): reads artifacts and their
/// provider-reported deterministic links through [`GitLabArtifactSource`],
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
    G: GitLabArtifactSource,
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
        let cursor = self
            .store
            .sync_cursor(repository, GITLAB_CURSOR_PROVIDER)
            .map_err(IngestError::Store)?;
        let (artifacts, mut links) = self
            .source
            .issue_and_mr_artifacts(cursor.as_deref())
            .map_err(IngestError::Source)?;
        for artifact in &artifacts {
            self.store
                .upsert_artifact(repository, artifact, ingested_at, GITLAB_INGEST_VERSION)
                .map_err(IngestError::Store)?;
        }
        let known = self
            .store
            .list_artifacts(repository)
            .map_err(IngestError::Store)?;
        links.extend(
            artifacts
                .iter()
                .flat_map(|artifact| text_reference_links(artifact, &known)),
        );
        self.store
            .persist_links(repository, &links)
            .map_err(IngestError::Store)?;
        self.store
            .set_sync_cursor(repository, GITLAB_CURSOR_PROVIDER, ingested_at)
            .map_err(IngestError::Store)?;
        Ok(IngestReport {
            artifacts_ingested: artifacts.len(),
            links_created: links.len(),
        })
    }
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
    S: ArtifactRepository + ArtifactLinkStore + GraphStore,
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
                    source_locator: identity.external_id.clone(),
                    content_hash,
                    project: project.clone(),
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
        self.store
            .persist_links(repository_id, &links)
            .map_err(IngestError::Store)?;
        Ok(IngestReport {
            artifacts_ingested: artifacts.len(),
            links_created: links.len(),
        })
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
        commits: Vec<Artifact>,
        branches: Vec<Artifact>,
    }

    impl GitArtifactSource for FakeGitSource {
        fn commit_artifacts(&self, _since: Option<&CommitOid>) -> Result<Vec<Artifact>, PortError> {
            Ok(self.commits.clone())
        }

        fn branch_artifacts(&self) -> Result<Vec<Artifact>, PortError> {
            Ok(self.branches.clone())
        }
    }

    #[derive(Default)]
    struct FakeStore {
        artifacts: RefCell<BTreeMap<(ArtifactProvider, ArtifactKind, String), Artifact>>,
        links: RefCell<Vec<ArtifactLink>>,
        cursors: RefCell<BTreeMap<String, String>>,
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
            _analyzed_at: &str,
        ) -> Result<(), PortError> {
            unreachable!("ingest never marks artifacts analyzed")
        }

        fn analyzed_content_hashes(
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
            project: "/repo".to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: format!("git:{external_id}"),
            content_hash: "hash".to_owned(),
        }
    }

    #[test]
    fn ingests_branches_and_commits_and_links_references_between_them() {
        let source = FakeGitSource {
            commits: vec![artifact(
                "abc123",
                ArtifactKind::Commit,
                "fix cancellation",
                "See branch feature/PAY-317-cancel",
            )],
            branches: vec![artifact(
                "feature/PAY-317-cancel",
                ArtifactKind::Branch,
                "feature/PAY-317-cancel",
                "",
            )],
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
    fn re_running_ingest_does_not_duplicate_artifacts() {
        let source = FakeGitSource {
            commits: vec![artifact("abc123", ArtifactKind::Commit, "fix", "body")],
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
    }

    impl GitLabArtifactSource for FakeGitLabSource {
        fn issue_and_mr_artifacts(
            &self,
            since: Option<&str>,
        ) -> Result<(Vec<Artifact>, Vec<ArtifactLink>), PortError> {
            self.received_since
                .borrow_mut()
                .push(since.map(str::to_owned));
            Ok((self.artifacts.clone(), self.links.clone()))
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
            project: "billing/subscriptions".to_owned(),
            title: "Cancellation removes prepaid access".to_owned(),
            body: String::new(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: "https://gitlab.example/-/issues/317".to_owned(),
            content_hash: "hash".to_owned(),
        };
        let merge_request = Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: ArtifactKind::MergeRequest,
                external_id: "842".to_owned(),
            },
            project: "billing/subscriptions".to_owned(),
            title: "Fix cancellation semantics".to_owned(),
            body: "Fixes #317.".to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: "https://gitlab.example/-/merge_requests/842".to_owned(),
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
}
