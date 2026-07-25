//! Conversation-specific server boundary owned by the Chat & Product UX track.

use axum::Router;

use crate::AppState;

/// Return chat routes contributed by the conversation track.
///
/// Contract Freeze 1 intentionally contributes no live routes. Track B can
/// replace this empty router without editing top-level router composition.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
}
