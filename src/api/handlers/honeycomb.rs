//! Honeycomb service control, independent of IAM test sessions and app secrets.
use crate::{
    api::ApiState,
    error::AppError,
    infrastructure::testing::{TestEnvironments, honeycomb::Operation},
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use secrecy::ExposeSecret as _;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

fn authenticate<'a>(
    state: &'a ApiState,
    headers: &HeaderMap,
) -> Result<&'a TestEnvironments, AppError> {
    verify_service_token(state.honeycomb_service_token.as_ref(), headers)?;
    state.tests.as_ref().ok_or(AppError::DependencyUnavailable {
        dependency: "testing_postgresql",
    })
}
fn verify_service_token(
    expected: Option<&secrecy::SecretString>,
    headers: &HeaderMap,
) -> Result<(), AppError> {
    if headers.get_all("authorization").iter().count() != 1 {
        return Err(AppError::Unauthenticated);
    }
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthenticated)?;
    let expected = expected.ok_or(AppError::Unauthenticated)?;
    if !bool::from(
        Sha256::digest(token.as_bytes())
            .ct_eq(&Sha256::digest(expected.expose_secret().as_bytes())),
    ) {
        return Err(AppError::Unauthenticated);
    }
    Ok(())
}
/// Executes a coordinator operation without requiring enabled test sessions.
/// # Errors
/// Rejects service authentication, path mismatches, stale operations or storage errors.
pub async fn apply(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path((org, id, operation)): Path<(String, Uuid, Uuid)>,
    Json(body): Json<Operation>,
) -> Result<Json<Value>, AppError> {
    let tests = authenticate(&state, &headers)?;
    if body.org_id != org || body.environment_id != id || body.operation_id != operation {
        return Err(AppError::Validation);
    }
    Ok(Json(tests.apply_honeycomb(&body).await?))
}
/// Reads a durable operation receipt without exposing secrets.
/// # Errors
/// Rejects invalid service credentials or an unknown operation.
pub async fn receipt(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path((org, id, operation)): Path<(String, Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(
        authenticate(&state, &headers)?
            .honeycomb_receipt(&org, id, operation)
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use secrecy::SecretString;

    #[test]
    fn only_the_dedicated_service_token_authorizes_control_operations() -> anyhow::Result<()> {
        let expected = SecretString::from("service-credential-01234567890123456789");
        let mut headers = HeaderMap::new();
        assert!(verify_service_token(Some(&expected), &headers).is_err());
        for rejected in [
            "ask_application_secret",
            "environment-root-key-0123456789",
            "internal-provisioning-token",
            "user-session",
        ] {
            headers.insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {rejected}"))?,
            );
            assert!(verify_service_token(Some(&expected), &headers).is_err());
        }
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", expected.expose_secret()))?,
        );
        assert!(verify_service_token(None, &headers).is_err());
        verify_service_token(Some(&expected), &headers)?;
        headers.append(
            "authorization",
            HeaderValue::from_static("Bearer another-token"),
        );
        assert!(verify_service_token(Some(&expected), &headers).is_err());
        Ok(())
    }
}
