//! Test environment control and mandatory request-plane selection.
use crate::{
    api::ApiState,
    domain::Actor,
    error::AppError,
    infrastructure::{
        iam::IamClient,
        testing::{CreateTestEnvironment, TestEnvironment, TestEnvironments},
    },
};
use axum::{
    Extension, Json,
    extract::{Path, Query, Request, State, rejection},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

/// Isolated runtime dependencies attached only after validating a root key.
#[derive(Clone)]
pub(crate) struct EnvironmentContext {
    pub pool: PgPool,
    pub iam: IamClient,
}

/// Selects a test context before authentication or any handler runs.
pub async fn select(State(state): State<ApiState>, mut request: Request, next: Next) -> Response {
    let mut headers = request.headers().get_all("x-remind-test-key").iter();
    let key = match (headers.next(), headers.next()) {
        (None, None) => return next.run(request).await,
        (Some(key), None) => match key.to_str() {
            Ok(key) => SecretString::from(key),
            Err(_) => return AppError::Unauthenticated.into_response(),
        },
        _ => return AppError::Unauthenticated.into_response(),
    };
    let path = request.uri().path();
    // Environment management is a production-org operation, never a sandbox
    // identity's route to production. Internal provisioning is not public SDK API.
    if !path.starts_with("/api/v1/") || path.starts_with("/api/v1/test-environments") {
        return AppError::Forbidden.into_response();
    }
    let tests = match manager(&state) {
        Ok(tests) => tests,
        Err(error) => return error.into_response(),
    };
    let cleaning =
        path == "/api/v1/testing-environment/cleanings" && request.method() == http::Method::POST;
    let configuring =
        path == "/api/v1/testing-environment/iam" && request.method() == http::Method::PUT;
    let current = path == "/api/v1/testing-environment" && request.method() == http::Method::GET;
    let mut lease = match tests.enter(&key, cleaning || configuring).await {
        Ok(lease) => lease,
        Err(error) => return error.into_response(),
    };
    let response = if configuring {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Configuration {
            iam_app_secret: SecretString,
        }
        use axum::extract::FromRequest as _;
        let Json(input) = match Json::<Configuration>::from_request(request, &state).await {
            Ok(input) => input,
            Err(error) => return super::internal::map_json_rejection(&error).into_response(),
        };
        match tests.configure_iam(&mut lease, input.iam_app_secret).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => return error.into_response(),
        }
    } else if cleaning {
        match tests.clean(&mut lease).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => return error.into_response(),
        }
    } else if current {
        Json(&lease.environment).into_response()
    } else {
        let Some(iam) = lease.iam.clone() else {
            return AppError::conflict("test_iam_application_not_configured").into_response();
        };
        request.extensions_mut().insert(EnvironmentContext {
            pool: lease.pool.clone(),
            iam,
        });
        next.run(request).await
    };
    let success = response.status().is_success();
    if let Err(error) = lease.finish(success).await {
        return error.into_response();
    }
    no_store(response)
}

/// Clear production-mode error for commands which require an environment key.
pub async fn test_only() -> Response {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":{"code":"test_environment_required","message":"This action is only possible for a test environment. Use remind --test <test_id> <command>.","request_id":crate::request_context::current_request_id()}}))).into_response()
}

/// Creates an empty sandbox owned by the authenticated production organization.
///
/// # Errors
///
/// Returns validation, IAM test-binding, duplicate-name, or storage failures.
pub async fn create(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    body: Result<Json<CreateTestEnvironment>, rejection::JsonRejection>,
) -> Result<Response, AppError> {
    let Json(input) = body.map_err(|error| super::internal::map_json_rejection(&error))?;
    let (environment, key) = manager(&state)?.create(&actor, input).await?;
    Ok(no_store(
        (
            StatusCode::CREATED,
            Json(serde_json::json!({"environment":environment,"key":key.expose_secret()})),
        )
            .into_response(),
    ))
}

/// Listing input; UUID cursor and bounded page size.
#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    include_deleted: bool,
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// Lists the organization's test environments.
///
/// # Errors
///
/// Returns validation for invalid pagination or a database failure.
pub async fn list(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    query: Result<Query<ListQuery>, rejection::QueryRejection>,
) -> Result<Json<serde_json::Value>, AppError> {
    let Query(query) = query.map_err(|_| AppError::Validation)?;
    let limit = query.limit.unwrap_or(50);
    let items = manager(&state)?
        .list(&actor, query.include_deleted, query.after, limit)
        .await?;
    let next_cursor = if items.len() == usize::try_from(limit).unwrap_or(0) {
        items.last().map(|row| row.id)
    } else {
        None
    };
    Ok(Json(
        serde_json::json!({"items":items,"next_cursor":next_cursor}),
    ))
}

/// Reads an environment including recoverable metadata.
///
/// # Errors
///
/// Returns not found outside the owning organization or after the recovery deadline.
pub async fn get(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<Json<TestEnvironment>, AppError> {
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    Ok(Json(manager(&state)?.get(&actor, id).await?))
}

/// Retrieves the active key for its creator or an org administrator.
///
/// # Errors
///
/// Returns forbidden unless creator or org administrator, or not found for inactive environments.
pub async fn key(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<Response, AppError> {
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    let key = manager(&state)?.key(&actor, id).await?;
    Ok(key_response(id, &key))
}

/// Rotates the active environment root key.
///
/// # Errors
///
/// Returns authorization or lifecycle conflict errors, or a storage failure.
pub async fn rotate(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<Response, AppError> {
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    let key = manager(&state)?.rotate(&actor, id, false).await?;
    Ok(key_response(id, &key))
}

/// Restores an environment within 30 days with a new root key.
///
/// # Errors
///
/// Returns not found after the recovery deadline, forbidden, or an active-name conflict.
pub async fn restore(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<Response, AppError> {
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    let key = manager(&state)?.rotate(&actor, id, true).await?;
    Ok(key_response(id, &key))
}

/// Revokes access immediately and starts the 30-day recovery window.
///
/// # Errors
///
/// Returns forbidden for unauthorized members, not found, or a storage failure.
pub async fn delete(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<StatusCode, AppError> {
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    manager(&state)?.delete(&actor, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn manager(state: &ApiState) -> Result<&TestEnvironments, AppError> {
    state.tests.as_ref().ok_or(AppError::DependencyUnavailable {
        dependency: "testing_database",
    })
}
fn key_response(id: Uuid, key: &SecretString) -> Response {
    no_store(
        Json(serde_json::json!({"environment_id":id,"key":key.expose_secret()})).into_response(),
    )
}
fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
