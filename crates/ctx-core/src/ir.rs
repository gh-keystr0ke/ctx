use serde::{Deserialize, Serialize};

/// A byte/line range in a source file. Ranges describe a version; they never
/// participate in stable symbol identity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
    Trace,
}

impl HttpMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Trace => "TRACE",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamSource {
    Path,
    Query,
    Header,
    Cookie,
    Body,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApiParam {
    pub name: String,
    pub type_hint: Option<String>,
    pub source: ParamSource,
    pub required: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpenApiResponse {
    pub status: String,
    pub description: Option<String>,
    #[serde(default)]
    pub content_types: Vec<String>,
    pub type_hint: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpenApiOperation {
    pub operation_id: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub security: Vec<String>,
    #[serde(default)]
    pub servers: Vec<String>,
    #[serde(default)]
    pub request_content_types: Vec<String>,
    #[serde(default)]
    pub responses: Vec<OpenApiResponse>,
}

/// One HTTP endpoint deterministically declared by source syntax. Framework
/// names are evidence metadata emitted by adapters; core behavior depends
/// only on the normalized method/path/parameter contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApiEndpoint {
    pub path: String,
    pub method: HttpMethod,
    pub params: Vec<ApiParam>,
    pub return_type: Option<String>,
    pub framework: String,
    pub range: SourceRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openapi: Option<OpenApiOperation>,
}

/// One statically recognizable outbound HTTP request. `url` may be a full
/// literal URL or a normalized template whose dynamic path segment is
/// represented as `{param}`; a wholly dynamic URL never reaches this IR.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalCall {
    pub method: HttpMethod,
    pub url: String,
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
    /// Specific columns statically named by the statement, when the SQL form
    /// reliably identifies them (an `UPDATE ... SET` clause's targets, or an
    /// `INSERT INTO table (a, b)` column list). Empty means the statement's
    /// column-level detail is unknown, not that it touches zero columns —
    /// `DELETE`, a bare `INSERT ... VALUES` with no column list, and
    /// `SELECT` never populate this rather than guess column attribution
    /// across joins.
    #[serde(default)]
    pub columns: Vec<String>,
}

/// One statically recognizable ORM model access whose table identity must be
/// resolved against schema-bearing symbols during indexing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OrmModelAccess {
    pub kind: DatabaseAccessKind,
    pub model_ref: String,
    pub columns: Vec<String>,
    pub range: SourceRange,
    pub statement_hash: String,
}

/// A statically recognized foreign-key target. `column` is `None` when a
/// recognizer locates the referenced table but not a specific column (for
/// example a bare `REFERENCES accounts` without a column list).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForeignKeyRef {
    pub table: String,
    pub column: Option<String>,
}

/// One column named by a static `CREATE TABLE`/`ALTER TABLE` recognizer, or
/// declared by an ORM model attribute. `data_type` is the raw declared type
/// text (for example `VARCHAR(255)`); it is not normalized or validated
/// against a SQL dialect. The constraint fields are `None`/`false` when a
/// recognizer cannot statically determine them, not when it has confirmed
/// their absence — callers must not read an unset constraint as "proven
/// absent".
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaColumn {
    pub name: String,
    pub data_type: String,
    #[serde(default)]
    pub nullable: Option<bool>,
    #[serde(default)]
    pub primary_key: bool,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub foreign_key: Option<ForeignKeyRef>,
    #[serde(default)]
    pub default: Option<String>,
}

/// One renamed column, as declared by a single `RENAME COLUMN old TO new`
/// clause.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ColumnRename {
    pub previous_name: String,
    pub new_name: String,
}

/// One `ALTER TABLE ... ALTER COLUMN` sub-clause touching an existing
/// column. Unlike `columns`, this never declares a column's full state —
/// only the single attribute this specific clause changes, since a
/// standalone `ALTER COLUMN` statement has no access to the column's prior
/// declaration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ColumnAlteration {
    pub column: String,
    pub new_type: Option<String>,
    /// `Some(false)` for `SET NOT NULL`, `Some(true)` for `DROP NOT NULL`.
    pub nullable: Option<bool>,
    pub default_changed: bool,
}

/// One statically recognized `CREATE [UNIQUE] INDEX` declaration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaIndex {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub unique: bool,
}

/// One statically recognized table declaration or alteration inside a
/// migration file, or one declarative ORM model class. Parser adapters are
/// responsible for recognizing the migration-tool/framework syntax; the core
/// only consumes this normalized, explainable fact, and never merges
/// declarations across migration files into one computed "current" schema.
///
/// A single DDL statement (or ORM class) can carry more than one kind of
/// operation at once (a real `ALTER TABLE` can add, drop, and rename columns
/// in one statement), so the fields below are independent, additive
/// observations rather than a tagged single-operation enum: `columns` is
/// what this statement declares present (a `CREATE TABLE`'s full set, or an
/// `ADD COLUMN`'s additions, or a static ORM model's declared attributes),
/// while `dropped_columns`/`renamed_columns`/`table_dropped`/`renamed_from`
/// describe operations that remove or rename earlier state instead of
/// declaring new state.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaTableDefinition {
    pub entity: String,
    /// Set only by a `CREATE TABLE` statement. Distinguishes `columns` being
    /// a brand-new table's initial column set from an `ALTER TABLE ... ADD
    /// COLUMN`'s additions to an already-existing table, which matters for
    /// review (adding a `NOT NULL` column without a default to an existing
    /// table is a well-known destructive migration pattern; the same column
    /// in a table's initial `CREATE TABLE` is not).
    #[serde(default)]
    pub table_created: bool,
    pub columns: Vec<SchemaColumn>,
    #[serde(default)]
    pub dropped_columns: Vec<String>,
    #[serde(default)]
    pub renamed_columns: Vec<ColumnRename>,
    #[serde(default)]
    pub column_alterations: Vec<ColumnAlteration>,
    /// Raw `CHECK (...)` expression text, normalized only by comment/whitespace
    /// stripping. Stored for textual equality/diffing, never evaluated or
    /// interpreted as structured constraint data.
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub indexes_added: Vec<SchemaIndex>,
    #[serde(default)]
    pub indexes_dropped: Vec<String>,
    #[serde(default)]
    pub table_dropped: bool,
    /// Set by `ALTER TABLE old RENAME TO new`; `entity` holds the new name.
    #[serde(default)]
    pub renamed_from: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orm_accesses: Vec<OrmModelAccess>,
    #[serde(default)]
    pub schema_tables: Vec<SchemaTableDefinition>,
    #[serde(default)]
    pub api_endpoints: Vec<ApiEndpoint>,
    #[serde(default)]
    pub external_calls: Vec<ExternalCall>,
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
