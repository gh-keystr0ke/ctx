use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    path::Path,
};

use ctx_adapters::{analyzer::AnalyzerRegistry, git::GitRepo, sqlite::SqliteStore};
use ctx_app::{
    index::IndexRunner,
    ports::{
        CommitMetadata, GitRepository, GraphStore, PortError, RepositoryDescriptor, SourceScope,
    },
};
use ctx_core::{
    coverage::{RANKING_VERSION, RankedDecision, analyze_coverage},
    domain::CommitOid,
    indexing::FileChange,
};

use super::{
    Result, fetch,
    manifest::{FillPoint, Language, Point, Project, SelectedBy, SelectedPoint, Selection},
};

pub(super) fn rank(cache: &Path, project: &Project) -> Result<Vec<RankedDecision>> {
    let checkout = fetch::checked_out(cache, project)?;
    let git = GitRepo::discover(&checkout)?;
    let scope = SourceScope {
        languages: vec![language_name(project.language).to_owned()],
        include: project.modules.clone(),
        exclude: project.excluded_paths.clone(),
    };
    let scoped = CorpusRepository { git: &git, scope };
    let analyzer = AnalyzerRegistry::builtins(&checkout, &scoped.scope.languages)?;
    let temporary = tempfile::tempdir()?;
    let mut store = SqliteStore::open(&temporary.path().join("corpus.db"), &checkout)?;
    let head = scoped.head()?;
    let result = IndexRunner::new(&scoped, &analyzer, &mut store).run(&head.authored_at)?;
    if !result.failed_files.is_empty() {
        return Err(io::Error::other(format!(
            "could not analyze {} corpus source file(s)",
            result.failed_files.len()
        ))
        .into());
    }
    let graph = store.load_graph(&scoped.descriptor()?.id)?;
    let symbols = graph.nodes.keys().cloned().collect();
    Ok(analyze_coverage(&graph, &symbols).decisions)
}

pub(super) fn freeze(project: &mut Project, ranked: &[RankedDecision]) -> Result<Selection> {
    if !project.points.is_empty() {
        return Err(io::Error::other("sample already frozen").into());
    }
    if project.sample.ranking_version != RANKING_VERSION {
        return Err(
            io::Error::other("this binary cannot reproduce the requested ranking version").into(),
        );
    }
    let mut ordering = round_robin(project, ranked);
    let mut random: Vec<_> = (0..ranked.len()).collect();
    random.sort_by_cached_key(|index| {
        let input = format!("{}{}", project.sample.seed, ranked[*index].anchor);
        (blake3::hash(input.as_bytes()).to_hex().to_string(), *index)
    });
    let mut random: VecDeque<_> = random.into();
    let mut used = BTreeSet::new();
    let mut selection = Selection {
        schema: 1,
        fill_order: Vec::new(),
        fill_points: BTreeMap::new(),
        points: BTreeMap::new(),
    };
    for selected_by in std::iter::repeat_n(SelectedBy::Top, project.sample.size.div_ceil(2)).chain(
        std::iter::repeat_n(SelectedBy::Random, project.sample.size / 2),
    ) {
        let queue = if selected_by == SelectedBy::Top {
            &mut ordering
        } else {
            &mut random
        };
        let index = next_index(queue, &mut used)
            .ok_or_else(|| io::Error::other("not enough decision points to freeze the sample"))?;
        append(
            project,
            &mut selection,
            ranked[index].anchor.to_string(),
            metadata(&ranked[index], index + 1),
            selected_by,
        );
    }
    for offset in 0..60usize.saturating_sub(project.sample.size) {
        let next = if offset % 2 == 0 {
            next_index(&mut ordering, &mut used).or_else(|| next_index(&mut random, &mut used))
        } else {
            next_index(&mut random, &mut used).or_else(|| next_index(&mut ordering, &mut used))
        };
        let Some(index) = next else { break };
        let point = &ranked[index];
        let anchor = point.anchor.to_string();
        selection.fill_order.push(anchor.clone());
        selection
            .fill_points
            .insert(anchor, metadata(point, index + 1));
    }
    Ok(selection)
}

fn next_index(queue: &mut VecDeque<usize>, used: &mut BTreeSet<usize>) -> Option<usize> {
    while let Some(index) = queue.pop_front() {
        if used.insert(index) {
            return Some(index);
        }
    }
    None
}

fn round_robin(project: &Project, ranked: &[RankedDecision]) -> VecDeque<usize> {
    let mut modules = vec![VecDeque::new(); project.modules.len()];
    for (index, point) in ranked.iter().enumerate() {
        if let Some(module) = project
            .modules
            .iter()
            .position(|module| Path::new(&point.file).starts_with(module))
        {
            modules[module].push_back(index);
        }
    }
    let mut ordered = VecDeque::new();
    while modules.iter().any(|module| !module.is_empty()) {
        for module in &mut modules {
            ordered.extend(module.pop_front());
        }
    }
    ordered
}

pub(super) fn metadata(point: &RankedDecision, rank: usize) -> FillPoint {
    FillPoint {
        location: format!("{}:{}", point.file, point.range.start_line),
        rank,
        score: point.score,
        contributions: point
            .contributions
            .iter()
            .map(|(key, value)| ((*key).to_owned(), *value))
            .collect(),
    }
}

pub(super) fn append(
    project: &mut Project,
    selection: &mut Selection,
    anchor: String,
    point: FillPoint,
    selected_by: SelectedBy,
) {
    let id = format!("{}-{:04}", project.name, project.points.len() + 1);
    selection.points.insert(
        id.clone(),
        SelectedPoint {
            selected_by,
            rank: point.rank,
            score: point.score,
            contributions: point.contributions,
        },
    );
    project.points.push(Point {
        id,
        anchor,
        location: point.location,
        draft: None,
        drafted_by: None,
        label: None,
        category: None,
        evidence: Vec::new(),
        verified_by: None,
        second_label: None,
        second_by: None,
        note: None,
    });
}

pub(super) fn language_name(language: Language) -> &'static str {
    match language {
        Language::Python => "python",
        Language::Rust => "rust",
        Language::Go => "go",
    }
}

struct CorpusRepository<'a> {
    git: &'a GitRepo,
    scope: SourceScope,
}

impl GitRepository for CorpusRepository<'_> {
    fn descriptor(&self) -> std::result::Result<RepositoryDescriptor, PortError> {
        self.git.descriptor()
    }

    fn head(&self) -> std::result::Result<CommitMetadata, PortError> {
        self.git.head()
    }

    fn all_source_files(&self) -> std::result::Result<Vec<String>, PortError> {
        let mut args = vec!["ls-files", "-z", "--"];
        args.extend(self.scope.include.iter().map(String::as_str));
        let output = fetch::git(self.git.root(), &args, false)
            .map_err(|error| PortError::new(error.to_string()))?;
        let extension = if self.scope.languages[0] == "python" {
            "py"
        } else if self.scope.languages[0] == "rust" {
            "rs"
        } else {
            "go"
        };
        Ok(output
            .split_terminator('\0')
            .filter(|path| {
                let path = Path::new(path);
                path.extension().is_some_and(|ext| ext == extension)
                    && !self
                        .scope
                        .exclude
                        .iter()
                        .any(|excluded| path.starts_with(excluded))
            })
            .map(str::to_owned)
            .collect())
    }

    fn changes_since(&self, oid: &CommitOid) -> std::result::Result<Vec<FileChange>, PortError> {
        self.git.changes_since(oid)
    }

    fn uncommitted_index_inputs(&self) -> std::result::Result<Vec<String>, PortError> {
        fetch::git(
            self.git.root(),
            &["status", "--porcelain", "--untracked-files=no"],
            false,
        )
        .map(|output| output.lines().map(str::to_owned).collect())
        .map_err(|error| PortError::new(error.to_string()))
    }

    fn source_scope(&self) -> SourceScope {
        self.scope.clone()
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use ctx_core::{
        coverage::DecisionAnchor,
        domain::StableKey,
        ir::{DecisionKind, SourceRange},
    };

    pub(in crate::corpus) fn project() -> Project {
        serde_json::from_value(serde_json::json!({
            "schema": 1, "guide_version": 1, "project": "sample", "language": "python",
            "role": "tuning", "repository": "test", "release": "v1", "commit": "test",
            "evidence_sources": [], "reference_patterns": [], "modules": ["a", "b"], "excluded_paths": [],
            "sample": {"seed": 42, "size": 4, "ranking_version": "v0"}, "points": [],
        })).unwrap()
    }

    pub(in crate::corpus) fn points() -> Vec<RankedDecision> {
        (0..10)
            .map(|ordinal| RankedDecision {
                anchor: DecisionAnchor {
                    symbol: StableKey::new("symbol:example").unwrap(),
                    fingerprint: "body".to_owned(),
                    ordinal,
                },
                file: if ordinal < 5 { "a/one.py" } else { "b/two.py" }.to_owned(),
                symbol: "example".to_owned(),
                kind: DecisionKind::Branch,
                range: SourceRange {
                    start_line: ordinal + 1,
                    ..SourceRange::default()
                },
                condition: "state".to_owned(),
                score: 10,
                ask: true,
                contributions: BTreeMap::from([("condition", 10)]),
            })
            .collect()
    }

    #[test]
    fn freezes_round_robin_and_fill_order_reproducibly() {
        let mut first = project();
        let mut second = project();
        let ranked = points();
        let selection = freeze(&mut first, &ranked).unwrap();
        let repeat = freeze(&mut second, &ranked).unwrap();
        assert_eq!(first.points[0].location, "a/one.py:1");
        assert_eq!(first.points[1].location, "b/two.py:6");
        assert_eq!(selection.fill_order.len(), 6);
        assert_eq!(
            yaml_serde::to_string(&first).unwrap(),
            yaml_serde::to_string(&second).unwrap()
        );
        assert_eq!(
            yaml_serde::to_string(&selection).unwrap(),
            yaml_serde::to_string(&repeat).unwrap()
        );
        assert!(freeze(&mut first, &ranked).is_err());
    }
}
