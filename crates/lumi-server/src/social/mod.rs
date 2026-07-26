//! Community Space application boundary.

pub(crate) mod api;
mod permissions;
mod service;
mod store;

pub(crate) use service::{SocialRuntime, SocialStoreError};
