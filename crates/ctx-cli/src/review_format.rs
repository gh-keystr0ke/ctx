use clap::ValueEnum;
use ctx_core::{
    domain::NodeKind,
    review::{ChangeKind, ReviewReport, Severity},
};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum ReviewFormat {
    Text,
    Markdown,
    Json,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum FailOn {
    Destructive,
    UntestedInvariant,
    High,
    Medium,
}

impl FailOn {
    pub(crate) fn blocks(self, report: &ReviewReport) -> bool {
        match self {
            Self::Destructive => {
                report.schema_findings.iter().any(|f| f.destructive)
                    || report.api_findings.iter().any(|f| f.destructive)
            }
            Self::UntestedInvariant => report.findings.iter().any(|f| {
                f.affected_intent.kind == NodeKind::Invariant
                    && (f.related_tests.is_empty() || !f.tests_modified)
            }),
            Self::High => report.findings.iter().any(|f| f.severity == Severity::High),
            Self::Medium => report
                .findings
                .iter()
                .any(|f| f.severity >= Severity::Medium),
        }
    }
}

fn summary(report: &ReviewReport) -> String {
    let count = |kind| {
        report
            .findings_by_intent
            .iter()
            .filter(|f| f.affected_intent.kind == kind)
            .count()
    };
    let changed = report.changed_entities.len() - report.suppressed_non_behavioral_changes;
    format!(
        "{changed} changed symbol(s) → {} requirements, {} invariants, {} decisions · {} schema · {} API",
        count(NodeKind::Requirement),
        count(NodeKind::Invariant),
        count(NodeKind::Decision),
        report.schema_findings.len(),
        report.api_findings.len()
    )
}

fn change_label(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::BehaviorPotentiallyChanged => "body changed",
        ChangeKind::ContractChanged => "contract changed",
        ChangeKind::FormattingOnly => "formatting changed",
        ChangeKind::Rename => "renamed",
        ChangeKind::RefactorLikely => "likely refactor",
        ChangeKind::Unknown => "unknown change",
    }
}

pub(crate) fn print_text(report: &ReviewReport, verbose: bool, show_tests: bool) {
    println!("{}", summary(report));
    if verbose && !report.new_behavioral_decisions.is_empty() {
        println!("New unexplained decisions:");
        for decision in &report.new_behavioral_decisions {
            println!(
                "  {}:{} {}",
                decision.file, decision.line, decision.condition
            );
        }
    }
    for group in &report.findings_by_intent {
        let intent = &group.affected_intent;
        let name = intent.name.split_whitespace().collect::<Vec<_>>().join(" ");
        let short = name
            .split_once(". ")
            .map_or(name.as_str(), |(sentence, _)| sentence);
        let title = if verbose {
            name.clone()
        } else {
            short.chars().take(160).collect()
        };
        println!("{:?} — {} — {title}", group.severity, intent.identifier);
        let mut signals = group.signals.iter();
        if !verbose {
            signals = group.signals[..group.signals.len().min(1)].iter();
        }
        for signal in signals {
            println!(
                "  {} ({}) — {}",
                signal.changed_entity,
                change_label(signal.change_kind),
                signal.reason
            );
            if verbose {
                for evidence in &signal.evidence {
                    println!("  Evidence: {evidence}");
                }
                if let Some(uncertainty) = &signal.uncertainty {
                    println!("  {uncertainty}");
                }
            }
        }
        if !verbose && group.signals.len() > 1 {
            println!(
                "  +{} more signals (use --verbose)",
                group.signals.len() - 1
            );
        }
        if let Some(signal) = group.signals.first() {
            let limit = if verbose {
                signal.related_tests.len()
            } else {
                3
            };
            let tests = signal
                .related_tests
                .iter()
                .take(limit)
                .map(|test| test.identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let more = signal.related_tests.len().saturating_sub(limit);
            let suffix = if more == 0 {
                String::new()
            } else {
                format!(" +{more} more")
            };
            if !tests.is_empty() {
                println!("  Tests: {tests}{suffix}");
            }
            println!("  {}", signal.suggested_action);
        }
    }
    print_api_findings(&report.api_findings);
    print_schema_findings(&report.schema_findings);
    for relationship in &report.stale_relationships {
        println!("Stale relationship: {relationship}");
    }
    if show_tests {
        println!("Tests to run (reachable from changed code):");
        for test in &report.tests_to_run {
            println!("  - {}", test.identifier);
        }
    }
    if verbose {
        println!(
            "Suppressed {} formatting/rename/refactor changes.",
            report.suppressed_non_behavioral_changes
        );
    }
}

fn escape(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace(char::from(92), "&#92;")
        .replace('|', "&#124;")
        .replace('`', "&#96;")
        .replace('*', "&#42;")
        .replace('_', "&#95;")
        .replace('[', "&#91;")
        .replace(']', "&#93;")
        .replace('@', "&#64;")
}

pub(crate) fn print_markdown(report: &ReviewReport) {
    println!("## ctx review\n\n{}\n", summary(report));
    if !report.findings_by_intent.is_empty() {
        println!("| Intent | Severity | Description |\n| --- | --- | --- |");
        for group in &report.findings_by_intent {
            println!(
                "| {} | {:?} | {} |",
                escape(&group.affected_intent.identifier),
                group.severity,
                escape(&group.affected_intent.name)
            );
        }
        for group in &report.findings_by_intent {
            println!(
                "\n<details>\n<summary>{}</summary>\n",
                escape(&group.affected_intent.identifier)
            );
            for signal in &group.signals {
                println!(
                    "- **{}** ({}). {}",
                    escape(&signal.changed_entity),
                    change_label(signal.change_kind),
                    escape(&signal.reason)
                );
                for evidence in &signal.evidence {
                    println!("  - Evidence: {}", escape(evidence));
                }
                for test in &signal.related_tests {
                    println!("  - Related test: {}", escape(&test.identifier));
                }
                if let Some(uncertainty) = &signal.uncertainty {
                    println!("  - {}", escape(uncertainty));
                }
                println!("  - {}", escape(&signal.suggested_action));
            }
            println!("\n</details>");
        }
    }
    if !report.schema_findings.is_empty() {
        println!("\n### Observed schema changes\n");
        for finding in &report.schema_findings {
            for change in &finding.changes {
                println!(
                    "- {} — {}",
                    escape(&finding.file_path),
                    escape(&change.description())
                );
            }
        }
    }
    if !report.api_findings.is_empty() {
        println!("\n### Observed API changes\n");
        for finding in &report.api_findings {
            for change in &finding.changes {
                println!(
                    "- {} — {}",
                    escape(&finding.file_path),
                    escape(&change.description())
                );
            }
        }
    }
    if !report.stale_relationships.is_empty() {
        println!("\n### Stale relationships\n");
        for relationship in &report.stale_relationships {
            println!("- {}", escape(relationship));
        }
    }
    if !report.tests_to_run.is_empty() {
        println!("\n### Tests to run\n");
        for test in &report.tests_to_run {
            println!("- {}", escape(&test.identifier));
        }
    }
}

fn print_api_findings(findings: &[ctx_core::review::ApiFinding]) {
    if findings.is_empty() {
        return;
    }
    println!("Observed API changes (deterministic; not proven requirement impact):");
    for finding in findings {
        println!(
            "{} — {}",
            if finding.destructive {
                "DESTRUCTIVE"
            } else {
                "informational"
            },
            finding.source_symbol
        );
        for change in &finding.changes {
            println!(
                "  - [{}] {}",
                if change.destructive {
                    "destructive"
                } else {
                    "informational"
                },
                change.description()
            );
        }
        if finding.related_intents.is_empty() {
            println!("  Possible product impact: no known mapping found (not proven unrelated).");
        } else {
            let intents = finding
                .related_intents
                .iter()
                .map(|intent| intent.identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  Possible product impact: {intents}");
        }
        if !finding.related_tests.is_empty() {
            let tests = finding
                .related_tests
                .iter()
                .map(|test| test.identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  Related tests: {tests}");
        }
        println!();
    }
}

fn print_schema_findings(findings: &[ctx_core::review::SchemaFinding]) {
    if findings.is_empty() {
        return;
    }
    println!("Observed schema changes (deterministic; not proven requirement impact):");
    for finding in findings {
        println!(
            "{} — {}",
            if finding.destructive {
                "DESTRUCTIVE"
            } else {
                "informational"
            },
            finding.source_symbol
        );
        for change in &finding.changes {
            println!(
                "  - [{}] {}",
                if change.destructive {
                    "destructive"
                } else {
                    "informational"
                },
                change.description()
            );
        }
        if finding.related_intents.is_empty() {
            println!("  Possible product impact: no known mapping found (not proven unrelated).");
        } else {
            let intents = finding
                .related_intents
                .iter()
                .map(|intent| intent.identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  Possible product impact: {intents}");
        }
        if !finding.related_tests.is_empty() {
            let tests = finding
                .related_tests
                .iter()
                .map(|test| test.identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  Related tests: {tests}");
        }
        println!();
    }
}
