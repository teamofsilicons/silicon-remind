//! Versioned public and internal route handlers.

pub mod auth;
pub mod destination;
pub mod health;
pub mod internal;
pub mod schedules;
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
