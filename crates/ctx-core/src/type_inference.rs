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

/// Stable source identity returned by a Python type oracle. Regular
/// declarations include their exact source range; synthesized declarations
/// retain only the URI and are not eligible for source-symbol matching.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PythonDeclaration {
    pub uri: String,
    pub name: Option<String>,
    pub range: Option<(TypePosition, TypePosition)>,
    pub category: Option<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PythonClassType {
    pub declaration: PythonDeclaration,
    pub is_instance: bool,
    pub type_arguments: Vec<PythonType>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PythonFunctionType {
    pub declaration: PythonDeclaration,
    pub bound_to: Option<Box<PythonType>>,
}

/// Type identity normalized from an external Python type checker. It keeps
/// declarations structural and intentionally omits display-only hover text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PythonType {
    Any,
    Unknown,
    Class(PythonClassType),
    Function(PythonFunctionType),
    Union { members: Vec<PythonType> },
    Other { oracle_kind: String },
}

impl PythonType {
    /// Produces a concise diagnostic representation without treating a
    /// presentation string as type identity.
    pub fn diagnostic_name(&self) -> String {
        match self {
            Self::Any => "Any".to_owned(),
            Self::Unknown => "Unknown".to_owned(),
            Self::Class(class) => class.declaration.name.clone().map_or_else(
                || format!("class@{}", class.declaration.uri),
                |name| format!("{name}@{}", class.declaration.uri),
            ),
            Self::Function(function) => function.declaration.name.clone().map_or_else(
                || format!("function@{}", function.declaration.uri),
                |name| format!("{name}@{}", function.declaration.uri),
            ),
            Self::Union { members } => members
                .iter()
                .map(Self::diagnostic_name)
                .collect::<Vec<_>>()
                .join(" | "),
            Self::Other { oracle_kind } => oracle_kind.clone(),
        }
    }
}
