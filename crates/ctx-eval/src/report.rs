//! Pure comparison between recorded use-case results and ground truth checks.
//!
//! Nothing here touches the filesystem, Git, or `SQLite`: [`evaluate`] is a
//! deterministic function from a [`CaseRun`] plus a [`Check`] to a
//! [`CheckOutcome`], so the scoring rules are testable without building a
//! repository.

use std::collections::BTreeSet;

use ctx_app::status::StatusReport;
use ctx_core::{
    context_pack::ContextPack,
    graph::NodeSummary,
    impact::ImpactReport,
    review::{ChangeKind, ReviewReport, SchemaFinding, Severity},
};
use serde::Serialize;

/// One product-quality dimension a [`Check`] contributes evidence for.
///
/// This is what turns a pile of pass/fail assertions into the precision- and
/// recall-shaped numbers `product_conclu.md` asks for, instead of a vanity
/// pass count.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// A required signal was surfaced (missing it is a false negative).
    Recall,
    /// A forbidden signal stayed out (surfacing it is a false positive/noise).
    Precision,
    /// The change/edge classification matched the expected label.
    Classification,
    /// A Context Pack stayed inside its requested token budget.
    Budget,
}

/// One assertion about a case's recorded [`CaseRun`].
#[derive(Clone, Debug)]
pub enum Check {
    FindingIntentPresent(&'static str),
    FindingIntentAbsent(&'static str),
    FindingSeverity(&'static str, Severity),
    NoFindings,
    StaleRelationshipContains(&'static str),
    ChangeKindIs {
        canonical_path: &'static str,
        kind: ChangeKind,
    },
    ChangeSignalContains {
        canonical_path: &'static str,
        needle: &'static str,
    },
    ImpactIntentPresent(&'static str),
    ImpactIntentAbsent(&'static str),
    ImpactDataContractPresent(&'static str),
    ImpactDataContractAbsent(&'static str),
    ContextIdentifierPresent(&'static str),
    ContextIdentifierAbsent(&'static str),
    ContextWithinBudget,
    NoSchemaFindings,
    SchemaChangeDescriptionContains(&'static str),
    SchemaChangeDescriptionAbsent(&'static str),
    SchemaFindingDestructive(&'static str),
    SchemaFindingNotDestructive(&'static str),
    NoSchemaDivergences,
    SchemaDivergenceContains(&'static str),
    SchemaFindingRelatedIntentPresent {
        source_symbol: &'static str,
        intent: &'static str,
    },
    SchemaFindingRelatedIntentAbsent {
        source_symbol: &'static str,
        intent: &'static str,
    },
}

impl Check {
    #[must_use]
    pub const fn kind(&self) -> CheckKind {
        match self {
            Self::FindingIntentPresent(_)
            | Self::ImpactIntentPresent(_)
            | Self::ImpactDataContractPresent(_)
            | Self::ContextIdentifierPresent(_)
            | Self::SchemaChangeDescriptionContains(_)
            | Self::SchemaDivergenceContains(_)
            | Self::SchemaFindingRelatedIntentPresent { .. } => CheckKind::Recall,
            Self::FindingIntentAbsent(_)
            | Self::NoFindings
            | Self::ImpactIntentAbsent(_)
            | Self::ImpactDataContractAbsent(_)
            | Self::ContextIdentifierAbsent(_)
            | Self::NoSchemaFindings
            | Self::SchemaChangeDescriptionAbsent(_)
            | Self::NoSchemaDivergences
            | Self::SchemaFindingRelatedIntentAbsent { .. } => CheckKind::Precision,
            Self::FindingSeverity(..)
            | Self::StaleRelationshipContains(_)
            | Self::ChangeKindIs { .. }
            | Self::ChangeSignalContains { .. }
            | Self::SchemaFindingDestructive(_)
            | Self::SchemaFindingNotDestructive(_) => CheckKind::Classification,
            Self::ContextWithinBudget => CheckKind::Budget,
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::FindingIntentPresent(id) => format!("review surfaces a finding for {id}"),
            Self::FindingIntentAbsent(id) => format!("review does not surface a finding for {id}"),
            Self::FindingSeverity(id, severity) => {
                format!("the {id} finding has severity {severity:?}")
            }
            Self::NoFindings => "review surfaces no findings".to_owned(),
            Self::StaleRelationshipContains(needle) => {
                format!("review reports a stale relationship mentioning {needle}")
            }
            Self::ChangeKindIs {
                canonical_path,
                kind,
            } => format!("{canonical_path} is classified as {kind:?}"),
            Self::ChangeSignalContains {
                canonical_path,
                needle,
            } => format!("{canonical_path} reports change signal '{needle}'"),
            Self::ImpactIntentPresent(id) => format!("impact includes {id}"),
            Self::ImpactIntentAbsent(id) => format!("impact excludes {id}"),
            Self::ImpactDataContractPresent(id) => {
                format!("impact includes data contract {id}")
            }
            Self::ImpactDataContractAbsent(id) => {
                format!("impact excludes data contract {id}")
            }
            Self::ContextIdentifierPresent(id) => format!("context pack includes {id}"),
            Self::ContextIdentifierAbsent(id) => format!("context pack excludes {id}"),
            Self::ContextWithinBudget => "context pack stays within its token budget".to_owned(),
            Self::NoSchemaFindings => "review surfaces no schema findings".to_owned(),
            Self::SchemaChangeDescriptionContains(needle) => {
                format!("a schema finding describes a change mentioning '{needle}'")
            }
            Self::SchemaChangeDescriptionAbsent(needle) => {
                format!("no schema finding describes a change mentioning '{needle}'")
            }
            Self::SchemaFindingDestructive(needle) => {
                format!("the schema finding for {needle} is destructive")
            }
            Self::SchemaFindingNotDestructive(needle) => {
                format!("the schema finding for {needle} is not destructive")
            }
            Self::NoSchemaDivergences => "status reports no schema divergences".to_owned(),
            Self::SchemaDivergenceContains(needle) => {
                format!("status reports a schema divergence mentioning '{needle}'")
            }
            Self::SchemaFindingRelatedIntentPresent {
                source_symbol,
                intent,
            } => format!("the schema finding for {source_symbol} relates to {intent}"),
            Self::SchemaFindingRelatedIntentAbsent {
                source_symbol,
                intent,
            } => format!("the schema finding for {source_symbol} does not relate to {intent}"),
        }
    }
}

/// The use-case outputs recorded while executing one evaluation case.
#[derive(Default)]
pub struct CaseRun {
    pub review: Option<ReviewReport>,
    pub impact: Option<ImpactReport>,
    pub context: Option<ContextPack>,
    pub status: Option<StatusReport>,
}

/// The verdict for one [`Check`] against a recorded [`CaseRun`].
#[derive(Clone, Debug, Serialize)]
pub struct CheckOutcome {
    pub description: String,
    pub kind: CheckKind,
    pub passed: bool,
    pub detail: String,
}

/// The outcome of driving one evaluation case: either every step ran and its
/// checks were scored, or the harness itself failed before that was
/// possible (a Git, storage, or use-case error, not a ground-truth mismatch).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CaseReport {
    Completed {
        id: &'static str,
        description: &'static str,
        passed: bool,
        checks: Vec<CheckOutcome>,
    },
    Errored {
        id: &'static str,
        description: &'static str,
        error: String,
    },
}

impl CaseReport {
    #[must_use]
    pub fn completed(
        id: &'static str,
        description: &'static str,
        checks: Vec<CheckOutcome>,
    ) -> Self {
        let passed = checks.iter().all(|outcome| outcome.passed);
        Self::Completed {
            id,
            description,
            passed,
            checks,
        }
    }

    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self, Self::Completed { passed: true, .. })
    }
}

/// Aggregate, non-vanity counts across a corpus run: how many required
/// signals were actually found (recall), how many forbidden signals stayed
/// out (precision/noise), how many classifications matched, and how many
/// Context Packs respected their budget.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub total_cases: usize,
    pub passed_cases: usize,
    pub errored_cases: usize,
    pub total_checks: usize,
    pub passed_checks: usize,
    pub recall_checks: usize,
    pub recall_passed: usize,
    pub precision_checks: usize,
    pub precision_passed: usize,
    pub classification_checks: usize,
    pub classification_passed: usize,
    pub budget_checks: usize,
    pub budget_passed: usize,
}

/// Computes a [`Summary`] from a completed corpus run. Pure: only reads the
/// already-recorded outcomes.
#[must_use]
pub fn summarize(reports: &[CaseReport]) -> Summary {
    let mut summary = Summary {
        total_cases: reports.len(),
        ..Summary::default()
    };
    for report in reports {
        match report {
            CaseReport::Errored { .. } => summary.errored_cases += 1,
            CaseReport::Completed { passed, checks, .. } => {
                if *passed {
                    summary.passed_cases += 1;
                }
                for outcome in checks {
                    summary.total_checks += 1;
                    if outcome.passed {
                        summary.passed_checks += 1;
                    }
                    let (checks, passed_count) = match outcome.kind {
                        CheckKind::Recall => {
                            (&mut summary.recall_checks, &mut summary.recall_passed)
                        }
                        CheckKind::Precision => {
                            (&mut summary.precision_checks, &mut summary.precision_passed)
                        }
                        CheckKind::Classification => (
                            &mut summary.classification_checks,
                            &mut summary.classification_passed,
                        ),
                        CheckKind::Budget => {
                            (&mut summary.budget_checks, &mut summary.budget_passed)
                        }
                    };
                    *checks += 1;
                    if outcome.passed {
                        *passed_count += 1;
                    }
                }
            }
        }
    }
    summary
}

/// Scores a present/absent membership check against a labeled set, producing
/// a `(passed, detail)` pair shared by the finding/impact/context checks.
fn membership_outcome(
    members: &BTreeSet<&str>,
    id: &str,
    expect_present: bool,
    label: &str,
) -> (bool, String) {
    let present = members.contains(id);
    (present == expect_present, format!("{label}: {members:?}"))
}

/// Evaluates one [`Check`] against a recorded [`CaseRun`].
#[must_use]
pub fn evaluate(run: &CaseRun, check: &Check) -> CheckOutcome {
    let (passed, detail) = match check {
        Check::FindingIntentPresent(id) => {
            membership_outcome(&finding_intents(run), id, true, "finding intents")
        }
        Check::FindingIntentAbsent(id) => {
            membership_outcome(&finding_intents(run), id, false, "finding intents")
        }
        Check::FindingSeverity(id, expected) => match matching_finding_severity(run, id) {
            Some(actual) => (
                actual == *expected,
                format!("{id} severity was {actual:?}, expected {expected:?}"),
            ),
            None => (false, format!("no finding for {id}")),
        },
        Check::NoFindings => {
            let count = run
                .review
                .as_ref()
                .map_or(0, |report| report.findings.len());
            (count == 0, format!("{count} finding(s) present"))
        }
        Check::StaleRelationshipContains(needle) => {
            let hit = run.review.as_ref().is_some_and(|report| {
                report
                    .stale_relationships
                    .iter()
                    .any(|entry| entry.contains(needle))
            });
            (
                hit,
                format!(
                    "stale relationships: {:?}",
                    run.review
                        .as_ref()
                        .map(|report| &report.stale_relationships)
                ),
            )
        }
        Check::ChangeKindIs {
            canonical_path,
            kind,
        } => change_kind_outcome(run, canonical_path, *kind),
        Check::ChangeSignalContains {
            canonical_path,
            needle,
        } => change_signal_outcome(run, canonical_path, needle),
        Check::ImpactIntentPresent(id) => {
            membership_outcome(&impact_identifiers(run), id, true, "impact identifiers")
        }
        Check::ImpactIntentAbsent(id) => {
            membership_outcome(&impact_identifiers(run), id, false, "impact identifiers")
        }
        Check::ImpactDataContractPresent(id) => membership_outcome(
            &impact_data_contracts(run),
            id,
            true,
            "impact data contracts",
        ),
        Check::ImpactDataContractAbsent(id) => membership_outcome(
            &impact_data_contracts(run),
            id,
            false,
            "impact data contracts",
        ),
        Check::ContextIdentifierPresent(id) => {
            membership_outcome(&context_identifiers(run), id, true, "context identifiers")
        }
        Check::ContextIdentifierAbsent(id) => {
            membership_outcome(&context_identifiers(run), id, false, "context identifiers")
        }
        Check::ContextWithinBudget => match &run.context {
            Some(pack) => (
                pack.estimated_tokens <= pack.token_budget,
                format!(
                    "{}/{} estimated tokens",
                    pack.estimated_tokens, pack.token_budget
                ),
            ),
            None => (false, "no context pack was compiled".to_owned()),
        },
        Check::NoSchemaFindings
        | Check::SchemaChangeDescriptionContains(_)
        | Check::SchemaChangeDescriptionAbsent(_)
        | Check::SchemaFindingDestructive(_)
        | Check::SchemaFindingNotDestructive(_)
        | Check::NoSchemaDivergences
        | Check::SchemaDivergenceContains(_)
        | Check::SchemaFindingRelatedIntentPresent { .. }
        | Check::SchemaFindingRelatedIntentAbsent { .. } => evaluate_schema(run, check),
    };
    CheckOutcome {
        description: check.describe(),
        kind: check.kind(),
        passed,
        detail,
    }
}

fn change_kind_outcome(
    run: &CaseRun,
    canonical_path: &str,
    expected: ChangeKind,
) -> (bool, String) {
    match matching_change_kind(run, canonical_path) {
        Some(actual) => (
            actual == expected,
            format!("{canonical_path} was classified {actual:?}, expected {expected:?}"),
        ),
        None => (false, format!("no changed entity for {canonical_path}")),
    }
}

fn change_signal_outcome(run: &CaseRun, canonical_path: &str, needle: &str) -> (bool, String) {
    match matching_change_signals(run, canonical_path) {
        Some(signals) => (
            signals.iter().any(|signal| signal.contains(needle)),
            format!("{canonical_path} signals: {signals:?}"),
        ),
        None => (false, format!("no changed entity for {canonical_path}")),
    }
}

fn evaluate_schema(run: &CaseRun, check: &Check) -> (bool, String) {
    match check {
        Check::NoSchemaFindings => {
            let count = run
                .review
                .as_ref()
                .map_or(0, |report| report.schema_findings.len());
            (count == 0, format!("{count} schema finding(s) present"))
        }
        Check::SchemaChangeDescriptionContains(needle) => {
            let descriptions = schema_change_descriptions(run);
            (
                descriptions.iter().any(|entry| entry.contains(needle)),
                format!("schema change descriptions: {descriptions:?}"),
            )
        }
        Check::SchemaChangeDescriptionAbsent(needle) => {
            let descriptions = schema_change_descriptions(run);
            (
                !descriptions.iter().any(|entry| entry.contains(needle)),
                format!("schema change descriptions: {descriptions:?}"),
            )
        }
        Check::SchemaFindingDestructive(needle) => match matching_schema_finding(run, needle) {
            Some(finding) => (
                finding.destructive,
                format!("{needle} destructive={}", finding.destructive),
            ),
            None => (false, format!("no schema finding matching '{needle}'")),
        },
        Check::SchemaFindingNotDestructive(needle) => match matching_schema_finding(run, needle) {
            Some(finding) => (
                !finding.destructive,
                format!("{needle} destructive={}", finding.destructive),
            ),
            None => (false, format!("no schema finding matching '{needle}'")),
        },
        Check::NoSchemaDivergences => {
            let count = run
                .status
                .as_ref()
                .map_or(0, |status| status.schema_divergences.len());
            (count == 0, format!("{count} schema divergence(s) present"))
        }
        Check::SchemaDivergenceContains(needle) => {
            let labels = schema_divergence_labels(run);
            (
                labels.iter().any(|entry| entry.contains(needle)),
                format!("schema divergences: {labels:?}"),
            )
        }
        Check::SchemaFindingRelatedIntentPresent {
            source_symbol,
            intent,
        } => {
            let related = related_intent_identifiers(run, source_symbol);
            (
                related.contains(intent),
                format!("related intents for {source_symbol}: {related:?}"),
            )
        }
        Check::SchemaFindingRelatedIntentAbsent {
            source_symbol,
            intent,
        } => {
            let related = related_intent_identifiers(run, source_symbol);
            (
                !related.contains(intent),
                format!("related intents for {source_symbol}: {related:?}"),
            )
        }
        _ => unreachable!("evaluate_schema is only called for schema-related checks"),
    }
}

fn related_intent_identifiers<'a>(run: &'a CaseRun, source_symbol: &str) -> BTreeSet<&'a str> {
    matching_schema_finding(run, source_symbol).map_or_else(BTreeSet::new, |finding| {
        finding
            .related_intents
            .iter()
            .map(|intent| intent.identifier.as_str())
            .collect()
    })
}

fn finding_intents(run: &CaseRun) -> BTreeSet<&str> {
    run.review
        .as_ref()
        .map(|report| {
            report
                .findings
                .iter()
                .map(|finding| finding.affected_intent.identifier.as_str())
                .collect()
        })
        .unwrap_or_default()
}

fn matching_finding_severity(run: &CaseRun, id: &str) -> Option<Severity> {
    run.review.as_ref().and_then(|report| {
        report
            .findings
            .iter()
            .find(|finding| finding.affected_intent.identifier == id)
            .map(|finding| finding.severity)
    })
}

fn matching_change_kind(run: &CaseRun, canonical_path: &str) -> Option<ChangeKind> {
    run.review.as_ref().and_then(|report| {
        report
            .changed_entities
            .iter()
            .find(|entity| {
                entity.before.as_deref() == Some(canonical_path)
                    || entity.after.as_deref() == Some(canonical_path)
            })
            .map(|entity| entity.change_kind)
    })
}

fn matching_change_signals<'a>(run: &'a CaseRun, canonical_path: &str) -> Option<&'a [String]> {
    run.review.as_ref().and_then(|report| {
        report
            .changed_entities
            .iter()
            .find(|entity| {
                entity.before.as_deref() == Some(canonical_path)
                    || entity.after.as_deref() == Some(canonical_path)
            })
            .map(|entity| entity.signals.as_slice())
    })
}

fn impact_identifiers(run: &CaseRun) -> BTreeSet<&str> {
    let Some(report) = &run.impact else {
        return BTreeSet::new();
    };
    let groups: [&[NodeSummary]; 7] = [
        &report.features,
        &report.requirements,
        &report.invariants,
        &report.decisions,
        &report.data_contracts,
        &report.implementation,
        &report.tests,
    ];
    groups
        .into_iter()
        .flatten()
        .map(|node| node.identifier.as_str())
        .collect()
}

fn impact_data_contracts(run: &CaseRun) -> BTreeSet<&str> {
    run.impact
        .as_ref()
        .map(|report| {
            report
                .data_contracts
                .iter()
                .map(|node| node.identifier.as_str())
                .collect()
        })
        .unwrap_or_default()
}

fn context_identifiers(run: &CaseRun) -> BTreeSet<&str> {
    run.context
        .as_ref()
        .map(|pack| {
            pack.items
                .iter()
                .map(|item| item.identifier.as_str())
                .collect()
        })
        .unwrap_or_default()
}

fn schema_change_descriptions(run: &CaseRun) -> Vec<String> {
    run.review.as_ref().map_or_else(Vec::new, |report| {
        report
            .schema_findings
            .iter()
            .flat_map(|finding| {
                finding
                    .changes
                    .iter()
                    .map(ctx_core::schema::SchemaChange::description)
            })
            .collect()
    })
}

fn matching_schema_finding<'a>(run: &'a CaseRun, needle: &str) -> Option<&'a SchemaFinding> {
    run.review.as_ref().and_then(|report| {
        report
            .schema_findings
            .iter()
            .find(|finding| finding.source_symbol.contains(needle))
    })
}

fn schema_divergence_labels(run: &CaseRun) -> Vec<String> {
    run.status.as_ref().map_or_else(Vec::new, |status| {
        status
            .schema_divergences
            .iter()
            .map(|divergence| {
                format!(
                    "{}.{} ({:?})",
                    divergence.entity, divergence.column, divergence.kind
                )
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use ctx_core::{
        context_pack::{ContextItem, ContextPriority},
        domain::NodeKind,
        graph::NodeSummary,
        review::{ChangedEntity, ReviewFinding},
    };

    use super::*;

    fn summary(identifier: &str, kind: NodeKind) -> NodeSummary {
        NodeSummary {
            stable_key: format!("stable:{identifier}"),
            kind,
            identifier: identifier.to_owned(),
            name: identifier.to_owned(),
            visibility: None,
        }
    }

    fn finding(identifier: &str, severity: Severity) -> ReviewFinding {
        ReviewFinding {
            severity,
            confidence: 0.9,
            changed_entity: "changed".to_owned(),
            change_kind: ChangeKind::BehaviorPotentiallyChanged,
            affected_intent: summary(identifier, NodeKind::Invariant),
            reason: "reason".to_owned(),
            evidence: Vec::new(),
            related_tests: Vec::new(),
            tests_modified: false,
            possible_requirement_drift: false,
            uncertainty: None,
            suggested_action: "action".to_owned(),
        }
    }

    #[test]
    fn required_intent_missing_from_findings_fails_the_check() {
        let run = CaseRun {
            review: Some(ReviewReport {
                base: "HEAD".to_owned(),
                changed_entities: Vec::new(),
                findings: vec![finding("INV-A", Severity::High)],
                schema_findings: Vec::new(),
                stale_relationships: Vec::new(),
                suppressed_non_behavioral_changes: 0,
            }),
            impact: None,
            context: None,
            status: None,
        };
        let missing = evaluate(&run, &Check::FindingIntentPresent("INV-B"));
        assert!(!missing.passed);
        assert_eq!(missing.kind, CheckKind::Recall);
        let present = evaluate(&run, &Check::FindingIntentPresent("INV-A"));
        assert!(present.passed);
    }

    #[test]
    fn forbidden_intent_present_fails_the_precision_check() {
        let run = CaseRun {
            review: Some(ReviewReport {
                base: "HEAD".to_owned(),
                changed_entities: Vec::new(),
                findings: vec![finding("ADR-A", Severity::Medium)],
                schema_findings: Vec::new(),
                stale_relationships: Vec::new(),
                suppressed_non_behavioral_changes: 0,
            }),
            impact: None,
            context: None,
            status: None,
        };
        let outcome = evaluate(&run, &Check::FindingIntentAbsent("ADR-A"));
        assert!(!outcome.passed);
        assert_eq!(outcome.kind, CheckKind::Precision);
    }

    #[test]
    fn change_kind_check_matches_before_or_after_canonical_path() {
        let run = CaseRun {
            review: Some(ReviewReport {
                base: "HEAD".to_owned(),
                changed_entities: vec![ChangedEntity {
                    stable_key: None,
                    before: Some("pkg.cancel".to_owned()),
                    after: Some("pkg.cancel_subscription".to_owned()),
                    file_path: "pkg.py".to_owned(),
                    change_kind: ChangeKind::Rename,
                    signals: Vec::new(),
                }],
                findings: Vec::new(),
                schema_findings: Vec::new(),
                stale_relationships: Vec::new(),
                suppressed_non_behavioral_changes: 1,
            }),
            impact: None,
            context: None,
            status: None,
        };
        let outcome = evaluate(
            &run,
            &Check::ChangeKindIs {
                canonical_path: "pkg.cancel",
                kind: ChangeKind::Rename,
            },
        );
        assert!(outcome.passed);
    }

    #[test]
    fn context_within_budget_fails_without_a_compiled_pack() {
        let outcome = evaluate(&CaseRun::default(), &Check::ContextWithinBudget);
        assert!(!outcome.passed);
    }

    #[test]
    fn context_within_budget_checks_estimate_against_requested_budget() {
        let run = CaseRun {
            review: None,
            impact: None,
            context: Some(ContextPack {
                task: "task".to_owned(),
                token_budget: 100,
                estimated_tokens: 120,
                truncated: true,
                seeds: Vec::new(),
                items: vec![ContextItem {
                    priority: ContextPriority::Invariant,
                    kind: NodeKind::Invariant,
                    identifier: "INV-A".to_owned(),
                    title: "title".to_owned(),
                    content: "content".to_owned(),
                    estimated_tokens: 120,
                }],
                evidence: Vec::new(),
                uncertainties: Vec::new(),
            }),
            status: None,
        };
        assert!(!evaluate(&run, &Check::ContextWithinBudget).passed);
        assert!(evaluate(&run, &Check::ContextIdentifierPresent("INV-A")).passed);
        assert!(evaluate(&run, &Check::ContextIdentifierAbsent("REQ-OTHER")).passed);
    }
}
