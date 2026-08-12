//! The rust-analyzer-backed host session for RAQL.
//!
//! This crate owns workspace lifecycle only: loading a Cargo workspace
//! into a rust-analyzer database, keeping that database in sync with the
//! filesystem, warmup, and running a planned program against it. Rust
//! semantic truth lives in `raql-ra` (SPEC §6/§8); row semantics live in
//! `raql-engine` (SPEC §11). Nothing here computes either.
//!
//! Module map:
//! - [`workspace_service`] — the session: load, sync, warm, run.
//! - [`workspace_loader`] — the initial Cargo/RA workspace load.
//! - [`projection`] — the output boundary (SPEC §13.1): snapshot-scoped
//!   `raql_ra::Value`s become plain data before leaving the session.
//! - [`capability`] — which catalog extern predicates this host answers.
#![forbid(unsafe_code)]

use thiserror::Error;

mod capability;
mod projection;
mod workspace_loader;
mod workspace_service;

pub use projection::{ProjectedRunResult, ProjectedValue};
pub use raql_host::{CapabilityId, CapabilitySet};

pub mod daemon {
    pub use super::workspace_service::WarmupSnapshot;
    pub use super::workspace_service::WorkspaceService as DaemonWorkspace;
    pub use super::workspace_service::resolve_workspace_root;
}

#[cfg(test)]
mod workspace_service_tests;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RaHostInitError {
    #[error("workspace manifest not found for `{input_path}`: {details}")]
    WorkspaceNotFound { input_path: String, details: String },
    #[error("failed to load workspace `{manifest}`: {details}")]
    WorkspaceLoad { manifest: String, details: String },
    #[error("failed to build semantic workspace snapshot: {details}")]
    SemanticBuild { details: String },
}
