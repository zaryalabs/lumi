//! AI route-composition seam shared by independent implementation tracks.

use axum::Router;

use crate::AppState;

/// Return the complete AI route contribution under `/api/v1`.
pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .merge(super::providers::routes())
        .merge(super::chat::routes())
}
