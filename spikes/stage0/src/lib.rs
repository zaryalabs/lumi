#![deny(missing_docs)]
//! Executable risk probes for architecture decisions accepted during Stage 0.
//!
//! The crate is intentionally outside production crates. Its tests exercise
//! risky protocol and content-processing boundaries before those boundaries
//! move into application services.

pub mod auth;
pub mod epub;
pub mod explicit_context;
pub mod generated_lum;
pub mod hierarchical_summary;
pub mod mcp;
pub mod openrouter;
