use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

use ctx_adapters::{git::GitRepo, sqlite::SqliteStore, worktree::WorktreeSnapshot};
use ctx_app::{ports::GitRepository, status::index_freshness};
use serde_json::json;

use super::link_repairs;
use crate::{Cli, CliError, database_path};

pub(super) fn apply(cli: &Cli, git: &GitRepo, commit: bool) -> Result<(), CliError> {
    let head = git.head()?;
    let store = SqliteStore::open_preview(&database_path(git.root())?, git.context_root())?;
    let repository = git.descriptor()?;
    let freshness = index_freshness(git, &store, &repository.id)?;
    if freshness.commit.as_ref() != Some(&head.oid) {
        return Err(io::Error::other("index HEAD before applying safe repairs").into());
    }

    let mut snapshot = WorktreeSnapshot::read(git)?;
    let issues = snapshot.issues();
    let links = link_repairs(&snapshot, &issues, &store, &repository.id)?;
    let config = super::config::Config::read(git)?;
    let mut repairs = BTreeMap::<&str, Vec<_>>::new();
    for link in &links {
        if link.replacement.is_some() && config.safe(link.test) {
            repairs.entry(link.path).or_default().push(link);
        }
    }

    let mut edits = Vec::new();
    for (path, links) in repairs {
        edits.push(prepare_file(git, &mut snapshot, path, &links)?);
    }
    let mut branch = None;
    if !edits.is_empty() {
        let remaining = snapshot.issues();
        let every_issue_explained = remaining.iter().all(|issue| {
            issues.iter().any(|before| {
                before.path == issue.path
                    && before.locator == issue.locator
                    && before.message == issue.message
            })
        });
        if remaining.len() >= issues.len() || !every_issue_explained {
            return Err(
                io::Error::other("repair preview did not reduce mapping issues safely").into(),
            );
        }
        let temp = tempfile::NamedTempFile::new()?;
        let mut preview = SqliteStore::open(temp.path(), git.context_root())?;
        let report = snapshot.persist_preview(git, head.clone(), &mut preview)?;
        if links.iter().any(|link| {
            link.replacement
                .as_ref()
                .is_some_and(|target| report.business_context.unresolved_symbols.contains(target))
        }) {
            return Err(io::Error::other("repair preview did not resolve a replacement").into());
        }
        if git.head_oid()? != head.oid {
            return Err(io::Error::other("HEAD changed during repair").into());
        }
        if commit {
            branch = Some(prepare_commit(git.context_root())?);
        }
        write_edits(&edits)?;
        if branch.is_some() {
            commit_edits(git.context_root(), &edits)?;
        }
    }
    let applied = links
        .iter()
        .filter(|link| link.replacement.is_some() && config.safe(link.test))
        .collect::<Vec<_>>();
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"applied": applied, "branch": branch}))?
        );
    } else {
        println!("Applied {} safe repair(s).", applied.len());
    }
    Ok(())
}

fn prepare_commit(root: &Path) -> io::Result<String> {
    let status = run_git(root, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        return Err(io::Error::other(
            "repair --commit requires a clean context worktree",
        ));
    }
    let branch = format!("ctx/repair-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    run_git(root, &["switch", "-c", &branch])?;
    Ok(branch)
}

fn commit_edits(root: &Path, edits: &[Edit]) -> io::Result<()> {
    let root = fs::canonicalize(root)?;
    let paths = edits
        .iter()
        .map(|edit| {
            edit.path
                .strip_prefix(&root)
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| io::Error::other("repair path is outside the context worktree"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut args = vec![
        "commit",
        "--only",
        "-m",
        "docs(context): repair symbol mappings",
        "--",
    ];
    args.extend(paths);
    run_git(&root, &args)?;
    Ok(())
}

fn run_git(root: &Path, args: &[&str]) -> io::Result<String> {
    let output = Command::new("git").current_dir(root).args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(io::Error::other)
}

pub(super) struct Edit {
    pub path: PathBuf,
    pub original: String,
    pub patched: String,
}

fn prepare_file(
    git: &GitRepo,
    snapshot: &mut WorktreeSnapshot,
    relative: &str,
    repairs: &[&super::LinkRepair<'_>],
) -> io::Result<Edit> {
    let path = git.context_root().join(relative);
    let canonical = fs::canonicalize(&path)?;
    if !canonical.starts_with(fs::canonicalize(git.context_root().join(".context"))?)
        || fs::symlink_metadata(&path)?.is_symlink()
    {
        return Err(io::Error::other(
            "context repair path escapes the context directory",
        ));
    }
    let original = fs::read_to_string(&canonical)?;
    let mut patched = original.clone();
    let mut expected = BTreeMap::new();
    for repair in repairs {
        let replacement = repair.replacement.as_deref().unwrap_or(repair.original);
        expected.insert(repair.locator, (repair.original, replacement));
        patched = patch_scalar(&patched, repair, replacement)?;
    }
    snapshot.preview_link_edits(git.context_root(), &path, &patched, &expected)?;
    Ok(Edit {
        path: canonical,
        original,
        patched,
    })
}

fn patch_scalar(
    contents: &str,
    repair: &super::LinkRepair<'_>,
    replacement: &str,
) -> io::Result<String> {
    let mut output = String::new();
    let mut found = 0;
    for line in contents.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = &line[..line.len() - trimmed.len()];
        let start = trimmed
            .strip_prefix("- symbol: ")
            .or_else(|| trimmed.strip_prefix("- "));
        if let Some((offset, length)) =
            start.and_then(|text| scalar_position(text, repair.original))
        {
            found += 1;
            let index = line.len() - start.unwrap_or("").len() + offset;
            let ending = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
            let rule = if repair.test {
                "test-rename"
            } else {
                "identity-rename"
            };
            let commit = repair.commit.as_deref().unwrap_or("");
            write!(output, "{indent}# repair:{rule} {commit}{ending}").map_err(io::Error::other)?;
            output.push_str(&line[..index]);
            output.push_str(replacement);
            output.push_str(&line[index + length..]);
        } else {
            output.push_str(line);
        }
    }
    if found != 1 {
        return Err(io::Error::other(format!(
            "cannot safely edit {}#{}",
            repair.path, repair.locator
        )));
    }
    Ok(output)
}

fn scalar_position(text: &str, original: &str) -> Option<(usize, usize)> {
    let (text, quote, offset) = match text.as_bytes().first() {
        Some(quote @ (b'\'' | b'"')) => (&text[1..], Some(char::from(*quote)), 1),
        _ => (text, None, 0),
    };
    let mut remaining = text.strip_prefix(original)?;
    if let Some(quote) = quote {
        remaining = remaining.strip_prefix(quote)?;
    }
    let suffix = remaining.trim_start();
    (suffix.is_empty() || (suffix.starts_with('#') && suffix.len() < remaining.len()))
        .then_some((offset, original.len()))
}

pub(super) fn write_edits(edits: &[Edit]) -> io::Result<()> {
    for (index, edit) in edits.iter().enumerate() {
        if let Err(error) = persist_context_edit(&edit.path, &edit.original, &edit.patched) {
            for previous in edits[..index].iter().rev() {
                persist_context_edit(&previous.path, &previous.patched, &previous.original)
                    .map_err(|rollback| {
                        io::Error::other(format!(
                            "{error}; rollback failed on {}: {rollback}",
                            previous.path.display()
                        ))
                    })?;
            }
            return Err(error);
        }
    }
    Ok(())
}

fn persist_context_edit(path: &Path, before: &str, after: &str) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("missing context directory"))?;
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    temp.write_all(after.as_bytes())?;
    temp.as_file()
        .set_permissions(fs::metadata(path)?.permissions())?;
    if fs::read_to_string(path)? != before {
        return Err(io::Error::other(format!(
            "{} changed during repair",
            path.display()
        )));
    }
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}
