use std::{
    path::{Path, PathBuf},
    process::Command,
};

use ctx_app::ports::{
    CommitMetadata, GitRepository, PortError, RepositoryDescriptor, ReviewChangeSet,
    ReviewRepository,
};
use ctx_core::{
    domain::{CommitOid, RepositoryId},
    indexing::FileChange,
};
use serde::Deserialize;
use thiserror::Error;

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
    path_filter: PathFilter,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ConfigFile {
    language: Option<String>,
    #[serde(default)]
    paths: ConfigPaths,
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
        let path_filter = load_path_filter(&root)?;
        Ok(Self { root, path_filter })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Keeps machine-local `SQLite` state out of Git without modifying the
    /// repository's shared `.gitignore`.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when the repository exclude file cannot be found,
    /// read, or updated.
    pub fn ignore_local_database(&self) -> Result<(), GitError> {
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
        for pattern in [".ctx/ctx.db", ".ctx/ctx.db-shm", ".ctx/ctx.db-wal"] {
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

    fn output(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
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
        let commit = format!("{revision}^{{commit}}");
        let bytes = self.output(&["rev-parse", "--verify", "--end-of-options", &commit])?;
        Ok(std::str::from_utf8(&bytes)
            .map_err(|_| GitError::InvalidUtf8)?
            .trim()
            .to_owned())
    }

    fn source_allowed(&self, path: &str) -> bool {
        is_indexable_python(path) && self.path_filter.allows(path)
    }
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
        let mut paths = parse_nul_paths(&bytes)?;
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
            parse_name_status(&bytes)?,
            &self.path_filter,
        ))
    }

    fn uncommitted_index_inputs(&self) -> Result<Vec<String>, PortError> {
        let source_bytes = self
            .output(&["diff", "--name-status", "-z", "-M", "HEAD", "--"])
            .map_err(port_error)?;
        let context_bytes = self
            .output(&["diff", "--name-only", "-z", "HEAD", "--", ".context"])
            .map_err(port_error)?;
        let untracked_bytes = self
            .output(&["ls-files", "-z", "--others", "--exclude-standard"])
            .map_err(port_error)?;
        let mut paths = change_paths(&filter_changes(
            parse_name_status(&source_bytes)?,
            &self.path_filter,
        ));
        paths.extend(parse_nul_strings(&context_bytes)?);
        paths.extend(
            parse_nul_strings(&untracked_bytes)?
                .into_iter()
                .filter(|path| self.source_allowed(path) || path.starts_with(".context/")),
        );
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

impl ReviewRepository for GitRepo {
    fn review_changes(&self, base: &str) -> Result<ReviewChangeSet, PortError> {
        let base = self.resolve_revision(base).map_err(port_error)?;
        let source_bytes = self
            .output(&["diff", "--name-status", "-z", "-M", &base, "--"])
            .map_err(port_error)?;
        let context_bytes = self
            .output(&["diff", "--name-only", "-z", &base, "--", ".context"])
            .map_err(port_error)?;
        let untracked_bytes = self
            .output(&["ls-files", "-z", "--others", "--exclude-standard"])
            .map_err(port_error)?;
        let mut source_changes =
            filter_changes(parse_name_status(&source_bytes)?, &self.path_filter);
        let mut changed_context_files = parse_nul_strings(&context_bytes)?;
        for path in parse_nul_strings(&untracked_bytes)? {
            if self.source_allowed(&path)
                && !source_changes
                    .iter()
                    .any(|change| change.current_path() == Some(path.as_str()))
            {
                source_changes.push(FileChange::Added { path: path.clone() });
            }
            if path.starts_with(".context/") {
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

fn parse_nul_paths(bytes: &[u8]) -> Result<Vec<String>, PortError> {
    let mut paths = parse_nul_strings(bytes)?;
    paths.retain(|path| is_indexable_python(path));
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

fn parse_name_status(bytes: &[u8]) -> Result<Vec<FileChange>, PortError> {
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
            add_rename(&mut changes, old_path, new_path);
            continue;
        }
        let path = fields
            .get(index)
            .ok_or_else(|| PortError::new("change is missing its path"))?;
        index += 1;
        if is_indexable_python(path) {
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

fn add_rename(changes: &mut Vec<FileChange>, old_path: &str, new_path: &str) {
    match (is_indexable_python(old_path), is_indexable_python(new_path)) {
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

fn is_indexable_python(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
        && !path.split('/').any(|component| {
            matches!(
                component,
                ".git"
                    | ".ctx"
                    | ".venv"
                    | "venv"
                    | "vendor"
                    | "generated"
                    | "build"
                    | "dist"
                    | "target"
                    | "__pycache__"
            )
        })
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

fn load_path_filter(root: &Path) -> Result<PathFilter, GitError> {
    let path = root.join(".ctx").join("config.toml");
    if !path.exists() {
        return Ok(PathFilter::default());
    }
    let content = std::fs::read_to_string(&path).map_err(|error| GitError::Config {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let config: ConfigFile = toml::from_str(&content).map_err(|error| GitError::Config {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    if config.language.as_deref().unwrap_or("python") != "python" {
        return Err(GitError::Config {
            path: path.display().to_string(),
            message: "only language = \"python\" is supported in this release".to_owned(),
        });
    }
    let defaults = PathFilter::default();
    Ok(PathFilter {
        include: if config.paths.include.is_empty() {
            defaults.include
        } else {
            config.paths.include
        },
        exclude: if config.paths.exclude.is_empty() {
            defaults.exclude
        } else {
            config.paths.exclude
        },
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_delimited_renames_and_filters_non_python() {
        let bytes = b"R100\0old.py\0new.py\0M\0README.md\0A\0src/app.py\0";
        let changes = parse_name_status(bytes).expect("valid status");
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().any(|change| matches!(
            change,
            FileChange::Renamed { old_path, new_path }
                if old_path == "old.py" && new_path == "new.py"
        )));
    }

    #[test]
    fn excludes_generated_and_virtual_environment_sources() {
        assert!(is_indexable_python("src/app.py"));
        assert!(!is_indexable_python("vendor/pkg.py"));
        assert!(!is_indexable_python(".venv/lib.py"));
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
}
