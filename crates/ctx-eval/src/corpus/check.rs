use super::manifest::{EvidenceKind, Label, Project, Selection};
use serde::de::DeserializeOwned;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Validates the labeling guide, project manifests, and selection files.
///
/// # Errors
/// Returns the path and reason for the first unreadable or invalid file.
pub fn check(root: &Path) -> Result<(), String> {
    let guide = read(&root.join("LABELING.md"))?;
    let version = guide
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("Version: "))
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or("LABELING.md must start with a version")?;
    let directory = root.join("projects");
    let mut paths = fs::read_dir(&directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(project) = name.strip_suffix(".selection.yaml") {
            let manifest: Project = parse(&directory.join(format!("{project}.yaml")))?;
            let selection: Selection = parse(&path)?;
            check_selection(&manifest, &selection)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml"))
        {
            let manifest: Project = parse(&path)?;
            check_project(&manifest, version)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn parse<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    yaml_serde::from_str(&read(path)?).map_err(|error| format!("{}: {error}", path.display()))
}

fn check_project(project: &Project, guide_version: u32) -> Result<(), String> {
    if project.schema != 1 || project.guide_version != guide_version {
        return Err("invalid schema or labeling guide version".to_owned());
    }
    let mut ids = BTreeSet::new();
    for point in &project.points {
        if !ids.insert(&point.id) {
            return Err(format!("duplicate point ID {}", point.id));
        }
        if point.label == Some(Label::Significant)
            && !point.evidence.iter().any(|evidence| {
                matches!(
                    evidence.kind,
                    EvidenceKind::Docs | EvidenceKind::Spec | EvidenceKind::Issue
                )
            })
        {
            return Err(format!("significant point {} requires evidence", point.id));
        }
        if point.label == Some(Label::Trivial) && point.category.is_none() {
            return Err(format!("trivial point {} requires a category", point.id));
        }
    }
    Ok(())
}

fn check_selection(project: &Project, selection: &Selection) -> Result<(), String> {
    if selection.schema != 1 {
        return Err("invalid selection schema".to_owned());
    }
    let ids: BTreeSet<_> = project.points.iter().map(|point| &point.id).collect();
    if let Some(id) = selection.points.keys().find(|id| !ids.contains(id)) {
        return Err(format!("selection references unknown point {id}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r"
schema: 1
guide_version: 1
project: example
language: python
role: tuning
repository: example
release: v1
commit: example
evidence_sources: []
reference_patterns: []
modules: [example.py]
excluded_paths: []
sample: { seed: 42, size: 30, ranking_version: v0 }
points:
  - { id: example-0001, anchor: example, location: 'example.py:1', label: trivial, category: validation }
";

    #[test]
    fn requires_the_current_guide_and_schema() {
        let mut project: Project = yaml_serde::from_str(MANIFEST).unwrap();
        assert!(check_project(&project, 1).is_ok());
        assert!(check_project(&project, 2).is_err());
        project.schema = 2;
        assert!(check_project(&project, 1).is_err());
    }

    #[test]
    fn enforces_unique_ids_and_label_requirements() {
        let mut project: Project = yaml_serde::from_str(MANIFEST).unwrap();
        project.points[0].category = None;
        assert!(check_project(&project, 1).unwrap_err().contains("category"));
        project.points[0].label = Some(Label::Significant);
        assert!(check_project(&project, 1).unwrap_err().contains("evidence"));
        project.points[0].label = Some(Label::Unclear);
        let other: Project = yaml_serde::from_str(MANIFEST).unwrap();
        project.points.extend(other.points);
        assert!(
            check_project(&project, 1)
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn rejects_ranking_fields_on_project_points() {
        let yaml = MANIFEST.replace("label: trivial", "label: trivial, score: 9");
        assert!(yaml_serde::from_str::<Project>(&yaml).is_err());
    }

    #[test]
    fn rejects_selection_entries_outside_the_project() {
        let project = yaml_serde::from_str(MANIFEST).unwrap();
        let yaml = "schema: 1\npoints: { unknown: { selected_by: top, rank: 1, score: 8, contributions: {} } }";
        let selection = yaml_serde::from_str(yaml).unwrap();
        assert!(
            check_selection(&project, &selection)
                .unwrap_err()
                .contains("unknown")
        );
    }
}
