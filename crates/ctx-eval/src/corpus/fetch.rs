use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use super::manifest::{EvidenceSource, Project};

pub(super) fn cache_root() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("CTX_CORPUS_CACHE") {
        return Ok(PathBuf::from(path));
    }
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".cache/ctx-corpus"))
        .ok_or_else(|| io::Error::other("set CTX_CORPUS_CACHE"))
}

pub(super) fn checked_out(cache: &Path, project: &Project) -> io::Result<PathBuf> {
    let checkout = destination(cache, &project.name, &project.commit)?;
    verify(&checkout, &project.commit)?;
    Ok(checkout)
}

pub(super) fn fetch(cache: &Path, project: &Project) -> io::Result<()> {
    clone_pinned(
        &destination(cache, &project.name, &project.commit)?,
        &project.repository,
        &project.commit,
    )?;
    for source in &project.evidence_sources {
        let path = evidence_checkout(cache, project, source)?;
        clone_pinned(&path, &source.repository, &source.commit)?;
    }
    Ok(())
}

pub(super) fn evidence_checkout(
    cache: &Path,
    project: &Project,
    source: &EvidenceSource,
) -> io::Result<PathBuf> {
    if project.repository == source.repository && project.commit == source.commit {
        return destination(cache, &project.name, &project.commit);
    }
    let digest = blake3::hash(source.repository.as_bytes()).to_hex();
    let commit = &source.commit;
    Ok(destination(cache, &project.name, commit)?
        .with_file_name(format!("{commit}-{}", &digest[..12])))
}

fn destination(cache: &Path, name: &str, commit: &str) -> io::Result<PathBuf> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(io::Error::other("invalid corpus project name"));
    }
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other("corpus commits require full Git hashes"));
    }
    Ok(cache.join(name).join(commit))
}

fn clone_pinned(path: &Path, repository: &str, commit: &str) -> io::Result<()> {
    if path.exists() {
        return verify(path, commit);
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing cache directory"))?;
    fs::create_dir_all(parent)?;
    let temp = tempfile::tempdir_in(parent)?;
    let checkout = temp.path().join("checkout");
    let destination = checkout
        .to_str()
        .ok_or_else(|| io::Error::other("cache path requires UTF-8"))?;
    git(
        temp.path(),
        &[
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            "--",
            repository,
            destination,
        ],
        true,
    )?;
    git(&checkout, &["checkout", "--detach", commit], true)?;
    verify(&checkout, commit)?;
    match fs::rename(&checkout, path) {
        Err(_) if path.exists() => verify(path, commit),
        result => result,
    }
}

fn verify(path: &Path, commit: &str) -> io::Result<()> {
    let head = git(path, &["rev-parse", "HEAD"], false)?;
    if head.trim() != commit {
        return Err(io::Error::other(format!(
            "{} is not at {commit}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn git(root: &Path, args: &[&str], network: bool) -> io::Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    if !network {
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_pins_a_local_repository_and_is_idempotent() {
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        git(source.path(), &["init", "-q"], false).unwrap();
        git(
            source.path(),
            &[
                "-c",
                "user.name=Eval",
                "-c",
                "user.email=eval@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
            false,
        )
        .unwrap();
        let sha = git(source.path(), &["rev-parse", "HEAD"], false).unwrap();
        let sha = sha.trim();
        let checkout = cache.path().join(sha);
        clone_pinned(&checkout, source.path().to_str().unwrap(), sha).unwrap();
        clone_pinned(&checkout, "unused", sha).unwrap();
        verify(&checkout, sha).unwrap();
    }
}
