use ctx_core::{
    artifact::{Artifact, ArtifactProvider},
    domain::{NodeKind, StableKey},
    graph::GraphNode,
    indexing::PlannedNodeAttributes,
};

pub(crate) fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Feature => "Feature",
        NodeKind::Requirement => "Requirement",
        NodeKind::Invariant => "Invariant",
        NodeKind::Decision => "Decision",
        NodeKind::DomainConcept => "Domain concept",
        NodeKind::ExternalSystem => "External system",
        NodeKind::File => "File",
        NodeKind::CodeSymbol => "Code symbol",
        NodeKind::Endpoint => "Endpoint",
        NodeKind::ApiEndpoint => "API endpoint",
        NodeKind::DbEntity => "Database entity",
        NodeKind::Event => "Event",
    }
}

pub(crate) fn kind_slug(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Feature => "feature",
        NodeKind::Requirement => "requirement",
        NodeKind::Invariant => "invariant",
        NodeKind::Decision => "decision",
        NodeKind::DomainConcept => "domain-concept",
        NodeKind::ExternalSystem => "external-system",
        NodeKind::File => "file",
        NodeKind::CodeSymbol => "code-symbol",
        NodeKind::Endpoint => "endpoint",
        NodeKind::ApiEndpoint => "api-endpoint",
        NodeKind::DbEntity => "db-entity",
        NodeKind::Event => "event",
    }
}

pub(crate) fn entity_filename(key: &StableKey, extension: &str) -> String {
    format!(
        "{}.{}",
        blake3::hash(key.as_str().as_bytes()).to_hex(),
        extension
    )
}

pub(crate) fn external_url(value: &str) -> Option<&str> {
    (value.starts_with("https://") || value.starts_with("http://")).then_some(value)
}

pub(crate) fn code_url(remote: Option<&str>, commit: &str, node: &GraphNode) -> Option<String> {
    let PlannedNodeAttributes::Symbol {
        file_path, range, ..
    } = &node.attributes
    else {
        return None;
    };
    let remote = normalized_remote(remote?)?;
    let encoded_path = file_path
        .split('/')
        .map(url_segment)
        .collect::<Vec<_>>()
        .join("/");
    let separator = if remote.contains("gitlab") {
        "/-/blob/"
    } else {
        "/blob/"
    };
    Some(format!(
        "{remote}{separator}{commit}/{encoded_path}#L{}-L{}",
        range.start_line, range.end_line
    ))
}

pub(crate) fn artifact_url(artifact: &Artifact, remote: Option<&str>) -> Option<String> {
    if let Some(url) = external_url(artifact.source_locator.as_str()) {
        return Some(url.to_owned());
    }
    if artifact.identity.provider == ArtifactProvider::Git {
        return normalized_remote(remote?).map(|base| {
            format!(
                "{base}/commit/{}",
                url_segment(&artifact.identity.external_id)
            )
        });
    }
    None
}

fn normalized_remote(remote: &str) -> Option<String> {
    let trimmed = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    if let Some(path) = trimmed.strip_prefix("git@") {
        let (host, repository) = path.split_once(':')?;
        return Some(format!("https://{host}/{repository}"));
    }
    if let Some(path) = trimmed.strip_prefix("ssh://git@") {
        let (host, repository) = path.split_once('/')?;
        return Some(format!("https://{host}/{repository}"));
    }
    external_url(trimmed).map(str::to_owned)
}

fn url_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(encoded, "%{byte:02X}").expect("writing to a string cannot fail");
        }
    }
    encoded
}

pub(crate) fn node_body(node: &GraphNode) -> Option<&str> {
    match &node.attributes {
        PlannedNodeAttributes::Business { body, .. } => Some(body),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_remotes_become_browser_urls() {
        assert_eq!(
            normalized_remote("git@gitlab.example.com:team/project.git"),
            Some("https://gitlab.example.com/team/project".to_owned())
        );
    }

    #[test]
    fn entity_paths_do_not_expose_stable_keys() {
        let key = StableKey::new("symbol:rust:crate.module.fn:Function").expect("key");
        let name = entity_filename(&key, "html");
        assert_eq!(name.len(), 69);
        assert!(!name.contains("symbol"));
    }
}
