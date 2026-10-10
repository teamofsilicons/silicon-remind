//! Viewer grants (`/viewers`) and Silicon allow-lists (`/allowed-accounts`).
//! Both describe real accounts, so they are refused inside a test environment.

use axum::{
    Extension, Json,
    extract::{Path, Query, rejection},
    http::StatusCode,
};

use crate::{
    api::{ScopedState, models},
    application::sharing::ViewerGrants,
    domain::Actor,
    error::AppError,
    infrastructure::sharing::{Allowance, ViewerGrant},
};

/// `GET /api/v2/viewers`: grants on the caller's (or its Silicons') reminders,
/// and grants the caller received.
///
/// # Errors
///
/// Returns database errors.
pub async fn list_viewers(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
) -> Result<Json<ViewerGrants>, AppError> {
    Ok(Json(state.sharing.grants(&actor).await?))
}

/// `POST /api/v2/viewers`: lets an account read a Silicon's reminders.
///
/// # Errors
///
/// Returns validation, authorization, allow-list, or lookup errors.
pub async fn grant_viewer(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    body: Result<Json<models::AccountTargetRequest>, rejection::JsonRejection>,
) -> Result<(StatusCode, Json<ViewerGrant>), AppError> {
    refuse_in_test_environment(state.is_test)?;
    let Json(request) = body.map_err(|error| super::map_json_rejection(&error))?;
    let (grant, created) = state
        .sharing
        .grant(&actor, &request.id, request.silicon_id.as_deref())
        .await?;
    Ok((created_or_ok(created), Json(grant)))
}

/// `DELETE /api/v2/viewers/{viewer}`: ends a grant.
///
/// # Errors
///
/// Returns authorization or not-found errors.
pub async fn revoke_viewer(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    path: Result<Path<String>, rejection::PathRejection>,
    query: Result<Query<models::SiliconSelector>, rejection::QueryRejection>,
) -> Result<StatusCode, AppError> {
    refuse_in_test_environment(state.is_test)?;
    let Path(viewer) = path.map_err(|_| AppError::Validation)?;
    let Query(query) = query.map_err(|_| AppError::Validation)?;
    state
        .sharing
        .revoke(&actor, &viewer, query.silicon_id.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v2/allowed-accounts`: the allow-list of the calling Silicon or of
/// the Silicons the calling Carbon looks after.
///
/// # Errors
///
/// Returns authorization or database errors.
pub async fn list_allowed(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    query: Result<Query<models::SiliconSelector>, rejection::QueryRejection>,
) -> Result<Json<serde_json::Value>, AppError> {
    let Query(query) = query.map_err(|_| AppError::Validation)?;
    let items = state
        .sharing
        .allowances(&actor, query.silicon_id.as_deref())
        .await?;
    Ok(Json(serde_json::json!({ "items": items })))
}

/// `POST /api/v2/allowed-accounts`: allows an account to share reminders with a Silicon.
///
/// # Errors
///
/// Returns validation, authorization, or lookup errors.
pub async fn allow_account(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    body: Result<Json<models::AccountTargetRequest>, rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Allowance>), AppError> {
    refuse_in_test_environment(state.is_test)?;
    let Json(request) = body.map_err(|error| super::map_json_rejection(&error))?;
    let (allowance, created) = state
        .sharing
        .allow(&actor, &request.id, request.silicon_id.as_deref())
        .await?;
    Ok((created_or_ok(created), Json(allowance)))
}

/// `DELETE /api/v2/allowed-accounts/{account}`: removes an allow-list entry and
/// the grants it made possible.
///
/// # Errors
///
/// Returns authorization or not-found errors.
pub async fn disallow_account(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    path: Result<Path<String>, rejection::PathRejection>,
    query: Result<Query<models::SiliconSelector>, rejection::QueryRejection>,
) -> Result<StatusCode, AppError> {
    refuse_in_test_environment(state.is_test)?;
    let Path(account) = path.map_err(|_| AppError::Validation)?;
    let Query(query) = query.map_err(|_| AppError::Validation)?;
    state
        .sharing
        .disallow(&actor, &account, query.silicon_id.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

const fn created_or_ok(created: bool) -> StatusCode {
    if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    }
}

fn refuse_in_test_environment(is_test: bool) -> Result<(), AppError> {
    if is_test {
        Err(AppError::forbidden(
            "not_in_test_environment",
            "Sharing applies to real accounts and their reminders; run it without --test.",
        ))
    } else {
        Ok(())
    }
}
