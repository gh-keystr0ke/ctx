//! Resolves an optional external location for `.context/` and
//! `.ctx-candidates/`, kept in a machine-local registry that lives outside
//! any repository -- so redirecting a checkout someone else owns never
//! writes a single byte into it, not even a gitignored file (`ADR-CTX-050`).
//!
//! The registry itself must never be committed: it maps an absolute source
//! checkout path to an absolute external context path, both meaningful only
//! on this machine.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::git::GitError;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Registry {
    #[serde(default)]
    contexts: BTreeMap<String, String>,
}

fn key_for(source_root: &Path) -> String {
    source_root.display().to_string()
}

/// The registry file's location: `$CTX_CONTEXTS_FILE` when set (used by
/// tests and by anyone who wants the registry itself somewhere else),
/// otherwise `$XDG_CONFIG_HOME/ctx/contexts.toml`, otherwise
/// `$HOME/.config/ctx/contexts.toml`.
fn default_registry_path() -> Option<PathBuf> {
    if let Some(value) = env::var_os("CTX_CONTEXTS_FILE") {
        return Some(PathBuf::from(value));
    }
    if let Some(value) = env::var_os("XDG_CONFIG_HOME")
        && !value.is_empty()
    {
        return Some(PathBuf::from(value).join("ctx").join("contexts.toml"));
    }
    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join("ctx")
            .join("contexts.toml")
    })
}

fn read_registry(path: &Path) -> Result<Registry, GitError> {
    if !path.exists() {
        return Ok(Registry::default());
    }
    let content = fs::read_to_string(path).map_err(|source| GitError::Config {
        path: path.display().to_string(),
        message: source.to_string(),
    })?;
    toml::from_str(&content).map_err(|error| GitError::Config {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

fn write_registry(path: &Path, registry: &Registry) -> Result<(), GitError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| GitError::Config {
            path: parent.display().to_string(),
            message: source.to_string(),
        })?;
    }
    let content = toml::to_string_pretty(registry).map_err(|error| GitError::Config {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    fs::write(path, content).map_err(|source| GitError::Config {
        path: path.display().to_string(),
        message: source.to_string(),
    })
}

/// Looks up the external context path registered for `source_root` in the
/// registry file at `registry_path`, if any.
///
/// # Errors
///
/// Returns [`GitError`] when the registry file exists but cannot be read or
/// parsed.
pub fn resolve_at(registry_path: &Path, source_root: &Path) -> Result<Option<PathBuf>, GitError> {
    let registry = read_registry(registry_path)?;
    Ok(registry
        .contexts
        .get(&key_for(source_root))
        .map(PathBuf::from))
}

/// Registers `context_root` as the external context location for
/// `source_root` in the registry file at `registry_path`, creating the file
/// (and its parent directories) if needed.
///
/// # Errors
///
/// Returns [`GitError`] when the registry file cannot be read, parsed, or
/// written.
pub fn set_at(
    registry_path: &Path,
    source_root: &Path,
    context_root: &Path,
) -> Result<(), GitError> {
    let mut registry = read_registry(registry_path)?;
    registry
        .contexts
        .insert(key_for(source_root), context_root.display().to_string());
    write_registry(registry_path, &registry)
}

/// Looks up the external context path registered for `source_root`, if any,
/// using this machine's default registry location.
///
/// # Errors
///
/// Returns [`GitError`] when the registry file exists but cannot be read or
/// parsed. A missing `$HOME`/`$XDG_CONFIG_HOME` is treated as "no registry",
/// not an error: a plain checkout with no external context configured must
/// keep working.
pub fn resolve(source_root: &Path) -> Result<Option<PathBuf>, GitError> {
    let Some(path) = default_registry_path() else {
        return Ok(None);
    };
    resolve_at(&path, source_root)
}

/// Registers `context_root` as the external context location for
/// `source_root`, using this machine's default registry location. Returns
/// the registry file path that was written.
///
/// # Errors
///
/// Returns [`GitError`] when neither `$XDG_CONFIG_HOME` nor `$HOME` is set,
/// or when the registry file cannot be read, parsed, or written.
pub fn set(source_root: &Path, context_root: &Path) -> Result<PathBuf, GitError> {
    let path = default_registry_path().ok_or_else(|| GitError::Config {
        path: "~/.config/ctx/contexts.toml".to_owned(),
        message: "neither XDG_CONFIG_HOME nor HOME is set; cannot locate the ctx registry"
            .to_owned(),
    })?;
    set_at(&path, source_root, context_root)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolving_against_a_missing_registry_file_is_none_not_an_error() {
        let directory = tempfile::tempdir().expect("temp dir");
        let registry_path = directory.path().join("contexts.toml");

        let resolved =
            resolve_at(&registry_path, Path::new("/repos/zed")).expect("resolve succeeds");

        assert_eq!(resolved, None);
    }

    #[test]
    fn a_registered_source_root_resolves_to_its_context_root() {
        let directory = tempfile::tempdir().expect("temp dir");
        let registry_path = directory.path().join("contexts.toml");

        set_at(
            &registry_path,
            Path::new("/repos/zed"),
            Path::new("/home/ks/ctx-contexts/zed"),
        )
        .expect("set succeeds");
        let resolved =
            resolve_at(&registry_path, Path::new("/repos/zed")).expect("resolve succeeds");

        assert_eq!(resolved, Some(PathBuf::from("/home/ks/ctx-contexts/zed")));
    }

    #[test]
    fn an_unregistered_source_root_resolves_to_none_even_with_other_entries_present() {
        let directory = tempfile::tempdir().expect("temp dir");
        let registry_path = directory.path().join("contexts.toml");
        set_at(
            &registry_path,
            Path::new("/repos/zed"),
            Path::new("/home/ks/ctx-contexts/zed"),
        )
        .expect("set succeeds");

        let resolved =
            resolve_at(&registry_path, Path::new("/repos/other")).expect("resolve succeeds");

        assert_eq!(resolved, None);
    }

    #[test]
    fn setting_twice_for_the_same_source_root_overwrites_the_mapping() {
        let directory = tempfile::tempdir().expect("temp dir");
        let registry_path = directory.path().join("contexts.toml");
        set_at(
            &registry_path,
            Path::new("/repos/zed"),
            Path::new("/home/ks/ctx-contexts/zed-old"),
        )
        .expect("first set succeeds");

        set_at(
            &registry_path,
            Path::new("/repos/zed"),
            Path::new("/home/ks/ctx-contexts/zed-new"),
        )
        .expect("second set succeeds");
        let resolved =
            resolve_at(&registry_path, Path::new("/repos/zed")).expect("resolve succeeds");

        assert_eq!(
            resolved,
            Some(PathBuf::from("/home/ks/ctx-contexts/zed-new"))
        );
    }

    #[test]
    fn set_creates_missing_parent_directories() {
        let directory = tempfile::tempdir().expect("temp dir");
        let registry_path = directory.path().join("nested").join("contexts.toml");

        set_at(
            &registry_path,
            Path::new("/repos/zed"),
            Path::new("/home/ks/ctx-contexts/zed"),
        )
        .expect("set succeeds despite missing parent directories");

        assert!(registry_path.exists());
    }

    #[test]
    fn a_malformed_registry_file_is_a_reported_error_not_a_silent_empty_registry() {
        let directory = tempfile::tempdir().expect("temp dir");
        let registry_path = directory.path().join("contexts.toml");
        fs::write(&registry_path, "not valid toml [[[").expect("write malformed registry");

        let error = resolve_at(&registry_path, Path::new("/repos/zed")).unwrap_err();

        assert!(matches!(error, GitError::Config { .. }));
    }
}
