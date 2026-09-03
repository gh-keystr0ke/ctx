use std::path::{Path, PathBuf};

use clap::Subcommand;
use ctx_adapters::{git::GitRepo, sqlite::SqliteStore};
use ctx_app::report::ReportService;
use ctx_report::{HtmlRenderer, MarkdownRenderer, ReportRenderer};
use serde_json::json;

use crate::{Cli, CliError, database_path};

#[derive(Debug, Subcommand)]
pub enum ReportCommand {
    /// Generate a local, interactive static site.
    Html {
        /// Output directory (relative paths resolve from the repository root).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Generate deterministic Markdown suitable for a docs repository.
    Markdown {
        /// Output directory (relative paths resolve from the repository root).
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

pub fn report(cli: &Cli, git: &GitRepo, command: &ReportCommand) -> Result<(), CliError> {
    let database = database_path(git.root())?;
    let store = SqliteStore::open(&database, git.context_root())?;
    let data = ReportService::new(git, &store).build()?;
    let (format, requested, rendered) = match command {
        ReportCommand::Html { out } => ("html", out.as_deref(), HtmlRenderer.render(&data)?),
        ReportCommand::Markdown { out } => {
            ("markdown", out.as_deref(), MarkdownRenderer.render(&data)?)
        }
    };
    let output = resolve_output(git.root(), requested, format);
    let file_count = rendered.files().len() + 1;
    rendered.write_to(&output)?;
    git.ignore_local_state()?;
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "format": format,
                "path": output,
                "source_commit": data.meta.source_commit,
                "files": file_count,
            }))?
        );
    } else {
        println!(
            "Generated {format} report with {file_count} files at {}",
            output.display()
        );
    }
    Ok(())
}

fn resolve_output(root: &Path, requested: Option<&Path>, format: &str) -> PathBuf {
    requested.map_or_else(
        || root.join(".ctx").join("report").join(format),
        |path| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_paths_resolve_from_the_repository_root() {
        let root = Path::new("/workspace/repository");
        assert_eq!(
            resolve_output(root, Some(Path::new("docs/context")), "markdown"),
            root.join("docs/context")
        );
        assert_eq!(
            resolve_output(root, None, "html"),
            root.join(".ctx/report/html")
        );
    }
}
