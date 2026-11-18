use std::{collections::BTreeMap, io, time::Duration};

use ctx_adapters::{
    agent_contract, business_context::YamlBusinessContextReader, git::GitRepo, sqlite::SqliteStore,
    worktree::WorktreeSnapshot,
};
use ctx_app::{
    ports::{BusinessContextReader, GitRepository, GraphStore, ReviewRepository},
    status::index_freshness,
    verification::StaleClaimOutcome,
};
use ctx_core::{
    business::BusinessDocument,
    graph::GraphSnapshot,
    indexing::PlannedNodeAttributes,
    repair::{IndexedDrift, indexed_drift},
    verification::{ArtifactEvidenceContext, semantic_candidates},
};
use serde::Deserialize;
use serde_json::json;

use super::{
    LinkRepair,
    config::Config,
    link_repairs,
    queue::{Action, Proposal, Queue, Status},
};
use crate::{Cli, CliError, agent_dispatch::ConfiguredAgent, database_path, verify_command};

pub(super) fn propose(
    cli: &Cli,
    git: &GitRepo,
    agent: Option<&str>,
    model: Option<&str>,
    author: &str,
) -> Result<(), CliError> {
    let config = Config::read(git)?;
    let snapshot = WorktreeSnapshot::read(git)?;
    let issues = snapshot.issues();
    let store = SqliteStore::open_preview(&database_path(git.root())?, git.context_root())?;
    let repository = git.descriptor()?;
    let freshness = index_freshness(git, &store, &repository.id)?;
    let head = freshness
        .commit
        .as_ref()
        .filter(|commit| *commit == &freshness.head)
        .ok_or_else(|| io::Error::other("index HEAD before proposing repairs"))?;
    let graph = store.load_graph(&repository.id)?;
    let drift = indexed_drift(&graph);
    let links = link_repairs(&snapshot, &issues, &store, &repository.id)?;
    let all_docs = YamlBusinessContextReader::new(git.context_root().to_path_buf()).read_all()?;
    let docs = all_docs.iter().map(|doc| (doc.id.as_str(), doc)).collect();
    let agent = agent
        .map(|name| {
            ConfiguredAgent::from_name(
                name,
                model.map(str::to_owned),
                cli.verbose > 0,
                cli.pace_agent(),
            )
            .map(|configured| {
                configured
                    .with_ledger(git.root())
                    .with_timeout(Duration::from_secs(30))
            })
            .map_err(CliError::UnsupportedAgent)
            .map(|configured| (name, configured))
        })
        .transpose()?;
    let mut proposed = Proposer {
        git,
        graph: &graph,
        docs,
        queue: Queue::new(git.context_root()),
        head: head.as_str(),
        agent: agent.as_ref().map(|(name, agent)| (*name, agent)),
        model,
        limit: config.max_proposals,
        items: Vec::new(),
    };
    proposed.duplicates(&drift)?;
    proposed.broken_links(&issues, &links)?;
    if let Some((name, _)) = &agent {
        proposed.unmapped(&drift)?;
        let verify_command::StaleReviewResults(_, _, results) =
            verify_command::verify_stale_results(cli, git, name, model.map(str::to_owned), author)?;
        proposed.rejected_stale(results)?;
    }
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"proposals": proposed.items}))?
        );
    } else {
        println!("Queued {} repair proposal(s).", proposed.items.len());
    }
    Ok(())
}

struct Proposer<'a> {
    git: &'a GitRepo,
    graph: &'a GraphSnapshot,
    docs: BTreeMap<&'a str, &'a BusinessDocument>,
    queue: Queue,
    head: &'a str,
    agent: Option<(&'a str, &'a ConfiguredAgent)>,
    model: Option<&'a str>,
    limit: usize,
    items: Vec<Proposal>,
}

impl Proposer<'_> {
    fn duplicates(&mut self, drift: &IndexedDrift) -> Result<(), CliError> {
        for pair in &drift.duplicate_documents {
            if self.full() {
                break;
            }
            let (Some(retained), Some(superseded)) = (
                self.docs.get(pair.retained.as_str()).copied(),
                self.docs.get(pair.superseded.as_str()).copied(),
            ) else {
                continue;
            };
            self.enqueue(
                retained,
                "R7",
                Action::Merge {
                    superseded: superseded.id.clone(),
                    superseded_path: superseded.source_uri.clone(),
                    superseded_hash: superseded.content_hash.clone(),
                },
                "Possible lexical duplicate. Combine links and supply the merged wording."
                    .to_owned(),
                vec![superseded.source_uri.clone()],
                None,
            )?;
        }
        Ok(())
    }

    fn broken_links(
        &mut self,
        issues: &[ctx_adapters::worktree::ContextIssue],
        links: &[LinkRepair<'_>],
    ) -> Result<(), CliError> {
        for link in links
            .iter()
            .filter(|link| matches!(link.class, "R3" | "R4"))
        {
            if self.full() {
                break;
            }
            let Some(doc) = self
                .docs
                .values()
                .find(|doc| doc.source_uri == link.path)
                .copied()
            else {
                continue;
            };
            let issue = issues
                .iter()
                .find(|issue| issue.path == link.path && issue.locator == link.locator);
            let mut choices = Vec::new();
            if let Some(issue) = issue {
                choices.extend(
                    issue
                        .suggestions
                        .iter()
                        .take(5)
                        .map(|symbol| Action::Replace {
                            locator: link.locator.to_owned(),
                            original: link.original.to_owned(),
                            symbol: symbol.clone(),
                        }),
                );
            }
            if link.class == "R4" {
                choices.push(Action::Remove {
                    locator: link.locator.to_owned(),
                    original: link.original.to_owned(),
                });
                choices.push(Action::Deprecate);
            }
            if let Some((action, reason, evidence)) =
                self.choose(doc, link.class, &choices, link.original)
            {
                self.enqueue(
                    doc,
                    link.class,
                    action,
                    reason,
                    evidence,
                    self.agent.map(|(name, _)| name),
                )?;
            } else if self.agent.is_none() && link.class == "R4" {
                self.enqueue(
                    doc,
                    link.class,
                    Action::Remove {
                        locator: link.locator.to_owned(),
                        original: link.original.to_owned(),
                    },
                    "The previous symbol identity is retired. Confirm removal or defer review."
                        .to_owned(),
                    link.commit.clone().into_iter().collect(),
                    None,
                )?;
            }
        }
        Ok(())
    }

    fn unmapped(&mut self, drift: &IndexedDrift) -> Result<(), CliError> {
        if self.full() || drift.unmapped_documents.is_empty() {
            return Ok(());
        }
        let candidates = semantic_candidates(self.graph, &ArtifactEvidenceContext::default());
        for id in &drift.unmapped_documents {
            if self.full() {
                break;
            }
            let Some(doc) = self.docs.get(id.as_str()).copied() else {
                continue;
            };
            let choices: Vec<_> = candidates
                .iter()
                .filter(|candidate| candidate.target_identifier == *id)
                .take(5)
                .map(|candidate| Action::Add {
                    symbol: candidate.source_identifier.clone(),
                })
                .collect();
            if let Some((action, reason, evidence)) =
                self.choose(doc, "R8", &choices, "missing implementation links")
            {
                self.enqueue(
                    doc,
                    "R8",
                    action,
                    reason,
                    evidence,
                    self.agent.map(|(name, _)| name),
                )?;
            }
        }
        Ok(())
    }

    fn rejected_stale(
        &mut self,
        results: Vec<ctx_app::verification::ReviewedStaleClaim>,
    ) -> Result<(), CliError> {
        for result in results {
            if self.full() {
                break;
            }
            let StaleClaimOutcome::SuggestedRemoval { reasoning } = result.outcome else {
                continue;
            };
            let (doc, symbol) = if let Some(doc) = self.docs.get(result.target.as_str()) {
                (*doc, result.source)
            } else if let Some(doc) = self.docs.get(result.source.as_str()) {
                (*doc, result.target)
            } else {
                continue;
            };
            let choices =
                ["intentional change", "possible regression"].map(|classification| Action::Amend {
                    related_symbol: symbol.clone(),
                    classification: classification.to_owned(),
                });
            if let Some((action, reason, evidence)) = self.choose(doc, "R6", &choices, &reasoning) {
                self.enqueue(
                    doc,
                    "R6",
                    action,
                    reason,
                    evidence,
                    self.agent.map(|(name, _)| name),
                )?;
            }
        }
        Ok(())
    }

    fn full(&self) -> bool {
        self.items.len() >= self.limit
    }

    fn choose(
        &self,
        doc: &BusinessDocument,
        class: &str,
        choices: &[Action],
        observed: &str,
    ) -> Option<(Action, String, Vec<String>)> {
        let (_, agent) = self.agent?;
        if choices.is_empty() {
            return None;
        }
        let evidence: Vec<_> = choices
            .iter()
            .filter_map(|action| match action {
                Action::Replace { symbol, .. }
                | Action::Add { symbol }
                | Action::Amend {
                    related_symbol: symbol,
                    ..
                } => self.source_context(symbol),
                _ => None,
            })
            .collect();
        let package = json!({"class": class, "document": doc.id, "wording": bounded(&doc.body),
            "observation": bounded(observed), "choices": choices, "evidence": evidence});
        match agent_contract::repair_choice::<_, Choice>(agent, &package) {
            Ok(decision) => Some((
                choices.get(decision.index?)?.clone(),
                bounded(&decision.reason),
                evidence,
            )),
            Err(error) => {
                eprintln!("Repair proposal for {} was unavailable: {error}", doc.id);
                None
            }
        }
    }

    fn source_context(&self, symbol: &str) -> Option<String> {
        let node = self
            .graph
            .resolve(symbol)
            .into_iter()
            .find(|node| node.identifier() == symbol)?;
        let PlannedNodeAttributes::Symbol {
            file_path, range, ..
        } = &node.attributes
        else {
            return None;
        };
        let source = self.git.source_at(self.head, file_path).ok().flatten()?;
        let snippet = source
            .lines()
            .skip(range.start_line.saturating_sub(1))
            .take(18)
            .collect::<Vec<_>>()
            .join("\n");
        let mut evidence = format!(
            "{symbol} {file_path}:{}\n{}",
            range.start_line,
            bounded(&snippet)
        );
        if let Ok(history) =
            self.git
                .decision_history(self.head, file_path, range.start_line, range.end_line)
            && let Some((commit, subject)) = history.into_iter().next()
        {
            use std::fmt::Write as _;
            let _ = write!(evidence, "\n{commit} {}", bounded(&subject));
        }
        Some(evidence)
    }

    fn enqueue(
        &mut self,
        doc: &BusinessDocument,
        class: &str,
        action: Action,
        reason: String,
        evidence: Vec<String>,
        producer: Option<&str>,
    ) -> Result<(), CliError> {
        let mut proposal = Proposal {
            id: String::new(),
            document: doc.id.clone(),
            path: doc.source_uri.clone(),
            document_hash: doc.content_hash.clone(),
            class: class.to_owned(),
            revision: self.head.to_owned(),
            producer: producer.map(str::to_owned),
            model: self.model.map(str::to_owned),
            reason,
            evidence,
            action,
            status: Status::Pending,
            decided_by: None,
        };
        if self.queue.enqueue(&mut proposal)? {
            self.items.push(proposal);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    index: Option<usize>,
    reason: String,
}

fn bounded(text: &str) -> String {
    text.chars().take(800).collect()
}
