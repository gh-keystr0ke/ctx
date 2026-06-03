use serde::{Deserialize, Serialize};

use crate::domain::{NodeKind, RelationKind, StableKey};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusinessKind {
    Feature,
    Requirement,
    Invariant,
    Decision,
}

impl BusinessKind {
    pub const fn node_kind(self) -> NodeKind {
        match self {
            Self::Feature => NodeKind::Feature,
            Self::Requirement => NodeKind::Requirement,
            Self::Invariant => NodeKind::Invariant,
            Self::Decision => NodeKind::Decision,
        }
    }

    pub const fn implementation_relation(self) -> RelationKind {
        match self {
            Self::Feature | Self::Requirement => RelationKind::Implements,
            Self::Invariant => RelationKind::Enforces,
            Self::Decision => RelationKind::Satisfies,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExplicitSymbolLink {
    pub symbol: String,
    pub locator: String,
}

/// A normalized business-context document with its source identity attached.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BusinessDocument {
    pub id: String,
    pub kind: BusinessKind,
    pub title: String,
    pub body: String,
    pub status: String,
    pub feature: Option<String>,
    pub implementation: Vec<ExplicitSymbolLink>,
    pub tests: Vec<ExplicitSymbolLink>,
    pub source_uri: String,
    pub content_hash: String,
}

impl BusinessDocument {
    /// Returns the stable node identity derived from the human-owned ID.
    ///
    /// # Errors
    ///
    /// Returns an identifier error only when the document ID is empty or has
    /// surrounding whitespace.
    pub fn stable_key(&self) -> Result<StableKey, crate::domain::InvalidIdentifier> {
        StableKey::new(format!("intent:{}", self.id))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextImportStats {
    pub documents_created: usize,
    pub documents_versioned: usize,
    pub documents_retired: usize,
    pub explicit_links_created: usize,
    pub unresolved_symbols: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invariant_links_are_enforcement_assertions() {
        assert_eq!(
            BusinessKind::Invariant.implementation_relation(),
            RelationKind::Enforces
        );
        assert_eq!(BusinessKind::Invariant.node_kind(), NodeKind::Invariant);
    }
}
