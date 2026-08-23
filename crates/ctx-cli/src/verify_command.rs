use super::{
    CandidateOutcome, Cli, CliError, ConfiguredAgent, GitRepo, GitRepository, GraphStore,
    IsTerminal, KnowledgeVerificationService, NodeKind, Path, PlannedNodeAttributes,
    ReviewedCandidate, SqliteStore, StaleClaim, StaleClaimOutcome, Utc, VerificationDecision,
    VerificationError, VerificationService, Write, YamlBusinessContextReader, database_path, io,
    json, tab_title,
};

pub(super) fn verify(
    cli: &Cli,
    git: &GitRepo,
    accept: Option<&str>,
    reject: Option<&str>,
    author: &str,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let head = git.head()?;
    let now = Utc::now().to_rfc3339();
    let mut service = VerificationService::new(&mut store);
    if let Some((fingerprint, decision)) = accept
        .map(|value| (value, VerificationDecision::Accept))
        .or_else(|| reject.map(|value| (value, VerificationDecision::Reject)))
    {
        service.decide(&repository.id, &head, fingerprint, decision, author, &now)?;
        if cli.json {
            println!(
                "{}",
                json!({"ok": true, "fingerprint": fingerprint, "decision": decision})
            );
        } else {
            println!("Recorded {decision:?} for {fingerprint}");
        }
        return Ok(());
    }
    let candidates = service.candidates(&repository.id)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&candidates)?);
        return Ok(());
    }
    if candidates.is_empty() {
        println!("No high-confidence semantic candidates.");
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        print_candidates(&candidates);
        return Ok(());
    }
    for candidate in candidates {
        println!();
        println!(
            "Possible relation: {} {:?} {}",
            candidate.source_identifier, candidate.relation, candidate.target_identifier
        );
        println!("Confidence score: {:.2}", candidate.score.total);
        for evidence in &candidate.evidence {
            println!("  - {evidence}");
        }
        loop {
            print!("[y] accept  [n] reject  [s] skip  [e] explain: ");
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            match answer.trim().to_ascii_lowercase().as_str() {
                "y" => {
                    service.decide(
                        &repository.id,
                        &head,
                        &candidate.fingerprint,
                        VerificationDecision::Accept,
                        author,
                        &now,
                    )?;
                    break;
                }
                "n" => {
                    service.decide(
                        &repository.id,
                        &head,
                        &candidate.fingerprint,
                        VerificationDecision::Reject,
                        author,
                        &now,
                    )?;
                    break;
                }
                "s" => break,
                "e" => println!("Score breakdown: {:#?}", candidate.score),
                _ => println!("Please enter y, n, s, or e."),
            }
        }
    }
    Ok(())
}

pub(super) fn verify_knowledge(
    cli: &Cli,
    git: &GitRepo,
    accept: Option<&str>,
    reject: Option<&str>,
    id: Option<&str>,
    author: &str,
    force: bool,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let now = Utc::now().to_rfc3339();
    let writer = YamlBusinessContextReader::new(git.context_root().to_path_buf());
    let mut service = KnowledgeVerificationService::new(&mut store, &writer);

    if let Some(fingerprint) = accept {
        let document_id = id.ok_or(CliError::MissingKnowledgeId)?;
        let path = service.accept(
            &repository.id,
            fingerprint,
            document_id,
            author,
            &now,
            force,
            ctx_core::knowledge::DecisionMethod::Human,
        )?;
        if cli.json {
            println!(
                "{}",
                json!({"ok": true, "fingerprint": fingerprint, "id": document_id, "path": path})
            );
        } else {
            println!("Accepted {fingerprint} as {document_id} -> {path}");
        }
        return Ok(());
    }
    if let Some(fingerprint) = reject {
        service.reject(
            &repository.id,
            fingerprint,
            author,
            &now,
            ctx_core::knowledge::DecisionMethod::Human,
        )?;
        if cli.json {
            println!("{}", json!({"ok": true, "fingerprint": fingerprint}));
        } else {
            println!("Rejected {fingerprint}");
        }
        return Ok(());
    }

    let candidates = service.candidates(&repository.id)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&candidates)?);
        return Ok(());
    }
    if candidates.is_empty() {
        println!("No pending AI-derived knowledge candidates.");
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        print_knowledge_candidates(&candidates);
        return Ok(());
    }
    for candidate in candidates {
        review_knowledge_candidate_interactively(
            &mut service,
            &repository.id,
            &candidate,
            author,
            &now,
            force,
        )?;
    }
    Ok(())
}

/// Collapses `statement` to one printable line for `--auto`'s per-candidate
/// result output, truncated so one long candidate can't push a cluster's
/// whole result block off-screen.
fn summarize_statement(statement: &str) -> String {
    const MAX_CHARS: usize = 100;
    let collapsed = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_CHARS {
        return format!("\"{collapsed}\"");
    }
    let truncated: String = collapsed.chars().take(MAX_CHARS).collect();
    format!("\"{truncated}…\"")
}

pub(super) fn verify_knowledge_auto(
    cli: &Cli,
    git: &GitRepo,
    agent: &str,
    model: Option<String>,
    id_prefix: &str,
    author: &str,
    force: bool,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let now = Utc::now().to_rfc3339();
    let writer = YamlBusinessContextReader::new(git.context_root().to_path_buf());

    // Same reasoning as `enrich`'s own progress output: a real review call
    // per cluster can take tens of seconds, and with several clusters,
    // silence the whole time looks indistinguishable from a hang. Printed
    // to stderr so --json output stays parseable.
    //
    // Also mirrored into the terminal tab title, so the same
    // `[position/total]` is visible at a glance across several open tabs
    // without switching to the one actually running `--auto`.
    let mut report_progress =
        |position: usize, total: usize, cluster: &ctx_core::verification::CandidateCluster| {
            tab_title::set_title(&format!("ctx verify --auto [{position}/{total}] ({agent})"));
            eprintln!(
                "[{position}/{total}] reviewing cluster ({:?}, {} candidate(s)) via {agent}...",
                cluster.kind,
                cluster.fingerprints.len()
            );
        };
    // Printed right after each cluster's decisions are recorded, so the
    // "reviewing cluster..." line above is never the last thing shown for
    // it -- a real user asked what a cluster's outcome actually was right
    // after the progress-output fix landed, since until now `--auto` never
    // showed one.
    let mut report_result = |_position: usize, _total: usize, reviewed: &[ReviewedCandidate]| {
        for candidate in reviewed {
            let summary = summarize_statement(&candidate.statement);
            match &candidate.outcome {
                CandidateOutcome::Accepted { document_id } => {
                    eprintln!("    -> accepted {document_id}: {summary}");
                }
                CandidateOutcome::Rejected => {
                    eprintln!("    -> rejected: {summary}");
                }
                CandidateOutcome::SkippedPossibleDuplicate { existing_id } => {
                    eprintln!("    -> skipped (possible duplicate of {existing_id}): {summary}");
                }
            }
        }
    };
    let review_agent = ConfiguredAgent::from_name(agent, model, cli.verbose > 0)
        .map_err(CliError::UnsupportedAgent)?;
    let report = KnowledgeVerificationService::new(&mut store, &writer).auto_with_progress(
        &repository.id,
        id_prefix,
        author,
        &now,
        force,
        &review_agent,
        &mut report_progress,
        &mut report_result,
    )?;
    tab_title::set_title("ctx");
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Reviewed {} cluster(s) via {agent}: {} document(s) written, {} candidate(s) accepted, {} rejected, {} left pending as possible duplicates",
            report.clusters_reviewed,
            report.documents_written,
            report.candidates_accepted,
            report.candidates_rejected,
            report.candidates_skipped_possible_duplicate
        );
    }
    Ok(())
}

/// Bounded excerpt cap so one huge symbol body never dominates a stale-claim
/// review prompt -- there's no token-budget renderer to reuse here (that
/// machinery is `ctx-core`-internal), so a flat byte cap does the same job.
const MAX_STALE_CLAIM_EXCERPT_BYTES: usize = 6000;

/// Fills in each claim's `symbol_excerpt` by reading the current file at the
/// `CodeSymbol` side's own indexed byte range -- safe to do even though the
/// claim went stale, since the *symbol node itself* was already re-indexed
/// fresh (only the semantic edge asserting it still satisfies the product
/// intent is what's marked stale); `graph` is loaded from the same store
/// `stale_claims` used, so its ranges match current code. A file that can't
/// be read, or a range that no longer lands on a valid slice (for example
/// uncommitted working-tree edits since the last `ctx index`), leaves
/// `symbol_excerpt` as `None` rather than guessing or panicking.
fn enrich_stale_claims_with_current_code(
    claims: &mut [StaleClaim],
    graph: &ctx_core::graph::GraphSnapshot,
    repo_root: &Path,
) {
    for claim in claims {
        let Some(symbol) = [&claim.source, &claim.target]
            .into_iter()
            .find(|summary| summary.kind == NodeKind::CodeSymbol)
        else {
            continue;
        };
        let Ok(stable_key) = ctx_core::domain::StableKey::new(symbol.stable_key.clone()) else {
            continue;
        };
        let Some(node) = graph.nodes.get(&stable_key) else {
            continue;
        };
        let PlannedNodeAttributes::Symbol {
            file_path, range, ..
        } = &node.attributes
        else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(repo_root.join(file_path)) else {
            continue;
        };
        let Some(excerpt) = content.get(range.start_byte..range.end_byte) else {
            continue;
        };
        let bytes = excerpt.as_bytes();
        claim.symbol_excerpt = Some(if bytes.len() > MAX_STALE_CLAIM_EXCERPT_BYTES {
            format!(
                "{}\n... (truncated)",
                String::from_utf8_lossy(&bytes[..MAX_STALE_CLAIM_EXCERPT_BYTES])
            )
        } else {
            excerpt.to_owned()
        });
    }
}

pub(super) fn verify_stale(
    cli: &Cli,
    git: &GitRepo,
    agent: &str,
    model: Option<String>,
    author: &str,
) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let mut store = SqliteStore::open(&database_path, git.context_root())?;
    let repository = git.descriptor()?;
    let head = git.head()?;
    let now = Utc::now().to_rfc3339();

    let mut claims = VerificationService::new(&mut store).stale_claims(&repository.id)?;
    if claims.is_empty() {
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "claims_reviewed": 0, "reactivated": 0, "suggested_removals": 0, "results": []
                }))?
            );
        } else {
            println!("No stale semantic claims.");
        }
        return Ok(());
    }
    let graph = store.load_graph(&repository.id)?;
    enrich_stale_claims_with_current_code(&mut claims, &graph, git.root());

    eprintln!("Reviewing {} stale claim(s) via {agent}...", claims.len());
    let review_agent = ConfiguredAgent::from_name(agent, model, cli.verbose > 0)
        .map_err(CliError::UnsupportedAgent)?;
    let (report, results) = VerificationService::new(&mut store).review_stale_claims(
        &repository.id,
        &head,
        &claims,
        &review_agent,
        author,
        &now,
    )?;
    print_stale_review(cli, agent, &report, &results)
}

fn print_stale_review(
    cli: &Cli,
    agent: &str,
    report: &ctx_app::verification::StaleClaimReviewReport,
    results: &[ctx_app::verification::ReviewedStaleClaim],
) -> Result<(), CliError> {
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"report": report, "results": results}))?
        );
        return Ok(());
    }
    for result in results {
        match &result.outcome {
            StaleClaimOutcome::Reactivated => {
                println!("  -> reactivated: {} -> {}", result.source, result.target);
            }
            StaleClaimOutcome::SuggestedRemoval { reasoning } => {
                println!(
                    "  -> suggest removing: {} -> {} ({reasoning})",
                    result.source, result.target
                );
            }
            StaleClaimOutcome::AlreadyChanged => {
                println!(
                    "  -> already changed, skipped: {} -> {}",
                    result.source, result.target
                );
            }
        }
    }
    println!(
        "Reviewed {} stale claim(s) via {agent}: {} reactivated, {} suggested for removal (not applied automatically)",
        report.claims_reviewed, report.reactivated, report.suggested_removals
    );
    Ok(())
}

fn review_knowledge_candidate_interactively(
    service: &mut KnowledgeVerificationService<'_, SqliteStore, YamlBusinessContextReader>,
    repository: &ctx_core::domain::RepositoryId,
    candidate: &ctx_core::knowledge::KnowledgeCandidate,
    author: &str,
    now: &str,
    force: bool,
) -> Result<(), CliError> {
    println!();
    println!("Candidate ({:?}): {}", candidate.kind, candidate.statement);
    for evidence in &candidate.evidence {
        println!("  evidence: {} — {}", evidence.locator, evidence.excerpt);
    }
    if !candidate.implementation_candidates.is_empty() {
        println!(
            "  implementation candidates: {}",
            candidate.implementation_candidates.join(", ")
        );
    }
    if !candidate.test_candidates.is_empty() {
        println!(
            "  test candidates: {}",
            candidate.test_candidates.join(", ")
        );
    }
    loop {
        print!("[y] accept  [n] reject  [s] skip: ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        match answer.trim().to_ascii_lowercase().as_str() {
            "y" => {
                print!("Stable ID to allocate (e.g. REQ-SUB-014): ");
                io::stdout().flush()?;
                let mut chosen_id = String::new();
                io::stdin().read_line(&mut chosen_id)?;
                let chosen_id = chosen_id.trim();
                if chosen_id.is_empty() {
                    println!("An ID is required to accept.");
                    continue;
                }
                accept_knowledge_candidate_interactively(
                    service, repository, candidate, chosen_id, author, now, force,
                )?;
                break;
            }
            "n" => {
                service.reject(
                    repository,
                    &candidate.fingerprint,
                    author,
                    now,
                    ctx_core::knowledge::DecisionMethod::Human,
                )?;
                break;
            }
            "s" => break,
            _ => println!("Please enter y, n, or s."),
        }
    }
    Ok(())
}

fn accept_knowledge_candidate_interactively(
    service: &mut KnowledgeVerificationService<'_, SqliteStore, YamlBusinessContextReader>,
    repository: &ctx_core::domain::RepositoryId,
    candidate: &ctx_core::knowledge::KnowledgeCandidate,
    chosen_id: &str,
    author: &str,
    now: &str,
    force: bool,
) -> Result<(), CliError> {
    match service.accept(
        repository,
        &candidate.fingerprint,
        chosen_id,
        author,
        now,
        force,
        ctx_core::knowledge::DecisionMethod::Human,
    ) {
        Ok(path) => {
            println!("Accepted as {chosen_id} -> {path}");
            Ok(())
        }
        Err(VerificationError::PossibleDuplicate { existing_id, .. }) => {
            print!(
                "Looks like a restatement of already-active {existing_id} — create {chosen_id} anyway? [y/n]: "
            );
            io::stdout().flush()?;
            let mut confirm = String::new();
            io::stdin().read_line(&mut confirm)?;
            if confirm.trim().eq_ignore_ascii_case("y") {
                let path = service.accept(
                    repository,
                    &candidate.fingerprint,
                    chosen_id,
                    author,
                    now,
                    true,
                    ctx_core::knowledge::DecisionMethod::Human,
                )?;
                println!("Accepted as {chosen_id} -> {path}");
            } else {
                println!(
                    "Skipped -- consider attaching this evidence to {existing_id} manually instead."
                );
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn print_knowledge_candidates(candidates: &[ctx_core::knowledge::KnowledgeCandidate]) {
    for candidate in candidates {
        println!(
            "{}: ({:?}) {}",
            candidate.fingerprint, candidate.kind, candidate.statement
        );
    }
}

fn print_candidates(candidates: &[ctx_core::verification::SemanticCandidate]) {
    for candidate in candidates {
        println!(
            "{}: {} {:?} {} ({:.2})",
            candidate.fingerprint,
            candidate.source_identifier,
            candidate.relation,
            candidate.target_identifier,
            candidate.score.total
        );
    }
}
