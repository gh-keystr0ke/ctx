//! Deterministic repository-to-business artifact scoping.
//!
//! The planner is deliberately pure: callers provide the artifacts and their
//! already-grounded links, and receive an auditable keep/prune decision for
//! every artifact. Jira artifacts are never roots because a Jira project can
//! contain work for several repositories.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::artifact::{
    Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkKind, ArtifactLinkTarget,
    ArtifactProvider,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactScopeDisposition {
    Keep,
    Prune,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ArtifactScopeReason {
    BusinessAnchoredGit,
    RepositoryLinkedMergeRequest,
    DirectJiraReference,
    RelatedJiraIssue { depth: usize },
    RetainedParent,
    SnapshotManagedArtifact,
    NoBusinessAnchor,
    NoRepositoryLink,
    JiraNotReferencedByRepository,
    ParentNotRetained,
    UnsupportedInBusinessScope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactScopeDecision {
    pub identity: ArtifactIdentity,
    pub disposition: ArtifactScopeDisposition,
    pub reason: ArtifactScopeReason,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactScopePlan {
    pub decisions: Vec<ArtifactScopeDecision>,
}

impl ArtifactScopePlan {
    #[must_use]
    pub fn kept_identities(&self) -> HashSet<ArtifactIdentity> {
        self.decisions
            .iter()
            .filter(|decision| decision.disposition == ArtifactScopeDisposition::Keep)
            .map(|decision| decision.identity.clone())
            .collect()
    }

    #[must_use]
    pub fn pruned_identities(&self) -> HashSet<ArtifactIdentity> {
        self.decisions
            .iter()
            .filter(|decision| decision.disposition == ArtifactScopeDisposition::Prune)
            .map(|decision| decision.identity.clone())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BusinessScopeOptions {
    /// Number of Jira `RelatedIssue` hops admitted after a repository-backed
    /// Jira reference. Zero means that only directly referenced issues are in
    /// scope.
    pub related_jira_depth: usize,
}

/// Plans the strict business-linked subset of an artifact snapshot in O(A+L).
///
/// Repository Git artifacts are discovery evidence, not sufficient business
/// context. A Git artifact is retained only when it resolves to a Jira issue
/// directly or through a repository-linked merge request. Jira membership by
/// itself is never evidence: traversal into Jira starts only at a literal
/// reference from Git, a selected merge request, or one of that MR's comments.
#[must_use]
pub fn plan_business_scope(
    artifacts: &[Artifact],
    links: &[ArtifactLink],
    options: BusinessScopeOptions,
) -> ArtifactScopePlan {
    let known: HashMap<&ArtifactIdentity, &Artifact> = artifacts
        .iter()
        .map(|artifact| (&artifact.identity, artifact))
        .collect();
    let (comments_by_parent, parent_by_comment) = comment_indexes(&known, links);
    let repository_linked_mrs = repository_linked_merge_requests(&known, links);
    let (direct_jira, jira_referencing_sources) =
        direct_jira_references(&known, links, &repository_linked_mrs, &parent_by_comment);
    let kept_jira_depth =
        expand_related_jira(&known, links, &direct_jira, options.related_jira_depth);
    let anchored_mrs = business_anchored_merge_requests(
        &repository_linked_mrs,
        &jira_referencing_sources,
        &comments_by_parent,
    );
    let anchored_git =
        business_anchored_git(&known, links, &anchored_mrs, &jira_referencing_sources);
    let mut kept_parents = anchored_mrs.clone();
    kept_parents.extend(kept_jira_depth.keys().copied());
    let decision_context = ScopeDecisionContext {
        repository_linked_mrs: &repository_linked_mrs,
        anchored_mrs: &anchored_mrs,
        anchored_git: &anchored_git,
        kept_jira_depth: &kept_jira_depth,
        kept_parents: &kept_parents,
        parent_by_comment: &parent_by_comment,
    };

    ArtifactScopePlan {
        decisions: artifacts
            .iter()
            .map(|artifact| scope_decision(artifact, &decision_context))
            .collect(),
    }
}

fn comment_indexes<'a>(
    known: &HashMap<&'a ArtifactIdentity, &'a Artifact>,
    links: &'a [ArtifactLink],
) -> (
    HashMap<&'a ArtifactIdentity, Vec<&'a ArtifactIdentity>>,
    HashMap<&'a ArtifactIdentity, &'a ArtifactIdentity>,
) {
    let mut comments_by_parent: HashMap<&ArtifactIdentity, Vec<&ArtifactIdentity>> = HashMap::new();
    let mut parent_by_comment: HashMap<&ArtifactIdentity, &ArtifactIdentity> = HashMap::new();
    for link in links {
        let ArtifactLinkTarget::Artifact(target) = &link.target else {
            continue;
        };
        if link.kind == ArtifactLinkKind::CommentsOn
            && known.contains_key(&link.source)
            && known.contains_key(target)
        {
            comments_by_parent
                .entry(target)
                .or_default()
                .push(&link.source);
            parent_by_comment.insert(&link.source, target);
        }
    }
    (comments_by_parent, parent_by_comment)
}

fn repository_linked_merge_requests<'a>(
    known: &HashMap<&'a ArtifactIdentity, &'a Artifact>,
    links: &'a [ArtifactLink],
) -> HashSet<&'a ArtifactIdentity> {
    let mut repository_linked_mrs = HashSet::new();
    for link in links {
        let ArtifactLinkTarget::Artifact(target) = &link.target else {
            continue;
        };
        if !matches!(
            link.kind,
            ArtifactLinkKind::References | ArtifactLinkKind::ContainsCommit
        ) {
            continue;
        }
        match (known.get(&link.source), known.get(target)) {
            (Some(source), Some(target_artifact))
                if is_repository_git(source) && is_merge_request(target_artifact) =>
            {
                repository_linked_mrs.insert(target);
            }
            (Some(source), Some(target_artifact))
                if is_merge_request(source) && is_repository_git(target_artifact) =>
            {
                repository_linked_mrs.insert(&link.source);
            }
            _ => {}
        }
    }
    repository_linked_mrs
}

fn direct_jira_references<'a>(
    known: &HashMap<&'a ArtifactIdentity, &'a Artifact>,
    links: &'a [ArtifactLink],
    repository_linked_mrs: &HashSet<&'a ArtifactIdentity>,
    parent_by_comment: &HashMap<&'a ArtifactIdentity, &'a ArtifactIdentity>,
) -> (HashSet<&'a ArtifactIdentity>, HashSet<&'a ArtifactIdentity>) {
    let mut direct_jira = HashSet::new();
    let mut jira_referencing_sources = HashSet::new();
    for link in links {
        let ArtifactLinkTarget::Artifact(target) = &link.target else {
            continue;
        };
        if link.kind != ArtifactLinkKind::References || !is_jira_issue_identity(target, known) {
            continue;
        }
        let source_is_repository_evidence = known.get(&link.source).is_some_and(|artifact| {
            is_repository_git(artifact) || repository_linked_mrs.contains(&artifact.identity)
        }) || parent_by_comment
            .get(&link.source)
            .is_some_and(|parent| repository_linked_mrs.contains(parent));
        if source_is_repository_evidence {
            direct_jira.insert(target);
            jira_referencing_sources.insert(&link.source);
        }
    }
    (direct_jira, jira_referencing_sources)
}

fn expand_related_jira<'a>(
    known: &HashMap<&'a ArtifactIdentity, &'a Artifact>,
    links: &'a [ArtifactLink],
    direct_jira: &HashSet<&'a ArtifactIdentity>,
    max_depth: usize,
) -> HashMap<&'a ArtifactIdentity, usize> {
    let mut kept_jira_depth: HashMap<&ArtifactIdentity, usize> =
        direct_jira.iter().map(|identity| (*identity, 0)).collect();
    let mut queue: VecDeque<(&ArtifactIdentity, usize)> =
        direct_jira.iter().map(|identity| (*identity, 0)).collect();
    let mut related_jira: HashMap<&ArtifactIdentity, Vec<&ArtifactIdentity>> = HashMap::new();
    for link in links {
        let ArtifactLinkTarget::Artifact(target) = &link.target else {
            continue;
        };
        if link.kind == ArtifactLinkKind::RelatedIssue
            && is_jira_issue_identity(&link.source, known)
            && is_jira_issue_identity(target, known)
        {
            related_jira.entry(&link.source).or_default().push(target);
            related_jira.entry(target).or_default().push(&link.source);
        }
    }
    while let Some((issue, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for related in related_jira.get(issue).into_iter().flatten() {
            if kept_jira_depth.contains_key(*related) {
                continue;
            }
            let related_depth = depth + 1;
            kept_jira_depth.insert(related, related_depth);
            queue.push_back((related, related_depth));
        }
    }
    kept_jira_depth
}

fn business_anchored_merge_requests<'a>(
    repository_linked_mrs: &HashSet<&'a ArtifactIdentity>,
    jira_referencing_sources: &HashSet<&'a ArtifactIdentity>,
    comments_by_parent: &HashMap<&'a ArtifactIdentity, Vec<&'a ArtifactIdentity>>,
) -> HashSet<&'a ArtifactIdentity> {
    repository_linked_mrs
        .iter()
        .copied()
        .filter(|mr| {
            jira_referencing_sources.contains(*mr)
                || comments_by_parent.get(*mr).is_some_and(|comments| {
                    comments
                        .iter()
                        .any(|comment| jira_referencing_sources.contains(comment))
                })
        })
        .collect()
}

fn business_anchored_git<'a>(
    known: &HashMap<&'a ArtifactIdentity, &'a Artifact>,
    links: &'a [ArtifactLink],
    anchored_mrs: &HashSet<&'a ArtifactIdentity>,
    jira_referencing_sources: &HashSet<&'a ArtifactIdentity>,
) -> HashSet<&'a ArtifactIdentity> {
    let mut anchored_git = HashSet::new();
    for source in jira_referencing_sources {
        if known
            .get(*source)
            .is_some_and(|artifact| is_repository_git(artifact))
        {
            anchored_git.insert(*source);
        }
    }
    for link in links {
        let ArtifactLinkTarget::Artifact(target) = &link.target else {
            continue;
        };
        if !matches!(
            link.kind,
            ArtifactLinkKind::References | ArtifactLinkKind::ContainsCommit
        ) {
            continue;
        }
        if anchored_mrs.contains(&link.source)
            && known
                .get(target)
                .is_some_and(|artifact| is_repository_git(artifact))
        {
            anchored_git.insert(target);
        }
        if anchored_mrs.contains(target)
            && known
                .get(&link.source)
                .is_some_and(|artifact| is_repository_git(artifact))
        {
            anchored_git.insert(&link.source);
        }
    }
    anchored_git
}

struct ScopeDecisionContext<'identity, 'plan> {
    repository_linked_mrs: &'plan HashSet<&'identity ArtifactIdentity>,
    anchored_mrs: &'plan HashSet<&'identity ArtifactIdentity>,
    anchored_git: &'plan HashSet<&'identity ArtifactIdentity>,
    kept_jira_depth: &'plan HashMap<&'identity ArtifactIdentity, usize>,
    kept_parents: &'plan HashSet<&'identity ArtifactIdentity>,
    parent_by_comment: &'plan HashMap<&'identity ArtifactIdentity, &'identity ArtifactIdentity>,
}

fn scope_decision(
    artifact: &Artifact,
    context: &ScopeDecisionContext<'_, '_>,
) -> ArtifactScopeDecision {
    let (disposition, reason) = if is_snapshot_managed(artifact) {
        (
            ArtifactScopeDisposition::Keep,
            ArtifactScopeReason::SnapshotManagedArtifact,
        )
    } else if context.anchored_git.contains(&artifact.identity) {
        (
            ArtifactScopeDisposition::Keep,
            ArtifactScopeReason::BusinessAnchoredGit,
        )
    } else if context.anchored_mrs.contains(&artifact.identity) {
        (
            ArtifactScopeDisposition::Keep,
            ArtifactScopeReason::RepositoryLinkedMergeRequest,
        )
    } else if let Some(depth) = context.kept_jira_depth.get(&artifact.identity) {
        if *depth == 0 {
            (
                ArtifactScopeDisposition::Keep,
                ArtifactScopeReason::DirectJiraReference,
            )
        } else {
            (
                ArtifactScopeDisposition::Keep,
                ArtifactScopeReason::RelatedJiraIssue { depth: *depth },
            )
        }
    } else if context
        .parent_by_comment
        .get(&artifact.identity)
        .is_some_and(|parent| context.kept_parents.contains(parent))
    {
        (
            ArtifactScopeDisposition::Keep,
            ArtifactScopeReason::RetainedParent,
        )
    } else {
        (
            ArtifactScopeDisposition::Prune,
            prune_reason(
                artifact,
                context.repository_linked_mrs,
                context.parent_by_comment,
            ),
        )
    };
    ArtifactScopeDecision {
        identity: artifact.identity.clone(),
        disposition,
        reason,
    }
}

fn prune_reason(
    artifact: &Artifact,
    repository_linked_mrs: &HashSet<&ArtifactIdentity>,
    parent_by_comment: &HashMap<&ArtifactIdentity, &ArtifactIdentity>,
) -> ArtifactScopeReason {
    if is_repository_git(artifact) {
        ArtifactScopeReason::NoBusinessAnchor
    } else if is_merge_request(artifact) {
        if repository_linked_mrs.contains(&artifact.identity) {
            ArtifactScopeReason::NoBusinessAnchor
        } else {
            ArtifactScopeReason::NoRepositoryLink
        }
    } else if artifact.identity.provider == ArtifactProvider::Jira
        && artifact.identity.kind == ArtifactKind::Issue
    {
        ArtifactScopeReason::JiraNotReferencedByRepository
    } else if parent_by_comment.contains_key(&artifact.identity) {
        ArtifactScopeReason::ParentNotRetained
    } else {
        ArtifactScopeReason::UnsupportedInBusinessScope
    }
}

fn is_repository_git(artifact: &Artifact) -> bool {
    artifact.identity.provider == ArtifactProvider::Git
        && matches!(
            artifact.identity.kind,
            ArtifactKind::Commit | ArtifactKind::Branch
        )
}

fn is_merge_request(artifact: &Artifact) -> bool {
    matches!(
        (artifact.identity.provider, artifact.identity.kind),
        (ArtifactProvider::GitLab, ArtifactKind::MergeRequest)
            | (ArtifactProvider::GitHub, ArtifactKind::PullRequest)
    )
}

fn is_jira_issue_identity(
    identity: &ArtifactIdentity,
    known: &HashMap<&ArtifactIdentity, &Artifact>,
) -> bool {
    known.get(identity).is_some_and(|artifact| {
        artifact.identity.provider == ArtifactProvider::Jira
            && artifact.identity.kind == ArtifactKind::Issue
    })
}

fn is_snapshot_managed(artifact: &Artifact) -> bool {
    artifact.identity.provider == ArtifactProvider::Code
}

#[cfg(test)]
mod tests {
    use crate::{
        artifact::{ArtifactIdentity, ArtifactLinkTarget},
        domain::{Project, Url},
    };

    use super::*;

    fn artifact(provider: ArtifactProvider, kind: ArtifactKind, id: &str) -> Artifact {
        Artifact {
            identity: ArtifactIdentity {
                provider,
                kind,
                external_id: id.to_owned(),
            },
            project: Project("project".to_owned()),
            title: id.to_owned(),
            body: String::new(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: Url(id.to_owned()),
            content_hash: id.to_owned(),
        }
    }

    fn link(source: &Artifact, target: &Artifact, kind: ArtifactLinkKind) -> ArtifactLink {
        ArtifactLink {
            source: source.identity.clone(),
            target: ArtifactLinkTarget::Artifact(target.identity.clone()),
            kind,
            evidence_locator: "test".to_owned(),
        }
    }

    fn kept(plan: &ArtifactScopePlan, artifact: &Artifact) -> bool {
        plan.kept_identities().contains(&artifact.identity)
    }

    #[test]
    fn keeps_only_a_git_mr_jira_chain_with_business_context() {
        let branch = artifact(ArtifactProvider::Git, ArtifactKind::Branch, "feature/PSI-7");
        let commit = artifact(ArtifactProvider::Git, ArtifactKind::Commit, "abc");
        let mr = artifact(ArtifactProvider::GitLab, ArtifactKind::MergeRequest, "42");
        let mr_comment = artifact(ArtifactProvider::GitLab, ArtifactKind::ReviewComment, "n1");
        let jira = artifact(ArtifactProvider::Jira, ArtifactKind::Issue, "PSI-7");
        let unrelated_jira = artifact(ArtifactProvider::Jira, ArtifactKind::Issue, "PSI-8");
        let artifacts = vec![
            branch.clone(),
            commit.clone(),
            mr.clone(),
            mr_comment.clone(),
            jira.clone(),
            unrelated_jira.clone(),
        ];
        let links = vec![
            link(&mr, &branch, ArtifactLinkKind::References),
            link(&mr, &commit, ArtifactLinkKind::ContainsCommit),
            link(&mr_comment, &mr, ArtifactLinkKind::CommentsOn),
            link(&mr, &jira, ArtifactLinkKind::References),
        ];

        let plan = plan_business_scope(&artifacts, &links, BusinessScopeOptions::default());

        for retained in [&branch, &commit, &mr, &mr_comment, &jira] {
            assert!(kept(&plan, retained), "{} must be retained", retained.title);
        }
        assert!(!kept(&plan, &unrelated_jira));
    }

    #[test]
    fn jira_artifacts_cannot_seed_each_other_without_repository_evidence() {
        let old_jira = artifact(ArtifactProvider::Jira, ArtifactKind::Issue, "PSI-1");
        let related_jira = artifact(ArtifactProvider::Jira, ArtifactKind::Issue, "PSI-2");
        let artifacts = vec![old_jira.clone(), related_jira.clone()];
        let links = vec![link(&old_jira, &related_jira, ArtifactLinkKind::References)];

        let plan = plan_business_scope(
            &artifacts,
            &links,
            BusinessScopeOptions {
                related_jira_depth: 1,
            },
        );

        assert!(plan.kept_identities().is_empty());
    }

    #[test]
    fn repository_linked_mr_without_jira_is_not_business_context() {
        let branch = artifact(
            ArtifactProvider::Git,
            ArtifactKind::Branch,
            "feature/no-ticket",
        );
        let mr = artifact(ArtifactProvider::GitLab, ArtifactKind::MergeRequest, "42");
        let artifacts = vec![branch.clone(), mr.clone()];
        let links = vec![link(&mr, &branch, ArtifactLinkKind::References)];

        let plan = plan_business_scope(&artifacts, &links, BusinessScopeOptions::default());

        assert!(!kept(&plan, &branch));
        assert!(!kept(&plan, &mr));
    }

    #[test]
    fn jira_related_issue_expansion_is_bounded_by_depth() {
        let commit = artifact(ArtifactProvider::Git, ArtifactKind::Commit, "abc");
        let first = artifact(ArtifactProvider::Jira, ArtifactKind::Issue, "PSI-1");
        let second = artifact(ArtifactProvider::Jira, ArtifactKind::Issue, "PSI-2");
        let third = artifact(ArtifactProvider::Jira, ArtifactKind::Issue, "PSI-3");
        let artifacts = vec![commit.clone(), first.clone(), second.clone(), third.clone()];
        let links = vec![
            link(&commit, &first, ArtifactLinkKind::References),
            link(&first, &second, ArtifactLinkKind::RelatedIssue),
            link(&second, &third, ArtifactLinkKind::RelatedIssue),
        ];

        let plan = plan_business_scope(
            &artifacts,
            &links,
            BusinessScopeOptions {
                related_jira_depth: 1,
            },
        );

        assert!(kept(&plan, &commit));
        assert!(kept(&plan, &first));
        assert!(kept(&plan, &second));
        assert!(!kept(&plan, &third));
    }

    #[test]
    fn code_artifacts_are_left_for_snapshot_reconciliation() {
        let comment = artifact(
            ArtifactProvider::Code,
            ArtifactKind::CodeComment,
            "src/lib.rs:1",
        );

        let plan = plan_business_scope(
            std::slice::from_ref(&comment),
            &[],
            BusinessScopeOptions::default(),
        );

        assert!(kept(&plan, &comment));
    }
}
