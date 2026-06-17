//! Deterministic product-quality evaluation harness for `ctx`.
//!
//! This is a development tool, not a shipped product surface: it builds a
//! small Git-history corpus (see [`cases`]), drives it through the same
//! `ctx-app` use cases the CLI exposes (see [`harness`]), and scores the
//! results against machine-readable ground truth (see [`report`]) instead of
//! reporting a vanity pass count.

pub mod cases;
pub mod harness;
pub mod report;
pub mod runner;
