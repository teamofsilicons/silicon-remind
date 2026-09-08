//! Stable application errors and their public HTTP representation.

use std::borrow::Cow;

use axum::{Json, http::StatusCode};
use serde::Serialize;
use thiserror::Error;
use tracing::{error, warn};

/// Error returned by a Remind application use case or transport boundary.
#[derive(Debug, Error)]
pub enum AppError {
    /// Request input is syntactically valid but violates domain validation.
    #[error("request validation failed")]
    Validation,
    /// Credential is absent, malformed, inactive, expired, or revoked.
    #[error("authentication is required")]
    Unauthenticated,
    /// Authenticated actor lacks authority for the requested operation.
    #[error("the actor is not authorized for this action")]
    Forbidden,
    /// Resource does not exist or is intentionally hidden from the actor.
    #[error("resource was not found")]
    NotFound,
    /// Mutation conflicts with current state or an idempotency invariant.
    #[error("request conflicts with current state")]
    Conflict {
        /// Stable, machine-readable conflict code.
        code: Cow<'static, str>,
    },
    /// A legacy single-subscription endpoint has no active receiver.
    #[error("no active webhook subscription is configured")]
    WebhookNotConfigured,
    /// Caller exceeded a request or abuse-control limit.
    #[error("rate limit exceeded")]
    RateLimited {
        /// Seconds the client should wait before retrying.
        retry_after_seconds: u64,
    },
    /// Request processing exceeded the configured server deadline.
    #[error("request processing deadline exceeded")]
    Timeout,
    /// Request body exceeds the configured maximum.
    #[error("request body is too large")]
    PayloadTooLarge,
    /// The route exists but not for the requested HTTP method.
    #[error("method is not allowed for this route")]
    MethodNotAllowed,
    /// A required remote or durable dependency is temporarily unavailable.
    #[error("required dependency {dependency} is unavailable")]
    DependencyUnavailable {
        /// Static dependency label safe for operational logs.
        dependency: &'static str,
    },
    /// A transport layer rejected a request before a typed handler ran.
    #[error("request was rejected with HTTP status {status}")]
    TransportRejected {
        /// Original transport status preserved in the response.
        status: StatusCode,
    },
    /// Unexpected internal failure whose source must not cross the API boundary.
    #[error("internal service error in {category}")]
    Internal {
        /// Static subsystem label safe for operational logs.
        category: &'static str,
        /// Original error retained for diagnostics and error chaining.
        #[source]
        source: anyhow::Error,
    },
}

/// Public error envelope defined by `openapi.yaml`.
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    /// Stable public error details.
    pub error: PublicError,
}

/// Public, non-sensitive error details.
#[derive(Debug, Serialize)]
pub struct PublicError {
    /// Stable machine-readable error code.
    pub code: Cow<'static, str>,
    /// Human-readable non-sensitive summary.
    pub message: &'static str,
    /// Correlation identifier for support and logs.
    pub request_id: String,
}

impl AppError {
    /// Creates a state-conflict error with a stable domain code.
    #[must_use]
    pub fn conflict(code: impl Into<Cow<'static, str>>) -> Self {
        Self::Conflict { code: code.into() }
    }

    /// Creates an internal error while retaining its source for diagnostics.
    pub fn internal(category: &'static str, source: impl Into<anyhow::Error>) -> Self {
        Self::Internal {
            category,
            source: source.into(),
        }
    }

    /// Returns the HTTP status exposed for this failure class.
    #[must_use]
    pub const fn status_code(&self) -> StatusCode {
        match self {
            Self::Validation => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict { .. } | Self::WebhookNotConfigured => StatusCode::CONFLICT,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Timeout => StatusCode::REQUEST_TIMEOUT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::DependencyUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::TransportRejected { status } => *status,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Returns the stable public error code.
    #[must_use]
    pub fn code(&self) -> Cow<'static, str> {
        match self {
            Self::Validation => Cow::Borrowed("validation_failed"),
            Self::Unauthenticated => Cow::Borrowed("unauthenticated"),
            Self::Forbidden => Cow::Borrowed("forbidden"),
            Self::NotFound => Cow::Borrowed("not_found"),
            Self::Conflict { code } => code.clone(),
            Self::WebhookNotConfigured => Cow::Borrowed("webhook_not_configured"),
            Self::RateLimited { .. } => Cow::Borrowed("rate_limited"),
            Self::Timeout => Cow::Borrowed("request_timeout"),
            Self::PayloadTooLarge => Cow::Borrowed("payload_too_large"),
            Self::MethodNotAllowed => Cow::Borrowed("method_not_allowed"),
            Self::DependencyUnavailable { .. } => Cow::Borrowed("dependency_unavailable"),
            Self::TransportRejected { .. } => Cow::Borrowed("request_rejected"),
            Self::Internal { .. } => Cow::Borrowed("internal_error"),
        }
    }

    fn public_message(&self) -> &'static str {
        match self {
            Self::Validation => "The request contains invalid data.",
            Self::Unauthenticated => "Authentication is required.",
            Self::Forbidden => "The actor is not authorized for this action.",
            Self::NotFound => "The requested resource was not found.",
            Self::Conflict { code } if code.as_ref() == "test_reminder_limit" => {
                "Test environments allow at most 100 retained reminders. This limit applies only to test environments."
            }
            Self::Conflict { code } if code.as_ref() == "test_iam_application_not_configured" => {
                "Configure the test-only IAM Application secret first: remind --test <test_id> configure-iam. Production credentials are never used."
            }
            Self::Conflict { .. } => "The request conflicts with the current resource state.",
            Self::WebhookNotConfigured => "No active webhook subscription is configured.",
            Self::RateLimited { .. } => "Too many requests. Retry later.",
            Self::Timeout => "The request exceeded its processing deadline.",
            Self::PayloadTooLarge => "The request body exceeds the allowed size.",
            Self::MethodNotAllowed => "The HTTP method is not allowed for this route.",
            Self::DependencyUnavailable { .. } => "A required service is temporarily unavailable.",
            Self::TransportRejected { .. } => "The HTTP request was rejected.",
            Self::Internal { .. } => "An internal service error occurred.",
        }
    }

    fn record_diagnostic(&self) {
        match self {
            Self::DependencyUnavailable { dependency } => {
                warn!(
                    error.dependency = *dependency,
                    "required dependency unavailable"
                );
            }
            Self::Internal { category, .. } => {
                // The source is intentionally excluded: arbitrary provider and
                // database errors can contain credentials or reminder text.
                error!(error.category = *category, "internal application failure");
            }
            _ => {}
        }
    }
}

impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        self.record_diagnostic();
        let retry_after_seconds = match &self {
            Self::RateLimited {
                retry_after_seconds,
            } => Some(*retry_after_seconds),
            _ => None,
        };
        let mut response = (
            self.status_code(),
            Json(ErrorEnvelope {
                error: PublicError {
                    code: self.code(),
                    message: self.public_message(),
                    request_id: crate::request_context::current_request_id_or_unavailable(),
                },
            }),
        )
            .into_response();

        if let Some(seconds) = retry_after_seconds
            && let Ok(value) = http::HeaderValue::from_str(&seconds.to_string())
        {
            response
                .headers_mut()
                .insert(http::header::RETRY_AFTER, value);
        }
        response
    }
}

impl From<sqlx::Error> for AppError {
    fn from(source: sqlx::Error) -> Self {
        if let Some(database) = source.as_database_error() {
            match database.constraint() {
                Some("test_reminder_limit") => return Self::conflict("test_reminder_limit"),
                Some("testing_environments_active_name") => {
                    return Self::conflict("test_environment_name_taken");
                }
                _ => {}
            }
        }
        let category = match source {
            sqlx::Error::PoolTimedOut => "database_pool_timeout",
            sqlx::Error::PoolClosed => "database_pool_closed",
            sqlx::Error::RowNotFound => "database_row_not_found",
            _ => "database_operation",
        };
        Self::internal(category, source)
    }
}

impl From<crate::infrastructure::postgres::RepositoryError> for AppError {
    fn from(error: crate::infrastructure::postgres::RepositoryError) -> Self {
        use crate::infrastructure::postgres::RepositoryError;

        match error {
            RepositoryError::IdempotencyConflict => Self::conflict("idempotency_conflict"),
            RepositoryError::IdempotencyIncomplete => Self::conflict("idempotency_in_progress"),
            RepositoryError::EventReceiptConflict => Self::conflict("event_id_conflict"),
            RepositoryError::SiliconUnavailable => Self::conflict("silicon_unavailable"),
            RepositoryError::WebhookNotConfigured => Self::WebhookNotConfigured,
            RepositoryError::NotFound => Self::NotFound,
            RepositoryError::VersionConflict => Self::conflict("schedule_version_conflict"),
            RepositoryError::InvalidState => Self::conflict("invalid_schedule_state"),
            RepositoryError::InvalidInput(_) => Self::Validation,
            RepositoryError::LeaseLost => {
                Self::internal("worker_lease_lost", anyhow::anyhow!("delivery lease lost"))
            }
            RepositoryError::Database(source) => Self::from(source),
            RepositoryError::Serialization(source) => {
                Self::internal("idempotency_serialization", source)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse as _;
    use http_body_util::BodyExt as _;
    use serde_json::Value;

    use super::AppError;

    #[tokio::test]
    async fn response_matches_openapi_envelope_and_request_scope() -> anyhow::Result<()> {
        let response = crate::request_context::scope("req_42".to_owned(), async {
            AppError::NotFound.into_response()
        })
        .await;
        assert_eq!(response.status(), http::StatusCode::NOT_FOUND);

        let body = response.into_body().collect().await?.to_bytes();
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(value["error"]["code"], "not_found");
        assert_eq!(value["error"]["request_id"], "req_42");
        assert!(value["error"].get("message").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn missing_webhook_uses_the_product_required_message() -> anyhow::Result<()> {
        let response = crate::request_context::scope("req_webhook".to_owned(), async {
            AppError::WebhookNotConfigured.into_response()
        })
        .await;
        assert_eq!(response.status(), http::StatusCode::CONFLICT);

        let body = response.into_body().collect().await?.to_bytes();
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(value["error"]["code"], "webhook_not_configured");
        assert_eq!(
            value["error"]["message"],
            "No active webhook subscription is configured."
        );
        Ok(())
    }

    #[test]
    fn conflict_preserves_its_stable_code() {
        let error = AppError::conflict("idempotency_conflict");
        assert_eq!(error.status_code(), http::StatusCode::CONFLICT);
        assert_eq!(error.code(), "idempotency_conflict");
    }

    #[test]
    fn internal_source_is_not_part_of_the_public_message() {
        let error = AppError::internal(
            "test_subsystem",
            anyhow::anyhow!("sensitive upstream detail"),
        );
        assert_eq!(
            error.public_message(),
            "An internal service error occurred."
        );
        assert!(!error.public_message().contains("sensitive"));
    }

    #[test]
    fn rate_limit_response_has_retry_after_header() {
        let response = AppError::RateLimited {
            retry_after_seconds: 17,
        }
        .into_response();
        assert_eq!(
            response
                .headers()
                .get(http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("17")
        );
    }
}
