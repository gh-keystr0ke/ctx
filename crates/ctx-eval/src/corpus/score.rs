use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

use ctx_core::coverage::{RANKING_VERSION, RankedDecision};
use serde::Serialize;
use serde_json::json;

use super::{
    Result,
    manifest::{Label, Project, Role, SelectedBy, Selection},
    sample,
};

#[derive(Default, Serialize)]
struct Counts {
    top: usize,
    top_significant: usize,
    top_trivial: usize,
    trivial: usize,
    asked_trivial: usize,
    significant: usize,
}

impl Counts {
    fn add(&mut self, other: &Self) {
        self.top += other.top;
        self.top_significant += other.top_significant;
        self.top_trivial += other.top_trivial;
        self.trivial += other.trivial;
        self.asked_trivial += other.asked_trivial;
        self.significant += other.significant;
    }

    fn metrics(&self) -> serde_json::Value {
        json!({
            "precision_at_5": ratio(self.top_significant, self.top_significant + self.top_trivial),
            "trivial_false_positive_rate": ratio(self.asked_trivial, self.trivial),
            "top_evaluated": self.top_significant + self.top_trivial,
            "significant_count": self.significant,
            "trivial_count": self.trivial,
            "quota_met": self.significant >= 15 && self.trivial >= 15,
            "counts": self,
        })
    }
}

#[allow(clippy::cast_precision_loss)]
fn ratio(top: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| top as f64 / total as f64)
}

pub(super) fn score(root: &Path, cache: &Path, role: Role) -> Result<serde_json::Value> {
    let mut projects = BTreeMap::new();
    let mut languages = BTreeMap::<_, Counts>::new();
    let mut combined = Counts::default();
    let mut insufficient = BTreeMap::new();
    for project in super::read_projects(root)? {
        if project.role != role {
            continue;
        }
        let selection = super::read_selection(root, &project.name)?;
        let ranked = sample::rank(cache, &project)?;
        let (counts, missing, errors) = score_project(&project, &selection, &ranked)?;
        if (counts.top_significant + counts.top_trivial) * 5 < counts.top * 4 {
            insufficient.insert(project.name.clone(), missing);
        }
        languages
            .entry(sample::language_name(project.language))
            .or_default()
            .add(&counts);
        combined.add(&counts);
        projects.insert(
            project.name,
            json!({
                "commit": project.commit,
                "guide_version": project.guide_version,
                "metrics": counts.metrics(),
                "trivial_signals": errors,
            }),
        );
    }
    if projects.is_empty() {
        return Err(io::Error::other("no manifests for the selected role").into());
    }
    if role == Role::Gate && !insufficient.is_empty() {
        return Ok(
            json!({"status": "incomplete", "ranking_version": RANKING_VERSION, "extend": insufficient}),
        );
    }
    Ok(json!({
        "status": if insufficient.is_empty() {"complete"} else {"incomplete"},
        "ranking_version": RANKING_VERSION,
        "projects": projects,
        "languages": languages.into_iter().map(|(key, counts)| (key, counts.metrics())).collect::<BTreeMap<_, _>>(),
        "combined": combined.metrics(),
        "extend": insufficient,
    }))
}

fn score_project(
    project: &Project,
    selection: &Selection,
    ranked: &[RankedDecision],
) -> Result<(Counts, Vec<String>, BTreeMap<String, usize>)> {
    let by_anchor: BTreeMap<_, _> = ranked
        .iter()
        .map(|point| (point.anchor.to_string(), point))
        .collect();
    let labeled: BTreeMap<_, _> = project
        .points
        .iter()
        .map(|point| (point.anchor.as_str(), point))
        .collect();
    let mut counts = Counts::default();
    let mut errors = BTreeMap::new();
    for point in &project.points {
        let ranked = by_anchor
            .get(&point.anchor)
            .ok_or_else(|| io::Error::other(format!("anchor_lost: {}", point.id)))?;
        if selection
            .points
            .get(&point.id)
            .is_some_and(|value| value.selected_by == SelectedBy::Pool)
        {
            continue;
        }
        match point.label.filter(|_| point.verified_by.is_some()) {
            Some(Label::Significant) => counts.significant += 1,
            Some(Label::Trivial) => {
                counts.trivial += 1;
                counts.asked_trivial += usize::from(ranked.ask);
                if ranked.ask && project.role == Role::Tuning {
                    for feature in ranked.contributions.keys() {
                        *errors.entry((*feature).to_owned()).or_default() += 1;
                    }
                }
            }
            _ => {}
        }
    }
    let mut missing = Vec::new();
    for anchor in top_five(project, ranked) {
        counts.top += 1;
        match labeled
            .get(anchor.as_str())
            .and_then(|point| point.label.filter(|_| point.verified_by.is_some()))
        {
            Some(Label::Significant) => counts.top_significant += 1,
            Some(Label::Trivial) => counts.top_trivial += 1,
            _ => missing.push(anchor),
        }
    }
    Ok((counts, missing, errors))
}

fn top_five(project: &Project, ranked: &[RankedDecision]) -> BTreeSet<String> {
    project
        .modules
        .iter()
        .flat_map(|module| {
            ranked
                .iter()
                .filter(move |point| point.ask && Path::new(&point.file).starts_with(module))
                .take(5)
                .map(|point| point.anchor.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::sample::tests::{points, project};

    #[test]
    fn includes_pool_only_in_precision_and_detects_missing_anchors() {
        let ranked = points();
        let mut project = project();
        let mut selection = sample::freeze(&mut project, &ranked).unwrap();
        project.points[0].label = Some(Label::Significant);
        project.points[0].verified_by = Some("reviewer".to_owned());
        project.points[1].label = Some(Label::Trivial);
        project.points[1].verified_by = Some("reviewer".to_owned());
        selection
            .points
            .get_mut(&project.points[1].id)
            .unwrap()
            .selected_by = SelectedBy::Pool;
        let (counts, missing, _) = score_project(&project, &selection, &ranked).unwrap();
        assert_eq!(counts.metrics()["precision_at_5"], 0.5);
        assert_eq!(counts.trivial, 0);
        assert_eq!(counts.significant, 1);
        assert_eq!(missing.len(), 8);
        project.points[0].anchor = "removed".to_owned();
        assert!(
            score_project(&project, &selection, &ranked)
                .err()
                .unwrap()
                .to_string()
                .contains("anchor_lost")
        );
    }
}
