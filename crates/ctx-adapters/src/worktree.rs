use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use ctx_app::{
    context::ContextImportError,
    index::{IndexError, IndexReport},
    ports::{
        BusinessContextReader, BusinessContextStore, CommitMetadata, GitRepository, IndexStore,
    },
};
use ctx_core::{
    business::{BusinessDocument, BusinessKind},
    domain::NodeKind,
    graph::{GraphNode, GraphSnapshot, find_symbols},
    indexing::{FileChange, IndexPlan, RepositorySnapshot, plan_incremental_index},
};
use serde::Serialize;

use crate::{
    analyzer::AnalyzerRegistry,
    business_context::YamlBusinessContextReader,
    git::GitRepo,
    local_index::{InvalidFile, LocalIndexError, LocalIndexReport},
    sqlite::SqliteStore,
};

pub const PREVIEW_NOTICE: &str = "PREVIEW: uncommitted working tree";

pub struct WorktreeSnapshot {
    head: CommitMetadata,
    plan: IndexPlan,
    documents: Vec<BusinessDocument>,
}

#[derive(Debug, Serialize)]
pub struct ContextIssue {
    pub path: String,
    pub locator: String,
    pub message: String,
    pub suggestions: Vec<String>,
}

impl WorktreeSnapshot {
    /// Analyzes the current worktree without opening a database.
    ///
    /// # Errors
    /// Returns an error for invalid sources or product documents.
    pub fn read(git: &GitRepo) -> Result<Self, LocalIndexError> {
        let head = git.head()?;
        let analyzer = AnalyzerRegistry::builtins(git.root(), &git.source_scope().languages)?;
        let documents = YamlBusinessContextReader::new(git.context_root().to_path_buf())
            .read_all()
            .map_err(ContextImportError::Read)?;
        let mut analyses = BTreeMap::new();
        let mut invalid = Vec::new();
        for path in git.all_source_files()? {
            let result = match fs::read_to_string(git.root().join(&path)) {
                Ok(text) => analyzer.analyze_strict(&path, &text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => Err((1, error.to_string())),
            };
            match result {
                Ok(file_ir) => {
                    analyses.insert(path, file_ir);
                }
                Err((line, reason)) => invalid.push(InvalidFile { path, line, reason }),
            }
        }
        if !invalid.is_empty() {
            return Err(LocalIndexError::InvalidFiles(invalid));
        }
        let changes = analyses
            .keys()
            .map(|path| FileChange::Added { path: path.clone() })
            .collect::<Vec<_>>();
        let plan = plan_incremental_index(&RepositorySnapshot::default(), &analyses, &changes)
            .map_err(IndexError::from)?;
        Ok(Self {
            head,
            plan,
            documents,
        })
    }

    /// Checks exact context mappings and offers canonical names for unmatched symbols.
    #[must_use]
    pub fn issues(&self) -> Vec<ContextIssue> {
        let graph = GraphSnapshot {
            nodes: self
                .plan
                .nodes_to_write
                .iter()
                .filter(|node| node.kind == NodeKind::CodeSymbol)
                .map(|node| {
                    (
                        node.stable_key.clone(),
                        GraphNode {
                            stable_key: node.stable_key.clone(),
                            kind: node.kind,
                            name: node.name.clone(),
                            content_hash: node.content_hash.clone(),
                            attributes: node.attributes.clone(),
                        },
                    )
                })
                .collect(),
            edges: Vec::new(),
        };
        let mut canonical = BTreeMap::<&str, usize>::new();
        for node in graph.nodes.values() {
            *canonical.entry(node.identifier()).or_default() += 1;
        }
        let features = self
            .documents
            .iter()
            .filter(|doc| doc.kind == BusinessKind::Feature)
            .map(|doc| doc.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut issues = Vec::new();
        for document in &self.documents {
            if let Some(feature) = &document.feature
                && !features.contains(feature.as_str())
            {
                issues.push(ContextIssue {
                    path: document.source_uri.clone(),
                    locator: "feature".to_owned(),
                    message: format!("feature '{feature}' was not found"),
                    suggestions: Vec::new(),
                });
            }
            for link in document.implementation.iter().chain(&document.tests) {
                let symbol = link.symbol.as_str();
                if graph.nodes.keys().any(|key| key.as_str() == symbol)
                    || canonical.get(symbol) == Some(&1)
                {
                    continue;
                }
                let short = symbol.rsplit(['.', ':']).next().unwrap_or(symbol);
                let suggestions = find_symbols(short, &graph)
                    .into_iter()
                    .map(|node| node.identifier)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .take(5)
                    .collect();
                let message = if canonical.contains_key(symbol) {
                    format!("symbol '{symbol}' is ambiguous")
                } else {
                    format!("symbol '{symbol}' was not found")
                };
                issues.push(ContextIssue {
                    path: document.source_uri.clone(),
                    locator: link.locator.clone(),
                    message,
                    suggestions,
                });
            }
        }
        issues
    }

    /// Stores a snapshot in a freshly created preview database.
    ///
    /// # Errors
    /// Returns an error if HEAD changed or the preview cannot be stored.
    pub fn persist_preview(
        mut self,
        git: &GitRepo,
        store: &mut SqliteStore,
    ) -> Result<LocalIndexReport, LocalIndexError> {
        if git.head_oid()? != self.head.oid {
            return Err(ctx_app::ports::PortError::new(
                "HEAD changed during preview; retry the command",
            )
            .into());
        }
        self.head.oid = self.head.oid.worktree();
        self.head.parent_oid = None;
        let now = chrono::Utc::now().to_rfc3339();
        let repository = git.descriptor()?;
        store.ensure_repository(&repository, &now)?;
        store.apply_index(&repository.id, &self.head, &now, &self.plan)?;
        let business_context =
            store.sync_context(&repository.id, &self.head, &now, &self.documents)?;
        store
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        Ok(LocalIndexReport {
            code: IndexReport {
                commit: self.head.oid.to_string(),
                already_current: false,
                stats: self.plan.stats,
                failed_files: Vec::new(),
            },
            business_context,
            uncommitted_files_ignored: None,
        })
    }
}
