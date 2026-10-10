//! `POST /webhook/`: Silicon Accounts app webhook deliveries.
//!
//! The signature is verified over the raw body bytes with Remind's `whsec_…`
//! secret (any configured secret, so a rotation can overlap), with the
//! standard five-minute timestamp tolerance. Unsigned or wrongly signed
//! deliveries get 401, signed but malformed bodies 400, and everything else
//! 200 once applied (or recognised as a duplicate, or ignored).

use axum::{
    Json,
    body::Bytes,
    extract::{State, rejection},
    http::{HeaderMap, StatusCode},
};
use secrecy::ExposeSecret as _;
use silicon_accounts_client::{
    DEFAULT_WEBHOOK_TOLERANCE, SIGNATURE_HEADER, TIMESTAMP_HEADER, WebhookError,
    verify_and_parse_webhook,
};

use crate::{
    api::{ApiState, models::AccountsEventAccepted},
    error::AppError,
    infrastructure::account_events,
};

/// Verifies and applies one Silicon Accounts webhook delivery.
///
/// # Errors
///
/// Returns 401 for a missing or mismatched signature or a stale timestamp, 400
/// for a signed body that is not an event, and 5xx when applying it failed
/// (Silicon Accounts then retries; the event id keeps it from applying twice).
pub async fn receive(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Bytes, rejection::BytesRejection>,
) -> Result<(StatusCode, Json<AccountsEventAccepted>), AppError> {
    let body = body.map_err(|_| AppError::PayloadTooLarge)?;
    let timestamp = header_value(&headers, TIMESTAMP_HEADER);
    let signature = header_value(&headers, SIGNATURE_HEADER);
    let mut failure = None;
    let mut verified = None;
    for secret in &state.webhook_secrets {
        match verify_and_parse_webhook(
            secret.expose_secret(),
            timestamp,
            signature,
            &body,
            DEFAULT_WEBHOOK_TOLERANCE,
        ) {
            Ok(event) => {
                verified = Some(event);
                break;
            }
            Err(WebhookError::InvalidBody(reason)) => {
                return Err(AppError::described(
                    StatusCode::BAD_REQUEST,
                    "webhook_body_invalid",
                    format!("The signed body is not a Silicon Accounts event: {reason}"),
                ));
            }
            Err(error) => failure = Some(error),
        }
    }
    let event = verified.ok_or_else(|| {
        let reason = failure.map_or_else(
            || "no webhook secret is configured".to_owned(),
            |error| error.to_string(),
        );
        AppError::unauthenticated("webhook_signature_invalid", reason)
    })?;
    let outcome =
        account_events::apply(&state.identity, state.tests.as_ref(), &event, &body).await?;
    tracing::info!(
        event.kind = %event.event_type,
        event.outcome = outcome.as_str(),
        "Silicon Accounts webhook delivery handled"
    );
    Ok((
        StatusCode::OK,
        Json(AccountsEventAccepted {
            event_id: event.event_id,
            status: outcome.as_str(),
        }),
    ))
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}
