//! Request-local correlation data shared across transport and application code.

use std::future::Future;

use uuid::Uuid;

/// Value used when an error is rendered outside an HTTP request scope.
pub const UNAVAILABLE_REQUEST_ID: &str = "unavailable";

tokio::task_local! {
    static REQUEST_ID: String;
}

/// Runs a future with its validated correlation identifier.
pub async fn scope<T>(request_id: String, future: impl Future<Output = T>) -> T {
    REQUEST_ID.scope(request_id, future).await
}

/// Returns the correlation identifier for the current request, when available.
#[must_use]
pub fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(Clone::clone).ok()
}

/// Returns the current request identifier or the stable out-of-scope marker.
#[must_use]
pub fn current_request_id_or_unavailable() -> String {
    current_request_id().unwrap_or_else(|| UNAVAILABLE_REQUEST_ID.to_owned())
}

/// Generates a time-sortable request correlation identifier.
#[must_use]
pub fn generate_request_id() -> String {
    Uuid::now_v7().to_string()
}

/// Checks whether a caller-supplied request identifier is safe to reflect and log.
///
/// Identifiers are deliberately restricted to a compact ASCII alphabet to
/// prevent control-character injection into headers and structured logs.
#[must_use]
pub fn is_valid_request_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::{
        UNAVAILABLE_REQUEST_ID, current_request_id, current_request_id_or_unavailable,
        generate_request_id, is_valid_request_id, scope,
    };

    #[tokio::test]
    async fn request_id_is_scoped_without_leaking() {
        assert_eq!(current_request_id(), None);
        let inside = scope("req_123".to_owned(), async { current_request_id() }).await;
        assert_eq!(inside.as_deref(), Some("req_123"));
        assert_eq!(current_request_id(), None);
        assert_eq!(current_request_id_or_unavailable(), UNAVAILABLE_REQUEST_ID);
    }

    #[test]
    fn validates_only_log_safe_identifiers() {
        assert!(is_valid_request_id("request-01_A"));
        assert!(!is_valid_request_id(""));
        assert!(!is_valid_request_id("contains spaces"));
        assert!(!is_valid_request_id("contains\nnewline"));
        assert!(!is_valid_request_id(&"x".repeat(65)));
    }

    #[test]
    fn generated_identifiers_are_valid() {
        assert!(is_valid_request_id(&generate_request_id()));
    }
}
