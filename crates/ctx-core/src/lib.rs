//! Deterministic domain model and algorithms for `ctx`.
//!
//! This crate deliberately contains no filesystem, Git, database, terminal, or
//! network code. Callers provide observations; the core returns decisions.

pub mod artifact;
pub mod business;
pub mod codedoc;
pub mod context_pack;
pub mod domain;
pub mod explain;
pub mod graph;
pub mod impact;
pub mod indexing;
pub mod ir;
pub mod knowledge;
pub mod linking;
pub mod neighborhood;
pub mod review;
pub mod schema;
pub mod verification;
