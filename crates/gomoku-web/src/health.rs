use std::sync::Arc;

use async_trait::async_trait;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;

use crate::app::AppState;

/// Anything that can tell whether the database answers queries.
#[async_trait]
pub trait HealthProbe: Send + Sync {
    async fn check(&self) -> Result<(), String>;
}

#[async_trait]
impl HealthProbe for gomoku_db::Database {
    async fn check(&self) -> Result<(), String> {
        self.ping().await.map_err(|e| e.to_string())
    }
}

/// `GET /api/health`: 200 when the database answers, 503 otherwise. The error detail goes to the
/// log only, never to the client.
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let probe: Arc<dyn HealthProbe> = state.health.clone();
    match probe.check().await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "data": { "database": "up" } })),
        ),
        Err(detail) => {
            tracing::error!(%detail, "health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "ok": false,
                    "error": { "code": "DATABASE_UNAVAILABLE", "message": "Database unavailable" }
                })),
            )
        }
    }
}
