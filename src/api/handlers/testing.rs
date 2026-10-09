//! Test environments: management routes and the key-selected request plane.
//!
//! `X-Remind-Test-Key` selects an environment before authentication runs. The
//! request then authenticates with the caller's real Silicon Accounts token and
//! reads and writes that environment's isolated data instead of production.
use crate::{
    api::ApiState,
    domain::{AccountRef, Actor, Relation},
    error::AppError,
    infrastructure::testing::{
        CreateTestEnvironment, EnvironmentAccess, TestEnvironment, TestEnvironments,
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

/// Isolated data access attached after a valid key admitted the request.
#[derive(Clone)]
pub(crate) struct EnvironmentContext {
    pub pool: PgPool,
}

/// Selects a test environment before authentication or any handler runs.
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
    // Managing environments is a production action, never a sandbox's.
    if !(path.starts_with("/api/v2/") || path == "/api/versions")
        || path.starts_with("/api/v2/test-environments")
    {
        return AppError::forbidden(
            "test_key_not_allowed_here",
            "X-Remind-Test-Key selects a test environment for /api/v2 data routes; manage environments without it.",
        )
        .into_response();
    }
    let tests = match manager(&state) {
        Ok(tests) => tests,
        Err(error) => return error.into_response(),
    };
    let cleaning =
        path == "/api/v2/testing-environment/cleanings" && request.method() == http::Method::POST;
    let current = path == "/api/v2/testing-environment" && request.method() == http::Method::GET;
    let mut lease = match tests.enter(&key, cleaning).await {
        Ok(lease) => lease,
        Err(error) => return error.into_response(),
    };
    let response = if cleaning {
        match tests.clean(&mut lease).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => return error.into_response(),
        }
    } else if current {
        Json(&lease.environment).into_response()
    } else {
        request.extensions_mut().insert(EnvironmentContext {
            pool: lease.pool.clone(),
        });
        next.run(request).await
    };
    let success = response.status().is_success();
    if let Err(error) = lease.finish(success).await {
        return error.into_response();
    }
    no_store(response)
}

/// Answers routes that exist only inside a test environment.
pub async fn test_only() -> Response {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":{"code":"test_environment_required","message":"This action is only possible for a test environment. Use remind --test <test_id> <command>.","request_id":crate::request_context::current_request_id()}}))).into_response()
}

/// `POST /api/v2/test-environments`: creates an empty environment owned by the caller.
///
/// # Errors
///
/// Returns validation, duplicate-name, or storage failures.
pub async fn create(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    body: Result<Json<CreateTestEnvironment>, rejection::JsonRejection>,
) -> Result<Response, AppError> {
    crate::application::schedules::require_access_token(&actor)?;
    let Json(input) = body.map_err(|error| super::map_json_rejection(&error))?;
    let (mut environment, key) = manager(&state)?.create(&actor.uuid, input).await?;
    environment.owner = Some(actor.account());
    Ok(no_store(
        (
            StatusCode::CREATED,
            Json(serde_json::json!({"environment":environment,"key":key.expose_secret()})),
        )
            .into_response(),
    ))
}

/// Listing input: UUID cursor and bounded page size.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    include_deleted: bool,
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// `GET /api/v2/test-environments`: environments of the caller's circle.
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
    let mut items = manager(&state)?
        .list(&access(&actor), query.include_deleted, query.after, limit)
        .await?;
    for item in &mut items {
        item.owner = owner_ref(&state, &actor, item).await?;
    }
    let next_cursor = if items.len() == usize::try_from(limit).unwrap_or(0) {
        items.last().map(|row| row.id)
    } else {
        None
    };
    Ok(Json(
        serde_json::json!({"items":items,"next_cursor":next_cursor}),
    ))
}

/// `GET /api/v2/test-environments/{id}`: one environment, including recoverable ones.
///
/// # Errors
///
/// Returns not found outside the caller's circle or after the recovery deadline.
pub async fn get(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<Json<TestEnvironment>, AppError> {
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    let mut environment = manager(&state)?.get(&access(&actor), id).await?;
    environment.owner = owner_ref(&state, &actor, &environment).await?;
    Ok(Json(environment))
}

/// `GET /api/v2/test-environments/{id}/key`: the active key (the owner's circle).
///
/// # Errors
///
/// Returns not found for invisible or retired environments.
pub async fn key(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<Response, AppError> {
    crate::application::schedules::require_access_token(&actor)?;
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    let key = manager(&state)?.key(&access(&actor), id).await?;
    Ok(key_response(id, &key))
}

/// `POST /api/v2/test-environments/{id}/key-rotations`: replaces the key.
///
/// # Errors
///
/// Returns authorization or lifecycle errors, or a storage failure.
pub async fn rotate(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<Response, AppError> {
    crate::application::schedules::require_access_token(&actor)?;
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    let key = manager(&state)?.rotate(&access(&actor), id, false).await?;
    Ok(key_response(id, &key))
}

/// `POST /api/v2/test-environments/{id}/restorations`: restores a retired
/// environment within 30 days, with a new key.
///
/// # Errors
///
/// Returns not found after the recovery deadline, forbidden, or a name conflict.
pub async fn restore(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<Response, AppError> {
    crate::application::schedules::require_access_token(&actor)?;
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    let key = manager(&state)?.rotate(&access(&actor), id, true).await?;
    Ok(key_response(id, &key))
}

/// `DELETE /api/v2/test-environments/{id}`: retires the environment now and
/// starts its 30-day recovery window.
///
/// # Errors
///
/// Returns forbidden for non-managers, not found, or a storage failure.
pub async fn delete(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<StatusCode, AppError> {
    crate::application::schedules::require_access_token(&actor)?;
    let Path(id) = path.map_err(|_| AppError::Validation)?;
    manager(&state)?.delete(&access(&actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What the caller may do with environments: its circle reads them, and the
/// caller manages its own and (for a Carbon) those of the Silicons it looks after.
pub(crate) fn access(actor: &Actor) -> EnvironmentAccess {
    let mut managers = vec![actor.uuid.clone()];
    let mut readers = vec![actor.uuid.clone()];
    for owner in &actor.visible {
        match owner.relation {
            Relation::Custodian => {
                managers.push(owner.account.uuid.clone());
                readers.push(owner.account.uuid.clone());
            }
            Relation::Sibling => readers.push(owner.account.uuid.clone()),
            Relation::Own | Relation::Shared => {}
        }
    }
    if let Some(custodian) = &actor.custodian {
        readers.push(custodian.uuid.clone());
    }
    EnvironmentAccess { readers, managers }
}

async fn owner_ref(
    state: &ApiState,
    actor: &Actor,
    environment: &TestEnvironment,
) -> Result<Option<AccountRef>, AppError> {
    let Some(owner) = environment.owner_uuid.as_deref() else {
        return Ok(None);
    };
    if owner == actor.uuid {
        return Ok(Some(actor.account()));
    }
    if let Some(visible) = actor.visible_by_uuid(owner) {
        return Ok(Some(visible.account.clone()));
    }
    Ok(state.identity.find(owner).await?.map(|row| row.reference()))
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
