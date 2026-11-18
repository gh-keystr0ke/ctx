//! Deterministic observations for context repair.

use serde::Serialize;

use crate::{
    graph::GraphSnapshot,
    verification::{duplicate_documents, intents_without_mapping, stale_semantic_claims},
};

#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct DuplicateDocuments {
    pub retained: String,
    pub superseded: String,
}

#[derive(Debug, Default, Eq, PartialEq, Serialize)]
pub struct IndexedDrift {
    pub stale_claims: Vec<String>,
    pub duplicate_documents: Vec<DuplicateDocuments>,
    pub unmapped_documents: Vec<String>,
}

#[must_use]
pub fn indexed_drift(graph: &GraphSnapshot) -> IndexedDrift {
    IndexedDrift {
        stale_claims: stale_semantic_claims(graph),
        duplicate_documents: duplicate_documents(graph),
        unmapped_documents: intents_without_mapping(graph),
    }
}
