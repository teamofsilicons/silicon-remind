//! Public versioned schedule and execution-history handlers.

use axum::{
    Json,
    extract::{Extension, Path, Query, rejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse as _, Response},
};
use uuid::Uuid;

use crate::api::ScopedState;

use crate::{
    api::models,
    application::schedules::{next_execution_cursor, next_schedule_cursor, request_hash},
    domain::Actor,
    error::AppError,
};

const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// `GET /api/v1/schedules`.
///
/// # Errors
///
/// Returns validation or persistence errors.
pub async fn list(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    query: Result<Query<models::ListSchedulesQuery>, rejection::QueryRejection>,
) -> Result<Json<models::PageResponse<models::ScheduleResponse>>, AppError> {
    let Query(query) = query.map_err(|_| AppError::Validation)?;
    let page = state
        .schedules
        .list(
            &actor,
            query.silicon_id,
            query.section,
            query.status,
            query.cursor.as_deref(),
            query.limit,
        )
        .await?;
    let next_cursor = next_schedule_cursor(&page)?;
    let items = page
        .items
        .iter()
        .map(models::ScheduleResponse::from)
        .collect();
    Ok(Json(models::PageResponse { items, next_cursor }))
}

/// `POST /api/v1/schedules`.
///
/// # Errors
///
/// Returns authentication policy, validation, idempotency, or persistence
/// errors.
pub async fn create(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    body: Result<Json<models::CreateScheduleRequest>, rejection::JsonRejection>,
) -> Result<Response, AppError> {
    let Json(request) = body.map_err(|error| map_json_rejection(&error))?;
    let hash = request_hash(&request)?;
    let command = request.try_into()?;
    let idempotency_key = idempotency_key(&headers)?;
    let mutation = state
        .schedules
        .create(&actor, command, idempotency_key, hash)
        .await?;
    mutation_response(mutation.status_code, mutation.body)
}

/// `PATCH /api/v1/schedules`.
///
/// # Errors
///
/// Returns validation, ownership, state, idempotency, or persistence errors.
pub async fn update_statuses(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    body: Result<Json<models::BulkScheduleStatusRequest>, rejection::JsonRejection>,
) -> Result<Response, AppError> {
    let Json(request) = body.map_err(|error| map_json_rejection(&error))?;
    let hash = request_hash(&request)?;
    let idempotency_key = idempotency_key(&headers)?;
    let mutation = state
        .schedules
        .update_statuses(
            &actor,
            request.schedule_ids,
            request.status,
            idempotency_key,
            hash,
        )
        .await?;
    mutation_response(mutation.status_code, mutation.body)
}

/// `GET /api/v1/schedules/{schedule_id}`.
///
/// # Errors
///
/// Returns validation, not-found, or persistence errors.
pub async fn get(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<Json<models::ScheduleResponse>, AppError> {
    let Path(schedule_id) = path.map_err(|_| AppError::Validation)?;
    let schedule = state.schedules.get(&actor, schedule_id).await?;
    Ok(Json(models::ScheduleResponse::from(&schedule)))
}

/// `PATCH /api/v1/schedules/{schedule_id}`.
///
/// # Errors
///
/// Returns validation, ownership, state, idempotency, or persistence errors.
pub async fn patch(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
    headers: HeaderMap,
    body: Result<Json<models::PatchScheduleRequest>, rejection::JsonRejection>,
) -> Result<Response, AppError> {
    let Path(schedule_id) = path.map_err(|_| AppError::Validation)?;
    let Json(request) = body.map_err(|error| map_json_rejection(&error))?;
    let hash = request_hash(&request)?;
    let idempotency_key = idempotency_key(&headers)?;
    let mutation = state
        .schedules
        .patch(&actor, schedule_id, request.into(), idempotency_key, hash)
        .await?;
    mutation_response(mutation.status_code, mutation.body)
}

/// `DELETE /api/v1/schedules/{schedule_id}`.
///
/// # Errors
///
/// Returns validation, ownership/not-found, or persistence errors.
pub async fn delete(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<StatusCode, AppError> {
    let Path(schedule_id) = path.map_err(|_| AppError::Validation)?;
    state.schedules.delete(&actor, schedule_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/schedules/{schedule_id}/executions`.
///
/// # Errors
///
/// Returns validation, not-found, stored-data, or persistence errors.
pub async fn list_executions(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
    query: Result<Query<models::PageQuery>, rejection::QueryRejection>,
) -> Result<Json<models::PageResponse<models::ExecutionResponse>>, AppError> {
    let Path(schedule_id) = path.map_err(|_| AppError::Validation)?;
    let Query(query) = query.map_err(|_| AppError::Validation)?;
    let page = state
        .schedules
        .list_executions(&actor, schedule_id, query.cursor.as_deref(), query.limit)
        .await?;
    let next_cursor = next_execution_cursor(&page)?;
    let items = page
        .items
        .iter()
        .map(models::ExecutionResponse::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| AppError::internal("stored_execution", error))?;
    Ok(Json(models::PageResponse { items, next_cursor }))
}

pub(crate) fn idempotency_key(headers: &HeaderMap) -> Result<String, AppError> {
    let mut values = headers.get_all(IDEMPOTENCY_HEADER).iter();
    let value = values.next().ok_or(AppError::Validation)?;
    if values.next().is_some() {
        return Err(AppError::Validation);
    }
    value
        .to_str()
        .map(str::to_owned)
        .map_err(|_| AppError::Validation)
}

fn mutation_response(status_code: u16, body: serde_json::Value) -> Result<Response, AppError> {
    let status = StatusCode::from_u16(status_code)
        .map_err(|error| AppError::internal("stored_http_status", error))?;
    Ok((status, Json(body)).into_response())
}

fn map_json_rejection(rejection: &rejection::JsonRejection) -> AppError {
    if matches!(rejection, rejection::JsonRejection::BytesRejection(_)) {
        AppError::PayloadTooLarge
    } else {
        AppError::Validation
    }
}

#[cfg(test)]
mod tests {
    use axum::{Json, body::Body, extract::FromRequest as _, response::IntoResponse as _};
    use http::{HeaderMap, HeaderValue, Request, StatusCode};
    use http_body_util::BodyExt as _;

    use super::{idempotency_key, map_json_rejection};
    use crate::{api::models::CreateScheduleRequest, domain::CreateScheduleCommand};

    #[tokio::test]
    async fn create_without_timezone_returns_actionable_json_error() -> anyhow::Result<()> {
        for timezone in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("")),
            Some(serde_json::json!(" \t\n")),
        ] {
            let mut body = serde_json::json!({
                "text": "report",
                "kind": "one_time",
                "cron": "0 9 * * *"
            });
            if let Some(timezone) = timezone {
                body["timezone"] = timezone;
            }
            let request = Request::builder()
                .method("POST")
                .uri("/api/v1/schedules")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))?;
            let Json(request) = Json::<CreateScheduleRequest>::from_request(request, &())
                .await
                .map_err(|error| map_json_rejection(&error))?;
            let Err(error) = CreateScheduleCommand::try_from(request) else {
                anyhow::bail!("creation accepted a missing timezone");
            };
            let response = error.into_response();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let body = response.into_body().collect().await?.to_bytes();
            let envelope: serde_json::Value = serde_json::from_slice(&body)?;
            assert_eq!(envelope["error"]["code"], "timezone_required");
            assert_eq!(
                envelope["error"]["message"],
                "Providing a timezone is mandatory. Set the `timezone` JSON field to an IANA timezone identifier, for example \"timezone\": \"Asia/Kolkata\"."
            );
            assert!(envelope["error"]["request_id"].is_string());
        }
        Ok(())
    }

    #[test]
    fn duplicate_idempotency_headers_are_rejected() {
        let mut headers = HeaderMap::new();
        headers.append(
            "idempotency-key",
            HeaderValue::from_static("first-idempotency-key"),
        );
        headers.append(
            "idempotency-key",
            HeaderValue::from_static("second-idempotency-key"),
        );
        assert!(idempotency_key(&headers).is_err());
    }
}
