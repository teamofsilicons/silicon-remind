//! Request correlation, authentication, and low-cardinality observations.

use std::any::Any;

use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderValue, header},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use chrono::Utc;
use secrecy::{ExposeSecret as _, SecretString};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

use crate::{
    api::ApiState, domain::is_valid_iam_label, error::AppError, infrastructure::iam::IamError,
    metrics::HttpLabels, request_context as correlation,
};

const ORG_HEADER: &str = "x-org-id";
const REQUEST_ID_HEADER: &str = "x-request-id";

pub(crate) fn panic_response(_panic: Box<dyn Any + Send + 'static>) -> Response {
    AppError::internal(
        "request_panic",
        anyhow::anyhow!("request handler panicked; details intentionally redacted"),
    )
    .into_response()
}

/// Establishes one task-local request ID and reflects it in the response.
pub async fn request_context(request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| correlation::is_valid_request_id(value))
        .map_or_else(correlation::generate_request_id, str::to_owned);
    let response = correlation::scope(request_id.clone(), next.run(request)).await;
    with_request_id(response, &request_id)
}

/// Records one HTTP result without logging tenant IDs, UUID paths, or text.
pub async fn observe(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |path| path.as_str().to_owned());
    let method = request.method().as_str().to_owned();
    let response = next.run(request).await;
    let status_class = format!("{}xx", response.status().as_u16() / 100);
    state
        .metrics
        .http_requests
        .get_or_create(&HttpLabels {
            route,
            method,
            status_class,
        })
        .inc();
    response
}

/// Enforces the application deadline while preserving the stable JSON error
/// envelope established by the outer request-correlation middleware.
pub async fn enforce_timeout(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Response {
    match tokio::time::timeout(state.request_timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => AppError::Timeout.into_response(),
    }
}

/// Introspects the public bearer token and installs a strict actor extension.
pub async fn authenticate(
    State(state): State<ApiState>,
    mut request: Request,
    next: Next,
) -> Response {
    let state = state.scoped(request.extensions());
    let result = async {
        let token = bearer_token(request.headers())?;
        let org_id = unique_header(request.headers(), ORG_HEADER)
            .filter(|value| is_valid_iam_label(value))
            .ok_or(AppError::Validation)?;
        let actor = state
            .iam
            .authenticate(&token, org_id, Utc::now())
            .await
            .map_err(|error| map_iam_error(&error))?;
        if let Some(organization_id) = actor.organization_iam_id {
            let mut tx = state.repository.pool().begin().await?;
            sqlx::query("INSERT INTO iam_organization_bindings(organization_id,org_id) VALUES ($1,$2) ON CONFLICT DO NOTHING")
                .bind(organization_id).bind(&actor.org_id).execute(&mut *tx).await?;
            let matches: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM iam_organization_bindings WHERE organization_id=$1 AND org_id=$2)")
                .bind(organization_id).bind(&actor.org_id).fetch_one(&mut *tx).await?;
            if !matches { return Err(AppError::Unauthenticated); }
            tx.commit().await?;
        }
        request.extensions_mut().insert(actor);
        Ok::<Response, AppError>(next.run(request).await)
    }
    .await;

    result.unwrap_or_else(axum::response::IntoResponse::into_response)
}

/// Authenticates service-only endpoints using a constant-time bearer check.
pub async fn authenticate_internal(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Response {
    let result = bearer_token(request.headers()).and_then(|provided| {
        if secrets_equal(&provided, &state.internal_api_token) {
            Ok(())
        } else {
            Err(AppError::Unauthenticated)
        }
    });
    match result {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

fn bearer_token(headers: &http::HeaderMap) -> Result<SecretString, AppError> {
    let value =
        unique_header(headers, header::AUTHORIZATION.as_str()).ok_or(AppError::Unauthenticated)?;
    let token = value
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty() && !token.bytes().any(|byte| byte.is_ascii_whitespace()))
        .ok_or(AppError::Unauthenticated)?;
    Ok(SecretString::from(token))
}

fn unique_header<'a>(headers: &'a http::HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

fn map_iam_error(error: &IamError) -> AppError {
    match error {
        IamError::Unauthenticated => AppError::Unauthenticated,
        IamError::Unavailable(_) => AppError::DependencyUnavailable { dependency: "iam" },
    }
}

fn secrets_equal(provided: &SecretString, expected: &SecretString) -> bool {
    let provided = Sha256::digest(provided.expose_secret().as_bytes());
    let expected = Sha256::digest(expected.expose_secret().as_bytes());
    bool::from(provided.as_slice().ct_eq(expected.as_slice()))
}

fn with_request_id(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header};
    use secrecy::SecretString;

    use crate::domain::is_valid_iam_label;

    use super::{bearer_token, secrets_equal};

    #[test]
    fn service_token_comparison_is_exact() {
        let expected = SecretString::from("a sufficiently long service token");
        assert!(secrets_equal(&expected, &expected));
        assert!(!secrets_equal(
            &SecretString::from("a sufficiently long service tokem"),
            &expected
        ));
    }

    #[test]
    fn organization_id_rejects_control_or_path_bytes() {
        assert!(is_valid_iam_label("org_team-1"));
        for value in ["ORGANIZATION", "org:team", "org/team", "org\nteam"] {
            assert!(!is_valid_iam_label(value));
        }
        assert!(is_valid_iam_label(&"o".repeat(50)));
        assert!(!is_valid_iam_label(&"o".repeat(51)));
    }

    #[test]
    fn duplicate_authorization_headers_are_rejected() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer first-token"),
        );
        headers.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer second-token"),
        );
        assert!(bearer_token(&headers).is_err());
    }
}
