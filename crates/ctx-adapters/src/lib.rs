//! Concrete adapters around the deterministic `ctx-core` domain.

pub mod agent_contract;
pub mod agent_registry;
pub mod analyzer;
pub mod antigravity;
pub mod business_context;
mod candidate_queue;
pub mod claude_code;
pub mod codex;
pub mod context_registry;
pub mod coverage_store;
mod database;
mod decisions;
pub mod federation;
pub mod git;
pub mod gitlab;
pub mod go;
pub mod goose;
pub mod http_retry;
pub mod jira;
pub mod language;
pub mod local_index;
pub mod openapi;
pub mod pyright;
pub mod python;
pub mod rust;
pub mod sqlite;

pub mod worktree;
