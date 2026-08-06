use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifies a repository independently of a database row ID.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepositoryId(String);

/// Identifies a node independently of a database row ID.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(String);

/// A Git object identifier used as a temporal validity boundary.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommitOid(String);

/// Stable, source-derived identity that survives database rebuilds.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StableKey(String);

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum InvalidIdentifier {
    #[error("identifier must not be empty")]
    Empty,
    #[error("identifier must not contain leading or trailing whitespace")]
    SurroundingWhitespace,
    #[error("Git object identifier must contain 4 to 64 hexadecimal characters")]
    InvalidCommitOid,
}

macro_rules! text_identifier {
    ($type:ident) => {
        impl $type {
            /// Creates an identifier from a non-empty, already-trimmed string.
            ///
            /// # Errors
            ///
            /// Returns [`InvalidIdentifier`] when the value is empty or has
            /// surrounding whitespace.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidIdentifier> {
                let value = value.into();
                if value.is_empty() {
                    return Err(InvalidIdentifier::Empty);
                }
                if value.trim() != value {
                    return Err(InvalidIdentifier::SurroundingWhitespace);
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $type {
            type Err = InvalidIdentifier;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

text_identifier!(RepositoryId);
text_identifier!(NodeId);
text_identifier!(StableKey);

impl CommitOid {
    /// Creates a normalized hexadecimal Git object identifier.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidIdentifier::InvalidCommitOid`] for non-hexadecimal
    /// values and identifiers outside Git's supported abbreviation range.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidIdentifier> {
        let value = value.into();
        if !(4..=64).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(InvalidIdentifier::InvalidCommitOid);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitOid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for CommitOid {
    type Err = InvalidIdentifier;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// A bounded confidence score. It expresses ranking strength, not probability.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Confidence(f32);

#[derive(Clone, Copy, Debug, Error, PartialEq)]
#[error("confidence must be finite and between 0 and 1, got {0}")]
pub struct InvalidConfidence(f32);

impl Confidence {
    pub const CERTAIN: Self = Self(1.0);

    /// Creates a finite confidence value in the inclusive range 0 through 1.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidConfidence`] when the value is not finite or is outside
    /// the valid range.
    pub fn new(value: f32) -> Result<Self, InvalidConfidence> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidConfidence(value))
        }
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Feature,
    Requirement,
    Invariant,
    Decision,
    DomainConcept,
    ExternalSystem,
    File,
    CodeSymbol,
    Endpoint,
    ApiEndpoint,
    DbEntity,
    Event,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Contains,
    Calls,
    References,
    ReadsFrom,
    WritesTo,
    DefinesSchema,
    Exposes,
    CallsExternal,
    Emits,
    Handles,
    Implements,
    Enforces,
    CoveredBy,
    DependsOn,
    Satisfies,
}

impl RelationKind {
    pub const fn is_semantic(self) -> bool {
        matches!(
            self,
            Self::Implements | Self::Enforces | Self::CoveredBy | Self::DependsOn | Self::Satisfies
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimClass {
    Fact,
    Assertion,
    Inference,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    StaticAnalysis,
    Human,
    Documentation,
    LlmInference,
    Runtime,
    ExternalSystem,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Active,
    Stale,
    Rejected,
}

/// Commit-based validity. `valid_to = None` means the claim is current.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Validity {
    pub valid_from: CommitOid,
    pub valid_to: Option<CommitOid>,
}

impl Validity {
    pub const fn current(valid_from: CommitOid) -> Self {
        Self {
            valid_from,
            valid_to: None,
        }
    }

    pub const fn is_current(&self) -> bool {
        self.valid_to.is_none()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub repository_id: RepositoryId,
    pub kind: NodeKind,
    pub stable_key: StableKey,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub repository_id: RepositoryId,
    pub source: NodeId,
    pub target: NodeId,
    pub kind: RelationKind,
    pub claim_class: ClaimClass,
    pub source_kind: SourceKind,
    pub confidence: Confidence,
    pub status: ClaimStatus,
    pub validity: Validity,
    pub producer: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub source_kind: SourceKind,
    pub source_uri: String,
    pub locator: String,
    pub excerpt_hash: String,
    pub strength: Confidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_rejects_values_outside_the_domain() {
        assert!(Confidence::new(-0.01).is_err());
        assert!(Confidence::new(1.01).is_err());
        assert!(Confidence::new(f32::NAN).is_err());
        let confidence = Confidence::new(0.72).expect("valid score").get();
        assert!((confidence - 0.72).abs() < f32::EPSILON);
    }

    #[test]
    fn commit_oids_are_validated_and_normalized() {
        let oid = CommitOid::new("AB12CD34").expect("valid oid");
        assert_eq!(oid.as_str(), "ab12cd34");
        assert!(CommitOid::new("xyz!").is_err());
        assert!(CommitOid::new("abc").is_err());
    }

    #[test]
    fn identifiers_reject_accidental_whitespace() {
        assert_eq!(StableKey::new(""), Err(InvalidIdentifier::Empty));
        assert_eq!(
            RepositoryId::new(" repo"),
            Err(InvalidIdentifier::SurroundingWhitespace)
        );
    }

    #[test]
    fn relation_classification_is_explicit() {
        assert!(RelationKind::Enforces.is_semantic());
        assert!(!RelationKind::Calls.is_semantic());
    }
}
