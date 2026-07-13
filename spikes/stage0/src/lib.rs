#![deny(missing_docs)]
//! Executable risk probes for S1 decisions accepted during Stage 0.
//!
//! The crate is intentionally outside production crates. Its tests exercise
//! cryptographic protocol shape and the selected EPUB parsing/sanitizing stack
//! before those boundaries move into application services.

pub mod auth;
pub mod epub;
