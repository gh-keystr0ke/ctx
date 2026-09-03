use std::{collections::BTreeMap, fmt::Write as _, path::PathBuf};

use ctx_app::report::{
    ReportCatalog, ReportData, ReportDetail, ReportDirectory, ReportFile, ReportRelation,
    ReportSymbol,
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

const STYLE: &str = include_str!("assets/style.css");
const APP: &str = include_str!("assets/app.js");

#[derive(Clone, Copy, Debug, Default)]
pub struct HtmlRenderer;

impl ReportRenderer for HtmlRenderer {
    fn render(&self, data: &ReportData) -> Result<RenderedReport, RenderError> {
        let mut report = RenderedReport::new(ReportFormat::Html, &data.meta.source_commit);
        report.insert("index.html", render_index(data)?);
        report.insert("tree.html", render_tree(data)?);
        report.insert(
            "search-index.json",
            format!("{}\n", serde_json::to_string(&data.search_index)?),
        );
        report.insert("assets/style.css", STYLE.to_owned());
        report.insert("assets/app.js", APP.to_owned());
        for catalog in &data.catalogs {
            report.insert(
                PathBuf::from("catalog").join(format!("{}.html", kind_slug(catalog.kind))),
                render_catalog(catalog),
            );
        }
        for (key, detail) in &data.details {
            report.insert(
                PathBuf::from("entity").join(entity_filename(key, "html")),
                render_detail(data, key, detail),
            );
        }
        Ok(report)
    }
}

fn render_index(data: &ReportData) -> Result<String, RenderError> {
    let mut body = String::new();
    write!(
        body,
        "<section class=\"hero\"><p class=\"eyebrow\">Repository at commit</p><h1>Context dashboard</h1><code>{}</code><p class=\"health health--{}\">{:?}</p></section>",
        html(&data.meta.source_commit),
        html(&format!("{:?}", data.meta.health).to_lowercase()),
        data.meta.health
    )
    .expect("string write");
    write!(
        body,
        "<section class=\"metrics\"><article><strong>{}</strong><span>files</span></article><article><strong>{}</strong><span>symbols</span></article><article><strong>{}</strong><span>active claims</span></article><article><strong>{}</strong><span>stale claims</span></article></section>",
        data.meta.knowledge.files,
        data.meta.knowledge.symbols,
        data.meta.knowledge.active_edges,
        data.meta.knowledge.stale_semantic_edges
    )
    .expect("string write");
    body.push_str("<section><div class=\"section-heading\"><h2>Catalogs</h2><a href=\"tree.html\">Browse source tree →</a></div><div class=\"cards\">");
    for catalog in &data.catalogs {
        write!(
            body,
            "<a class=\"card kind--{}\" href=\"catalog/{}.html\"><span>{}</span><strong>{}</strong></a>",
            kind_slug(catalog.kind),
            kind_slug(catalog.kind),
            html(kind_name(catalog.kind)),
            catalog.nodes.len()
        )
        .expect("string write");
    }
    body.push_str("</div></section><section><div class=\"section-heading\"><h2>Product graph</h2><span>Drag to pan · scroll to zoom · select a node to open</span></div><div id=\"graph-filters\" class=\"graph-filters\">");
    for catalog in &data.catalogs {
        write!(
            body,
            "<button type=\"button\" data-kind=\"{}\" aria-pressed=\"true\">{}</button>",
            kind_slug(catalog.kind).replace('-', "_"),
            html(kind_name(catalog.kind))
        )
        .expect("string write");
    }
    body.push_str("</div><canvas id=\"graph-canvas\" tabindex=\"0\" aria-label=\"Interactive product relationship graph\"></canvas>");
    let graph_links = data
        .dashboard_graph
        .nodes
        .iter()
        .filter_map(|node| {
            let key = StableKey::new(&node.stable_key).ok()?;
            data.details.contains_key(&key).then(|| {
                (
                    node.stable_key.clone(),
                    format!("entity/{}", entity_filename(&key, "html")),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    write!(
        body,
        "<script id=\"graph-data\" type=\"application/json\">{}</script><script id=\"graph-links\" type=\"application/json\">{}</script></section>",
        json_for_script(&data.dashboard_graph)?,
        json_for_script(&graph_links)?
    )
    .expect("string write");
    if !data.meta.notices.is_empty() {
        body.push_str("<section><h2>Attention</h2><ul class=\"notice-list\">");
        for notice in &data.meta.notices {
            write!(body, "<li>{}</li>", html(notice)).expect("string write");
        }
        body.push_str("</ul></section>");
    }
    Ok(page("Context dashboard", "", &body))
}

fn render_catalog(catalog: &ReportCatalog) -> String {
    let mut body = String::new();
    write!(
        body,
        "<section class=\"hero compact kind--{}\"><p class=\"eyebrow\">Catalog</p><h1>{}</h1><p>{} entities, ordered by stable identity.</p></section><section class=\"catalog-list\">",
        kind_slug(catalog.kind),
        html(kind_name(catalog.kind)),
        catalog.nodes.len()
    )
    .expect("string write");
    for node in &catalog.nodes {
        render_node_row(&mut body, node, "../entity/");
    }
    if catalog.nodes.is_empty() {
        body.push_str("<p class=\"empty\">No entities of this kind.</p>");
    }
    body.push_str("</section>");
    page(kind_name(catalog.kind), "../", &body)
}

fn render_tree(data: &ReportData) -> Result<String, RenderError> {
    let mut body = String::new();
    body.push_str("<section class=\"hero compact\"><p class=\"eyebrow\">Implementation</p><h1>Source tree</h1><p>Folders, files, and structurally indexed symbols.</p></section><section class=\"search-panel\"><label for=\"tree-search\">Search the complete report</label><input id=\"tree-search\" type=\"search\" placeholder=\"Substring of name, identifier, kind, or path\" autocomplete=\"off\"><ol id=\"search-results\" class=\"search-results\"></ol></section><section class=\"tree\">");
    for directory in &data.tree.directories {
        render_directory(&mut body, directory);
    }
    for file in &data.tree.files {
        render_file(&mut body, file);
    }
    if !data.tree.unattached_symbols.is_empty() {
        body.push_str("<details><summary>Unattached symbols</summary><ul>");
        for symbol in &data.tree.unattached_symbols {
            render_symbol(&mut body, symbol);
        }
        body.push_str("</ul></details>");
    }
    body.push_str("</section>");
    write!(
        body,
        "<script id=\"search-data\" type=\"application/json\">{}</script><script id=\"search-links\" type=\"application/json\">{}</script>",
        json_for_script(&data.search_index)?,
        json_for_script(
            &data
                .details
                .keys()
                .map(|key| {
                    (
                        key.to_string(),
                        format!("entity/{}", entity_filename(key, "html")),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        )?
    )
    .expect("string write");
    Ok(page("Source tree", "", &body))
}

fn render_directory(output: &mut String, directory: &ReportDirectory) {
    write!(
        output,
        "<details open><summary><span class=\"tree-icon\">▾</span>{}</summary><div class=\"tree-branch\">",
        html(&directory.name)
    )
    .expect("string write");
    for child in &directory.directories {
        render_directory(output, child);
    }
    for file in &directory.files {
        render_file(output, file);
    }
    output.push_str("</div></details>");
}

fn render_file(output: &mut String, file: &ReportFile) {
    write!(
        output,
        "<details><summary><span class=\"tree-icon\">◇</span>{}</summary><ul>",
        html(&file.node.identifier)
    )
    .expect("string write");
    for symbol in &file.symbols {
        render_symbol(output, symbol);
    }
    output.push_str("</ul></details>");
}

fn render_symbol(output: &mut String, symbol: &ReportSymbol) {
    write!(
        output,
        "<li><a href=\"entity/{}\">{}</a><span class=\"tag\">{:?}</span>",
        entity_filename(
            &StableKey::new(&symbol.node.stable_key).expect("stored stable key is valid"),
            "html"
        ),
        html(&symbol.node.name),
        symbol.symbol_kind
    )
    .expect("string write");
    if !symbol.children.is_empty() {
        output.push_str("<ul>");
        for child in &symbol.children {
            render_symbol(output, child);
        }
        output.push_str("</ul>");
    }
    output.push_str("</li>");
}

fn render_detail(data: &ReportData, key: &StableKey, detail: &ReportDetail) -> String {
    let mut body = String::new();
    write!(
        body,
        "<section class=\"hero compact kind--{}\"><p class=\"eyebrow\">{}</p><h1>{}</h1><code>{}</code></section>",
        kind_slug(detail.node.kind),
        html(kind_name(detail.node.kind)),
        html(&detail.node.name),
        html(detail.node.identifier())
    )
    .expect("string write");
    if let Some(content) = node_body(&detail.node) {
        write!(
            body,
            "<section><h2>Definition</h2><div class=\"prose\">{}</div></section>",
            paragraphs(content)
        )
        .expect("string write");
    }
    render_implementation(&mut body, data, detail);
    if let Some(provenance) = &detail.knowledge_provenance {
        write!(
            body,
            "<section><h2>Knowledge provenance</h2><dl><dt>Producer</dt><dd>{}</dd><dt>Model</dt><dd>{}</dd><dt>Decision</dt><dd>{:?} by {} at {}</dd></dl>",
            html(&provenance.agent_producer),
            html(provenance.agent_model.as_deref().unwrap_or("not recorded")),
            provenance.decision_method,
            html(&provenance.decided_by),
            html(&provenance.decided_at)
        )
        .expect("string write");
        render_artifacts(
            &mut body,
            "Source artifacts",
            &detail.provenance_artifacts,
            data,
        );
        body.push_str("</section>");
    }
    if !detail.artifact_history.is_empty() {
        body.push_str("<section><h2>Artifact history</h2><ul class=\"artifact-list\">");
        for linked in &detail.artifact_history {
            render_artifact_item(
                &mut body,
                &linked.artifact,
                data,
                Some(&format!("{:?}", linked.kind)),
            );
        }
        body.push_str("</ul></section>");
    }
    body.push_str("<section><h2>Relationships</h2>");
    if detail.relations.is_empty() {
        body.push_str("<p class=\"empty\">No stored relationships.</p>");
    } else {
        body.push_str("<div class=\"relations\">");
        for relation in &detail.relations {
            render_relation(&mut body, data, key, relation);
        }
        body.push_str("</div>");
    }
    body.push_str("</section>");
    page(&detail.node.name, "../", &body)
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
    output.push_str("<section><h2>Implementation</h2><dl>");
    write!(
        output,
        "<dt>File</dt><dd>{}</dd><dt>Lines</dt><dd>{}–{}</dd>",
        html(file_path),
        range.start_line,
        range.end_line
    )
    .expect("string write");
    if let Some(signature) = signature {
        write!(
            output,
            "<dt>Signature</dt><dd><code>{}</code></dd>",
            html(signature)
        )
        .expect("string write");
    }
    if let Some(url) = code_url(
        data.meta.remote_url.as_deref(),
        &data.meta.source_commit,
        &detail.node,
    ) {
        write!(
            output,
            "<dt>Source</dt><dd><a href=\"{}\">Open code at commit</a></dd>",
            attr(&url)
        )
        .expect("string write");
    }
    output.push_str("</dl></section>");
}

fn render_relation(
    output: &mut String,
    data: &ReportData,
    subject: &StableKey,
    relation: &ReportRelation,
) {
    let outgoing = relation.source.stable_key == subject.as_str();
    let other = if outgoing {
        &relation.target
    } else {
        &relation.source
    };
    write!(
        output,
        "<article class=\"relation\"><p class=\"eyebrow\">{} · {:?} · {:?}</p><h3>{} <span>{:?}</span> ",
        if outgoing { "outgoing" } else { "incoming" },
        relation.claim_class,
        relation.status,
        html(&relation.source.name),
        relation.kind
    )
    .expect("string write");
    render_summary_link(output, data, other);
    write!(
        output,
        "</h3><p>Confidence {:.2} · valid from <code>{}</code></p>",
        relation.confidence,
        html(&relation.valid_from)
    )
    .expect("string write");
    if let Some(reason) = &relation.stale_reason {
        write!(output, "<p class=\"warning\">{}</p>", html(reason)).expect("string write");
    }
    if !relation.evidence.is_empty() {
        output.push_str("<ul class=\"evidence\">");
        for evidence in &relation.evidence {
            output.push_str("<li>");
            if let Some(url) = external_url(&evidence.source_uri) {
                write!(
                    output,
                    "<a href=\"{}\">{}</a>",
                    attr(url),
                    html(&evidence.locator)
                )
                .expect("string write");
            } else {
                write!(output, "{}", html(&evidence.locator)).expect("string write");
            }
            write!(
                output,
                " <span>{:?}, strength {:.2}</span></li>",
                evidence.source_kind,
                evidence.strength.get()
            )
            .expect("string write");
        }
        output.push_str("</ul>");
    }
    output.push_str("</article>");
}

fn render_summary_link(output: &mut String, data: &ReportData, node: &NodeSummary) {
    let key = StableKey::new(&node.stable_key).expect("stored stable key is valid");
    if data.details.contains_key(&key) {
        write!(
            output,
            "<a href=\"{}\">{}</a>",
            entity_filename(&key, "html"),
            html(&node.name)
        )
        .expect("string write");
    } else if let Some(url) = external_url(&node.identifier) {
        write!(output, "<a href=\"{}\">{}</a>", attr(url), html(&node.name)).expect("string write");
    } else {
        output.push_str(&html(&node.name));
    }
}

fn render_artifacts(output: &mut String, heading: &str, artifacts: &[Artifact], data: &ReportData) {
    if artifacts.is_empty() {
        return;
    }
    write!(
        output,
        "<h3>{}</h3><ul class=\"artifact-list\">",
        html(heading)
    )
    .expect("string write");
    for artifact in artifacts {
        render_artifact_item(output, artifact, data, None);
    }
    output.push_str("</ul>");
}

fn render_artifact_item(
    output: &mut String,
    artifact: &Artifact,
    data: &ReportData,
    note: Option<&str>,
) {
    output.push_str("<li>");
    if let Some(url) = artifact_url(artifact, data.meta.remote_url.as_deref()) {
        write!(
            output,
            "<a href=\"{}\">{}</a>",
            attr(&url),
            html(&artifact.title)
        )
        .expect("string write");
    } else {
        output.push_str(&html(&artifact.title));
    }
    write!(
        output,
        " <span>{:?} {:?}</span>",
        artifact.identity.provider, artifact.identity.kind
    )
    .expect("string write");
    if let Some(note) = note {
        write!(output, " <span>{}</span>", html(note)).expect("string write");
    }
    output.push_str("</li>");
}

fn render_node_row(output: &mut String, node: &NodeSummary, prefix: &str) {
    let key = StableKey::new(&node.stable_key).expect("stored stable key is valid");
    write!(
        output,
        "<article><a href=\"{}{}\"><h2>{}</h2><code>{}</code></a></article>",
        prefix,
        entity_filename(&key, "html"),
        html(&node.name),
        html(&node.identifier)
    )
    .expect("string write");
}

fn page(title: &str, root: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{} · ctx report</title><link rel=\"stylesheet\" href=\"{}assets/style.css\"></head><body><header><a class=\"brand\" href=\"{}index.html\">ctx / report</a><nav><a href=\"{}index.html\">Dashboard</a><a href=\"{}tree.html\">Source tree</a></nav></header><main>{}</main><footer>Generated from the repository's ctx index.</footer><script src=\"{}assets/app.js\"></script></body></html>\n",
        html(title),
        root,
        root,
        root,
        root,
        body,
        root
    )
}

fn paragraphs(value: &str) -> String {
    value
        .split("\n\n")
        .fold(String::new(), |mut output, paragraph| {
            write!(output, "<p>{}</p>", html(paragraph).replace('\n', "<br>"))
                .expect("string write");
            output
        })
}

fn html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn attr(value: &str) -> String {
    html(value)
}

fn json_for_script(value: &impl serde::Serialize) -> Result<String, serde_json::Error> {
    serde_json::to_string(value).map(|json| {
        json.replace('&', "\\u0026")
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029")
    })
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::*;

    #[test]
    fn embedded_json_cannot_close_its_script_element() {
        #[derive(Serialize)]
        struct Payload<'a> {
            value: &'a str,
        }
        let rendered = json_for_script(&Payload {
            value: "</script><b>&",
        })
        .expect("json");
        assert!(!rendered.contains('<'));
        assert!(!rendered.contains('>'));
        assert!(!rendered.contains('&'));
    }
}
