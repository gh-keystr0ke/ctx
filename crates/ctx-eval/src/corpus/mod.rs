//! Tools for the pinned, externally sourced decision point corpus.

mod agreement;
mod check;
mod fetch;
mod manifest;
mod review;
mod sample;

pub use check::check;

mod commands;
mod score;

pub use commands::Cli;

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use manifest::{Project, Selection};
use serde::{Serialize, de::DeserializeOwned};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn project_path(root: &Path, name: &str) -> io::Result<PathBuf> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(io::Error::other("invalid corpus project name"));
    }
    Ok(root.join("projects").join(format!("{name}.yaml")))
}

fn selection_path(root: &Path, name: &str) -> io::Result<PathBuf> {
    Ok(project_path(root, name)?.with_extension("selection.yaml"))
}

fn read_project(root: &Path, name: &str) -> Result<Project> {
    let project: Project = read_yaml(&project_path(root, name)?)?;
    if project.name != name {
        return Err(io::Error::other("project name does not match the manifest filename").into());
    }
    Ok(project)
}

fn read_selection(root: &Path, name: &str) -> Result<Selection> {
    read_yaml(&selection_path(root, name)?)
}

fn read_projects(root: &Path) -> Result<Vec<Project>> {
    let mut paths = fs::read_dir(root.join("projects"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    paths
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let stem = name.strip_suffix(".yaml")?;
            (!stem.ends_with(".selection")).then_some(stem)
        })
        .map(|name| read_project(root, name))
        .collect()
}

fn read_yaml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(yaml_serde::from_str(&fs::read_to_string(path)?)?)
}

fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing output directory"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let yaml = yaml_serde::to_string(value)?;
    temporary.write_all(yaml.as_bytes())?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}
