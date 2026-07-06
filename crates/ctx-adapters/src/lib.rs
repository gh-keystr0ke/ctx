//! Concrete adapters around the deterministic `ctx-core` domain.

pub mod analyzer;
pub mod business_context;
mod database;
pub mod git;
pub mod gitlab;
pub mod go;
pub mod goose;
pub mod language;
pub mod python;
pub mod rust;
pub mod sqlite;
