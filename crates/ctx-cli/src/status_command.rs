use ctx_adapters::{git::GitRepo, sqlite::SqliteStore};
use ctx_app::status::{IndexState, StatusHealth, StatusService};

use crate::{Cli, CliError, database_path, short_oid};

pub fn status(cli: &Cli, git: &GitRepo) -> Result<(), CliError> {
    let database_path = database_path(git.root())?;
    let store = SqliteStore::open(&database_path, git.context_root())?;
    let status = StatusService::new(git, &store).inspect()?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    println!("Repository: {}", status.repository);
    println!("Health: {}", health_label(status.health));
    println!(
        "Index: {} (HEAD {})",
        index_state_label(status.index_state),
        short_oid(status.head_commit.as_str())
    );
    if let Some(indexed) = &status.knowledge.last_indexed_commit {
        println!("Last indexed commit: {}", short_oid(indexed.as_str()));
    }
    println!(
        "Source scope: {} [{}]",
        status.source_scope.languages.join(", "),
        status.source_scope.include.join(", ")
    );
    println!();
    println!("Code:");
    println!("  Files: {}", status.knowledge.files);
    println!("  Symbols: {}", status.knowledge.symbols);
    println!("  Database entities: {}", status.knowledge.db_entities);
    println!("Product context:");
    println!("  Features: {}", status.knowledge.features);
    println!("  Requirements: {}", status.knowledge.requirements);
    println!("  Invariants: {}", status.knowledge.invariants);
    println!("  Decisions: {}", status.knowledge.decisions);
    println!(
        "  Public documents: {} out of {}",
        status.knowledge.public_documents,
        status.knowledge.features
            + status.knowledge.requirements
            + status.knowledge.invariants
            + status.knowledge.decisions
    );
    println!("Relationships:");
    println!("  Structural facts: {}", status.knowledge.structural_facts);
    println!(
        "  Active assertions: {}",
        status.knowledge.active_assertions
    );
    println!(
        "  Active inferences: {}",
        status.knowledge.active_inferences
    );
    println!(
        "  Stale semantics: {}",
        status.knowledge.stale_semantic_edges
    );
    println!(
        "  Rejected inferences: {}",
        status.knowledge.rejected_semantic_edges
    );
    if status.uncommitted_index_inputs.is_empty() {
        println!("Index inputs: clean");
    } else {
        println!("Index inputs differing from HEAD:");
        for path in &status.uncommitted_index_inputs {
            println!("  - {path}");
        }
    }
    if !status.schema_divergences.is_empty() {
        println!("SQLAlchemy/migration schema divergences (best-effort, presence-only):");
        for divergence in &status.schema_divergences {
            let label = match divergence.kind {
                ctx_core::schema::DivergenceKind::ExpectedByOrmOnly => {
                    "ORM expects this column; no migration declares it"
                }
                ctx_core::schema::DivergenceKind::DeclaredByMigrationOnly => {
                    "a migration declares this column; the ORM model has no field for it"
                }
            };
            println!("  - {}.{}: {label}", divergence.entity, divergence.column);
        }
    }
    if !status.notices.is_empty() {
        println!();
        println!("Why this health state:");
        for notice in &status.notices {
            println!("  - {notice}");
        }
    }
    if !status.suggested_actions.is_empty() {
        println!("Next actions:");
        for action in &status.suggested_actions {
            println!("  - {action}");
        }
    }
    Ok(())
}

const fn health_label(health: StatusHealth) -> &'static str {
    match health {
        StatusHealth::Ready => "ready",
        StatusHealth::NeedsIndex => "needs index",
        StatusHealth::NeedsContext => "needs product context",
        StatusHealth::NeedsMappings => "needs semantic mappings",
        StatusHealth::NeedsAttention => "needs attention",
    }
}

const fn index_state_label(state: IndexState) -> &'static str {
    match state {
        IndexState::NotIndexed => "not indexed",
        IndexState::Behind => "behind",
        IndexState::Current => "current",
    }
}
