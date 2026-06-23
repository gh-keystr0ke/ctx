use serde::{Deserialize, Serialize};

/// A byte/line range in a source file. Ranges describe a version; they never
/// participate in stable symbol identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Module,
    TypeAlias,
    Constant,
    Test,
    /// One versioned migration file (for example a goose `-- +goose Up`
    /// script) that declares or alters a database table's schema.
    SchemaMigration,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CallSite {
    pub callee: String,
    pub range: SourceRange,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseAccessKind {
    Read,
    Write,
}

/// One database entity read or written by a symbol through a statically
/// visible SQL literal. Parser adapters are responsible for recognizing the
/// language syntax; the core only consumes this normalized, explainable fact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DatabaseAccess {
    pub entity: String,
    pub kind: DatabaseAccessKind,
    pub range: SourceRange,
    pub statement_hash: String,
}

/// One column named by a static `CREATE TABLE`/`ALTER TABLE` recognizer.
/// `data_type` is the raw declared type text (for example `VARCHAR(255)`); it
/// is not normalized or validated against a SQL dialect.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaColumn {
    pub name: String,
    pub data_type: String,
}

/// One statically recognized table declaration or alteration inside a
/// migration file. Parser adapters are responsible for recognizing the
/// migration-tool syntax; the core only consumes this normalized,
/// explainable fact, and never merges declarations across migration files
/// into one computed "current" schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaTableDefinition {
    pub entity: String,
    pub columns: Vec<SchemaColumn>,
    pub range: SourceRange,
}

/// Language-neutral symbol definition produced by a parser adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymbolDefinition {
    pub name: String,
    pub canonical_path: String,
    pub kind: SymbolKind,
    pub range: SourceRange,
    pub signature: Option<String>,
    pub body_hash: String,
    pub structural_fingerprint: String,
    pub calls: Vec<CallSite>,
    #[serde(default)]
    pub database_accesses: Vec<DatabaseAccess>,
    #[serde(default)]
    pub schema_tables: Vec<SchemaTableDefinition>,
}

/// Language-neutral analysis for one complete source file version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileAnalysis {
    pub path: String,
    pub language: String,
    #[serde(default)]
    pub analysis_version: String,
    pub content_hash: String,
    pub symbols: Vec<SymbolDefinition>,
}
