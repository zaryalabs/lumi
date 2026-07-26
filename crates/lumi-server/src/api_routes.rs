//! Top-level `/api/v1` route composition.
//!
//! Feature tracks contribute routers through narrow module seams. The
//! integration owner is the only role that changes this file.

use axum::{
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, patch, post, put},
    Router,
};

use crate::{
    ai, auth_api, cancel_job, capabilities, continue_reading, create_annotation, delete_annotation,
    delete_material, delete_telegram_bot_token, download_source_document, export_annotations,
    get_annotation, get_blob_manifest, get_job, get_job_diagnostics, get_material,
    get_normalized_package, get_page_fidelity_document, get_progress, get_reader_settings,
    get_reading_document, get_revision, get_revision_resource, get_telegram_bot_settings, health,
    import_fixture_material, import_web_url, list_annotations, list_imports, list_materials, mcp,
    move_reading_position, readiness, retry_job, schema_migrations, update_annotation,
    update_library_state, update_reader_settings, update_telegram_bot_token, upload_document,
    AppState,
};

/// Return unauthenticated API routes.
pub(crate) fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .route("/capabilities", get(capabilities))
        .route("/schema/migrations", get(schema_migrations))
        .merge(auth_api::public_routes())
}

/// Return authenticated API routes from all feature modules.
pub(crate) fn protected_routes(state: &AppState) -> Router<AppState> {
    existing_product_routes()
        .merge(ai::routes::protected_routes())
        .merge(crate::learning::routes::protected_routes())
        .merge(crate::audio::protected_routes())
        .merge(mcp::management_routes())
        .layer(DefaultBodyLimit::max(201 * 1024 * 1024))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_api::require_session,
        ))
}

fn existing_product_routes() -> Router<AppState> {
    Router::new()
        .merge(auth_api::protected_account_routes())
        .route("/materials", get(list_materials))
        .route("/materials/continue-reading", get(continue_reading))
        .route(
            "/materials/{material_id}",
            get(get_material).delete(delete_material),
        )
        .route(
            "/materials/{material_id}/library-state",
            patch(update_library_state).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/materials/{material_id}/source",
            get(download_source_document),
        )
        .route(
            "/materials/{material_id}/annotations",
            get(list_annotations)
                .post(create_annotation)
                .layer(DefaultBodyLimit::max(512 * 1024)),
        )
        .route(
            "/materials/{material_id}/annotations/export",
            get(export_annotations),
        )
        .route(
            "/materials/{material_id}/annotations/{annotation_id}",
            get(get_annotation)
                .put(update_annotation)
                .delete(delete_annotation)
                .layer(DefaultBodyLimit::max(512 * 1024)),
        )
        .route(
            "/materials/{material_id}/progress",
            get(get_progress)
                .put(move_reading_position)
                .layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .route(
            "/reader/settings",
            get(get_reader_settings)
                .put(update_reader_settings)
                .layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/settings/telegram", get(get_telegram_bot_settings))
        .route(
            "/settings/telegram/token",
            put(update_telegram_bot_token)
                .delete(delete_telegram_bot_token)
                .layer(DefaultBodyLimit::max(1024)),
        )
        .route("/revisions/{revision_id}", get(get_revision))
        .route(
            "/revisions/{revision_id}/package",
            get(get_normalized_package),
        )
        .route(
            "/revisions/{revision_id}/reading-document",
            get(get_reading_document),
        )
        .route(
            "/revisions/{revision_id}/page-fidelity-document",
            get(get_page_fidelity_document),
        )
        .route(
            "/revisions/{revision_id}/resources/{content_hash}",
            get(get_revision_resource),
        )
        .route("/blobs/{manifest_id}", get(get_blob_manifest))
        .route(
            "/imports/fixtures/{fixture_slug}",
            post(import_fixture_material),
        )
        .route("/imports", get(list_imports).post(upload_document))
        .route(
            "/imports/url",
            post(import_web_url).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/jobs/{job_id}", get(get_job))
        .route("/jobs/{job_id}/diagnostics", get(get_job_diagnostics))
        .route("/jobs/{job_id}/cancel", post(cancel_job))
        .route("/jobs/{job_id}/retry", post(retry_job))
}
