use std::{fmt::Write as _, path::PathBuf};

use ctx_app::report::{
    ReportData, ReportDetail, ReportDirectory, ReportFile, ReportRelation, ReportSymbol,
};
use ctx_core::{
    artifact::Artifact, domain::StableKey, graph::NodeSummary, indexing::PlannedNodeAttributes,
};

use crate::{
    RenderError, RenderedReport, ReportFormat, ReportRenderer,
    common::{
        artifact_url, code_url, entity_filename, external_url, kind_name, kind_slug, node_body,
    },
};

#[derive(Clone, Copy, Debug, Default)]
pub struct MarkdownRenderer;

impl ReportRenderer for MarkdownRenderer {
    fn render(&self, data: &ReportData) -> Result<RenderedReport, RenderError> {
        let mut report = RenderedReport::new(ReportFormat::Markdown, &data.meta.source_commit);
        report.insert("index.md", render_index(data));
        report.insert("tree.md", render_tree(data));
        for catalog in &data.catalogs {
            report.insert(
                PathBuf::from("catalog").join(format!("{}.md", kind_slug(catalog.kind))),
                render_catalog(catalog),
            );
        }
        for (key, detail) in &data.details {
            report.insert(
                PathBuf::from("entity").join(entity_filename(key, "md")),
                render_detail(data, key, detail),
            );
        }
        Ok(report)
    }
}

fn render_index(data: &ReportData) -> String {
    let mut output = String::from("# Context dashboard\n\n");
    write!(
        output,
        "Source commit: `{}`  \nIndex: `{:?}`  \nHealth: `{:?}`\n\n",
        code(&data.meta.source_commit),
        data.meta.index_state,
        data.meta.health
    )
    .expect("string write");
    output.push_str("## Repository knowledge\n\n| Metric | Count |\n| --- | ---: |\n");
    write!(
        output,
        "| Files | {} |\n| Symbols | {} |\n| Database entities | {} |\n| Active relationships | {} |\n| Stale semantic relationships | {} |\n\n",
        data.meta.knowledge.files,
        data.meta.knowledge.symbols,
        data.meta.knowledge.db_entities,
        data.meta.knowledge.active_edges,
        data.meta.knowledge.stale_semantic_edges
    )
    .expect("string write");
    output.push_str("## Catalogs\n\n");
    for catalog in &data.catalogs {
        writeln!(
            output,
            "- [{}](catalog/{}.md) — {}",
            md(kind_name(catalog.kind)),
            kind_slug(catalog.kind),
            catalog.nodes.len()
        )
        .expect("string write");
    }
    output.push_str("\n- [Source tree](tree.md)\n");
    if !data.meta.notices.is_empty() {
        output.push_str("\n## Attention\n\n");
        for notice in &data.meta.notices {
            writeln!(output, "- {}", md(notice)).expect("string write");
        }
    }
    output
}

fn render_catalog(catalog: &ctx_app::report::ReportCatalog) -> String {
    let mut output = format!(
        "# {} catalog\n\n[Dashboard](../index.md) · [Source tree](../tree.md)\n\n",
        md(kind_name(catalog.kind))
    );
    if catalog.nodes.is_empty() {
        output.push_str("No entities of this kind.\n");
        return output;
    }
    for node in &catalog.nodes {
        let key = StableKey::new(&node.stable_key).expect("stored stable key is valid");
        writeln!(
            output,
            "- [{}](../entity/{}) — `{}`",
            md(&node.name),
            entity_filename(&key, "md"),
            code(&node.identifier)
        )
        .expect("string write");
    }
    output
}

fn render_tree(data: &ReportData) -> String {
    let mut output = String::from("# Source tree\n\n[Dashboard](index.md)\n\n");
    for directory in &data.tree.directories {
        render_directory(&mut output, directory, 2);
    }
    for file in &data.tree.files {
        render_file(&mut output, file, 2);
    }
    if !data.tree.unattached_symbols.is_empty() {
        output.push_str("## Unattached symbols\n\n");
        for symbol in &data.tree.unattached_symbols {
            render_symbol(&mut output, symbol, 0);
        }
    }
    output
}

fn render_directory(output: &mut String, directory: &ReportDirectory, level: usize) {
    writeln!(output, "{} {}\n", "#".repeat(level), md(&directory.name)).expect("string write");
    for child in &directory.directories {
        render_directory(output, child, (level + 1).min(6));
    }
    for file in &directory.files {
        render_file(output, file, (level + 1).min(6));
    }
}

fn render_file(output: &mut String, file: &ReportFile, level: usize) {
    writeln!(
        output,
        "{} `{}`\n",
        "#".repeat(level),
        code(&file.node.identifier)
    )
    .expect("string write");
    for symbol in &file.symbols {
        render_symbol(output, symbol, 0);
    }
    output.push('\n');
}

fn render_symbol(output: &mut String, symbol: &ReportSymbol, depth: usize) {
    let key = StableKey::new(&symbol.node.stable_key).expect("stored stable key is valid");
    writeln!(
        output,
        "{}- [{}](entity/{}) — `{:?}` · `{}`",
        "  ".repeat(depth),
        md(&symbol.node.name),
        entity_filename(&key, "md"),
        symbol.symbol_kind,
        code(&symbol.node.identifier)
    )
    .expect("string write");
    for child in &symbol.children {
        render_symbol(output, child, depth + 1);
    }
}

fn render_detail(data: &ReportData, key: &StableKey, detail: &ReportDetail) -> String {
    let mut output = format!(
        "# {}\n\n[Dashboard](../index.md) · [Source tree](../tree.md)\n\nKind: **{}**  \nIdentifier: `{}`\n\n",
        md(&detail.node.name),
        md(kind_name(detail.node.kind)),
        code(detail.node.identifier())
    );
    if let Some(body) = node_body(&detail.node) {
        output.push_str("## Definition\n\n");
        output.push_str(&md_body(body));
        output.push_str("\n\n");
    }
    render_implementation(&mut output, data, detail);
    if let Some(provenance) = &detail.knowledge_provenance {
        output.push_str("## Knowledge provenance\n\n");
        writeln!(output, "- Producer: `{}`", code(&provenance.agent_producer))
            .expect("string write");
        writeln!(
            output,
            "- Model: `{}`",
            code(provenance.agent_model.as_deref().unwrap_or("not recorded"))
        )
        .expect("string write");
        writeln!(
            output,
            "- Decision: `{:?}` by `{}` at `{}`",
            provenance.decision_method,
            code(&provenance.decided_by),
            code(&provenance.decided_at)
        )
        .expect("string write");
        output.push('\n');
        render_artifacts(
            &mut output,
            "Source artifacts",
            &detail.provenance_artifacts,
            data,
        );
    }
    if !detail.artifact_history.is_empty() {
        output.push_str("## Artifact history\n\n");
        for linked in &detail.artifact_history {
            render_artifact(
                &mut output,
                &linked.artifact,
                data,
                Some(&format!("{:?}", linked.kind)),
            );
        }
        output.push('\n');
    }
    output.push_str("## Relationships\n\n");
    if detail.relations.is_empty() {
        output.push_str("No stored relationships.\n");
    } else {
        for relation in &detail.relations {
            render_relation(&mut output, data, key, relation);
        }
        render_mermaid(&mut output, detail);
    }
    output
}

fn render_implementation(output: &mut String, data: &ReportData, detail: &ReportDetail) {
    let PlannedNodeAttributes::Symbol {
        file_path,
        range,
        signature,
        ..
    } = &detail.node.attributes
    else {
        return;
    };
    output.push_str("## Implementation\n\n");
    writeln!(output, "- File: `{}`", code(file_path)).expect("string write");
    writeln!(output, "- Lines: {}–{}", range.start_line, range.end_line).expect("string write");
    if let Some(signature) = signature {
        writeln!(output, "- Signature: `{}`", code(signature)).expect("string write");
    }
    if let Some(url) = code_url(
        data.meta.remote_url.as_deref(),
        &data.meta.source_commit,
        &detail.node,
    ) {
        writeln!(output, "- [Open code at source commit](<{url}>)").expect("string write");
    }
    output.push('\n');
}

fn render_relation(
    output: &mut String,
    data: &ReportData,
    subject: &StableKey,
    relation: &ReportRelation,
) {
    let outgoing = relation.source.stable_key == subject.as_str();
    let direction = if outgoing { "outgoing" } else { "incoming" };
    write!(
        output,
        "- **{:?}** ({direction}, `{:?}`, `{:?}`, {:.2}): ",
        relation.kind, relation.claim_class, relation.status, relation.confidence
    )
    .expect("string write");
    render_summary_link(output, data, &relation.source);
    output.push_str(" → ");
    render_summary_link(output, data, &relation.target);
    output.push('\n');
    if let Some(reason) = &relation.stale_reason {
        writeln!(output, "  - Stale: {}", md(reason)).expect("string write");
    }
    for evidence in &relation.evidence {
        write!(output, "  - Evidence: ").expect("string write");
        if let Some(url) = external_url(&evidence.source_uri) {
            write!(output, "[{}](<{url}>)", md(&evidence.locator)).expect("string write");
        } else {
            write!(output, "`{}`", code(&evidence.locator)).expect("string write");
        }
        writeln!(
            output,
            " — `{:?}`, strength {:.2}",
            evidence.source_kind,
            evidence.strength.get()
        )
        .expect("string write");
    }
}

fn render_summary_link(output: &mut String, data: &ReportData, node: &NodeSummary) {
    let key = StableKey::new(&node.stable_key).expect("stored stable key is valid");
    if data.details.contains_key(&key) {
        write!(
            output,
            "[{}]({})",
            md(&node.name),
            entity_filename(&key, "md")
        )
        .expect("string write");
    } else if let Some(url) = external_url(&node.identifier) {
        write!(output, "[{}](<{url}>)", md(&node.name)).expect("string write");
    } else {
        write!(output, "{} (`{}`)", md(&node.name), code(&node.identifier)).expect("string write");
    }
}

fn render_artifacts(output: &mut String, heading: &str, artifacts: &[Artifact], data: &ReportData) {
    if artifacts.is_empty() {
        return;
    }
    writeln!(output, "### {}\n", md(heading)).expect("string write");
    for artifact in artifacts {
        render_artifact(output, artifact, data, None);
    }
    output.push('\n');
}

fn render_artifact(
    output: &mut String,
    artifact: &Artifact,
    data: &ReportData,
    note: Option<&str>,
) {
    output.push_str("- ");
    if let Some(url) = artifact_url(artifact, data.meta.remote_url.as_deref()) {
        write!(output, "[{}](<{url}>)", md(&artifact.title)).expect("string write");
    } else {
        output.push_str(&md(&artifact.title));
    }
    write!(
        output,
        " — `{:?}` / `{:?}`",
        artifact.identity.provider, artifact.identity.kind
    )
    .expect("string write");
    if let Some(note) = note {
        write!(output, " / `{}`", code(note)).expect("string write");
    }
    output.push('\n');
}

fn render_mermaid(output: &mut String, detail: &ReportDetail) {
    output.push_str("\n### Relationship graph\n\n```mermaid\ngraph LR\n");
    for relation in &detail.relations {
        let source = mermaid_id(&relation.source.stable_key);
        let target = mermaid_id(&relation.target.stable_key);
        writeln!(
            output,
            "    {source}[\"{}\"] -->|{:?}| {target}[\"{}\"]",
            mermaid_label(&relation.source.name),
            relation.kind,
            mermaid_label(&relation.target.name)
        )
        .expect("string write");
    }
    output.push_str("```\n");
}

fn mermaid_id(key: &str) -> String {
    format!("n{}", &blake3::hash(key.as_bytes()).to_hex()[..16])
}

fn mermaid_label(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;")
}

fn md(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('*', "\\*")
        .replace('_', "\\_")
}

fn code(value: &str) -> String {
    value.replace('`', "\\`")
}

fn md_body(value: &str) -> String {
    value
        .lines()
        .map(|line| {
            if line.starts_with('#') || line.starts_with('>') || line.starts_with("```") {
                format!("\\{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mermaid_ids_are_deterministic_and_identifier_safe() {
        let first = mermaid_id("feature:FEAT-1");
        assert_eq!(first, mermaid_id("feature:FEAT-1"));
        assert!(first.starts_with('n'));
        assert!(
            first
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        );
    }

    #[test]
    fn headings_in_business_text_cannot_change_page_structure() {
        assert_eq!(md_body("# injected\nnormal"), "\\# injected\nnormal");
    }
}
