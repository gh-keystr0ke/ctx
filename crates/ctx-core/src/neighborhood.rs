//! Bounded artifact-neighborhood assembly (prompt3.md PR-AI-001,
//! PR-AGENT-003): the unit of work handed to a semantic agent is one
//! artifact's own connected context — its direct deterministic links, the
//! code it touched, and the product knowledge already mapped nearby — never
//! the whole repository or the whole artifact backlog. Everything here is a
//! pure, one-hop read over already-linked data; no artifact/graph lookup
//! reaches further than what [`crate::linking`] and [`crate::artifact`]
//! already established deterministically.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    artifact::{Artifact, ArtifactIdentity, ArtifactLink, ArtifactLinkKind, ArtifactLinkTarget},
    context_pack::{estimate_tokens, truncate_to_tokens},
    domain::{ClaimStatus, NodeKind, RelationKind},
    graph::GraphSnapshot,
    indexing::PlannedNodeAttributes,
};

/// Caps how many linked artifacts/symbols/tests/related-knowledge items one
/// neighborhood may include, independent of the token budget — bounding the
/// *shape* of the input, not just its rendered size (PR-AGENT-003).
const MAX_LINKED_ARTIFACTS: usize = 20;
const MAX_CHANGED_SYMBOLS: usize = 30;
const MAX_NEARBY_TESTS: usize = 15;
const MAX_RELATED_KNOWLEDGE: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkedArtifact {
    pub kind: ArtifactLinkKind,
    pub artifact: Artifact,
}

/// An already-known Feature/Requirement/Invariant/Decision found near a
/// changed symbol, so an agent can answer "is this new, or additional
/// evidence for something already here?" (PR-INCR-002) instead of proposing
/// a duplicate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RelatedIntent {
    pub kind: NodeKind,
    pub identifier: String,
    pub statement_excerpt: String,
}

/// The bounded, one-hop context around a single artifact (PR-AI-001):
/// everything an agent needs to judge relevance and propose evidence-backed
/// candidates, and nothing beyond this one artifact's own connections.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtifactNeighborhood {
    pub subject: Artifact,
    pub linked_artifacts: Vec<LinkedArtifact>,
    /// Canonical paths of symbols this artifact's changeset touched
    /// (`ChangedSymbol`) or that a code comment/docstring discusses
    /// (`Discusses`).
    pub changed_symbols: Vec<String>,
    /// Canonical paths of tests structurally connected (one `Calls` hop) to
    /// a changed symbol, or already covering a related intent.
    pub nearby_tests: Vec<String>,
    pub related_knowledge: Vec<RelatedIntent>,
}

/// Assembles the bounded neighborhood for `subject` from its own
/// deterministic links, read in either direction. A link is stored by
/// whichever side structurally authored it — a comment records
/// `Comment → Ticket`, a merge request records `MergeRequest → Commit` — so
/// treating only outgoing links as "the subject's own" would silently hide
/// a ticket's own comments and a commit's own containing MR. Linked
/// artifacts are resolved against `known_artifacts` (a link to an artifact
/// not present there is simply omitted — never fabricated), and
/// changed/discussed symbols resolved against `graph`, together with their
/// nearby tests and already-mapped product knowledge. Still exactly one
/// hop: only links naming `subject` itself are considered, whichever
/// direction they were recorded in.
#[must_use]
pub fn build_neighborhood(
    subject: &Artifact,
    links: &[ArtifactLink],
    known_artifacts: &[Artifact],
    graph: &GraphSnapshot,
) -> ArtifactNeighborhood {
    let mut changed_symbol_keys = BTreeSet::new();
    let linked_artifacts = linked_artifacts(subject, links, known_artifacts, &mut changed_symbol_keys);

    let changed_symbols = changed_symbol_keys
        .iter()
        .filter_map(|key| graph.nodes.get(key))
        .map(|node| node.identifier().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_CHANGED_SYMBOLS)
        .collect::<Vec<_>>();

    let related_knowledge = related_knowledge(graph, &changed_symbol_keys);
    let mut nearby_tests = nearby_tests(graph, &changed_symbol_keys, &related_knowledge)
        .into_iter()
        .collect::<Vec<_>>();
    nearby_tests.truncate(MAX_NEARBY_TESTS);

    ArtifactNeighborhood {
        subject: subject.clone(),
        linked_artifacts,
        changed_symbols,
        nearby_tests,
        related_knowledge,
    }
}

fn find_artifact<'a>(
    known_artifacts: &'a [Artifact],
    identity: &ArtifactIdentity,
) -> Option<&'a Artifact> {
    known_artifacts
        .iter()
        .find(|artifact| &artifact.identity == identity)
}

/// Which links survive `MAX_LINKED_ARTIFACTS` must be decided by evidential
/// strength, not by storage order or direction: a ticket with many
/// incidental `References` mentions must never crowd out the comments
/// actually posted on it. A `match`, not `#[derive(Ord)]` on
/// `ArtifactLinkKind`, so this ranking stays independent of declaration
/// order.
const fn link_kind_rank(kind: ArtifactLinkKind) -> u8 {
    match kind {
        ArtifactLinkKind::CommentsOn => 0,
        ArtifactLinkKind::ContainsCommit => 1,
        ArtifactLinkKind::RelatedIssue => 2,
        ArtifactLinkKind::Discusses => 3,
        ArtifactLinkKind::ChangedSymbol => 4,
        ArtifactLinkKind::References => 5,
    }
}

/// Resolves `subject`'s linked artifacts from `links`, regardless of which
/// side the link's `source`/`target` names `subject`, collecting any
/// `CodeSymbol` targets from subject-authored links into
/// `changed_symbol_keys` along the way (a code symbol is never a link
/// source, so that case only ever arises on the outgoing side).
fn linked_artifacts(
    subject: &Artifact,
    links: &[ArtifactLink],
    known_artifacts: &[Artifact],
    changed_symbol_keys: &mut BTreeSet<crate::domain::StableKey>,
) -> Vec<LinkedArtifact> {
    let mut seen = HashSet::new();
    let mut linked = Vec::new();
    for link in links {
        let counterpart = if link.source == subject.identity {
            match &link.target {
                ArtifactLinkTarget::Artifact(identity) => Some(identity),
                ArtifactLinkTarget::CodeSymbol(stable_key) => {
                    changed_symbol_keys.insert(stable_key.clone());
                    None
                }
            }
        } else if let ArtifactLinkTarget::Artifact(identity) = &link.target
            && *identity == subject.identity
        {
            Some(&link.source)
        } else {
            None
        };
        let Some(identity) = counterpart else {
            continue;
        };
        if *identity == subject.identity {
            continue;
        }
        let Some(artifact) = find_artifact(known_artifacts, identity) else {
            continue;
        };
        if seen.insert((identity.clone(), link.kind)) {
            linked.push(LinkedArtifact {
                kind: link.kind,
                artifact: artifact.clone(),
            });
        }
    }
    linked.sort_by_key(|linked| {
        (
            link_kind_rank(linked.kind),
            linked.artifact.identity.provider,
            linked.artifact.identity.kind,
            linked.artifact.identity.external_id.clone(),
        )
    });
    linked.truncate(MAX_LINKED_ARTIFACTS);
    linked
}

/// Bounds how many historical artifacts one [`artifact_history`] call may
/// return, for the same reason [`MAX_LINKED_ARTIFACTS`] bounds one
/// neighborhood: a hot, long-lived symbol touched by hundreds of commits
/// over its life must never return unbounded history.
const MAX_ARTIFACT_HISTORY: usize = 20;

/// Every artifact whose changeset structurally touched `symbol`
/// (`ChangedSymbol`) or that discusses it (`Discusses`) — the reverse
/// direction of [`build_neighborhood`]'s artifact-to-symbol links, used to
/// answer "what commits/merge requests/comments touched this code over its
/// history" (`ctx explain`). Newest first when an artifact reports a
/// creation timestamp, with artifacts reporting none sorted last; ties
/// break deterministically by identity. Bounded by [`MAX_ARTIFACT_HISTORY`].
#[must_use]
pub fn artifact_history(
    symbol: &crate::domain::StableKey,
    links: &[ArtifactLink],
    known_artifacts: &[Artifact],
) -> Vec<LinkedArtifact> {
    let mut seen = HashSet::new();
    let mut history = links
        .iter()
        .filter(|link| link.target == ArtifactLinkTarget::CodeSymbol(symbol.clone()))
        .filter_map(|link| find_artifact(known_artifacts, &link.source).map(|artifact| (link.kind, artifact)))
        .filter(|(kind, artifact)| seen.insert((artifact.identity.clone(), *kind)))
        .map(|(kind, artifact)| LinkedArtifact {
            kind,
            artifact: artifact.clone(),
        })
        .collect::<Vec<_>>();
    history.sort_by(|left, right| {
        history_created_at(right)
            .cmp(&history_created_at(left))
            .then_with(|| left.artifact.identity.external_id.cmp(&right.artifact.identity.external_id))
    });
    history.truncate(MAX_ARTIFACT_HISTORY);
    history
}

fn history_created_at(entry: &LinkedArtifact) -> Option<&str> {
    entry
        .artifact
        .external_created_at
        .as_ref()
        .map(crate::domain::Timestamp::as_str)
}

fn related_knowledge(
    graph: &GraphSnapshot,
    changed_symbol_keys: &BTreeSet<crate::domain::StableKey>,
) -> Vec<RelatedIntent> {
    let mut identifiers = BTreeSet::new();
    let mut related = Vec::new();
    for edge in &graph.edges {
        if edge.status != ClaimStatus::Active
            || !matches!(
                edge.kind,
                RelationKind::Implements | RelationKind::Enforces | RelationKind::Satisfies
            )
        {
            continue;
        }
        if !changed_symbol_keys.contains(&edge.source) {
            continue;
        }
        let Some(node) = graph.nodes.get(&edge.target) else {
            continue;
        };
        if !identifiers.insert(node.stable_key.clone()) {
            continue;
        }
        let statement = match &node.attributes {
            PlannedNodeAttributes::Business { body, .. } => body.as_str(),
            _ => "",
        };
        related.push(RelatedIntent {
            kind: node.kind,
            identifier: node.identifier().to_owned(),
            statement_excerpt: truncate_to_tokens(statement, 40),
        });
    }
    // Sort before capping: which items survive the cap must depend on a
    // meaningful, deterministic order (identifier), not on `graph.edges`'
    // incidental storage order.
    related.sort_by(|left, right| left.identifier.cmp(&right.identifier));
    related.truncate(MAX_RELATED_KNOWLEDGE);
    related
}

fn nearby_tests(
    graph: &GraphSnapshot,
    changed_symbol_keys: &BTreeSet<crate::domain::StableKey>,
    related_knowledge: &[RelatedIntent],
) -> BTreeSet<String> {
    let related_identifiers = related_knowledge
        .iter()
        .map(|intent| intent.identifier.as_str())
        .collect::<BTreeSet<_>>();
    let mut tests = BTreeSet::new();
    for edge in &graph.edges {
        if edge.status != ClaimStatus::Active {
            continue;
        }
        // A test one `Calls` hop from a changed symbol.
        if edge.kind == RelationKind::Calls {
            let candidate = if changed_symbol_keys.contains(&edge.source) {
                Some(&edge.target)
            } else if changed_symbol_keys.contains(&edge.target) {
                Some(&edge.source)
            } else {
                None
            };
            if let Some(node) = candidate.and_then(|key| graph.nodes.get(key))
                && node.is_test()
            {
                tests.insert(node.identifier().to_owned());
            }
        }
        // A test already `CoveredBy`-linked to a related intent.
        if edge.kind == RelationKind::CoveredBy
            && let Some(intent_node) = graph.nodes.get(&edge.source)
            && related_identifiers.contains(intent_node.identifier())
            && let Some(test_node) = graph.nodes.get(&edge.target)
        {
            tests.insert(test_node.identifier().to_owned());
        }
    }
    tests
}

/// A neighborhood serialized into a bounded plain-text block suitable as
/// agent input, following prompt3.md section 6's grouped shape (Issue / MR /
/// Branch / Commit / Changed symbols / Tests), truncated to fit
/// `token_budget` the same way [`crate::context_pack::compile_context_pack`]
/// truncates a Context Pack rather than inventing a second budgeting
/// mechanism.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderedNeighborhood {
    pub text: String,
    pub token_budget: usize,
    pub estimated_tokens: usize,
    pub truncated: bool,
}

/// # Errors
/// Returns an error message when `token_budget` is zero.
pub fn render_neighborhood(
    neighborhood: &ArtifactNeighborhood,
    token_budget: usize,
) -> Result<RenderedNeighborhood, &'static str> {
    if token_budget == 0 {
        return Err("token budget must be greater than zero");
    }
    let mut sections = Vec::new();
    sections.push(format!(
        "{}:\n{} — {}",
        artifact_kind_label(neighborhood.subject.identity.kind),
        neighborhood.subject.identity.external_id,
        neighborhood.subject.title
    ));
    if !neighborhood.subject.body.is_empty() {
        sections.push(neighborhood.subject.body.clone());
    }
    for linked in &neighborhood.linked_artifacts {
        sections.push(format!(
            "{} ({:?}):\n{} — {}",
            artifact_kind_label(linked.artifact.identity.kind),
            linked.kind,
            linked.artifact.identity.external_id,
            linked.artifact.title
        ));
    }
    if !neighborhood.changed_symbols.is_empty() {
        sections.push(format!(
            "Changed symbols:\n{}",
            neighborhood.changed_symbols.join("\n")
        ));
    }
    if !neighborhood.nearby_tests.is_empty() {
        sections.push(format!("Tests:\n{}", neighborhood.nearby_tests.join("\n")));
    }
    if !neighborhood.related_knowledge.is_empty() {
        let lines = neighborhood
            .related_knowledge
            .iter()
            .map(|intent| {
                format!(
                    "{} ({:?}): {}",
                    intent.identifier, intent.kind, intent.statement_excerpt
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!(
            "Already-known related product knowledge (not new if this restates one of these):\n{lines}"
        ));
    }

    let mut used = 0;
    let mut included = Vec::new();
    let mut truncated = false;
    for section in sections {
        let cost = estimate_tokens(&section);
        let remaining = token_budget.saturating_sub(used);
        if remaining == 0 {
            truncated = true;
            break;
        }
        if cost > remaining {
            // Reserve a small margin for the ellipsis `truncate_to_tokens`
            // may append and the resulting round-up: re-estimating tokens
            // from the clipped text can otherwise land a token or two over
            // `remaining` (matching `context_pack::compile_context_pack`'s
            // own margin for the same reason).
            let clipped = truncate_to_tokens(&section, remaining.saturating_sub(4));
            let clipped_cost = estimate_tokens(&clipped);
            if !clipped.is_empty() && clipped_cost <= remaining {
                used += clipped_cost;
                included.push(clipped);
            }
            truncated = true;
            break;
        }
        used += cost;
        included.push(section);
    }

    Ok(RenderedNeighborhood {
        text: included.join("\n\n"),
        token_budget,
        estimated_tokens: used,
        truncated,
    })
}

const fn artifact_kind_label(kind: crate::artifact::ArtifactKind) -> &'static str {
    use crate::artifact::ArtifactKind;
    match kind {
        ArtifactKind::Commit => "Commit",
        ArtifactKind::Branch => "Branch",
        ArtifactKind::Issue => "Issue",
        ArtifactKind::MergeRequest => "MR",
        ArtifactKind::PullRequest => "PR",
        ArtifactKind::Comment => "Comment",
        ArtifactKind::ReviewComment => "Review comment",
        ArtifactKind::CodeComment => "Code comment",
        ArtifactKind::Docstring => "Docstring",
        ArtifactKind::Documentation => "Documentation",
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        artifact::ArtifactProvider,
        domain::{ClaimClass, Confidence, SourceKind, StableKey},
        graph::{GraphEdge, GraphNode},
        ir::{SourceRange, SymbolKind},
    };

    use super::*;

    fn artifact_from(
        provider: ArtifactProvider,
        external_id: &str,
        kind: crate::artifact::ArtifactKind,
        title: &str,
        body: &str,
    ) -> Artifact {
        Artifact {
            identity: ArtifactIdentity {
                provider,
                kind,
                external_id: external_id.to_owned(),
            },
            project: crate::domain::Project("billing/subscriptions".to_owned()),
            title: title.to_owned(),
            body: body.to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: crate::domain::Url(external_id.to_owned()),
            content_hash: "hash".to_owned(),
        }
    }

    fn artifact(
        external_id: &str,
        kind: crate::artifact::ArtifactKind,
        title: &str,
        body: &str,
    ) -> Artifact {
        artifact_from(ArtifactProvider::GitLab, external_id, kind, title, body)
    }

    fn symbol_node(key: &str, canonical: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::CodeSymbol,
            name: canonical.rsplit('.').next().unwrap_or(canonical).to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: "billing.py".to_owned(),
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
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            },
        }
    }

    fn test_node(key: &str, canonical: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::CodeSymbol,
            name: canonical.rsplit('.').next().unwrap_or(canonical).to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: "test_billing.py".to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind: SymbolKind::Test,
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
                schema_tables: Vec::new(),
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            },
        }
    }

    fn intent_node(key: &str, kind: NodeKind, id: &str, body: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind,
            name: id.to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Business {
                id: id.to_owned(),
                status: "active".to_owned(),
                visibility: crate::business::Visibility::Private,
                implementation_expected: true,
                body: body.to_owned(),
                feature: None,
                source_uri: "context.yaml".to_owned(),
            },
        }
    }

    fn edge(source: &GraphNode, target: &GraphNode, kind: RelationKind) -> GraphEdge {
        GraphEdge {
            source: source.stable_key.clone(),
            target: target.stable_key.clone(),
            kind,
            claim_class: if kind.is_semantic() {
                ClaimClass::Assertion
            } else {
                ClaimClass::Fact
            },
            source_kind: SourceKind::StaticAnalysis,
            confidence: Confidence::CERTAIN,
            status: ClaimStatus::Active,
            valid_from: "commit".to_owned(),
            valid_to: None,
            producer: "test".to_owned(),
            fingerprint: format!("{kind:?}:{}", source.stable_key),
            stale_reason: None,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn assembles_linked_artifacts_changed_symbols_tests_and_related_knowledge() {
        let issue = artifact(
            "317",
            crate::artifact::ArtifactKind::Issue,
            "Cancellation removes prepaid access",
            "A cancelled prepaid subscription must remain usable until paid_until.",
        );
        let mr = artifact(
            "842",
            crate::artifact::ArtifactKind::MergeRequest,
            "Fix cancellation",
            "Fixes #317.",
        );
        let cancel = symbol_node("cancel", "SubscriptionService.cancel");
        let test = test_node("test_cancel", "test_cancel_preserves_access");
        let requirement = intent_node(
            "req",
            NodeKind::Requirement,
            "REQ-SUB-014",
            "Cancellation preserves paid access until paid_until.",
        );
        let links = vec![
            ArtifactLink {
                source: mr.identity.clone(),
                target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
                kind: ArtifactLinkKind::References,
                evidence_locator: "text:#317".to_owned(),
            },
            ArtifactLink {
                source: mr.identity.clone(),
                target: ArtifactLinkTarget::CodeSymbol(cancel.stable_key.clone()),
                kind: ArtifactLinkKind::ChangedSymbol,
                evidence_locator: "changed_file:billing.py".to_owned(),
            },
        ];
        let graph = GraphSnapshot {
            nodes: [cancel.clone(), test.clone(), requirement.clone()]
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges: vec![
                edge(&test, &cancel, RelationKind::Calls),
                edge(&cancel, &requirement, RelationKind::Implements),
            ],
        };

        let neighborhood = build_neighborhood(&mr, &links, &[issue.clone(), mr.clone()], &graph);

        assert_eq!(neighborhood.linked_artifacts.len(), 1);
        assert_eq!(
            neighborhood.linked_artifacts[0].artifact.identity,
            issue.identity
        );
        assert_eq!(
            neighborhood.linked_artifacts[0].kind,
            ArtifactLinkKind::References
        );
        assert_eq!(
            neighborhood.changed_symbols,
            vec!["SubscriptionService.cancel".to_owned()]
        );
        assert_eq!(
            neighborhood.nearby_tests,
            vec!["test_cancel_preserves_access".to_owned()]
        );
        assert_eq!(neighborhood.related_knowledge.len(), 1);
        assert_eq!(neighborhood.related_knowledge[0].identifier, "REQ-SUB-014");
    }

    #[test]
    fn a_link_to_an_unknown_artifact_is_omitted_not_fabricated() {
        let mr = artifact(
            "842",
            crate::artifact::ArtifactKind::MergeRequest,
            "Fix",
            "Fixes #999.",
        );
        let links = vec![ArtifactLink {
            source: mr.identity.clone(),
            target: ArtifactLinkTarget::Artifact(ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind: crate::artifact::ArtifactKind::Issue,
                external_id: "999".to_owned(),
            }),
            kind: ArtifactLinkKind::References,
            evidence_locator: "text:#999".to_owned(),
        }];

        let neighborhood = build_neighborhood(
            &mr,
            &links,
            std::slice::from_ref(&mr),
            &GraphSnapshot::default(),
        );

        assert!(neighborhood.linked_artifacts.is_empty());
    }

    #[test]
    fn a_tickets_neighborhood_includes_the_comment_posted_on_it() {
        let issue = artifact(
            "317",
            crate::artifact::ArtifactKind::Issue,
            "Cancellation removes prepaid access",
            "body",
        );
        let comment = artifact_from(
            ArtifactProvider::GitLab,
            "317-comment-1",
            crate::artifact::ArtifactKind::Comment,
            "Do not revoke immediately",
            "Do not revoke an already paid entitlement immediately.",
        );
        let links = vec![ArtifactLink {
            source: comment.identity.clone(),
            target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
            kind: ArtifactLinkKind::CommentsOn,
            evidence_locator: "gitlab notes API: 317".to_owned(),
        }];

        let neighborhood = build_neighborhood(
            &issue,
            &links,
            &[issue.clone(), comment.clone()],
            &GraphSnapshot::default(),
        );

        assert_eq!(neighborhood.linked_artifacts.len(), 1);
        assert_eq!(
            neighborhood.linked_artifacts[0].kind,
            ArtifactLinkKind::CommentsOn
        );
        assert_eq!(
            neighborhood.linked_artifacts[0].artifact.identity,
            comment.identity
        );
    }

    #[test]
    fn a_commits_neighborhood_includes_the_merge_request_that_contains_it() {
        let mr = artifact(
            "842",
            crate::artifact::ArtifactKind::MergeRequest,
            "Fix cancellation",
            "body",
        );
        let commit = artifact_from(
            ArtifactProvider::Git,
            "abc123",
            crate::artifact::ArtifactKind::Commit,
            "fix cancellation",
            "fix cancellation",
        );
        let links = vec![ArtifactLink {
            source: mr.identity.clone(),
            target: ArtifactLinkTarget::Artifact(commit.identity.clone()),
            kind: ArtifactLinkKind::ContainsCommit,
            evidence_locator: "merge_request:842".to_owned(),
        }];

        let neighborhood = build_neighborhood(
            &commit,
            &links,
            &[mr.clone(), commit.clone()],
            &GraphSnapshot::default(),
        );

        assert_eq!(neighborhood.linked_artifacts.len(), 1);
        assert_eq!(
            neighborhood.linked_artifacts[0].kind,
            ArtifactLinkKind::ContainsCommit
        );
        assert_eq!(neighborhood.linked_artifacts[0].artifact.identity, mr.identity);
    }

    #[test]
    fn a_counterpart_linked_in_both_directions_is_listed_once() {
        let issue = artifact("317", crate::artifact::ArtifactKind::Issue, "t", "b");
        let comment = artifact_from(
            ArtifactProvider::GitLab,
            "317-comment-1",
            crate::artifact::ArtifactKind::Comment,
            "c",
            "c",
        );
        let links = vec![
            ArtifactLink {
                source: comment.identity.clone(),
                target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
                kind: ArtifactLinkKind::CommentsOn,
                evidence_locator: "a".to_owned(),
            },
            ArtifactLink {
                source: issue.identity.clone(),
                target: ArtifactLinkTarget::Artifact(comment.identity.clone()),
                kind: ArtifactLinkKind::CommentsOn,
                evidence_locator: "b".to_owned(),
            },
        ];

        let neighborhood = build_neighborhood(
            &issue,
            &links,
            &[issue.clone(), comment.clone()],
            &GraphSnapshot::default(),
        );

        assert_eq!(neighborhood.linked_artifacts.len(), 1);
    }

    #[test]
    fn the_linked_artifact_cap_keeps_provider_reported_links_over_text_mentions() {
        let issue = artifact("317", crate::artifact::ArtifactKind::Issue, "t", "b");
        let comment = artifact_from(
            ArtifactProvider::GitLab,
            "317-comment-1",
            crate::artifact::ArtifactKind::Comment,
            "c",
            "c",
        );
        let mut known = vec![issue.clone(), comment.clone()];
        let mut links = vec![ArtifactLink {
            source: comment.identity.clone(),
            target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
            kind: ArtifactLinkKind::CommentsOn,
            evidence_locator: "gitlab notes API: 317".to_owned(),
        }];
        for n in 0..25 {
            let commit = artifact_from(
                ArtifactProvider::Git,
                &format!("commit-{n:02}"),
                crate::artifact::ArtifactKind::Commit,
                "PAY-317 fix",
                "PAY-317 fix",
            );
            links.push(ArtifactLink {
                source: commit.identity.clone(),
                target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
                kind: ArtifactLinkKind::References,
                evidence_locator: "text:PAY-317".to_owned(),
            });
            known.push(commit);
        }

        let neighborhood = build_neighborhood(&issue, &links, &known, &GraphSnapshot::default());

        assert_eq!(neighborhood.linked_artifacts.len(), MAX_LINKED_ARTIFACTS);
        assert_eq!(
            neighborhood.linked_artifacts[0].kind,
            ArtifactLinkKind::CommentsOn
        );
    }

    #[test]
    fn artifact_history_lists_artifacts_that_touched_a_symbol_newest_first() {
        let cancel = symbol_node("cancel", "SubscriptionService.cancel");
        let mut older = artifact_from(
            ArtifactProvider::Git,
            "older",
            crate::artifact::ArtifactKind::Commit,
            "older fix",
            "older fix",
        );
        older.external_created_at = Some(crate::domain::Timestamp("2026-01-01T00:00:00Z".to_owned()));
        let mut newer = artifact_from(
            ArtifactProvider::Git,
            "newer",
            crate::artifact::ArtifactKind::Commit,
            "newer fix",
            "newer fix",
        );
        newer.external_created_at = Some(crate::domain::Timestamp("2026-06-01T00:00:00Z".to_owned()));
        let links = vec![
            ArtifactLink {
                source: older.identity.clone(),
                target: ArtifactLinkTarget::CodeSymbol(cancel.stable_key.clone()),
                kind: ArtifactLinkKind::ChangedSymbol,
                evidence_locator: "changed_file:billing.py".to_owned(),
            },
            ArtifactLink {
                source: newer.identity.clone(),
                target: ArtifactLinkTarget::CodeSymbol(cancel.stable_key.clone()),
                kind: ArtifactLinkKind::ChangedSymbol,
                evidence_locator: "changed_file:billing.py".to_owned(),
            },
        ];

        let history = artifact_history(&cancel.stable_key, &links, &[older.clone(), newer.clone()]);

        assert_eq!(
            history.iter().map(|entry| &entry.artifact.identity).collect::<Vec<_>>(),
            vec![&newer.identity, &older.identity]
        );
    }

    #[test]
    fn artifact_history_ignores_links_to_other_symbols() {
        let cancel = symbol_node("cancel", "SubscriptionService.cancel");
        let refund = symbol_node("refund", "SubscriptionService.refund");
        let commit = artifact_from(
            ArtifactProvider::Git,
            "abc123",
            crate::artifact::ArtifactKind::Commit,
            "fix refund",
            "fix refund",
        );
        let links = vec![ArtifactLink {
            source: commit.identity.clone(),
            target: ArtifactLinkTarget::CodeSymbol(refund.stable_key.clone()),
            kind: ArtifactLinkKind::ChangedSymbol,
            evidence_locator: "changed_file:billing.py".to_owned(),
        }];

        let history = artifact_history(&cancel.stable_key, &links, &[commit]);

        assert!(history.is_empty());
    }

    #[test]
    fn render_truncates_to_the_token_budget_and_reports_it() {
        let mr = artifact(
            "842",
            crate::artifact::ArtifactKind::MergeRequest,
            "Fix cancellation",
            &"word ".repeat(500),
        );
        let neighborhood = build_neighborhood(&mr, &[], &[], &GraphSnapshot::default());

        let rendered = render_neighborhood(&neighborhood, 20).expect("rendered neighborhood");

        assert!(rendered.truncated);
        assert!(rendered.estimated_tokens <= 20);
        assert!(!rendered.text.is_empty());
    }

    #[test]
    fn a_changed_symbol_not_yet_in_the_graph_is_silently_omitted() {
        let mr = artifact(
            "842",
            crate::artifact::ArtifactKind::MergeRequest,
            "Fix cancellation",
            "Fixes #317.",
        );
        let links = vec![ArtifactLink {
            source: mr.identity.clone(),
            target: ArtifactLinkTarget::CodeSymbol(
                StableKey::new("not-indexed-yet").expect("stable key"),
            ),
            kind: ArtifactLinkKind::ChangedSymbol,
            evidence_locator: "changed_file:billing.py".to_owned(),
        }];

        let neighborhood = build_neighborhood(&mr, &links, &[], &GraphSnapshot::default());

        assert!(neighborhood.changed_symbols.is_empty());
        assert!(neighborhood.related_knowledge.is_empty());
        assert!(neighborhood.nearby_tests.is_empty());
    }

    #[test]
    fn related_knowledge_caps_deterministically_by_identifier_not_edge_order() {
        let cancel = symbol_node("cancel", "SubscriptionService.cancel");
        let mut nodes = vec![cancel.clone()];
        let mut edges = Vec::new();
        // Insert requirements in descending identifier order so the graph's
        // own edge order is the opposite of the expected, sorted result.
        for n in (0..15).rev() {
            let requirement = intent_node(
                &format!("req-{n}"),
                NodeKind::Requirement,
                &format!("REQ-{n:02}"),
                "statement",
            );
            edges.push(edge(&cancel, &requirement, RelationKind::Implements));
            nodes.push(requirement);
        }
        let graph = GraphSnapshot {
            nodes: nodes
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges,
        };
        let changed_symbol_keys = BTreeSet::from([cancel.stable_key.clone()]);

        let related = related_knowledge(&graph, &changed_symbol_keys);

        assert_eq!(related.len(), MAX_RELATED_KNOWLEDGE);
        let identifiers = related
            .iter()
            .map(|intent| intent.identifier.clone())
            .collect::<Vec<_>>();
        let mut expected = identifiers.clone();
        expected.sort();
        assert_eq!(identifiers, expected, "related knowledge must be sorted");
        assert_eq!(
            identifiers[0], "REQ-00",
            "the cap must keep the lowest identifiers, not whichever edges happened to be inserted last"
        );
    }

    #[test]
    fn zero_token_budget_is_rejected() {
        let mr = artifact(
            "842",
            crate::artifact::ArtifactKind::MergeRequest,
            "Fix",
            "body",
        );
        let neighborhood = build_neighborhood(&mr, &[], &[], &GraphSnapshot::default());

        assert!(render_neighborhood(&neighborhood, 0).is_err());
    }
}
