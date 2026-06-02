use std::{
    path::{Path, PathBuf},
    process::Command,
};

use ctx_app::ports::{CommitMetadata, GitRepository, PortError, RepositoryDescriptor};
use ctx_core::{
    domain::{CommitOid, RepositoryId},
    indexing::FileChange,
};
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
}

pub struct GitRepo {
    root: PathBuf,
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
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
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
        parse_nul_paths(&bytes)
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
        parse_name_status(&bytes)
    }
}

fn parse_nul_paths(bytes: &[u8]) -> Result<Vec<String>, PortError> {
    let mut paths = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| {
            std::str::from_utf8(part)
                .map(str::to_owned)
                .map_err(|_| PortError::new("source path is not valid UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| is_indexable_python(path));
    paths.sort();
    Ok(paths)
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
}
