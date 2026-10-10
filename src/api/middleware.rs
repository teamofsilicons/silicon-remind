//! Request correlation, authentication, and low-cardinality observations.

use std::any::Any;

use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderValue, header},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use secrecy::{ExposeSecret as _, SecretString};

use crate::{
    api::ApiState, config::SCHEDULES_READ_SCOPE, domain::ReadScope, error::AppError,
    infrastructure::accounts::AccountsError, metrics::HttpLabels, request_context as correlation,
};

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
    let scoped = state.scoped(request.extensions());
    let enabled = request
        .headers()
        .get("x-remind-telemetry")
        .is_none_or(|v| v != "off");
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    if enabled
        && !route.contains("/telemetry/")
        && !route.starts_with("/health/")
        && route != "/metrics"
    {
        scoped.telemetry.record(scoped.repository.pool(), scoped.is_test, serde_json::json!({
            "source":"backend", "event":"request_completed", "step":"http_response", "progress":1.0,
            "route":route,"method":method,"status_code":response.status().as_u16(),
            "duration_ms":u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "request_id":crate::request_context::current_request_id()
        })).await;
    }
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

/// Routes that accept `Authorization: Proof sap_…` (GET only, scope
/// `remind.schedules.read`). Everything else needs an access token.
pub const PROOF_READ_ROUTES: &[&str] = &[
    "/api/v2/schedules",
    "/api/v2/schedules/{schedule_id}",
    "/api/v2/schedules/{schedule_id}/executions",
    "/api/v2/silicons",
    "/api/v2/auth/me",
];

/// Routes where a revoked sign-in must be refused at once: Remind asks Silicon
/// Accounts (introspection, cached up to 30 seconds) instead of trusting the
/// token's signature alone.
pub const INTROSPECTED_ROUTES: &[(&str, &str)] = &[
    ("DELETE", "/api/v2/schedules/{schedule_id}"),
    ("PUT", "/api/v2/webhook"),
    ("DELETE", "/api/v2/webhook"),
    ("POST", "/api/v2/webhooks"),
    ("DELETE", "/api/v2/webhooks/{subscription_id}"),
    ("POST", "/api/v2/viewers"),
    ("DELETE", "/api/v2/viewers/{viewer}"),
    ("POST", "/api/v2/allowed-accounts"),
    ("DELETE", "/api/v2/allowed-accounts/{account}"),
    ("GET", "/api/v2/test-environments/{id}/key"),
    ("POST", "/api/v2/test-environments/{id}/key-rotations"),
    ("POST", "/api/v2/test-environments/{id}/restorations"),
    ("DELETE", "/api/v2/test-environments/{id}"),
];

/// The credential a request presented.
#[derive(Debug)]
pub(crate) enum Presented {
    /// `Authorization: Bearer <Silicon Accounts access token>`.
    Bearer(SecretString),
    /// `Authorization: Proof <sap_… User verification proof>`.
    Proof(SecretString),
}

/// Authenticates every API route and installs the [`Actor`].
pub async fn authenticate(
    State(state): State<ApiState>,
    mut request: Request,
    next: Next,
) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_default();
    let method = request.method().clone();
    let result = async {
        let mut actor = match presented_credential(request.headers())? {
            Presented::Bearer(token) => {
                let gateway = state.identity.gateway();
                let claims = gateway
                    .verify_access_token(token.expose_secret())
                    .await
                    .map_err(map_accounts_error)?;
                state.request_budget.admit(&claims.sub)?;
                if INTROSPECTED_ROUTES.contains(&(method.as_str(), route.as_str()))
                    && !gateway
                        .token_is_active(token.expose_secret())
                        .await
                        .map_err(map_accounts_error)?
                {
                    return Err(AppError::unauthenticated(
                        "token_revoked",
                        "Silicon Accounts reports this access token is no longer active (signed out or access removed). Sign in again.",
                    ));
                }
                state.identity.resolve_bearer(&claims).await?
            }
            Presented::Proof(token) => {
                if method != http::Method::GET || !PROOF_READ_ROUTES.contains(&route.as_str()) {
                    return Err(AppError::unauthenticated(
                        "proof_not_accepted",
                        "Remind accepts proofs only for reading: GET /api/v2/schedules, /schedules/{id}, /schedules/{id}/executions, /silicons and /auth/me. Everything else needs the account's own access token.",
                    ));
                }
                if state.proof_issuers.is_empty() {
                    return Err(AppError::forbidden("proof_issuer_not_allowed", "This server has not enabled proof access from any application."));
                }
                let raw = token.expose_secret();
                if !raw.starts_with("sap_") || raw.len() > 4096 {
                    return Err(AppError::unauthenticated("proof_invalid", "The proof has an invalid format."));
                }
                let proof = state
                    .identity
                    .gateway()
                    .verify_proof(token.expose_secret())
                    .await
                    .map_err(map_accounts_error)?
                    .ok_or_else(|| {
                        AppError::unauthenticated(
                            "proof_invalid",
                            "The proof is not valid for Remind right now: it is unknown, expired, revoked, or was issued for another app.",
                        )
                    })?;
                check_proof(&state, &proof)?;
                if let Some(user) = &proof.user { state.request_budget.admit(&user.uuid)?; }
                state
                    .identity
                    .resolve_proof(&proof, proof.scopes.clone())
                    .await?
            }
        };
        // Whoever holds a test environment's key reads everything in it.
        if request
            .extensions()
            .get::<super::handlers::testing::EnvironmentContext>()
            .is_some()
        {
            actor.read = ReadScope::Everything;
        }
        request.extensions_mut().insert(actor);
        Ok::<Response, AppError>(next.run(request).await)
    }
    .await;
    result.unwrap_or_else(axum::response::IntoResponse::into_response)
}

/// Checks a valid proof against Remind's rules: a User verification proof
/// for Remind, carrying a scope Remind honours, from an app allowed to send it.
fn check_proof(
    state: &ApiState,
    proof: &silicon_accounts_client::ValidProof,
) -> Result<(), AppError> {
    if proof.kind != silicon_accounts_client::ProofKind::UserVerification || proof.user.is_none() {
        return Err(AppError::forbidden(
            "proof_kind_not_accepted",
            "Remind accepts only User verification proofs, which act for one account.",
        ));
    }
    if proof.receiving_app.app_id != state.identity.gateway().app_id() {
        return Err(AppError::unauthenticated(
            "proof_invalid",
            "The proof was issued for another app.",
        ));
    }
    if !proof
        .scopes
        .iter()
        .any(|scope| scope == SCHEDULES_READ_SCOPE)
    {
        return Err(AppError::forbidden(
            "proof_scope_missing",
            format!("The proof does not carry the `{SCHEDULES_READ_SCOPE}` scope Remind requires."),
        ));
    }
    let issuer = &proof.issuing_app.app_id;
    if !state.proof_issuers.allows(SCHEDULES_READ_SCOPE, issuer) {
        return Err(AppError::forbidden(
            "proof_issuer_not_allowed",
            format!(
                "Remind does not accept `{SCHEDULES_READ_SCOPE}` proofs from `{issuer}`. The operator lists accepted apps in REMIND_PROOF_ISSUERS."
            ),
        ));
    }
    Ok(())
}

/// Maps a Silicon Accounts failure to the HTTP error the caller sees.
pub(crate) fn map_accounts_error(error: AccountsError) -> AppError {
    match error {
        AccountsError::Rejected { code, message } => AppError::unauthenticated(code, message),
        AccountsError::Unavailable(detail) => {
            tracing::warn!(error.detail = %detail, "Silicon Accounts unavailable");
            AppError::DependencyUnavailable {
                dependency: "accounts",
            }
        }
        AccountsError::RateLimited => AppError::RateLimited {
            retry_after_seconds: 60,
        },
    }
}

/// Reads the single `Authorization` header: `Bearer <token>` or `Proof <token>`.
pub(crate) fn presented_credential(headers: &http::HeaderMap) -> Result<Presented, AppError> {
    let value = unique_header(headers, header::AUTHORIZATION.as_str()).ok_or_else(|| {
        AppError::unauthenticated(
            "unauthenticated",
            "Send `Authorization: Bearer <access token>` with a Silicon Accounts access token issued to Remind (sign in with `remind login`), or `Authorization: Proof <sap_…>` from an app allowed to read for an account.",
        )
    })?;
    let (scheme, token) = value.split_once(' ').unwrap_or((value, ""));
    if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(AppError::unauthenticated(
            "unauthenticated",
            "The Authorization header must be `Bearer <token>` or `Proof <token>` with exactly one token.",
        ));
    }
    match scheme.to_ascii_lowercase().as_str() {
        "bearer" if token.starts_with("sap_") => Err(AppError::unauthenticated(
            "proof_as_bearer",
            "That is a User verification proof; send it as `Authorization: Proof sap_…`.",
        )),
        "bearer" if token.starts_with("oat_") || token.starts_with("ort_") => {
            Err(AppError::unauthenticated(
                "legacy_token_rejected",
                "Remind no longer accepts this kind of token. Sign in with Silicon Accounts (`remind login`) and send its access token.",
            ))
        }
        "bearer" => Ok(Presented::Bearer(SecretString::from(token))),
        "proof" => Ok(Presented::Proof(SecretString::from(token))),
        _ => Err(AppError::unauthenticated(
            "unauthenticated",
            "Remind accepts the Bearer and Proof authorization schemes only.",
        )),
    }
}

fn unique_header<'a>(headers: &'a http::HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
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

    use super::{Presented, presented_credential};

    fn headers(values: &[&'static str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::AUTHORIZATION, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn bearer_and_proof_schemes_are_recognised() {
        assert!(matches!(
            presented_credential(&headers(&["Bearer eyJ.a.b"])),
            Ok(Presented::Bearer(_))
        ));
        assert!(matches!(
            presented_credential(&headers(&["bearer eyJ.a.b"])),
            Ok(Presented::Bearer(_))
        ));
        assert!(matches!(
            presented_credential(&headers(&["Proof sap_abc"])),
            Ok(Presented::Proof(_))
        ));
    }

    #[test]
    fn malformed_duplicate_and_legacy_credentials_are_refused_with_reasons() {
        for (values, code) in [
            (vec![], "unauthenticated"),
            (vec!["Bearer first", "Bearer second"], "unauthenticated"),
            (vec!["Bearer"], "unauthenticated"),
            (vec!["Basic dXNlcjpwYXNz"], "unauthenticated"),
            (vec!["Bearer sap_proof"], "proof_as_bearer"),
            (vec!["Bearer oat_legacy"], "legacy_token_rejected"),
        ] {
            let error = presented_credential(&headers(&values)).err();
            assert_eq!(
                error.map(|error| error.code()),
                Some(code.into()),
                "{values:?}"
            );
        }
    }
}
