use std::{collections::BTreeMap, path::Path};

use ctx_app::ports::{LanguageAnalyzer, PortError};
use ctx_core::ir::FileAnalysis;

use crate::{
    go::GoAnalyzer, goose::GooseAnalyzer, language::SupportedLanguage, openapi::OpenApiAnalyzer,
    python::PythonAnalyzer, rust::RustAnalyzer,
};

/// A self-describing language adapter that can be installed in the registry.
pub trait AnalyzerModule: LanguageAnalyzer {
    fn language(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
}

/// Dispatches source analysis to independently registered language modules.
///
/// The application layer continues to depend only on `LanguageAnalyzer`; new
/// parsers do not leak language-specific types into indexing or review flows.
pub struct AnalyzerRegistry {
    modules: BTreeMap<String, Box<dyn AnalyzerModule>>,
    extensions: BTreeMap<String, String>,
}

impl AnalyzerRegistry {
    /// Builds the enabled set of built-in modules for a repository.
    ///
    /// # Errors
    ///
    /// Returns [`PortError`] when a configured language is not available or a
    /// module attempts to claim an already registered name or extension.
    pub fn builtins(root: &Path, languages: &[String]) -> Result<Self, PortError> {
        let mut registry = Self {
            modules: BTreeMap::new(),
            extensions: BTreeMap::new(),
        };
        registry.register(Box::new(OpenApiAnalyzer::new(root.to_path_buf())))?;
        for language in languages {
            match SupportedLanguage::from_name(language) {
                Some(SupportedLanguage::Python) => {
                    registry.register(Box::new(PythonAnalyzer::new(root.to_path_buf())))?;
                }
                Some(SupportedLanguage::Rust) => {
                    registry.register(Box::new(RustAnalyzer::new(root.to_path_buf())))?;
                }
                Some(SupportedLanguage::Go) => {
                    registry.register(Box::new(GoAnalyzer::new(root.to_path_buf())))?;
                }
                Some(SupportedLanguage::GooseMigrations) => {
                    registry.register(Box::new(GooseAnalyzer::new(root.to_path_buf())))?;
                }
                None => {
                    return Err(PortError::new(format!(
                        "no analyzer module is registered for '{language}'"
                    )));
                }
            }
        }
        Ok(registry)
    }

    /// Installs one analyzer and claims its language name and extensions.
    ///
    /// # Errors
    ///
    /// Returns [`PortError`] if another module already owns the same language
    /// name or source extension.
    pub fn register(&mut self, module: Box<dyn AnalyzerModule>) -> Result<(), PortError> {
        let language = module.language().to_owned();
        if self.modules.contains_key(&language) {
            return Err(PortError::new(format!(
                "an analyzer module for '{language}' is already registered"
            )));
        }
        for extension in module.extensions() {
            let extension = extension.to_ascii_lowercase();
            if let Some(owner) = self.extensions.get(&extension) {
                return Err(PortError::new(format!(
                    "source extension '.{extension}' is already handled by '{owner}'"
                )));
            }
            self.extensions.insert(extension, language.clone());
        }
        self.modules.insert(language, module);
        Ok(())
    }

    fn module_for_path(&self, relative_path: &str) -> Result<&dyn AnalyzerModule, PortError> {
        let extension = Path::new(relative_path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| {
                PortError::new(format!(
                    "cannot select an analyzer for source '{relative_path}'"
                ))
            })?;
        let language = self.extensions.get(&extension).ok_or_else(|| {
            PortError::new(format!(
                "no enabled analyzer handles source '{relative_path}'"
            ))
        })?;
        self.modules
            .get(language)
            .map(Box::as_ref)
            .ok_or_else(|| PortError::new(format!("analyzer registry is missing '{language}'")))
    }
}

impl LanguageAnalyzer for AnalyzerRegistry {
    fn analysis_version(&self, relative_path: &str) -> Result<String, PortError> {
        self.module_for_path(relative_path)?
            .analysis_version(relative_path)
    }

    fn analyze(&self, relative_path: &str) -> Result<FileAnalysis, PortError> {
        self.module_for_path(relative_path)?.analyze(relative_path)
    }

    fn analyze_text(&self, relative_path: &str, source: &str) -> Result<FileAnalysis, PortError> {
        self.module_for_path(relative_path)?
            .analyze_text(relative_path, source)
    }
}
