use serde::{Deserialize, Serialize};

use crate::ir::SourceRange;

/// A zero-based source position using UTF-16 code units, matching the
/// position convention used by LSP-compatible type oracles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TypePosition {
    pub line: usize,
    pub character: usize,
}

/// The exact source node submitted to an external type oracle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TypeProbe {
    pub expression: String,
    pub range: SourceRange,
    pub start: TypePosition,
    pub end: TypePosition,
}

/// A syntactically recognizable Python write form that may benefit from
/// externally inferred types. This enum does not imply database semantics.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeWriteForm {
    AttrAssign,
    Add,
    AddAll,
    Merge,
    Delete,
}

/// One permissively extracted Python write candidate. Consumers must prove
/// the operation's domain semantics before turning a candidate into a claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TypeWriteCandidate {
    pub file_path: String,
    pub form: TypeWriteForm,
    pub probe: TypeProbe,
    /// The complete bound-method expression for call forms, for example
    /// `session.add`. Attribute assignments do not need a method probe.
    pub method_probe: Option<TypeProbe>,
    pub column: Option<String>,
    pub write_range: SourceRange,
    pub statement_hash: String,
}
