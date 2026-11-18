use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use ctx_core::{
    domain::{ClaimStatus, RelationKind},
    graph::GraphSnapshot,
    indexing::PlannedNodeAttributes,
};
use regex::Regex;
use serde::Serialize;

use super::{
    Result, fetch,
    manifest::{Evidence, EvidenceKind, EvidenceSource, Point, Project},
    review,
};

#[derive(Serialize)]
pub(super) struct Package {
    pub code: String,
    pub location: String,
    pub history: Vec<String>,
    pub evidence: BTreeMap<String, Evidence>,
}

pub(super) struct Documents {
    files: Vec<Document>,
}

struct Document {
    kind: EvidenceKind,
    path: String,
    template: String,
    commit: String,
    text: String,
}

impl Documents {
    pub(super) fn read(cache: &Path, project: &Project) -> Result<Self> {
        let mut files = Vec::new();
        for source in &project.evidence_sources {
            let checkout = fetch::evidence_checkout(cache, project, source)?;
            let head = fetch::git(&checkout, &["rev-parse", "HEAD"], false)?;
            if head.trim() != source.commit {
                return Err(
                    std::io::Error::other("evidence checkout does not match its manifest").into(),
                );
            }
            load_documents(source, &checkout, &mut files)?;
        }
        Ok(Self { files })
    }

    fn matches(&self, identifiers: &BTreeSet<String>) -> Vec<Evidence> {
        let mut hits = Vec::new();
        for document in &self.files {
            for (line, text) in document.text.lines().enumerate() {
                let text = text.trim();
                if text.len() < 20 {
                    continue;
                }
                let lower = text.to_ascii_lowercase();
                let score = identifiers
                    .iter()
                    .filter(|identifier| lower.contains(identifier.as_str()))
                    .count();
                if score > 0 {
                    hits.push((score, document, line + 1, text));
                }
            }
        }
        hits.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| (&left.1.path, left.2).cmp(&(&right.1.path, right.2)))
        });
        hits.into_iter()
            .take(8)
            .map(|(_, document, line, text)| Evidence {
                kind: document.kind,
                url: document
                    .template
                    .replace("{commit}", &document.commit)
                    .replace("{path}", &document.path)
                    .replace("{line}", &line.to_string()),
                quote: Some(text.chars().take(420).collect()),
            })
            .collect()
    }
}

fn load_documents(
    source: &EvidenceSource,
    checkout: &Path,
    files: &mut Vec<Document>,
) -> Result<()> {
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend(source.paths.iter().map(String::as_str));
    let listed = fetch::git(checkout, &args, false)?;
    for file in listed.split_terminator('\0') {
        let path = checkout.join(file);
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "md" | "rst" | "mdx" | "txt"))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.len() > 200_000 {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            files.push(Document {
                kind: source.kind,
                path: file.to_owned(),
                template: source.url_template.clone().unwrap_or_else(|| {
                    format!("{}/blob/{{commit}}/{{path}}#L{{line}}", source.repository)
                }),
                commit: source.commit.clone(),
                text,
            });
        }
    }
    Ok(())
}

pub(super) fn package(
    project: &Project,
    checkout: &Path,
    graph: &GraphSnapshot,
    documents: &Documents,
    point: &Point,
) -> Result<Package> {
    let code = review::code_excerpt(checkout, &point.location)?;
    let (path, line) = point
        .location
        .rsplit_once(':')
        .ok_or_else(|| std::io::Error::other("invalid point location"))?;
    let range = format!("{line},{line}:{path}");
    let history = fetch::git(
        checkout,
        &[
            "log",
            "-4",
            "--no-patch",
            "--format=%H %B",
            "-L",
            &range,
            "HEAD",
        ],
        false,
    )
    .unwrap_or_default();
    let history: Vec<String> = history
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(20)
        .map(|line| line.chars().take(320).collect())
        .collect();
    let mut found = issue_links(project, &history)?;
    let symbol = point.anchor.rsplitn(3, '#').last().unwrap_or_default();
    let identifiers = identifiers(
        &code,
        graph
            .nodes
            .values()
            .find(|node| node.stable_key.as_str() == symbol)
            .map(|node| node.name.as_str()),
    );
    found.extend(documents.matches(&identifiers));
    found.extend(test_evidence(graph, project, symbol));
    let evidence = found
        .into_iter()
        .take(14)
        .enumerate()
        .map(|(index, item)| (format!("E{}", index + 1), item))
        .collect();
    Ok(Package {
        code,
        location: point.location.clone(),
        history,
        evidence,
    })
}

fn issue_links(project: &Project, history: &[String]) -> Result<Vec<Evidence>> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for pattern in &project.reference_patterns {
        let regex = Regex::new(&pattern.regex)?;
        for line in history {
            for captures in regex.captures_iter(line).take(3) {
                let mut url = pattern.url.clone();
                for index in 1..captures.len() {
                    if let Some(value) = captures.get(index) {
                        url = url.replace(&format!("{{{index}}}"), value.as_str());
                    }
                }
                if seen.insert(url.clone()) {
                    result.push(Evidence {
                        kind: EvidenceKind::Issue,
                        url,
                        quote: Some(line.clone()),
                    });
                }
            }
        }
    }
    result.truncate(5);
    Ok(result)
}

fn identifiers(code: &str, name: Option<&str>) -> BTreeSet<String> {
    let focused = code
        .lines()
        .find(|line| line.starts_with('>'))
        .unwrap_or(code);
    name.into_iter()
        .chain(focused.split(|character: char| !character.is_alphanumeric() && character != '_'))
        .filter(|term| term.len() >= 4)
        .filter(|term| {
            !matches!(
                *term,
                "return" | "else" | "then" | "true" | "false" | "error" | "const" | "None"
            )
        })
        .take(14)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn test_evidence(graph: &GraphSnapshot, project: &Project, symbol: &str) -> Vec<Evidence> {
    graph
        .edges
        .iter()
        .filter(|edge| {
            edge.status == ClaimStatus::Active
                && matches!(edge.kind, RelationKind::Calls | RelationKind::CoveredBy)
        })
        .filter_map(|edge| {
            let key = if edge.source.as_str() == symbol {
                &edge.target
            } else if edge.target.as_str() == symbol {
                &edge.source
            } else {
                return None;
            };
            let test = graph.nodes.get(key).filter(|node| node.is_test())?;
            let PlannedNodeAttributes::Symbol {
                file_path, range, ..
            } = &test.attributes
            else {
                return None;
            };
            Some(Evidence {
                kind: EvidenceKind::Test,
                url: format!(
                    "{}/blob/{}/{}#L{}",
                    project.repository, project.commit, file_path, range.start_line
                ),
                quote: Some(test.identifier().to_owned()),
            })
        })
        .take(4)
        .collect()
}
