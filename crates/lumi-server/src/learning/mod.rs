//! Application services for deterministic learning.

pub(crate) mod generation;
pub(crate) mod limits;
pub(crate) mod repository;
pub(crate) mod routes;

pub(crate) use repository::{LearningMaterialContext, LearningRuntime};
