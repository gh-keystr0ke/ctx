use std::{
    path::{Path, PathBuf},
    process::Command,
};

use ctx_app::ports::{
    CommitMetadata, GitArtifactSource, GitRepository, PortError, RepositoryDescriptor,
    ReviewChangeSet, ReviewRepository, SourceScope,
};
use ctx_core::{
    artifact::{Artifact, ArtifactIdentity, ArtifactKind, ArtifactProvider},
    domain::{CommitOid, RepositoryId},
    indexing::FileChange,
};
use serde::Deserialize;
use thiserror::Error;

use crate::language::{SupportedLanguage, is_indexable_source};

#[derive(Debug, Error)]
pub enum GitError {
    #[error("'{path}' is not inside a Git repository")]
    NotRepository { path: String },
    #[error("failed to execute Git: {0}")]
    Io(#[from] std::io::Error),
    #[error("Git command failed: git {command}: {stderr}")]
    Command { command: String, stderr: String },
    #[error("Git output was not valid UTF-8")]
    InvalidUtf8,
    #[error("invalid ctx configuration at '{path}': {message}")]
    Config { path: String, message: String },
}

pub struct GitRepo {
    root: PathBuf,
    /// Where `.context/` and `.ctx-candidates/` are read from and written
    /// to. Equal to `root` unless a local, machine-only registry entry
    /// redirects it elsewhere (`ADR-CTX-050`) -- in that default case every
    /// method below behaves exactly as it did before that redirect existed.
    context_root: PathBuf,
    /// Whether `context_root` is itself inside a Git worktree. A redirected
    /// context store is plain files by default (no Git required at all --
    /// ADR-CTX-050); Git-tracked commit/staleness guarantees only apply when
    /// this is true. Always `true` when `context_root == root`.
    context_is_repository: bool,
    languages: Vec<SupportedLanguage>,
    path_filter: PathFilter,
    service_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ConfigFile {
    language: Option<String>,
    languages: Option<Vec<String>>,
    #[serde(default)]
    paths: ConfigPaths,
    service: Option<ConfigService>,
}

#[derive(Clone, Debug, Deserialize)]
struct ConfigService {
    name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ConfigPaths {
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Clone, Debug)]
struct PathFilter {
    include: Vec<String>,
    exclude: Vec<String>,
}

#[derive(Clone, Debug)]
struct RepositoryConfiguration {
    languages: Vec<SupportedLanguage>,
    path_filter: PathFilter,
    service_name: Option<String>,
}

impl Default for RepositoryConfiguration {
    fn default() -> Self {
        Self {
            languages: vec![SupportedLanguage::Python],
            path_filter: PathFilter::default(),
            service_name: None,
        }
    }
}

impl Default for PathFilter {
    fn default() -> Self {
        Self {
            include: vec!["src".to_owned(), "tests".to_owned()],
            exclude: vec![
                "generated".to_owned(),
                "vendor".to_owned(),
                "build".to_owned(),
                "dist".to_owned(),
                "target".to_owned(),
                ".venv".to_owned(),
            ],
        }
    }
}

impl GitRepo {
    /// Discovers the containing Git worktree.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when Git cannot be executed or `start` is outside a
    /// worktree.
    pub fn discover(start: &Path) -> Result<Self, GitError> {
        let output = Command::new("git")
            .args([
                "-C",
                &start.display().to_string(),
                "rev-parse",
                "--show-toplevel",
            ])
            .output()?;
        if !output.status.success() {
            return Err(GitError::NotRepository {
                path: start.display().to_string(),
            });
        }
        let root = std::str::from_utf8(&output.stdout)
            .map_err(|_| GitError::InvalidUtf8)?
            .trim();
        let root = PathBuf::from(root);
        let configuration = load_configuration(&root)?;
        let context_root = crate::context_registry::resolve(&root)?.unwrap_or_else(|| root.clone());
        let context_is_repository = context_root == root || is_inside_work_tree(&context_root);
        Ok(Self {
            root,
            context_root,
            context_is_repository,
            languages: configuration.languages,
            path_filter: configuration.path_filter,
            service_name: configuration.service_name,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where `.context/` and `.ctx-candidates/` currently live: `root`
    /// unless a local registry entry (`ctx context-store set`, `ADR-CTX-050`)
    /// redirects them to a separate location.
    pub fn context_root(&self) -> &Path {
        &self.context_root
    }

    /// Whether `.context/` and `.ctx-candidates/` have been redirected to a
    /// separate location rather than living under `root`.
    pub fn has_external_context(&self) -> bool {
        self.context_root != self.root
    }

    /// Whether the current context location (redirected or not) is itself
    /// inside a Git worktree. A redirected context store is plain files by
    /// default -- this is only true when someone explicitly made it a Git
    /// repository (`ctx context-store set --git`, or by hand).
    pub fn context_is_git_repository(&self) -> bool {
        self.context_is_repository
    }

    /// Returns the repository's explicit federation identity, when configured.
    pub fn service_name(&self) -> Option<&str> {
        self.service_name.as_deref()
    }

    /// Keeps machine-local `SQLite` state out of Git without modifying the
    /// repository's shared `.gitignore`.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when the repository exclude file cannot be found,
    /// read, or updated.
    pub fn ignore_local_state(&self) -> Result<(), GitError> {
        let bytes = self.output(&["rev-parse", "--git-path", "info/exclude"])?;
        let value = std::str::from_utf8(&bytes)
            .map_err(|_| GitError::InvalidUtf8)?
            .trim();
        let path = if Path::new(value).is_absolute() {
            PathBuf::from(value)
        } else {
            self.root.join(value)
        };
        let mut content = std::fs::read_to_string(&path)?;
        for pattern in [
            ".ctx/ctx.db",
            ".ctx/ctx.db-shm",
            ".ctx/ctx.db-wal",
            ".ctx/registry.toml",
            ".ctx/export.json",
        ] {
            if content.lines().any(|line| line.trim() == pattern) {
                continue;
            }
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(pattern);
            content.push('\n');
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Backwards-compatible name for callers that only need the database
    /// exclusions. New code should use [`Self::ignore_local_state`].
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when the repository exclude file cannot be found,
    /// read, or updated.
    pub fn ignore_local_database(&self) -> Result<(), GitError> {
        self.ignore_local_state()
    }

    fn output(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        run_git(&self.root, args)
    }

    /// Runs a Git command against [`Self::context_root`] instead of `root`.
    /// Identical to [`Self::output`] when no external context is configured,
    /// since `context_root` then equals `root`.
    fn context_output(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        run_git(&self.context_root, args)
    }

    /// Whether the context repository has any commits yet. A freshly
    /// `git init`-ed context store (no commits) has no `HEAD` to diff
    /// against -- callers must fall back to treating every present file as
    /// not-yet-committed instead of running a `diff ... HEAD` that would
    /// simply fail.
    fn context_has_commits(&self) -> bool {
        self.context_output(&["rev-parse", "--verify", "HEAD"])
            .is_ok()
    }

    fn optional_text(&self, args: &[&str]) -> Result<Option<String>, GitError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let value = std::str::from_utf8(&output.stdout)
            .map_err(|_| GitError::InvalidUtf8)?
            .trim();
        Ok((!value.is_empty()).then(|| value.to_owned()))
    }

    fn resolve_revision(&self, revision: &str) -> Result<String, GitError> {
        resolve_revision_at(&self.root, revision)
    }

    fn source_allowed(&self, path: &str) -> bool {
        is_indexable_source(path, &self.languages) && self.path_filter.allows(path)
    }

    /// A human-readable project label for artifacts sourced from this
    /// worktree: the configured remote when there is one, the local root
    /// path otherwise.
    fn project_label(&self) -> String {
        self.optional_text(&["config", "--get", "remote.origin.url"])
            .ok()
            .flatten()
            .unwrap_or_else(|| self.root.display().to_string())
    }
}

/// Ensures a Git repository exists at `path`, creating the directory and
/// running `git init` there when neither already exists. A no-op when `path`
/// is already inside a Git worktree.
///
/// # Errors
///
/// Returns [`GitError`] when the directory cannot be created or `git init`
/// fails.
pub fn ensure_repository(path: &Path) -> Result<(), GitError> {
    std::fs::create_dir_all(path)?;
    if is_inside_work_tree(path) {
        return Ok(());
    }
    run_git(path, &["init", "-q"]).map(|_| ())
}

/// Whether `path` is inside a Git worktree (its own repository root, or a
/// subdirectory of one). `false` when `path` doesn't exist yet.
fn is_inside_work_tree(path: &Path) -> bool {
    run_git(path, &["rev-parse", "--is-inside-work-tree"])
        .ok()
        .is_some_and(|bytes| std::str::from_utf8(&bytes).map(str::trim) == Ok("true"))
}

fn run_git(root: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(GitError::Command {
            command: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn resolve_revision_at(root: &Path, revision: &str) -> Result<String, GitError> {
    let commit = format!("{revision}^{{commit}}");
    let bytes = run_git(
        root,
        &["rev-parse", "--verify", "--end-of-options", &commit],
    )?;
    Ok(std::str::from_utf8(&bytes)
        .map_err(|_| GitError::InvalidUtf8)?
        .trim()
        .to_owned())
}

impl GitRepository for GitRepo {
    fn descriptor(&self) -> Result<RepositoryDescriptor, PortError> {
        let root_path = self.root.display().to_string();
        let digest = blake3::hash(root_path.as_bytes()).to_hex();
        let id = RepositoryId::new(format!("repo:{digest}"))
            .map_err(|error| PortError::new(error.to_string()))?;
        let remote_url = self
            .optional_text(&["config", "--get", "remote.origin.url"])
            .map_err(port_error)?;
        Ok(RepositoryDescriptor {
            id,
            root_path,
            remote_url,
        })
    }

    fn head(&self) -> Result<CommitMetadata, PortError> {
        let bytes = self
            .output(&["show", "-s", "--format=%H%x00%P%x00%aI", "HEAD"])
            .map_err(port_error)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| PortError::new("invalid Git UTF-8"))?;
        let mut fields = text.trim().split('\0');
        let oid_text = fields
            .next()
            .ok_or_else(|| PortError::new("Git did not return HEAD"))?;
        let parents = fields.next().unwrap_or_default();
        let authored_at = fields.next().unwrap_or_default().to_owned();
        let oid = CommitOid::new(oid_text).map_err(|error| PortError::new(error.to_string()))?;
        let parent_oid = parents
            .split_whitespace()
            .next()
            .map(CommitOid::new)
            .transpose()
            .map_err(|error| PortError::new(error.to_string()))?;
        Ok(CommitMetadata {
            oid,
            parent_oid,
            authored_at,
        })
    }

    fn all_source_files(&self) -> Result<Vec<String>, PortError> {
        let bytes = self
            .output(&[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .map_err(port_error)?;
        let mut paths = parse_nul_paths(&bytes, &self.languages)?;
        paths.retain(|path| self.path_filter.allows(path));
        Ok(paths)
    }

    fn changes_since(&self, oid: &CommitOid) -> Result<Vec<FileChange>, PortError> {
        let bytes = self
            .output(&[
                "diff",
                "--name-status",
                "-z",
                "-M",
                oid.as_str(),
                "HEAD",
                "--",
            ])
            .map_err(port_error)?;
        Ok(filter_changes(
            parse_name_status(&bytes, &self.languages)?,
            &self.path_filter,
        ))
    }

    fn uncommitted_index_inputs(&self) -> Result<Vec<String>, PortError> {
        let source_bytes = self
            .output(&["diff", "--name-status", "-z", "-M", "HEAD", "--"])
            .map_err(port_error)?;
        let untracked_bytes = self
            .output(&["ls-files", "-z", "--others", "--exclude-standard"])
            .map_err(port_error)?;
        let mut paths = change_paths(&filter_changes(
            parse_name_status(&source_bytes, &self.languages)?,
            &self.path_filter,
        ));
        paths.extend(
            parse_nul_strings(&untracked_bytes)?
                .into_iter()
                .filter(|path| {
                    self.source_allowed(path)
                        || (!self.has_external_context() && path.starts_with(".context/"))
                }),
        );
        if self.has_external_context() && self.context_is_repository {
            // Only .context/ must be committed before indexing -- the same
            // scope INV-COMMIT-001 has always had (ADR-EXT-004: the
            // .ctx-candidates/ queue is deliberately readable uncommitted, so
            // it must stay out of this check exactly as it always has been
            // for a non-redirected repository). Scoped to ".context" rather
            // than left unscoped, since context_root may be a subdirectory
            // of a larger repository rather than its toplevel.
            let mut context_paths = Vec::new();
            if self.context_has_commits() {
                let context_bytes = self
                    .context_output(&["diff", "--name-only", "-z", "HEAD", "--", ".context"])
                    .map_err(port_error)?;
                context_paths.extend(parse_nul_strings(&context_bytes)?);
            }
            let context_untracked_bytes = self
                .context_output(&[
                    "ls-files",
                    "-z",
                    "--others",
                    "--exclude-standard",
                    "--",
                    ".context",
                ])
                .map_err(port_error)?;
            let context_ignored_bytes = self
                .context_output(&[
                    "ls-files",
                    "-z",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "--",
                    ".context",
                ])
                .map_err(port_error)?;
            context_paths.extend(parse_nul_strings(&context_untracked_bytes)?);
            context_paths.extend(parse_nul_strings(&context_ignored_bytes)?);
            paths.extend(
                context_paths
                    .into_iter()
                    .map(|path| format!("context:{path}")),
            );
        } else if !self.has_external_context() {
            let context_bytes = self
                .output(&["diff", "--name-only", "-z", "HEAD", "--", ".context"])
                .map_err(port_error)?;
            let ignored_context_bytes = self
                .output(&[
                    "ls-files",
                    "-z",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "--",
                    ".context",
                ])
                .map_err(port_error)?;
            paths.extend(parse_nul_strings(&context_bytes)?);
            paths.extend(parse_nul_strings(&ignored_context_bytes)?);
        }
        // else: an external context store that isn't a Git repository is
        // plain files with no commit-before-index guarantee (ADR-CTX-050);
        // there is nothing to check.
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    fn source_scope(&self) -> SourceScope {
        SourceScope {
            languages: self
                .languages
                .iter()
                .map(|language| language.name().to_owned())
                .collect(),
            include: self.path_filter.include.clone(),
            exclude: self.path_filter.exclude.clone(),
        }
    }
}

impl ReviewRepository for GitRepo {
    fn review_changes(&self, base: &str) -> Result<ReviewChangeSet, PortError> {
        let source_base = self.resolve_revision(base).map_err(port_error)?;
        let source_bytes = self
            .output(&["diff", "--name-status", "-z", "-M", &source_base, "--"])
            .map_err(port_error)?;
        let untracked_bytes = self
            .output(&["ls-files", "-z", "--others", "--exclude-standard"])
            .map_err(port_error)?;
        let mut source_changes = filter_changes(
            parse_name_status(&source_bytes, &self.languages)?,
            &self.path_filter,
        );
        // With an external context store there is no single commit shared by
        // both repositories, so `base` is resolved a second time directly in
        // the context repository rather than reusing `source_base` (ADR-CTX-050):
        // it fails loudly (a plain Git error) when the context repository has
        // no equivalently named revision, instead of silently pairing
        // unrelated history. An external store that isn't a Git repository
        // at all has no history to diff, so it honestly reports no changes
        // rather than guessing; one that is a Git repository but has no
        // commits yet is diffed against nothing (everything present is
        // "new"), the same reasoning as `context_has_commits` elsewhere.
        let mut changed_context_files = if self.has_external_context() && self.context_is_repository
        {
            let context_untracked_bytes = self
                .context_output(&[
                    "ls-files",
                    "-z",
                    "--others",
                    "--exclude-standard",
                    "--",
                    ".context",
                ])
                .map_err(port_error)?;
            let mut changed = parse_nul_strings(&context_untracked_bytes)?;
            if self.context_has_commits() {
                let context_base =
                    resolve_revision_at(&self.context_root, base).map_err(port_error)?;
                let context_bytes = self
                    .context_output(&["diff", "--name-only", "-z", &context_base, "--", ".context"])
                    .map_err(port_error)?;
                changed.extend(parse_nul_strings(&context_bytes)?);
            }
            changed
        } else if self.has_external_context() {
            Vec::new()
        } else {
            let context_bytes = self
                .output(&["diff", "--name-only", "-z", &source_base, "--", ".context"])
                .map_err(port_error)?;
            parse_nul_strings(&context_bytes)?
        };
        for path in parse_nul_strings(&untracked_bytes)? {
            if self.source_allowed(&path)
                && !source_changes
                    .iter()
                    .any(|change| change.current_path() == Some(path.as_str()))
            {
                source_changes.push(FileChange::Added { path: path.clone() });
            }
            if !self.has_external_context() && path.starts_with(".context/") {
                changed_context_files.push(path);
            }
        }
        source_changes.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
        changed_context_files.sort();
        changed_context_files.dedup();
        Ok(ReviewChangeSet {
            source_changes,
            changed_context_files,
        })
    }

    fn source_at(&self, revision: &str, path: &str) -> Result<Option<String>, PortError> {
        let revision = self.resolve_revision(revision).map_err(port_error)?;
        let object = format!("{revision}:{path}");
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["show", &object])
            .output()
            .map_err(|error| PortError::new(error.to_string()))?;
        if !output.status.success() {
            return Ok(None);
        }
        String::from_utf8(output.stdout)
            .map(Some)
            .map_err(|_| PortError::new(format!("'{path}' at {revision} is not valid UTF-8")))
    }
}

const COMMIT_RECORD_SEPARATOR: u8 = 0x1e;

impl GitArtifactSource for GitRepo {
    fn commit_artifacts(&self, since: Option<&CommitOid>) -> Result<Vec<Artifact>, PortError> {
        let range = since.map_or_else(
            || "HEAD".to_owned(),
            |oid| format!("{}..HEAD", oid.as_str()),
        );
        let format = format!("--format=%H%x00%an%x00%aI%x00%B%x{COMMIT_RECORD_SEPARATOR:02x}");
        let bytes = self.output(&["log", &format, &range]).map_err(port_error)?;
        parse_commit_artifacts(&bytes, &self.project_label())
    }

    fn branch_artifacts(&self) -> Result<Vec<Artifact>, PortError> {
        let bytes = self
            .output(&["for-each-ref", "--format=%(refname:short)", "refs/heads/"])
            .map_err(port_error)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| PortError::new("invalid Git UTF-8"))?;
        let project = self.project_label();
        let mut branches = text
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| branch_artifact(name, &project))
            .collect::<Vec<_>>();
        branches.sort_by(|left, right| left.identity.external_id.cmp(&right.identity.external_id));
        Ok(branches)
    }
}

fn parse_nul_paths(
    bytes: &[u8],
    languages: &[SupportedLanguage],
) -> Result<Vec<String>, PortError> {
    let mut paths = parse_nul_strings(bytes)?;
    paths.retain(|path| is_indexable_source(path, languages));
    paths.sort();
    Ok(paths)
}

fn parse_nul_strings(bytes: &[u8]) -> Result<Vec<String>, PortError> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| {
            std::str::from_utf8(part)
                .map(str::to_owned)
                .map_err(|_| PortError::new("source path is not valid UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()
}

fn parse_name_status(
    bytes: &[u8],
    languages: &[SupportedLanguage],
) -> Result<Vec<FileChange>, PortError> {
    let fields = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| {
            std::str::from_utf8(part)
                .map_err(|_| PortError::new("Git diff path is not valid UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut changes = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let status = fields[index];
        index += 1;
        if status.starts_with('R') {
            let old_path = fields
                .get(index)
                .ok_or_else(|| PortError::new("rename is missing its old path"))?;
            let new_path = fields
                .get(index + 1)
                .ok_or_else(|| PortError::new("rename is missing its new path"))?;
            index += 2;
            add_rename(&mut changes, old_path, new_path, languages);
            continue;
        }
        let path = fields
            .get(index)
            .ok_or_else(|| PortError::new("change is missing its path"))?;
        index += 1;
        if is_indexable_source(path, languages) {
            let change = match status.chars().next() {
                Some('A') => FileChange::Added {
                    path: (*path).to_owned(),
                },
                Some('D') => FileChange::Deleted {
                    path: (*path).to_owned(),
                },
                _ => FileChange::Modified {
                    path: (*path).to_owned(),
                },
            };
            changes.push(change);
        }
    }
    changes.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
    Ok(changes)
}

fn change_paths(changes: &[FileChange]) -> Vec<String> {
    changes
        .iter()
        .flat_map(|change| match change {
            FileChange::Renamed { old_path, new_path } => {
                vec![old_path.clone(), new_path.clone()]
            }
            FileChange::Added { path }
            | FileChange::Modified { path }
            | FileChange::Deleted { path } => vec![path.clone()],
        })
        .collect()
}

fn add_rename(
    changes: &mut Vec<FileChange>,
    old_path: &str,
    new_path: &str,
    languages: &[SupportedLanguage],
) {
    match (
        is_indexable_source(old_path, languages),
        is_indexable_source(new_path, languages),
    ) {
        (true, true) => changes.push(FileChange::Renamed {
            old_path: old_path.to_owned(),
            new_path: new_path.to_owned(),
        }),
        (true, false) => changes.push(FileChange::Deleted {
            path: old_path.to_owned(),
        }),
        (false, true) => changes.push(FileChange::Added {
            path: new_path.to_owned(),
        }),
        (false, false) => {}
    }
}

impl PathFilter {
    fn allows(&self, path: &str) -> bool {
        let included = self.include.is_empty()
            || self
                .include
                .iter()
                .any(|prefix| path == prefix || path.starts_with(&format!("{prefix}/")));
        included
            && !self.exclude.iter().any(|excluded| {
                path == excluded
                    || path.starts_with(&format!("{excluded}/"))
                    || path.split('/').any(|component| component == excluded)
            })
    }
}

fn load_configuration(root: &Path) -> Result<RepositoryConfiguration, GitError> {
    let path = root.join(".ctx").join("config.toml");
    if !path.exists() {
        return Ok(RepositoryConfiguration::default());
    }
    let content = std::fs::read_to_string(&path).map_err(|error| GitError::Config {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let config: ConfigFile = toml::from_str(&content).map_err(|error| GitError::Config {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    if config.language.is_some() && config.languages.is_some() {
        return Err(GitError::Config {
            path: path.display().to_string(),
            message: "use either legacy `language` or `languages`, not both".to_owned(),
        });
    }
    let configured_names = config.languages.unwrap_or_else(|| {
        vec![
            config
                .language
                .unwrap_or_else(|| SupportedLanguage::Python.name().to_owned()),
        ]
    });
    if configured_names.is_empty() {
        return Err(GitError::Config {
            path: path.display().to_string(),
            message: "`languages` must contain at least one analyzer name".to_owned(),
        });
    }
    let mut languages = configured_names
        .iter()
        .map(|name| {
            SupportedLanguage::from_name(name).ok_or_else(|| GitError::Config {
                path: path.display().to_string(),
                message: format!(
                    "unsupported language '{name}'; available analyzers: python, rust"
                ),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    languages.sort();
    languages.dedup();
    let service_name = match config.service {
        Some(service) if service.name.trim().is_empty() => {
            return Err(GitError::Config {
                path: path.display().to_string(),
                message: "`[service].name` must be a non-empty string".to_owned(),
            });
        }
        Some(service) => Some(service.name),
        None => None,
    };
    let defaults = PathFilter::default();
    Ok(RepositoryConfiguration {
        languages,
        path_filter: PathFilter {
            include: if config.paths.include.is_empty() {
                defaults.include
            } else {
                normalize_path_entries(config.paths.include)
            },
            exclude: if config.paths.exclude.is_empty() {
                defaults.exclude
            } else {
                normalize_path_entries(config.paths.exclude)
            },
        },
        service_name,
    })
}

/// Strips trailing path separators from user-configured include/exclude
/// entries so a directory written as `"fixtures/"` matches the same way as
/// `"fixtures"`: [`PathFilter::allows`] otherwise compares against
/// `"{entry}/"`, which becomes a non-matching `"fixtures//"` when the entry
/// already ends in a slash.
fn normalize_path_entries(entries: Vec<String>) -> Vec<String> {
    entries
        .into_iter()
        .map(|entry| entry.trim_end_matches('/').to_owned())
        .collect()
}

fn filter_changes(changes: Vec<FileChange>, filter: &PathFilter) -> Vec<FileChange> {
    changes
        .into_iter()
        .filter_map(|change| match change {
            FileChange::Added { path } if filter.allows(&path) => Some(FileChange::Added { path }),
            FileChange::Modified { path } if filter.allows(&path) => {
                Some(FileChange::Modified { path })
            }
            FileChange::Deleted { path } if filter.allows(&path) => {
                Some(FileChange::Deleted { path })
            }
            FileChange::Renamed { old_path, new_path } => {
                match (filter.allows(&old_path), filter.allows(&new_path)) {
                    (true, true) => Some(FileChange::Renamed { old_path, new_path }),
                    (true, false) => Some(FileChange::Deleted { path: old_path }),
                    (false, true) => Some(FileChange::Added { path: new_path }),
                    (false, false) => None,
                }
            }
            FileChange::Added { .. } | FileChange::Modified { .. } | FileChange::Deleted { .. } => {
                None
            }
        })
        .collect()
}

#[allow(clippy::needless_pass_by_value)]
fn port_error(error: GitError) -> PortError {
    PortError::new(error.to_string())
}

fn parse_commit_artifacts(bytes: &[u8], project: &str) -> Result<Vec<Artifact>, PortError> {
    bytes
        .split(|byte| *byte == COMMIT_RECORD_SEPARATOR)
        .map(|record| record.strip_prefix(b"\n").unwrap_or(record))
        .filter(|record| !record.is_empty())
        .map(|record| commit_artifact_from_record(record, project))
        .collect()
}

fn commit_artifact_from_record(record: &[u8], project: &str) -> Result<Artifact, PortError> {
    let text =
        std::str::from_utf8(record).map_err(|_| PortError::new("commit log is not valid UTF-8"))?;
    let mut fields = text.splitn(4, '\0');
    let oid = fields
        .next()
        .ok_or_else(|| PortError::new("commit record is missing its OID"))?;
    let author = fields.next().unwrap_or_default();
    let authored_at = fields.next().unwrap_or_default();
    let body = fields.next().unwrap_or_default().trim_end_matches('\n');
    let title = body.lines().next().unwrap_or_default().to_owned();
    Ok(Artifact {
        identity: ArtifactIdentity {
            provider: ArtifactProvider::Git,
            kind: ArtifactKind::Commit,
            external_id: oid.to_owned(),
        },
        project: ctx_core::domain::Project(project.to_owned()),
        title,
        body: body.to_owned(),
        author: (!author.is_empty()).then(|| author.to_owned()),
        external_created_at: (!authored_at.is_empty()).then(|| ctx_core::domain::Timestamp(authored_at.to_owned())),
        external_updated_at: None,
        source_locator: ctx_core::domain::Url(format!("git:commit:{oid}")),
        content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
    })
}

fn branch_artifact(name: &str, project: &str) -> Artifact {
    Artifact {
        identity: ArtifactIdentity {
            provider: ArtifactProvider::Git,
            kind: ArtifactKind::Branch,
            external_id: name.to_owned(),
        },
        project: ctx_core::domain::Project(project.to_owned()),
        title: name.to_owned(),
        body: String::new(),
        author: None,
        external_created_at: None,
        external_updated_at: None,
        source_locator: ctx_core::domain::Url(format!("git:branch:{name}")),
        content_hash: blake3::hash(name.as_bytes()).to_hex().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_language_changes_and_filters_unconfigured_sources() {
        let bytes =
            b"R100\0old.py\0new.py\0M\0README.md\0A\0src/app.py\0A\0src/lib.rs\0A\0src/main.go\0";
        let changes =
            parse_name_status(bytes, &[SupportedLanguage::Python, SupportedLanguage::Rust])
                .expect("valid status");
        assert_eq!(changes.len(), 3);
        assert!(changes.iter().any(|change| matches!(
            change,
            FileChange::Renamed { old_path, new_path }
                if old_path == "old.py" && new_path == "new.py"
        )));
        assert!(changes.iter().any(|change| matches!(
            change,
            FileChange::Added { path } if path == "src/lib.rs"
        )));
    }

    #[test]
    fn legacy_single_language_configuration_remains_supported() {
        let root = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(root.path().join(".ctx")).expect("ctx directory");
        std::fs::write(
            root.path().join(".ctx/config.toml"),
            "language = \"rust\"\n",
        )
        .expect("configuration");

        let configuration = load_configuration(root.path()).expect("valid configuration");

        assert_eq!(configuration.languages, vec![SupportedLanguage::Rust]);
    }

    #[test]
    fn multi_language_configuration_is_normalized() {
        let root = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(root.path().join(".ctx")).expect("ctx directory");
        std::fs::write(
            root.path().join(".ctx/config.toml"),
            "languages = [\"rust\", \"python\", \"rust\"]\n",
        )
        .expect("configuration");

        let configuration = load_configuration(root.path()).expect("valid configuration");

        assert_eq!(
            configuration.languages,
            vec![SupportedLanguage::Python, SupportedLanguage::Rust]
        );
    }

    #[test]
    fn configured_boundaries_include_only_selected_sources() {
        let filter = PathFilter {
            include: vec!["app".to_owned(), "tests".to_owned()],
            exclude: vec!["generated".to_owned()],
        };

        assert!(filter.allows("app/service.py"));
        assert!(filter.allows("tests/test_service.py"));
        assert!(!filter.allows("scripts/service.py"));
        assert!(!filter.allows("app/generated/client.py"));
    }

    #[test]
    fn parses_commit_artifacts_from_real_git_log_record_output() {
        // Captured verbatim from `git log --format="%H%x00%an%x00%aI%x00%B%x1e"`
        // against a real two-commit repository: git inserts a literal `\n`
        // immediately after each `\x1e` separator (including a trailing one
        // after the last record), and each record's body itself ends with
        // its own `\n` before the separator.
        let bytes = b"96561490e109d358f69f2d9e531d9f16a06a30c3\0test\0\
2026-08-21T17:10:12+03:00\0second commit\n\x1e\n\
4925aed1ea285cfad69206d94b0c63f5075d0a77\0test\0\
2026-08-21T17:10:12+03:00\0first commit\n\nbody line 1\nbody line 2\n\x1e\n";

        let artifacts = parse_commit_artifacts(bytes, "billing/subscriptions").expect("commits");

        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].identity.kind, ArtifactKind::Commit);
        assert_eq!(artifacts[0].identity.provider, ArtifactProvider::Git);
        assert_eq!(
            artifacts[0].identity.external_id,
            "96561490e109d358f69f2d9e531d9f16a06a30c3"
        );
        assert_eq!(artifacts[0].title, "second commit");
        assert_eq!(artifacts[0].body, "second commit");
        assert_eq!(artifacts[1].title, "first commit");
        assert_eq!(
            artifacts[1].body,
            "first commit\n\nbody line 1\nbody line 2"
        );
        assert_eq!(artifacts[1].author.as_deref(), Some("test"));
        assert_eq!(
            artifacts[1].external_created_at.as_ref().map(ctx_core::domain::Timestamp::as_str),
            Some("2026-08-21T17:10:12+03:00")
        );
    }

    #[test]
    fn commit_and_branch_artifacts_round_trip_against_a_real_repository() {
        let root = tempfile::tempdir().expect("temp dir");
        run(root.path(), &["init", "-q"]);
        run(root.path(), &["config", "user.name", "ctx tests"]);
        run(
            root.path(),
            &["config", "user.email", "ctx@example.invalid"],
        );
        std::fs::write(root.path().join("a.txt"), "a").expect("write file");
        run(root.path(), &["add", "a.txt"]);
        run(
            root.path(),
            &["commit", "-q", "-m", "PAY-317 fix cancellation"],
        );
        run(root.path(), &["branch", "feature/PAY-317-cancel"]);

        let repository = GitRepo::discover(root.path()).expect("discover");
        let commits = repository.commit_artifacts(None).expect("commit artifacts");
        let branches = repository.branch_artifacts().expect("branch artifacts");

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].title, "PAY-317 fix cancellation");
        assert_eq!(commits[0].identity.kind, ArtifactKind::Commit);
        let branch_names = branches
            .iter()
            .map(|artifact| artifact.identity.external_id.as_str())
            .collect::<Vec<_>>();
        assert!(branch_names.contains(&"feature/PAY-317-cancel"));
        assert!(
            branches
                .iter()
                .all(|artifact| artifact.identity.kind == ArtifactKind::Branch)
        );
    }

    fn run(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn trailing_slash_on_configured_exclude_still_excludes() {
        let root = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(root.path().join(".ctx")).expect("ctx directory");
        std::fs::write(
            root.path().join(".ctx/config.toml"),
            "languages = [\"rust\"]\n\n[paths]\ninclude = [\"crates\"]\nexclude = [\"crates/fixtures/\"]\n",
        )
        .expect("configuration");

        let configuration = load_configuration(root.path()).expect("valid configuration");

        assert!(
            !configuration
                .path_filter
                .allows("crates/fixtures/broken/before.rs")
        );
        assert!(configuration.path_filter.allows("crates/lib/service.rs"));
    }

    #[test]
    fn rename_across_config_boundary_becomes_delete() {
        let changes = vec![FileChange::Renamed {
            old_path: "src/service.py".to_owned(),
            new_path: "archive/service.py".to_owned(),
        }];
        let filtered = filter_changes(changes, &PathFilter::default());

        assert_eq!(
            filtered,
            vec![FileChange::Deleted {
                path: "src/service.py".to_owned()
            }]
        );
    }

    fn init_repository(root: &Path) {
        run(root, &["init", "-q"]);
        run(root, &["config", "user.name", "ctx tests"]);
        run(root, &["config", "user.email", "ctx@example.invalid"]);
    }

    #[test]
    fn uncommitted_index_inputs_flags_dirty_state_in_an_external_context_repository() {
        let source_root = tempfile::tempdir().expect("temp dir");
        init_repository(source_root.path());
        std::fs::write(source_root.path().join("a.txt"), "a").expect("write file");
        run(source_root.path(), &["add", "a.txt"]);
        run(source_root.path(), &["commit", "-q", "-m", "seed source"]);

        let context_root = tempfile::tempdir().expect("temp dir");
        init_repository(context_root.path());
        std::fs::create_dir_all(context_root.path().join(".context/requirements"))
            .expect("context directory");
        std::fs::write(
            context_root.path().join(".context/requirements/req.yaml"),
            "id: REQ-1\n",
        )
        .expect("write requirement");
        run(context_root.path(), &["add", "."]);
        run(context_root.path(), &["commit", "-q", "-m", "seed context"]);

        let mut repository = GitRepo::discover(source_root.path()).expect("discover");
        repository.context_root = context_root.path().to_path_buf();
        assert!(repository.has_external_context());

        let clean = repository
            .uncommitted_index_inputs()
            .expect("uncommitted check");
        assert!(
            clean.is_empty(),
            "expected no uncommitted inputs while both repositories are clean, got {clean:?}"
        );

        std::fs::write(
            context_root.path().join(".context/requirements/req.yaml"),
            "id: REQ-1\nstatement: dirty\n",
        )
        .expect("dirty the context repository");

        let dirty = repository
            .uncommitted_index_inputs()
            .expect("uncommitted check");
        assert_eq!(
            dirty,
            vec!["context:.context/requirements/req.yaml".to_owned()]
        );
    }

    #[test]
    fn review_changes_diffs_an_external_context_repository_by_its_own_matching_revision() {
        let source_root = tempfile::tempdir().expect("temp dir");
        init_repository(source_root.path());
        std::fs::write(source_root.path().join("a.txt"), "a").expect("write file");
        run(source_root.path(), &["add", "a.txt"]);
        run(source_root.path(), &["commit", "-q", "-m", "seed source"]);
        run(source_root.path(), &["branch", "base"]);

        let context_root = tempfile::tempdir().expect("temp dir");
        init_repository(context_root.path());
        std::fs::create_dir_all(context_root.path().join(".context/requirements"))
            .expect("context directory");
        std::fs::write(
            context_root.path().join(".context/requirements/req.yaml"),
            "id: REQ-1\n",
        )
        .expect("write requirement");
        run(context_root.path(), &["add", "."]);
        run(context_root.path(), &["commit", "-q", "-m", "seed context"]);
        run(context_root.path(), &["branch", "base"]);
        std::fs::write(
            context_root.path().join(".context/requirements/req2.yaml"),
            "id: REQ-2\n",
        )
        .expect("write second requirement");
        run(context_root.path(), &["add", "."]);
        run(
            context_root.path(),
            &["commit", "-q", "-m", "add second requirement"],
        );

        let mut repository = GitRepo::discover(source_root.path()).expect("discover");
        repository.context_root = context_root.path().to_path_buf();

        let review = repository.review_changes("base").expect("review changes");

        assert_eq!(
            review.changed_context_files,
            vec![".context/requirements/req2.yaml".to_owned()]
        );
        assert!(review.source_changes.is_empty());
    }

    #[test]
    fn is_inside_work_tree_distinguishes_a_git_repository_from_a_plain_directory() {
        let repository = tempfile::tempdir().expect("temp dir");
        init_repository(repository.path());
        let plain = tempfile::tempdir().expect("temp dir");

        assert!(is_inside_work_tree(repository.path()));
        assert!(!is_inside_work_tree(plain.path()));
    }

    #[test]
    fn uncommitted_index_inputs_ignores_a_plain_folder_context_store_entirely() {
        let source_root = tempfile::tempdir().expect("temp dir");
        init_repository(source_root.path());
        std::fs::write(source_root.path().join("a.txt"), "a").expect("write file");
        run(source_root.path(), &["add", "a.txt"]);
        run(source_root.path(), &["commit", "-q", "-m", "seed source"]);

        // Never `git init`-ed: a plain directory, ADR-CTX-050's default mode.
        let context_root = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(context_root.path().join(".context/requirements"))
            .expect("context directory");
        std::fs::write(
            context_root.path().join(".context/requirements/req.yaml"),
            "id: REQ-1\n",
        )
        .expect("write requirement");

        let mut repository = GitRepo::discover(source_root.path()).expect("discover");
        repository.context_root = context_root.path().to_path_buf();
        repository.context_is_repository = is_inside_work_tree(context_root.path());
        assert!(repository.has_external_context());
        assert!(!repository.context_is_git_repository());

        let inputs = repository
            .uncommitted_index_inputs()
            .expect("uncommitted check");
        assert!(
            inputs.is_empty(),
            "a plain-folder context store has no commit gate: got {inputs:?}"
        );
    }

    #[test]
    fn uncommitted_index_inputs_and_review_handle_a_freshly_initialized_context_repository_with_no_commits_yet()
     {
        let source_root = tempfile::tempdir().expect("temp dir");
        init_repository(source_root.path());
        std::fs::write(source_root.path().join("a.txt"), "a").expect("write file");
        run(source_root.path(), &["add", "a.txt"]);
        run(source_root.path(), &["commit", "-q", "-m", "seed source"]);
        run(source_root.path(), &["branch", "base"]);

        let context_root = tempfile::tempdir().expect("temp dir");
        run(context_root.path(), &["init", "-q"]); // git-inited, zero commits: unborn HEAD

        let mut repository = GitRepo::discover(source_root.path()).expect("discover");
        repository.context_root = context_root.path().to_path_buf();
        repository.context_is_repository = is_inside_work_tree(context_root.path());
        assert!(repository.context_is_git_repository());
        assert!(!repository.context_has_commits());

        let inputs = repository
            .uncommitted_index_inputs()
            .expect("uncommitted check on an empty unborn repository");
        assert!(inputs.is_empty(), "nothing on disk yet: got {inputs:?}");

        std::fs::create_dir_all(context_root.path().join(".context/requirements"))
            .expect("context directory");
        std::fs::write(
            context_root.path().join(".context/requirements/req.yaml"),
            "id: REQ-1\n",
        )
        .expect("write requirement");

        let dirty = repository
            .uncommitted_index_inputs()
            .expect("uncommitted check");
        assert_eq!(
            dirty,
            vec!["context:.context/requirements/req.yaml".to_owned()]
        );

        // `review_changes` must not crash trying to resolve `base` in a
        // context repository that has no commits to resolve it against.
        let review = repository.review_changes("base").expect("review changes");
        assert_eq!(
            review.changed_context_files,
            vec![".context/requirements/req.yaml".to_owned()]
        );
    }
}
