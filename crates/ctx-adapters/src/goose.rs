use std::{fs, path::PathBuf};

use ctx_app::ports::{LanguageAnalyzer, PortError};
use ctx_core::ir::{
    FileAnalysis, SchemaColumn, SchemaTableDefinition, SourceRange, SymbolDefinition, SymbolKind,
};
use thiserror::Error;

use crate::{analyzer::AnalyzerModule, database::ddl_table_columns};

#[derive(Debug, Error)]
pub enum GooseAnalysisError {
    #[error("could not read goose migration '{path}': {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("goose migration '{0}' is not valid UTF-8")]
    InvalidUtf8(String),
}

/// Recognizes goose (<https://github.com/pressly/goose>) SQL migration files.
///
/// Only the `-- +goose Up` section is inspected; `-- +goose Down` is
/// evidence of how a change is reverted, not of the schema a repository is
/// currently declaring. A file with no goose annotations yields zero
/// symbols instead of an error, so an incidental `.sql` file does not break
/// indexing. Every declared table is one versioned `SchemaMigration` symbol,
/// so multiple migration files touching the same table each remain their
/// own commit-bounded, evidence-backed fact instead of being merged into one
/// guessed "current" schema.
pub struct GooseAnalyzer {
    root: PathBuf,
}

impl GooseAnalyzer {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Parses a goose migration file into the same normalized IR consumed by
    /// the language-neutral indexing, review, and graph layers.
    pub fn analyze_source(relative_path: &str, source: &str) -> FileAnalysis {
        let migration = migration_path(relative_path);
        let up_section = goose_up_section(source);
        let schema_tables = up_section.map(extract_schema_tables).unwrap_or_default();
        let symbols = if schema_tables.is_empty() {
            Vec::new()
        } else {
            vec![SymbolDefinition {
                name: migration.clone(),
                canonical_path: migration,
                kind: SymbolKind::SchemaMigration,
                range: SourceRange {
                    start_byte: 0,
                    end_byte: source.len(),
                    start_line: 1,
                    end_line: source.lines().count().max(1),
                },
                signature: None,
                body_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
                structural_fingerprint: structural_fingerprint(source.as_bytes()),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                schema_tables,
            }]
        };
        FileAnalysis {
            path: relative_path.to_owned(),
            language: "goose".to_owned(),
            analysis_version: "goose-migration-v1".to_owned(),
            content_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            symbols,
        }
    }
}

impl LanguageAnalyzer for GooseAnalyzer {
    fn analysis_version(&self, _relative_path: &str) -> Result<String, PortError> {
        Ok("goose-migration-v1".to_owned())
    }

    fn analyze(&self, relative_path: &str) -> Result<FileAnalysis, PortError> {
        let path = self.root.join(relative_path);
        let bytes = fs::read(&path).map_err(|source| {
            PortError::new(
                GooseAnalysisError::Read {
                    path: path.display().to_string(),
                    source,
                }
                .to_string(),
            )
        })?;
        let source = std::str::from_utf8(&bytes).map_err(|_| {
            PortError::new(GooseAnalysisError::InvalidUtf8(path.display().to_string()).to_string())
        })?;
        Ok(Self::analyze_source(relative_path, source))
    }

    fn analyze_text(&self, relative_path: &str, source: &str) -> Result<FileAnalysis, PortError> {
        Ok(Self::analyze_source(relative_path, source))
    }
}

impl AnalyzerModule for GooseAnalyzer {
    fn language(&self) -> &'static str {
        "goose"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["sql"]
    }
}

/// Returns the text between a `-- +goose Up` marker and the next `-- +goose
/// Down` marker (or end of file). Returns `None` when the file carries no
/// goose annotations at all.
fn goose_up_section(source: &str) -> Option<&str> {
    let up_marker = source.find("-- +goose Up")?;
    let after_up = &source[up_marker..];
    let body_start = after_up
        .find('\n')
        .map_or(after_up.len(), |index| index + 1);
    let body = &after_up[body_start..];
    Some(
        body.find("-- +goose Down")
            .map_or(body, |down_marker| &body[..down_marker]),
    )
}

fn extract_schema_tables(up_section: &str) -> Vec<SchemaTableDefinition> {
    let mut tables = Vec::new();
    let mut line_offset = 1usize;
    for statement in up_section.split(';') {
        let lines_in_statement = statement.matches('\n').count();
        if let Some((entity, columns)) = ddl_table_columns(statement) {
            tables.push(SchemaTableDefinition {
                entity,
                columns: columns
                    .into_iter()
                    .map(|(name, data_type)| SchemaColumn { name, data_type })
                    .collect(),
                range: SourceRange {
                    start_byte: 0,
                    end_byte: statement.len(),
                    start_line: line_offset,
                    end_line: line_offset + lines_in_statement,
                },
            });
        }
        line_offset += lines_in_statement;
    }
    tables
}

fn migration_path(relative_path: &str) -> String {
    relative_path.trim_end_matches(".sql").replace('/', ".")
}

fn structural_fingerprint(bytes: &[u8]) -> String {
    let normalized = bytes
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    blake3::hash(&normalized).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_schema_from_the_up_section_only() {
        let source = r"
-- +goose Up
CREATE TABLE subscriptions (
    id UUID PRIMARY KEY,
    status VARCHAR(50) NOT NULL
);

-- +goose Down
DROP TABLE subscriptions;
";
        let analysis = GooseAnalyzer::analyze_source(
            "migrations/20240102030405_create_subscriptions.sql",
            source,
        );

        assert_eq!(analysis.language, "goose");
        assert_eq!(analysis.symbols.len(), 1);
        let migration = &analysis.symbols[0];
        assert_eq!(migration.kind, SymbolKind::SchemaMigration);
        assert_eq!(
            migration.canonical_path,
            "migrations.20240102030405_create_subscriptions"
        );
        assert_eq!(migration.schema_tables.len(), 1);
        let table = &migration.schema_tables[0];
        assert_eq!(table.entity, "subscriptions");
        assert_eq!(
            table
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["id", "status"]
        );
        assert!(migration.database_accesses.is_empty());
    }

    #[test]
    fn ignores_a_non_goose_sql_file() {
        let analysis = GooseAnalyzer::analyze_source("scratch.sql", "SELECT * FROM subscriptions;");
        assert!(analysis.symbols.is_empty());
    }

    #[test]
    fn recognizes_a_later_migration_that_only_alters_an_existing_table() {
        let source = r"
-- +goose Up
ALTER TABLE subscriptions ADD COLUMN grace_period_days INT;

-- +goose Down
ALTER TABLE subscriptions DROP COLUMN grace_period_days;
";
        let analysis =
            GooseAnalyzer::analyze_source("migrations/20240103000000_add_grace_period.sql", source);
        let table = &analysis.symbols[0].schema_tables[0];
        assert_eq!(table.entity, "subscriptions");
        assert_eq!(table.columns[0].name, "grace_period_days");
    }

    #[test]
    fn a_migration_with_no_recognizable_ddl_produces_no_symbols() {
        let source = "-- +goose Up\nSELECT 1;\n\n-- +goose Down\nSELECT 1;\n";
        let analysis = GooseAnalyzer::analyze_source("migrations/20240101_noop.sql", source);
        assert!(analysis.symbols.is_empty());
    }
}
