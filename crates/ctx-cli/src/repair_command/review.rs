use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead},
    path::Path,
};

use ctx_adapters::{git::GitRepo, worktree::WorktreeSnapshot};
use serde_json::json;
use yaml_serde::{Mapping, Value};

use super::{
    apply::{Edit, write_edits},
    queue::{Action, Proposal, Queue, Status},
};
use crate::{Cli, CliError};

pub(super) fn review(cli: &Cli, git: &GitRepo, auto: bool) -> Result<(), CliError> {
    if auto && !super::config::agent_auto(git)? {
        return Err(io::Error::other(
            "commit [repair].agent_auto = true before using repair review --auto",
        )
        .into());
    }
    let queue = Queue::new(git.context_root());
    let mut documents = BTreeMap::<String, Vec<Proposal>>::new();
    for proposal in queue.pending()? {
        documents
            .entry(proposal.document.clone())
            .or_default()
            .push(proposal);
    }
    let mut decisions = Vec::new();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let snapshot = WorktreeSnapshot::read(git)?;
    for (document, mut proposals) in documents {
        if auto {
            for proposal in proposals.iter_mut().filter(|proposal| {
                matches!(proposal.class.as_str(), "R3" | "R8") && proposal.producer.is_some()
            }) {
                apply(git, &snapshot, proposal, None, proposal.producer.as_deref())?;
                finish(&queue, proposal, Status::Accepted, "agent", &mut decisions)?;
            }
        } else {
            show_card(git, &document, &proposals)?;
            if !review_card(
                git,
                &snapshot,
                &queue,
                &mut proposals,
                &mut input,
                &mut decisions,
            )? {
                break;
            }
        }
    }
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"decisions": decisions}))?
        );
    } else {
        println!("Recorded {} repair decision(s).", decisions.len());
    }
    Ok(())
}

fn show_card(git: &GitRepo, document: &str, proposals: &[Proposal]) -> io::Result<()> {
    eprintln!("{document}");
    for (index, proposal) in proposals.iter().enumerate() {
        eprintln!(
            "  {}. {} {:?}\n     {}",
            index + 1,
            proposal.class,
            proposal.action,
            proposal.reason
        );
        if let Action::Merge {
            superseded,
            superseded_path,
            ..
        } = &proposal.action
        {
            let retained = read_file(git, &proposal.path, &proposal.document_hash)?;
            let removed = read_file(
                git,
                superseded_path,
                match &proposal.action {
                    Action::Merge {
                        superseded_hash, ..
                    } => superseded_hash,
                    _ => unreachable!(),
                },
            )?;
            eprintln!("     {} | {superseded}", proposal.document);
            show_columns(body(&retained.yaml), body(&removed.yaml));
        }
    }
    eprintln!(
        "a <number> [wording] | e <number> <wording or symbol> | r <number> | d <number> | Enter next | q quit"
    );
    Ok(())
}

fn show_columns(left: &str, right: &str) {
    let mut left = left.lines();
    let mut right = right.lines();
    loop {
        match (left.next(), right.next()) {
            (None, None) => break,
            (left, right) => eprintln!(
                "     {:<52} | {}",
                left.unwrap_or_default(),
                right.unwrap_or_default()
            ),
        }
    }
}

fn body(yaml: &Value) -> &str {
    yaml.get("statement")
        .or_else(|| yaml.get("decision"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn review_card(
    git: &GitRepo,
    snapshot: &WorktreeSnapshot,
    queue: &Queue,
    proposals: &mut [Proposal],
    input: &mut impl BufRead,
    decisions: &mut Vec<serde_json::Value>,
) -> Result<bool, CliError> {
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 || line.trim() == "q" {
            return Ok(false);
        }
        if line.trim().is_empty() {
            return Ok(true);
        }
        let mut fields = line.trim().splitn(3, ' ');
        let command = fields.next().unwrap_or_default();
        let index = fields
            .next()
            .and_then(|field| field.parse::<usize>().ok())
            .and_then(|number| number.checked_sub(1));
        let change = fields.next().map(str::trim).filter(|text| !text.is_empty());
        let Some(proposal) = index
            .and_then(|index| proposals.get_mut(index))
            .filter(|proposal| proposal.status == Status::Pending)
        else {
            eprintln!("Choose a pending proposal number.");
            continue;
        };
        let status = match command {
            "a" | "e" => {
                let mut edited = proposal.clone();
                if command == "e"
                    && let Some(target) = change
                {
                    match &mut edited.action {
                        Action::Replace { symbol, .. } | Action::Add { symbol } => {
                            target.clone_into(symbol);
                        }
                        _ => {}
                    }
                }
                if let Err(error) = apply(git, snapshot, &edited, change, None) {
                    eprintln!("{error}");
                    continue;
                }
                proposal.action = edited.action;
                Status::Accepted
            }
            "r" => Status::Rejected,
            "d" => Status::Deferred,
            _ => {
                eprintln!("Choose an available action.");
                continue;
            }
        };
        finish(queue, proposal, status, "human", decisions)?;
        if status == Status::Accepted {
            return Ok(true);
        }
    }
}

fn finish(
    queue: &Queue,
    proposal: &mut Proposal,
    status: Status,
    method: &str,
    decisions: &mut Vec<serde_json::Value>,
) -> Result<(), CliError> {
    proposal.status = status;
    proposal.decided_by = Some(method.to_owned());
    queue.update(proposal)?;
    decisions.push(json!({"id": proposal.id, "document": proposal.document, "status": status, "method": method}));
    Ok(())
}

fn apply(
    git: &GitRepo,
    snapshot: &WorktreeSnapshot,
    proposal: &Proposal,
    text: Option<&str>,
    producer: Option<&str>,
) -> Result<(), CliError> {
    let mut file = read_file(git, &proposal.path, &proposal.document_hash)?;
    let map = file
        .yaml
        .as_mapping_mut()
        .ok_or_else(|| io::Error::other("context must be a YAML mapping"))?;
    let mut secondary = None;
    match &proposal.action {
        Action::Replace {
            locator,
            original,
            symbol,
        } => {
            validate_symbol(snapshot, symbol)?;
            let (list, index) = mapping_at(map, locator, original)?;
            list[index] = symbol_link(symbol, producer);
        }
        Action::Remove { locator, original } => {
            let (list, index) = mapping_at(map, locator, original)?;
            list.remove(index);
        }
        Action::Deprecate => {
            map.insert(key("status"), key("deprecated"));
        }
        Action::Amend { .. } => set_wording(map, text)?,
        Action::Merge {
            superseded_path,
            superseded_hash,
            ..
        } => {
            set_wording(map, text)?;
            let mut superseded = read_file(git, superseded_path, superseded_hash)?;
            let other = superseded
                .yaml
                .as_mapping_mut()
                .ok_or_else(|| io::Error::other("context must be a YAML mapping"))?;
            merge_links(map, other)?;
            other.insert(key("status"), key("superseded"));
            other.insert(key("superseded_by"), key(&proposal.document));
            secondary = Some(superseded.into_edit()?);
        }
        Action::Add { symbol } => {
            validate_symbol(snapshot, symbol)?;
            append_links(map, "implementation", &[symbol_link(symbol, producer)])?;
        }
    }
    let mut edits = vec![file.into_edit()?];
    edits.extend(secondary);
    write_edits(&edits)?;
    Ok(())
}

fn validate_symbol(snapshot: &WorktreeSnapshot, symbol: &str) -> io::Result<()> {
    if !snapshot.contains_unique_symbol(symbol) {
        return Err(io::Error::other(format!(
            "symbol {symbol} is missing or ambiguous"
        )));
    }
    Ok(())
}

fn set_wording(map: &mut Mapping, text: Option<&str>) -> io::Result<()> {
    let text = text
        .ok_or_else(|| io::Error::other("provide the final wording after the proposal number"))?;
    let field = if map.contains_key("decision") {
        "decision"
    } else {
        "statement"
    };
    if !map.contains_key(field) {
        return Err(io::Error::other(
            "proposal document has no editable wording",
        ));
    }
    map.insert(key(field), key(text));
    Ok(())
}

fn mapping_at<'a>(
    map: &'a mut Mapping,
    locator: &str,
    original: &str,
) -> io::Result<(&'a mut Vec<Value>, usize)> {
    let (field, index) = locator
        .split_once('[')
        .ok_or_else(|| io::Error::other("invalid link locator"))?;
    let index: usize = index
        .strip_suffix(']')
        .ok_or_else(|| io::Error::other("invalid link locator"))?
        .parse()
        .map_err(io::Error::other)?;
    if !matches!(field, "implementation" | "tests") {
        return Err(io::Error::other("invalid link field"));
    }
    let links = map
        .get_mut(field)
        .and_then(Value::as_sequence_mut)
        .ok_or_else(|| io::Error::other("link list is missing"))?;
    if links.get(index).and_then(symbol_name) != Some(original) {
        return Err(io::Error::other("link changed since the proposal"));
    }
    Ok((links, index))
}

fn merge_links(map: &mut Mapping, other: &Mapping) -> io::Result<()> {
    for field in ["implementation", "tests", "anchors"] {
        if let Some(links) = other.get(field).and_then(Value::as_sequence) {
            append_links(map, field, links)?;
        }
    }
    Ok(())
}

fn append_links(map: &mut Mapping, field: &str, additions: &[Value]) -> io::Result<()> {
    let list = map
        .entry(key(field))
        .or_insert_with(|| Value::Sequence(Vec::new()))
        .as_sequence_mut()
        .ok_or_else(|| io::Error::other("link list is not a YAML sequence"))?;
    for link in additions {
        if !list.iter().any(|existing| {
            existing == link
                || (symbol_name(link).is_some() && symbol_name(existing) == symbol_name(link))
        }) {
            list.push(link.clone());
        }
    }
    Ok(())
}

fn symbol_name(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("symbol").and_then(Value::as_str))
}

fn symbol_link(symbol: &str, producer: Option<&str>) -> Value {
    let mut map = Mapping::new();
    map.insert(key("symbol"), key(symbol));
    if let Some(agent) = producer {
        map.insert(key("repair_agent"), key(agent));
    }
    Value::Mapping(map)
}

fn key(text: &str) -> Value {
    Value::String(text.to_owned())
}

struct ContextFile {
    path: std::path::PathBuf,
    original: String,
    yaml: Value,
}

impl ContextFile {
    fn into_edit(self) -> io::Result<Edit> {
        let patched = yaml_serde::to_string(&self.yaml).map_err(io::Error::other)?;
        Ok(Edit {
            path: self.path,
            original: self.original,
            patched,
        })
    }
}

fn read_file(git: &GitRepo, relative: &str, hash: &str) -> io::Result<ContextFile> {
    let root = fs::canonicalize(git.context_root().join(".context"))?;
    let path = fs::canonicalize(git.context_root().join(relative))?;
    if !path.starts_with(root)
        || !path
            .extension()
            .is_some_and(|extension| extension == "yaml" || extension == "yml")
        || !Path::new(relative).starts_with(".context")
    {
        return Err(io::Error::other(
            "repair proposals require a YAML context document",
        ));
    }
    let original = fs::read_to_string(&path)?;
    if blake3::hash(original.as_bytes()).to_hex().as_str() != hash {
        return Err(io::Error::other(format!(
            "{} changed since the proposal",
            path.display()
        )));
    }
    let yaml = yaml_serde::from_str(&original).map_err(io::Error::other)?;
    Ok(ContextFile {
        path,
        original,
        yaml,
    })
}
