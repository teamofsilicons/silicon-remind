//! Explicit wire-version selection and durable, environment-scoped lifecycle state.
use crate::{
    api::{ApiState, ScopedState},
    error::AppError,
};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse as _, Response},
};

const CURRENT: i32 = 1;
type ContractState = (String, Option<chrono::DateTime<chrono::Utc>>);

/// Lists the service's implemented protocols and lifecycle policy.
/// # Errors
/// Fails closed when the contract registry cannot be read.
pub async fn versions(ScopedState(state): ScopedState) -> Result<Response, AppError> {
    sweep(state.repository.pool()).await?;
    let rows: Vec<(i32, String, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT version,status,sunset_at FROM api_contract_versions ORDER BY version",
    )
    .fetch_all(state.repository.pool())
    .await?;
    let items: Vec<_> = rows.into_iter().map(|(version,status,sunset_at)| serde_json::json!({"version":version,"status":status,"sunset_at":sunset_at,"implemented":version==CURRENT})).collect();
    Ok(Json(serde_json::json!({"service":"silicon-remind","current":CURRENT,"supported":[CURRENT],"protocol":"https+json","selection_header":"X-Remind-API-Version","versions":items,"sunset_idle_days":7,"policy":"https://docs.remind.teamofsilicons.com/version-policy/"})).into_response())
}

/// Applies the idle sunset only to deprecated, non-current versions.
/// # Errors
/// Returns registry storage failures.
pub async fn sweep(pool: &sqlx::PgPool) -> Result<(), AppError> {
    sqlx::query("UPDATE api_contract_versions SET status='sunset',sunset_at=clock_timestamp() WHERE version<>$1 AND status='deprecated' AND GREATEST(deprecated_at,COALESCE(last_requested_at,deprecated_at)) <= clock_timestamp()-interval '7 days'")
        .bind(CURRENT).execute(pool).await?;
    Ok(())
}

/// Rejects incompatible version selections before a handler can mutate data.
pub async fn negotiate(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if !path.starts_with("/api/v") || path == "/api/versions" {
        return next.run(request).await;
    }
    let path_version = path
        .strip_prefix("/api/v")
        .and_then(|s| s.split('/').next())
        .and_then(|s| s.parse::<i32>().ok());
    let headers: Vec<_> = request
        .headers()
        .get_all("x-remind-api-version")
        .iter()
        .collect();
    if path_version != Some(CURRENT)
        || headers.len() > 1
        || headers
            .first()
            .is_some_and(|h| h.to_str().ok() != Some("1"))
    {
        return (StatusCode::NOT_ACCEPTABLE,Json(serde_json::json!({"error":{"code":"unsupported_api_version","message":"Use /api/v1 and X-Remind-API-Version: 1; discover supported protocols at /api/versions","request_id":crate::request_context::current_request_id()}}))).into_response();
    }
    let state = state.scoped(request.extensions());
    // A single atomic write serializes usage with deprecation/sunset decisions.
    let result: Result<Option<ContractState>, sqlx::Error> = sqlx::query_as("UPDATE api_contract_versions SET last_requested_at=clock_timestamp(),request_count=request_count+1 WHERE version=$1 AND status<>'sunset' RETURNING status,deprecated_at")
        .bind(CURRENT).fetch_optional(state.repository.pool()).await;
    let deprecated = match result {
        Ok(Some((status,at))) => if status=="deprecated" { at } else { None },
        Ok(None) => return (StatusCode::GONE,Json(serde_json::json!({"error":{"code":"api_version_sunset","message":"This API version has been retired","request_id":crate::request_context::current_request_id()}}))).into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("x-remind-api-version", HeaderValue::from_static("1"));
    response.headers_mut().insert(
        "link",
        HeaderValue::from_static(
            "<https://docs.remind.teamofsilicons.com/version-policy/>; rel=\"deprecation\"",
        ),
    );
    if let Some(at) = deprecated
        && let Ok(value) = HeaderValue::from_str(&format!("@{}", at.timestamp()))
    {
        response.headers_mut().insert("deprecation", value);
    }
    response
}
