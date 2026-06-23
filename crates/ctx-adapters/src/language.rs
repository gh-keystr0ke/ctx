use std::path::Path;

/// A source language provided by the built-in analyzer modules.
///
/// Adding a language consists of implementing an analyzer module and declaring
/// its configuration name and extensions here. Repository discovery and the
/// analyzer registry then share one source of truth.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SupportedLanguage {
    Python,
    Rust,
    Go,
    GooseMigrations,
}

impl SupportedLanguage {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::GooseMigrations => "goose",
        }
    }

    pub const fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Python => &["py"],
            Self::Rust => &["rs"],
            Self::Go => &["go"],
            Self::GooseMigrations => &["sql"],
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "python" => Some(Self::Python),
            "rust" => Some(Self::Rust),
            "go" => Some(Self::Go),
            "goose" => Some(Self::GooseMigrations),
            _ => None,
        }
    }

    pub fn for_path(path: &str, enabled: &[Self]) -> Option<Self> {
        let extension = Path::new(path).extension()?.to_str()?;
        enabled.iter().copied().find(|language| {
            language
                .extensions()
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
    }
}

pub(crate) fn is_indexable_source(path: &str, languages: &[SupportedLanguage]) -> bool {
    SupportedLanguage::for_path(path, languages).is_some()
        && !path.split('/').any(|component| {
            matches!(
                component,
                ".git"
                    | ".ctx"
                    | ".venv"
                    | "venv"
                    | "vendor"
                    | "generated"
                    | "build"
                    | "dist"
                    | "target"
                    | "__pycache__"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_enabled_extensions_and_excludes_generated_trees() {
        let languages = [
            SupportedLanguage::Python,
            SupportedLanguage::Rust,
            SupportedLanguage::Go,
        ];

        assert!(is_indexable_source("src/app.py", &languages));
        assert!(is_indexable_source("crates/core/src/lib.rs", &languages));
        assert!(is_indexable_source("src/server.go", &languages));
        assert!(!is_indexable_source("vendor/pkg.py", &languages));
        assert!(!is_indexable_source("target/debug/build.rs", &languages));
        assert!(!is_indexable_source(
            "vendor/example.com/pkg/client.go",
            &languages
        ));
        assert!(!is_indexable_source(
            "src/server.go",
            &[SupportedLanguage::Rust]
        ));
    }
}
