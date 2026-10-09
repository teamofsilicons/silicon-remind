//! Versioned public route handlers and the Silicon Accounts webhook receiver.

pub mod accounts;
pub mod accounts_webhook;
pub mod destination;
pub mod health;
pub mod reports;
pub mod schedules;
pub mod sharing;
pub(crate) mod telemetry;
pub mod testing;

use axum::response::{IntoResponse as _, Response};

use crate::error::AppError;

/// Stable JSON fallback for unknown routes.
pub async fn not_found() -> Response {
    AppError::NotFound.into_response()
}

/// Stable JSON fallback for unsupported methods on known routes.
pub async fn method_not_allowed() -> Response {
    AppError::MethodNotAllowed.into_response()
}

/// Maps a JSON body rejection: oversize bodies are 413, everything else 422.
pub(crate) fn map_json_rejection(rejection: &axum::extract::rejection::JsonRejection) -> AppError {
    if matches!(
        rejection,
        axum::extract::rejection::JsonRejection::BytesRejection(_)
    ) {
        AppError::PayloadTooLarge
    } else {
        AppError::Validation
    }
}
