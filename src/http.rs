use std::path::Path;

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::Serialize;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::database::Database;

pub fn router(web_dist: &Path, database: Database) -> Router {
    let static_files =
        ServeDir::new(web_dist).not_found_service(ServeFile::new(web_dist.join("index.html")));

    let api = Router::new()
        .route("/health", get(health))
        .fallback(api_not_found);

    Router::new()
        .route("/health", get(health))
        .nest("/api/v1", api)
        .fallback_service(static_files)
        .layer(TraceLayer::new_for_http())
        .with_state(database)
}

async fn health(State(database): State<Database>) -> (StatusCode, Json<HealthResponse>) {
    match database.ping().await {
        Ok(()) => (StatusCode::OK, Json(HealthResponse { status: "ok" })),
        Err(_) => {
            tracing::warn!("database health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse {
                    status: "unavailable",
                }),
            )
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn api_not_found() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            code: "not_found",
            message: "API endpoint not found",
        }),
    )
}

#[derive(Serialize)]
struct ApiError {
    code: &'static str,
    message: &'static str,
}
