//! Community Space application boundary.

pub(crate) mod api;
mod communications;
mod discussion;
mod matching;
mod permissions;
mod service;
mod store;

pub(crate) use service::{SocialRuntime, SocialStoreError};
