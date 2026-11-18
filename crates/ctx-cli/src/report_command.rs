use std::path::{Path, PathBuf};

use clap::Subcommand;
use ctx_adapters::{coverage_store::CoverageStore, git::GitRepo, sqlite::SqliteStore};
use ctx_app::{
    ports::{GitRepository, GraphStore},
    report::{ReportData, ReportService},
};
use ctx_report::{HtmlRenderer, MarkdownRenderer, RenderedReport, ReportRenderer};
use serde_json::json;

use crate::{Cli, CliError, database_path};

#[derive(Debug, Subcommand)]
pub enum ReportCommand {
    /// Generate a local, interactive static site.
    Html {
        /// Render the isolated working-tree preview.
        #[arg(long, conflicts_with = "refresh")]
        preview: bool,
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
    let preview = matches!(command, ReportCommand::Html { preview: true, .. });
    let store = if preview {
        crate::worktree_command::open_preview(git)?
    } else {
        SqliteStore::open(&database_path(git.root())?, git.context_root())?
    };
    let service = ReportService::new(git, &store);
    let data = if preview {
        service.build_preview()?
    } else {
        service.build()?
    };
    let (format, requested, rendered) = match command {
        ReportCommand::Html { out, .. } => (
            "html",
            out.as_deref(),
            html_with_coverage(git, &store, &data)?,
        ),
        ReportCommand::Markdown { out } => {
            ("markdown", out.as_deref(), MarkdownRenderer.render(&data)?)
        }
    };
    let output = resolve_output(
        git.root(),
        requested,
        if preview { "preview" } else { format },
    );
    let file_count = rendered.files().len() + 1;
    rendered.write_to(&output)?;
    if !preview {
        git.ignore_local_state()?;
    }
    if cli.json {
        let mut payload = json!({
            "ok": true, "format": format, "path": output,
            "source_commit": data.meta.source_commit, "files": file_count,
        });
        if preview {
            crate::worktree_command::mark_preview(&mut payload);
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        if preview {
            println!("{}", ctx_adapters::worktree::PREVIEW_NOTICE);
        }
        println!(
            "Generated {format} report with {file_count} files at {}",
            output.display()
        );
    }
    Ok(())
}

fn html_with_coverage(
    git: &GitRepo,
    store: &SqliteStore,
    data: &ReportData,
) -> Result<RenderedReport, CliError> {
    let graph = store.load_graph(&git.descriptor()?.id)?;
    let selected = graph.nodes.keys().cloned().collect();
    let coverage = ctx_core::coverage::analyze_coverage(&graph, &selected);
    let records = CoverageStore::new(git.context_root()).read()?;
    let gaps = ctx_core::coverage::gap_report(&graph, &coverage.decisions, &records, &selected);
    Ok(HtmlRenderer.render_with_coverage(data, Some((&coverage, &gaps)))?)
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
