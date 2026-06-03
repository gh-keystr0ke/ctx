//! Deterministic domain model and algorithms for `ctx`.
//!
//! This crate deliberately contains no filesystem, Git, database, terminal, or
//! network code. Callers provide observations; the core returns decisions.

pub mod business;
pub mod domain;
pub mod indexing;
pub mod ir;
