//! Deterministic static renderers for [`ctx_app::report::ReportData`].

mod common;
mod html;
mod markdown;

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

pub use html::HtmlRenderer;
pub use markdown::MarkdownRenderer;
use tempfile::Builder;
use thiserror::Error;

use ctx_app::report::ReportData;

const MANIFEST: &str = ".ctx-report.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportFormat {
    Html,
    Markdown,
}

impl ReportFormat {
    const fn name(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Markdown => "markdown",
        }
    }
}

pub trait ReportRenderer {
    /// Produces every output file in memory without touching the filesystem.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] if serializing embedded report data fails.
    fn render(&self, data: &ReportData) -> Result<RenderedReport, RenderError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedReport {
    format: ReportFormat,
    source_commit: String,
    files: BTreeMap<PathBuf, String>,
}

impl RenderedReport {
    fn new(format: ReportFormat, source_commit: &str) -> Self {
        Self {
            format,
            source_commit: source_commit.to_owned(),
            files: BTreeMap::new(),
        }
    }

    fn insert(&mut self, path: impl Into<PathBuf>, contents: String) {
        self.files.insert(path.into(), contents);
    }

    #[must_use]
    pub fn files(&self) -> &BTreeMap<PathBuf, String> {
        &self.files
    }

    /// Replaces one previously generated report directory with this complete
    /// report. An existing unmarked directory is never overwritten.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] for an unsafe destination or any filesystem
    /// operation that prevents a complete staged report from being installed.
    pub fn write_to(&self, output: &Path) -> Result<(), RenderError> {
        validate_output(output)?;
        if output.exists() && (!output.is_dir() || !output.join(MANIFEST).is_file()) {
            return Err(RenderError::UnmanagedDestination(output.to_path_buf()));
        }
        let parent = output.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| RenderError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let staging = Builder::new()
            .prefix(".ctx-report-")
            .tempdir_in(parent)
            .map_err(|source| RenderError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        let staged_report = staging.path().join("report");
        fs::create_dir(&staged_report).map_err(|source| RenderError::Io {
            path: staged_report.clone(),
            source,
        })?;
        for (relative, contents) in &self.files {
            validate_relative_path(relative)?;
            let target = staged_report.join(relative);
            if let Some(directory) = target.parent() {
                fs::create_dir_all(directory).map_err(|source| RenderError::Io {
                    path: directory.to_path_buf(),
                    source,
                })?;
            }
            fs::write(&target, contents).map_err(|source| RenderError::Io {
                path: target,
                source,
            })?;
        }
        let manifest_path = staged_report.join(MANIFEST);
        fs::write(&manifest_path, self.manifest()).map_err(|source| RenderError::Io {
            path: manifest_path,
            source,
        })?;

        if output.exists() {
            let backup = staging.path().join("previous");
            fs::rename(output, &backup).map_err(|source| RenderError::Io {
                path: output.to_path_buf(),
                source,
            })?;
            if let Err(source) = fs::rename(&staged_report, output) {
                let _ = fs::rename(&backup, output);
                return Err(RenderError::Io {
                    path: output.to_path_buf(),
                    source,
                });
            }
        } else {
            fs::rename(&staged_report, output).map_err(|source| RenderError::Io {
                path: output.to_path_buf(),
                source,
            })?;
        }
        Ok(())
    }

    fn manifest(&self) -> String {
        format!(
            "{{\n  \"schema_version\": 1,\n  \"format\": \"{}\",\n  \"source_commit\": {}\n}}\n",
            self.format.name(),
            serde_json::to_string(&self.source_commit).expect("a string always serializes")
        )
    }
}

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("could not serialize report data: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("report destination must name a directory, not '{0}'")]
    InvalidDestination(PathBuf),
    #[error("refusing to replace unmanaged report destination '{0}'")]
    UnmanagedDestination(PathBuf),
    #[error("report contains an unsafe output path '{0}'")]
    UnsafeOutputPath(PathBuf),
    #[error("could not write report path '{path}': {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn validate_output(output: &Path) -> Result<(), RenderError> {
    if output.as_os_str().is_empty() || output.file_name().is_none() {
        return Err(RenderError::InvalidDestination(output.to_path_buf()));
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<(), RenderError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RenderError::UnsafeOutputPath(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmanaged_output_is_never_replaced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let output = temp.path().join("report");
        fs::create_dir(&output).expect("output");
        fs::write(output.join("keep.txt"), "mine").expect("user file");
        let report = RenderedReport::new(ReportFormat::Markdown, "abc");

        let error = report.write_to(&output).expect_err("must refuse");

        assert!(matches!(error, RenderError::UnmanagedDestination(_)));
        assert_eq!(fs::read_to_string(output.join("keep.txt")).unwrap(), "mine");
    }

    #[test]
    fn managed_output_is_replaced_as_one_complete_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let output = temp.path().join("report");
        let mut first = RenderedReport::new(ReportFormat::Markdown, "abc");
        first.insert("old.md", "old".to_owned());
        first.write_to(&output).expect("first report");
        let mut second = RenderedReport::new(ReportFormat::Markdown, "def");
        second.insert("new.md", "new".to_owned());

        second.write_to(&output).expect("replacement");

        assert!(!output.join("old.md").exists());
        assert_eq!(fs::read_to_string(output.join("new.md")).unwrap(), "new");
        assert!(output.join(MANIFEST).is_file());
    }
}
