//! Unauthenticated operational health and metrics endpoints.

use axum::{
    Json,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse as _, Response},
};
use serde::Serialize;

use crate::{api::ApiState, error::AppError, infrastructure::postgres};

/// Process liveness; does not contact dependencies.
pub async fn live() -> Json<HealthResponse> {
    Json(HealthResponse {
        service: "silicon-remind",
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Dependency readiness backed by PostgreSQL migration and schema validation.
///
/// # Errors
///
/// Returns dependency unavailable when PostgreSQL does not answer or its schema
/// does not satisfy the migrations embedded in this binary.
pub async fn ready(State(state): State<ApiState>) -> Result<Json<HealthResponse>, AppError> {
    postgres::health_check(state.repository.pool())
        .await
        .map_err(|_| AppError::DependencyUnavailable {
            dependency: "postgresql",
        })?;
    Ok(live().await)
}

/// Prometheus text exposition without tenant- or reminder-bearing labels.
///
/// # Errors
///
/// Returns an internal error if metric text encoding fails.
pub async fn metrics(State(state): State<ApiState>) -> Result<Response, AppError> {
    let body = state
        .metrics
        .encode()
        .map_err(|error| AppError::internal("metrics_encoding", error))?;
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    Ok(response)
}

/// Lightweight liveness/readiness payload.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    service: &'static str,
    status: &'static str,
    version: &'static str,
}
