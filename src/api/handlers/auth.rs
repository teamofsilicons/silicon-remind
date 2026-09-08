//! Application login endpoints. IAM app credentials never leave the backend.

use axum::{
    Extension, Json,
    extract::rejection::JsonRejection,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse as _, Response},
};
use secrecy::SecretString;
use serde::Deserialize;
use silicon_iam_client::Mutation;

use crate::api::ScopedState;

use crate::{domain::Actor, error::AppError, infrastructure::iam::IamError};

/// A short-lived token minted by IAM for this application.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    slt: SecretString,
}

/// An existing application refresh token.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshRequest {
    refresh_token: SecretString,
}

/// A token to revoke. A refresh token revokes its whole family.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogoutRequest {
    token: SecretString,
}

/// Exchanges an SLT for an IAM application session.
///
/// # Errors
///
/// Returns validation errors for malformed input, unauthorized for rejected SLTs, or dependency unavailable when IAM cannot answer.
pub async fn login(
    ScopedState(state): ScopedState,
    headers: HeaderMap,
    body: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    let Json(input) = body.map_err(|error| super::internal::map_json_rejection(&error))?;
    let tokens = state
        .iam
        .login(&input.slt, &mutation(&headers)?)
        .await
        .map_err(|error| map_error(&error))?;
    Ok(no_store(Json(tokens).into_response()))
}

/// Rotates an IAM application refresh token.
///
/// # Errors
///
/// Returns validation errors, unauthorized for expired or spent refresh tokens, or an IAM availability error.
pub async fn refresh(
    ScopedState(state): ScopedState,
    headers: HeaderMap,
    body: Result<Json<RefreshRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    let Json(input) = body.map_err(|error| super::internal::map_json_rejection(&error))?;
    let tokens = state
        .iam
        .refresh(&input.refresh_token, &mutation(&headers)?)
        .await
        .map_err(|error| map_error(&error))?;
    Ok(no_store(Json(tokens).into_response()))
}

/// Revokes application authority in IAM.
///
/// # Errors
///
/// Returns validation errors or an IAM failure; no local session state is cached.
pub async fn logout(
    ScopedState(state): ScopedState,
    headers: HeaderMap,
    body: Result<Json<LogoutRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    let Json(input) = body.map_err(|error| super::internal::map_json_rejection(&error))?;
    state
        .iam
        .logout(&input.token, &mutation(&headers)?)
        .await
        .map_err(|error| map_error(&error))?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

/// Returns the actor's current organization-bound identity and permissions.
pub async fn me(Extension(actor): Extension<Actor>) -> Response {
    no_store(Json(identity(&actor)).into_response())
}

/// Lists the organizations explicitly authorized through IAM for this session.
///
/// # Errors
/// Returns unauthorized for invalid authority or unavailable when IAM cannot answer.
pub async fn organizations(
    ScopedState(state): ScopedState,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let token = crate::api::middleware::bearer_token(&headers)?;
    let actors = state
        .iam
        .organizations(&token, chrono::Utc::now())
        .await
        .map_err(|error| map_error(&error))?;
    let items: Vec<_> = actors.iter().map(identity).collect();
    Ok(no_store(
        Json(serde_json::json!({"items": items})).into_response(),
    ))
}

fn identity(actor: &Actor) -> serde_json::Value {
    serde_json::json!({
            "principal_id": actor.id, "actor_type": actor.kind, "public_id": actor.public_id,
            "org_id": actor.org_id, "membership_id": actor.membership_id,
            "org_role": actor.org_role, "authorization_epoch": actor.authorization_epoch,
            "can_manage_reminders": actor.kind == crate::domain::ActorKind::Silicon,
    })
}

fn mutation(headers: &HeaderMap) -> Result<Mutation, AppError> {
    let mut values = headers.get_all("idempotency-key").iter();
    match (values.next(), values.next()) {
        (None, None) => Ok(Mutation::new()),
        (Some(value), None) => {
            let key = silicon_iam_client::IdempotencyKey::parse(
                value.to_str().map_err(|_| AppError::Validation)?,
            )
            .map_err(|_| AppError::Validation)?;
            Ok(Mutation::with_key(key))
        }
        _ => Err(AppError::Validation),
    }
}

fn map_error(error: &IamError) -> AppError {
    match error {
        IamError::Unauthenticated => AppError::Unauthenticated,
        IamError::Unavailable(_) => AppError::DependencyUnavailable { dependency: "iam" },
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
