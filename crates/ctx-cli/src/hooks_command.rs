use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use clap::Subcommand;
use serde_json::json;

use crate::{Cli, CliError, GitRepo};

const OPEN: &str = "# >>> ctx";
const CLOSE: &str = "# <<< ctx";
const POST_HOOKS: &[&str] = &["post-commit", "post-merge", "post-checkout", "post-rewrite"];

#[derive(Debug, Subcommand)]
pub(crate) enum HooksCommand {
    /// Install background indexing hooks.
    Install,
    /// Remove ctx's managed blocks.
    Uninstall,
}

pub(crate) fn run(cli: &Cli, git: &GitRepo, command: &HooksCommand) -> Result<(), CliError> {
    let binary = std::env::current_exe()?;
    let install = matches!(command, HooksCommand::Install);
    let mut changed = Vec::new();
    for name in POST_HOOKS
        .iter()
        .copied()
        .chain((!install).then_some("pre-commit"))
    {
        let path = hook_path(git, name)?;
        if edit_hook(
            &path,
            install
                .then(|| post_body(&binary, name == "post-commit"))
                .as_deref(),
        )? {
            changed.push(path);
        }
    }
    if cli.json {
        println!("{}", json!({"changed_files": changed}));
    } else {
        for path in changed {
            println!("{}", path.display());
        }
    }
    Ok(())
}

pub(crate) fn hook_path(git: &GitRepo, name: &str) -> Result<PathBuf, io::Error> {
    let output = Command::new("git")
        .current_dir(git.root())
        .args(["rev-parse", "--git-path", &format!("hooks/{name}")])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr)));
    }
    let path = String::from_utf8(output.stdout).map_err(io::Error::other)?;
    Ok(git.root().join(path.trim_end_matches('\n')))
}

pub(crate) fn shell_quote(path: &Path) -> String {
    shell_quote_text(&path.to_string_lossy())
}

pub(crate) fn shell_quote_text(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

fn post_body(binary: &Path, repair_plan: bool) -> String {
    let binary = shell_quote(binary);
    let repair = if repair_plan {
        format!(
            "\n  umask 077\n  repair_tmp=.ctx/logs/repair-plan-$$.json\n  if {binary} repair plan --json >\"$repair_tmp\" 2>>.ctx/logs/hooks.log; then\n    mv \"$repair_tmp\" .ctx/logs/repair-plan.json\n  else\n    rm -f \"$repair_tmp\"\n  fi"
        )
    } else {
        String::new()
    };
    format!(
        "(\n  unset GIT_INDEX_FILE\n  mkdir -p .ctx/logs && {binary} index --quiet >>.ctx/logs/hooks.log 2>&1{repair}\n) </dev/null >/dev/null 2>&1 &"
    )
}

fn managed_range(text: &str) -> Result<Option<std::ops::Range<usize>>, io::Error> {
    let mut start = None;
    let mut range = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        match line.trim_end_matches(['\r', '\n']) {
            OPEN if start.is_none() && range.is_none() => start = Some(offset),
            CLOSE if start.is_some() && range.is_none() => {
                range = Some(start.take().expect("opening marker")..offset + line.len());
            }
            OPEN | CLOSE => return Err(io::Error::other("invalid ctx hook markers")),
            _ => {}
        }
        offset += line.len();
    }
    if start.is_some() {
        return Err(io::Error::other("ctx hook has no closing marker"));
    }
    Ok(range)
}

pub(crate) fn plan_hook(existing: &str, body: Option<&str>) -> Result<String, io::Error> {
    let mut result = existing.to_owned();
    if let Some(range) = managed_range(existing)? {
        result.replace_range(
            range,
            &body.map_or_else(String::new, |body| format!("{OPEN}\n{body}\n{CLOSE}\n")),
        );
    } else if let Some(body) = body {
        if result.is_empty() {
            result.push_str("#!/bin/sh\n");
        }
        let offset = if result.starts_with("#!") {
            let Some(line) = result.lines().next() else {
                unreachable!()
            };
            if !shell_shebang(line) {
                return Err(io::Error::other(
                    "existing hook must use a POSIX-compatible shell",
                ));
            }
            if !result.contains('\n') {
                result.push('\n');
            }
            result.find('\n').expect("shebang line") + 1
        } else {
            0
        };
        result.insert_str(offset, &format!("{OPEN}\n{body}\n{CLOSE}\n"));
    }
    Ok(result)
}

fn shell_shebang(line: &str) -> bool {
    line.trim_start_matches("#!")
        .split_whitespace()
        .any(|word| {
            matches!(
                word.rsplit('/').next(),
                Some("sh" | "bash" | "dash" | "zsh" | "ksh")
            )
        })
}

fn edit_hook(path: &Path, body: Option<&str>) -> Result<bool, io::Error> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let original = std::str::from_utf8(&bytes);
    let preserved = path.with_extension("ctx-original");
    let executable_wrapper = r#"#!/bin/sh
exec "$0.ctx-original" "$@"
"#;
    let inactive_wrapper = "#!/bin/sh\n# ctx preserves the inactive original hook.\n";
    if let Ok(original) = original {
        let can_edit = original
            .lines()
            .next()
            .is_none_or(|line| !line.starts_with("#!") || shell_shebang(line))
            && (bytes.is_empty() || file_is_executable(path)?);
        if body.is_none() || can_edit {
            let desired = plan_hook(original, body)?;
            if original == desired {
                return Ok(false);
            }
            if body.is_none()
                && matches!(desired.as_str(), wrapper if wrapper == executable_wrapper || wrapper == inactive_wrapper)
                && preserved.is_file()
            {
                fs::rename(preserved, path)?;
            } else {
                write_file(path, &desired, true)?;
            }
            return Ok(true);
        }
    }
    let Some(body) = body else {
        return Ok(false);
    };
    if preserved.exists() {
        return Err(io::Error::other(format!(
            "hook backup already exists: {}",
            preserved.display()
        )));
    }
    let wrapper = if file_is_executable(path)? {
        executable_wrapper
    } else {
        inactive_wrapper
    };
    fs::copy(path, &preserved)?;
    if let Err(error) = write_file(path, &plan_hook(wrapper, Some(body))?, true) {
        fs::remove_file(&preserved)?;
        return Err(error);
    }
    Ok(true)
}

fn file_is_executable(path: &Path) -> Result<bool, io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        Ok(path.metadata()?.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        Ok(path.is_file())
    }
}

pub(crate) fn read_missing(path: &Path) -> Result<String, io::Error> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

pub(crate) fn write_file(path: &Path, contents: &str, executable: bool) -> Result<(), io::Error> {
    use std::io::Write as _;

    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))?;
    fs::create_dir_all(directory)?;
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    temp.write_all(contents.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let old = path
            .metadata()
            .ok()
            .map_or(0o644, |metadata| metadata.permissions().mode());
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(if executable {
                old | 0o111
            } else {
                old
            }))?;
    }
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_block_round_trips_a_user_hook() {
        let original = "#!/bin/bash\nprintf '%s' custom\nexit 0\n";
        let first = plan_hook(original, Some("ctx index --quiet")).expect("insert block");
        assert_eq!(
            plan_hook(&first, Some("ctx index --quiet")).expect("add again"),
            first
        );
        assert_eq!(plan_hook(&first, None).expect("remove block"), original);
        assert!(first.find(OPEN) < first.find("exit 0"));
    }
}
