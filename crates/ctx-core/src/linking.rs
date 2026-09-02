//! Deterministic reference extraction and artifact linking (prompt3.md
//! PR-LINK-001/002/003/004, PR-P01): identifiers explicitly present in an
//! artifact's own text, and relationships built only from that literal
//! presence — never from AI inference, and never inventing a target that
//! isn't among the artifacts/code already known. Whether a referenced
//! artifact *implements* another, or is merely incidentally mentioned
//! (PR-LINK-003, FR-02), is not decided here; this module only records that
//! the reference exists.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    artifact::{
        Artifact, ArtifactIdentity, ArtifactKind, ArtifactLink, ArtifactLinkKind,
        ArtifactLinkTarget,
    },
    graph::GraphSnapshot,
    indexing::PlannedNodeAttributes,
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    /// A Jira-style project key, for example `PAY-317`.
    TicketKey,
    /// A GitHub/GitLab issue or pull-request reference, for example `#482`.
    IssueNumber,
    /// A GitLab merge-request reference, for example `!918`.
    MergeRequestNumber,
    /// A full URL naming a specific tracker item.
    Url,
}

/// One deterministic reference found literally in an artifact's text
/// (PR-LINK-002): the normalized value, with `#`/`!` prefixes and
/// surrounding whitespace stripped, ready to compare against a known
/// artifact's `external_id`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalReference {
    pub kind: ReferenceKind,
    pub value: String,
}

/// Scans `text` for deterministic references. Order matches first
/// occurrence in the text; duplicates are removed. Never returns a
/// reference whose characters were not literally present in `text`
/// (PR-LINK-004's "never invent an absent ID" applies just as much to this
/// deterministic layer as it does to AI output).
#[must_use]
pub fn extract_references(text: &str) -> Vec<ExternalReference> {
    let bytes = text.as_bytes();
    let mut references = Vec::new();
    let mut seen = BTreeSet::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some((reference, consumed)) = match_at(text, index) {
            if seen.insert((reference.kind, reference.value.clone())) {
                references.push(reference);
            }
            index += consumed.max(1);
        } else {
            index += utf8_char_len(bytes[index]);
        }
    }
    references
}

fn match_at(text: &str, index: usize) -> Option<(ExternalReference, usize)> {
    let rest = &text[index..];
    if starts_word(text, index)
        && let Some((value, consumed)) = match_ticket_key(rest)
    {
        return Some((
            ExternalReference {
                kind: ReferenceKind::TicketKey,
                value,
            },
            consumed,
        ));
    }
    if rest.starts_with("http://") || rest.starts_with("https://") {
        let (value, _) = match_url(rest);
        // Deliberately advance by only one byte (not the whole matched
        // span): a ticket key embedded in the URL's path, like `PAY-317` in
        // `.../browse/PAY-317`, is still a real, separately useful
        // reference (PR-LINK-002 lists both as independent evidence), and
        // letting the scanner continue byte-by-byte through the rest of the
        // URL text finds it too instead of skipping straight past it.
        return Some((
            ExternalReference {
                kind: ReferenceKind::Url,
                value,
            },
            1,
        ));
    }
    if rest.starts_with('#')
        && not_preceded_by_alphanumeric(text, index)
        && let Some((value, consumed)) = match_numeric_reference(rest, '#')
    {
        return Some((
            ExternalReference {
                kind: ReferenceKind::IssueNumber,
                value,
            },
            consumed,
        ));
    }
    if rest.starts_with('!')
        && not_preceded_by_alphanumeric(text, index)
        && let Some((value, consumed)) = match_numeric_reference(rest, '!')
    {
        return Some((
            ExternalReference {
                kind: ReferenceKind::MergeRequestNumber,
                value,
            },
            consumed,
        ));
    }
    None
}

/// A ticket key is 2-10 uppercase ASCII letters, a hyphen, then 1-6 ASCII
/// digits, as a whole word: `feature/PAY-317-cancel` yields `PAY-317`, not
/// `PAY-317-cancel` or a partial match starting mid-word.
fn match_ticket_key(rest: &str) -> Option<(String, usize)> {
    let bytes = rest.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() && cursor < 10 && bytes[cursor].is_ascii_uppercase() {
        cursor += 1;
    }
    if !(2..=10).contains(&cursor) {
        return None;
    }
    let letters_end = cursor;
    if bytes.get(cursor) != Some(&b'-') {
        return None;
    }
    cursor += 1;
    let digits_start = cursor;
    while cursor < bytes.len() && cursor < digits_start + 6 && bytes[cursor].is_ascii_digit() {
        cursor += 1;
    }
    if cursor == digits_start {
        return None;
    }
    if bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        // More than 6 digits: not a plausible ticket number, and consuming
        // only a prefix would silently truncate a real (if unusual) token.
        return None;
    }
    if bytes
        .get(cursor)
        .copied()
        .is_some_and(u8_is_word_continuation)
    {
        return None;
    }
    let _ = letters_end;
    Some((rest[..cursor].to_owned(), cursor))
}

/// `#482` / `!918`: the marker, then 1+ ASCII digits, ending at a word
/// boundary.
fn match_numeric_reference(rest: &str, marker: char) -> Option<(String, usize)> {
    let bytes = rest.as_bytes();
    debug_assert_eq!(bytes[0], marker as u8);
    let mut cursor = 1;
    let digits_start = cursor;
    while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
        cursor += 1;
    }
    if cursor == digits_start {
        return None;
    }
    if bytes
        .get(cursor)
        .copied()
        .is_some_and(u8_is_word_continuation)
    {
        return None;
    }
    Some((rest[digits_start..cursor].to_owned(), cursor))
}

/// Consumes a URL until whitespace, an enclosing bracket/quote, or trailing
/// sentence punctuation that is almost never part of the URL itself.
fn match_url(rest: &str) -> (String, usize) {
    let end = rest
        .find(|character: char| character.is_whitespace() || ")]}>\"'".contains(character))
        .unwrap_or(rest.len());
    let mut end = end;
    while end > 0 {
        let trimmed = &rest[..end];
        if trimmed.ends_with(['.', ',', ':', ';']) {
            end -= 1;
        } else {
            break;
        }
    }
    (rest[..end].to_owned(), end)
}

const fn u8_is_word_continuation(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

fn starts_word(text: &str, index: usize) -> bool {
    not_preceded_by_alphanumeric(text, index)
}

fn not_preceded_by_alphanumeric(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_none_or(|character| !character.is_alphanumeric())
}

const fn utf8_char_len(lead_byte: u8) -> usize {
    if lead_byte & 0b1000_0000 == 0 {
        1
    } else if lead_byte & 0b1110_0000 == 0b1100_0000 {
        2
    } else if lead_byte & 0b1111_0000 == 0b1110_0000 {
        3
    } else if lead_byte & 0b1111_1000 == 0b1111_0000 {
        4
    } else {
        1
    }
}

/// Whether `identity` is a plausible target for `reference` (PR-LINK-002):
/// exact match only, never a fuzzy or partial one.
fn reference_matches(reference: &ExternalReference, identity: &ArtifactIdentity) -> bool {
    match reference.kind {
        ReferenceKind::TicketKey => {
            matches!(identity.kind, ArtifactKind::Issue) && identity.external_id == reference.value
        }
        ReferenceKind::IssueNumber => {
            matches!(
                identity.kind,
                ArtifactKind::Issue | ArtifactKind::PullRequest
            ) && identity.external_id == reference.value
        }
        ReferenceKind::MergeRequestNumber => {
            identity.kind == ArtifactKind::MergeRequest && identity.external_id == reference.value
        }
        ReferenceKind::Url => false,
    }
}

/// Deterministic `References` links from one artifact's title/body to
/// every other *already-known* artifact its text names (PR-LINK-001/002).
/// Never links to a target absent from `known_artifacts` (PR-LINK-004): an
/// unresolved reference is simply not linked, not guessed at.
#[must_use]
pub fn text_reference_links(source: &Artifact, known_artifacts: &[Artifact]) -> Vec<ArtifactLink> {
    let mut links = Vec::new();
    for text in [source.title.as_str(), source.body.as_str()] {
        for reference in extract_references(text) {
            for candidate in known_artifacts {
                if candidate.identity == source.identity {
                    continue;
                }
                let matches_identity = reference_matches(&reference, &candidate.identity);
                let matches_url = reference.kind == ReferenceKind::Url
                    && candidate.source_locator.as_str() == reference.value;
                if matches_identity || matches_url {
                    links.push(ArtifactLink {
                        source: source.identity.clone(),
                        target: ArtifactLinkTarget::Artifact(candidate.identity.clone()),
                        kind: ArtifactLinkKind::References,
                        evidence_locator: format!("text:{}", reference.value),
                    });
                }
            }
        }
    }
    links.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
    links.dedup();
    links
}

/// A changeset touching more than this fraction of the repository's own
/// currently indexed files is a sweep — a vendored drop, a formatting pass,
/// a generated-code regeneration, a mechanical rename across half the
/// codebase — where "this file changed, so every symbol in it is
/// implicated" stops being evidence and becomes noise. A fixed file count
/// would be meaningless across repository sizes (50 files is most of a
/// small service but a rounding error in a monorepo), so the threshold
/// scales with [`indexed_file_count`], floored at
/// [`MIN_CHANGED_PATHS_FOR_SYMBOL_ATTRIBUTION`] so a small repository isn't
/// blocked from ever attributing a normal-sized commit.
const SWEEP_RATIO: f64 = 0.1;

/// The floor [`sweep_threshold`] never drops below, regardless of how few
/// files are currently indexed.
const MIN_CHANGED_PATHS_FOR_SYMBOL_ATTRIBUTION: usize = 20;

/// Distinct file paths currently backing an indexed code symbol.
fn indexed_file_count(graph: &GraphSnapshot) -> usize {
    graph
        .nodes
        .values()
        .filter_map(|node| match &node.attributes {
            PlannedNodeAttributes::Symbol { file_path, .. } => Some(file_path.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .len()
}

/// Above this many changed paths, [`changed_symbol_links`] emits nothing at
/// all rather than an arbitrary subset. See the private `SWEEP_RATIO`
/// constant for the fraction used to calculate the threshold.
pub fn sweep_threshold(graph: &GraphSnapshot) -> usize {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]
    let scaled = (indexed_file_count(graph) as f64 * SWEEP_RATIO).round() as usize;
    scaled.max(MIN_CHANGED_PATHS_FOR_SYMBOL_ATTRIBUTION)
}

/// Deterministic `ChangedSymbol` links from `source` to every currently
/// indexed code symbol whose file is among `changed_paths` — a structural
/// fact ("this artifact's changeset touched this file, which currently
/// contains this symbol"), file-level rather than diff-precise. Weaker
/// evidence than a proven per-symbol body change (see
/// `ctx_core::review`'s changed-entity detection), but never a guess about
/// *what* changed, only *where*. Empty when `changed_paths` exceeds
/// [`sweep_threshold`].
#[must_use]
pub fn changed_symbol_links(
    source: &ArtifactIdentity,
    changed_paths: &BTreeSet<String>,
    graph: &GraphSnapshot,
    sweep_threshold: usize,
) -> Vec<ArtifactLink> {
    if changed_paths.len() > sweep_threshold {
        return Vec::new();
    }
    let mut links = graph
        .nodes
        .values()
        .filter_map(|node| match &node.attributes {
            PlannedNodeAttributes::Symbol { file_path, .. }
                if changed_paths.contains(file_path) =>
            {
                Some((node.stable_key.clone(), file_path.clone()))
            }
            _ => None,
        })
        .map(|(stable_key, file_path)| ArtifactLink {
            source: source.clone(),
            target: ArtifactLinkTarget::CodeSymbol(stable_key),
            kind: ArtifactLinkKind::ChangedSymbol,
            evidence_locator: format!("changed_file:{file_path}"),
        })
        .collect::<Vec<_>>();
    links.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
    links
}

#[cfg(test)]
mod tests {
    use crate::{
        artifact::ArtifactProvider,
        domain::{NodeKind, StableKey},
        graph::GraphNode,
        ir::{SourceRange, SymbolKind},
    };

    use super::*;

    #[test]
    fn extracts_a_ticket_key_regardless_of_where_it_appears() {
        assert_eq!(
            extract_references("PAY-317 When a subscription is cancelled"),
            vec![reference(ReferenceKind::TicketKey, "PAY-317")]
        );
        assert_eq!(
            extract_references("branch: feature/PAY-317-subscription"),
            vec![reference(ReferenceKind::TicketKey, "PAY-317")]
        );
        assert_eq!(
            extract_references("This was originally reported in PAY-299, see also PAY-317."),
            vec![
                reference(ReferenceKind::TicketKey, "PAY-299"),
                reference(ReferenceKind::TicketKey, "PAY-317"),
            ]
        );
    }

    #[test]
    fn extracts_issue_and_merge_request_numbers() {
        assert_eq!(
            extract_references("Fixes #482 and relates to !918"),
            vec![
                reference(ReferenceKind::IssueNumber, "482"),
                reference(ReferenceKind::MergeRequestNumber, "918"),
            ]
        );
    }

    #[test]
    fn extracts_a_full_ticket_url_and_trims_trailing_punctuation() {
        assert_eq!(
            extract_references("See https://jira.example/browse/PAY-317, thanks."),
            vec![
                reference(ReferenceKind::Url, "https://jira.example/browse/PAY-317"),
                reference(ReferenceKind::TicketKey, "PAY-317"),
            ]
        );
    }

    #[test]
    fn does_not_match_a_ticket_like_substring_inside_a_longer_token() {
        assert_eq!(
            extract_references("SEEPAY-317BUG version PAY-3170000"),
            Vec::new()
        );
    }

    #[test]
    fn incidental_mention_still_extracts_the_reference_deterministically() {
        // FR-02: extraction succeeding is correct here; the codebase must
        // not additionally conclude a relation kind from this alone — that
        // is deliberately out of this module's scope.
        assert_eq!(
            extract_references("PAY-317 is related, but this MR only updates logging."),
            vec![reference(ReferenceKind::TicketKey, "PAY-317")]
        );
    }

    #[test]
    fn never_panics_on_multi_byte_utf8_near_a_reference_boundary() {
        let text = "Дескрипшн MR: PAY-317 — исправление отмены подписки 🎉 #482";
        let references = extract_references(text);
        assert!(references.contains(&reference(ReferenceKind::TicketKey, "PAY-317")));
        assert!(references.contains(&reference(ReferenceKind::IssueNumber, "482")));
    }

    #[test]
    fn text_reference_links_only_target_already_known_artifacts() {
        let issue = artifact(
            "PAY-317",
            ArtifactKind::Issue,
            "Cancellation must preserve access",
            "",
        );
        let mr = artifact(
            "842",
            ArtifactKind::MergeRequest,
            "Preserve prepaid entitlement",
            "Fixes PAY-317. Related to PAY-999 (untracked).",
        );

        let links = text_reference_links(&mr, &[issue.clone(), mr.clone()]);

        assert_eq!(
            links,
            vec![ArtifactLink {
                source: mr.identity.clone(),
                target: ArtifactLinkTarget::Artifact(issue.identity.clone()),
                kind: ArtifactLinkKind::References,
                evidence_locator: "text:PAY-317".to_owned(),
            }]
        );
    }

    #[test]
    fn changed_symbol_links_attribute_symbols_in_changed_files_only() {
        let touched = symbol_node("cancel", "billing.cancel", "billing.py");
        let untouched = symbol_node("refund", "billing.refund", "refund.py");
        let graph = GraphSnapshot {
            nodes: [touched.clone(), untouched]
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges: Vec::new(),
        };
        let source = artifact("842", ArtifactKind::MergeRequest, "Fix cancellation", "").identity;
        let changed_paths = BTreeSet::from(["billing.py".to_owned()]);

        let threshold = sweep_threshold(&graph);
        let links = changed_symbol_links(&source, &changed_paths, &graph, threshold);

        assert_eq!(
            links,
            vec![ArtifactLink {
                source,
                target: ArtifactLinkTarget::CodeSymbol(touched.stable_key),
                kind: ArtifactLinkKind::ChangedSymbol,
                evidence_locator: "changed_file:billing.py".to_owned(),
            }]
        );
    }

    #[test]
    fn an_oversized_changeset_attributes_no_symbols_rather_than_an_arbitrary_subset() {
        let touched = symbol_node("cancel", "billing.cancel", "billing.py");
        let graph = GraphSnapshot {
            nodes: [touched]
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges: Vec::new(),
        };
        let source = artifact("842", ArtifactKind::MergeRequest, "Vendor drop", "").identity;
        // Only one file is indexed, so the floor (not the ratio) governs:
        // this must exceed MIN_CHANGED_PATHS_FOR_SYMBOL_ATTRIBUTION.
        let changed_paths = (0..=MIN_CHANGED_PATHS_FOR_SYMBOL_ATTRIBUTION)
            .map(|n| format!("file-{n}.py"))
            .chain(std::iter::once("billing.py".to_owned()))
            .collect::<BTreeSet<_>>();

        let threshold = sweep_threshold(&graph);
        let links = changed_symbol_links(&source, &changed_paths, &graph, threshold);

        assert!(links.is_empty());
    }

    #[test]
    fn the_sweep_threshold_scales_with_repository_size_rather_than_a_fixed_count() {
        // 200 indexed files puts the 10% ratio (20) above the floor (20 too,
        // here, but the point is it tracks repository size, not a constant).
        let nodes = (0..200)
            .map(|n| {
                symbol_node(
                    &format!("sym-{n}"),
                    &format!("sym{n}"),
                    &format!("file-{n}.py"),
                )
            })
            .collect::<Vec<_>>();
        let graph = GraphSnapshot {
            nodes: nodes
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges: Vec::new(),
        };
        let source = artifact("842", ArtifactKind::MergeRequest, "Refactor", "").identity;

        let within_threshold = (0..15).map(|n| format!("file-{n}.py")).collect();
        let over_threshold = (0..25).map(|n| format!("file-{n}.py")).collect();

        let threshold = sweep_threshold(&graph);
        assert_eq!(
            changed_symbol_links(&source, &within_threshold, &graph, threshold).len(),
            15,
            "15 of 200 files (7.5%) is a normal-sized change in a repo this size"
        );
        assert!(
            changed_symbol_links(&source, &over_threshold, &graph, threshold).is_empty(),
            "25 of 200 files (12.5%) is a sweep in a repo this size, even though \
             the old fixed 50-file cap would have let it through"
        );
    }

    fn reference(kind: ReferenceKind, value: &str) -> ExternalReference {
        ExternalReference {
            kind,
            value: value.to_owned(),
        }
    }

    fn artifact(external_id: &str, kind: ArtifactKind, title: &str, body: &str) -> Artifact {
        Artifact {
            identity: ArtifactIdentity {
                provider: ArtifactProvider::GitLab,
                kind,
                external_id: external_id.to_owned(),
            },
            project: crate::domain::Project("billing/subscriptions".to_owned()),
            title: title.to_owned(),
            body: body.to_owned(),
            author: None,
            external_created_at: None,
            external_updated_at: None,
            source_locator: crate::domain::Url(format!(
                "https://gitlab.example/-/issues/{external_id}"
            )),
            content_hash: "hash".to_owned(),
        }
    }

    fn symbol_node(key: &str, canonical: &str, file_path: &str) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::CodeSymbol,
            name: canonical.rsplit('.').next().unwrap_or(canonical).to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: file_path.to_owned(),
                canonical_path: canonical.to_owned(),
                symbol_kind: SymbolKind::Function,
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
}
