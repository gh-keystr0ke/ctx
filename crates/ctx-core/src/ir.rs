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
