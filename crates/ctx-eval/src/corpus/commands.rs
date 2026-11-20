use std::{collections::BTreeSet, io, path::Path};

use clap::{Parser, Subcommand};
use serde_json::json;

use super::{
    Result, check, fetch,
    manifest::{Label, Role, SelectedBy},
    read_project, read_selection, sample, score, write_yaml,
};

#[derive(Debug, Parser)]
#[command(
    name = "ctx-corpus",
    version,
    about = "Build and score the decision point corpus"
)]
pub struct Cli {
    #[arg(long, global = true)]
    root: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Check,
    Fetch {
        project: String,
    },
    Sample {
        project: String,
        #[arg(long, num_args = 0..)]
        extend: Option<Vec<String>>,
        #[arg(long, requires = "extend")]
        fill: bool,
        #[arg(long, requires = "fill", value_parser = clap::value_parser!(u32).range(60..))]
        fill_cap: Option<u32>,
    },
    Draft {
        project: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 30)]
        max_points: usize,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
    },
    Review {
        project: String,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        second: bool,
        #[arg(long, conflicts_with = "second")]
        point: Option<String>,
    },
    Agreement {
        project: String,
        #[arg(long)]
        min: Option<f64>,
    },
    Score {
        #[arg(long, conflicts_with = "gate", required_unless_present = "gate")]
        tuning: bool,
        #[arg(long)]
        gate: bool,
    },
}

impl Cli {
    /// Runs one corpus command.
    ///
    /// # Errors
    /// Returns the underlying I/O, validation, or indexing error.
    pub fn run(self) -> Result<()> {
        let default = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
        let root = self.root.as_deref().unwrap_or(&default);
        match self.command {
            Command::Check => check(root).map_err(|error| io::Error::other(error).into()),
            Command::Fetch { project } => Ok(fetch::fetch(
                &fetch::cache_root()?,
                &read_project(root, &project)?,
            )?),
            Command::Sample {
                project,
                extend,
                fill,
                fill_cap,
            } => sample_project(root, &project, extend.as_deref(), fill, fill_cap),
            Command::Draft {
                project,
                agent,
                model,
                max_points,
                timeout_ms,
            } => super::draft::draft(
                root,
                &project,
                &agent,
                model.as_deref(),
                max_points,
                std::time::Duration::from_millis(timeout_ms),
            ),
            Command::Review {
                project,
                reviewer,
                second,
                point,
            } => super::review::review(root, &project, &reviewer, second, point.as_deref()),
            Command::Agreement { project, min } => {
                let report = super::agreement::agreement(&read_project(root, &project)?);
                if report.n < 10 {
                    eprintln!("Agreement is based on fewer than ten points.");
                }
                println!("{}", serde_json::to_string_pretty(&report)?);
                if min.is_some_and(|threshold| report.kappa.is_none_or(|value| value < threshold)) {
                    return Err(io::Error::other("agreement below the requested minimum").into());
                }
                Ok(())
            }
            Command::Score { gate, .. } => {
                let report = score::score(
                    root,
                    &fetch::cache_root()?,
                    if gate { Role::Gate } else { Role::Tuning },
                )?;
                println!("{}", serde_json::to_string_pretty(&report)?);
                if report["status"] == "incomplete" {
                    return Err(io::Error::other("precision pool is incomplete").into());
                }
                Ok(())
            }
        }
    }
}

fn sample_project(
    root: &Path,
    name: &str,
    extend: Option<&[String]>,
    fill: bool,
    fill_cap: Option<u32>,
) -> Result<()> {
    let mut project = read_project(root, name)?;
    let selection = if let Some(anchors) = extend {
        let mut selection = read_selection(root, name)?;
        if fill {
            if !anchors.is_empty() {
                return Err(
                    io::Error::other("--fill takes its order from the selection file").into(),
                );
            }
            let cap = fill_cap.map_or(sample::DEFAULT_FILL_CAP, |cap| cap as usize);
            if cap > sample::DEFAULT_FILL_CAP
                && selection.fill_order.len() < cap.saturating_sub(project.sample.size)
            {
                let ranked = sample::rank_with(
                    &fetch::cache_root()?,
                    &project,
                    ctx_core::coverage::RankingVersion::Initial,
                )?;
                sample::extend_frozen_order(&mut project, &mut selection, &ranked, cap)?;
            }
            extend_fill(&mut project, &mut selection, cap)?;
        } else {
            extend_pool(&mut project, &mut selection, anchors)?;
        }
        selection
    } else {
        if !project.points.is_empty() {
            return Err(io::Error::other("sample already frozen").into());
        }
        let ranked = sample::rank_with(
            &fetch::cache_root()?,
            &project,
            ctx_core::coverage::RankingVersion::Initial,
        )?;
        sample::freeze(&mut project, &ranked)?
    };
    write_yaml(&super::project_path(root, name)?, &project)?;
    write_yaml(&super::selection_path(root, name)?, &selection)?;
    println!(
        "{}",
        json!({"project": name, "points": project.points.len()})
    );
    Ok(())
}

fn extend_fill(
    project: &mut super::manifest::Project,
    selection: &mut super::manifest::Selection,
    cap: usize,
) -> Result<()> {
    let mut significant = 0;
    let mut trivial = 0;
    for point in &project.points {
        if selection
            .points
            .get(&point.id)
            .is_some_and(|entry| entry.selected_by == SelectedBy::Pool)
        {
            continue;
        }
        match point.label.filter(|_| point.verified_by.is_some()) {
            Some(Label::Significant) => significant += 1,
            Some(Label::Trivial) => trivial += 1,
            Some(Label::Unclear) => {}
            None => {
                return Err(io::Error::other(
                    "review the current sample before adding another fill batch",
                )
                .into());
            }
        }
    }
    if significant >= 15 && trivial >= 15 {
        return Ok(());
    }
    let existing: BTreeSet<_> = project
        .points
        .iter()
        .map(|point| point.anchor.clone())
        .collect();
    let order = selection
        .fill_order
        .iter()
        .filter(|anchor| !existing.contains(*anchor))
        .take(10.min(cap.saturating_sub(project.points.len())))
        .cloned()
        .collect::<Vec<_>>();
    for anchor in order {
        let point = selection
            .fill_points
            .get(&anchor)
            .ok_or_else(|| io::Error::other("fill point metadata is missing"))?
            .clone();
        sample::append(project, selection, anchor, point, SelectedBy::Fill);
    }
    Ok(())
}

fn extend_pool(
    project: &mut super::manifest::Project,
    selection: &mut super::manifest::Selection,
    anchors: &[String],
) -> Result<()> {
    if anchors.is_empty() {
        return Err(io::Error::other("pass pool anchors or --fill").into());
    }
    let ranked = sample::rank(&fetch::cache_root()?, project)?;
    for anchor in anchors {
        if project.points.iter().any(|point| point.anchor == *anchor) {
            continue;
        }
        let (index, point) = ranked
            .iter()
            .enumerate()
            .find(|(_, point)| point.anchor.to_string() == *anchor)
            .ok_or_else(|| io::Error::other(format!("anchor_lost: {anchor}")))?;
        sample::append(
            project,
            selection,
            anchor.clone(),
            sample::metadata(point, index + 1),
            SelectedBy::Pool,
        );
    }
    Ok(())
}
