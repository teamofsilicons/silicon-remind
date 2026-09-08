//! Generic signed webhook delivery adapter.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::BytesMut;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac as _};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Serialize;
use sha2::Sha256;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

/// Resolved webhook endpoint and its write-only optional signing credential.
#[derive(Clone)]
pub struct WebhookDestination {
    /// Exact endpoint URL configured by the Silicon owner.
    pub endpoint_url: Url,
    /// Optional per-endpoint HMAC signing secret.
    pub signing_secret: SecretString,
}

impl std::fmt::Debug for WebhookDestination {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookDestination")
            .field("endpoint_url", &self.endpoint_url)
            .field("signing_secret", &"[REDACTED]")
            .finish()
    }
}

/// Immutable occurrence snapshot delivered to the configured webhook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReminderEvent {
    /// Stable occurrence and webhook idempotency identifier.
    pub execution_id: Uuid,
    /// Owning schedule identifier.
    pub schedule_id: Uuid,
    /// Destination Silicon public identifier.
    pub silicon_id: String,
    /// Reminder content captured when the occurrence was materialized.
    pub text: String,
    /// Exact intended occurrence instant.
    pub scheduled_for: DateTime<Utc>,
    /// IANA timezone retained for the receiving Silicon.
    pub timezone: String,
}

/// Local delivery receipt marker. Generic webhooks do not have to return IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebhookReceipt {
    /// Remind's stable occurrence ID used as the local receipt marker.
    pub event_id: Uuid,
}

/// Failure category used by the durable retry policy.
#[derive(Debug, Error)]
pub enum WebhookDeliveryError {
    /// A transient or ambiguous outcome that is safe to retry idempotently.
    #[error("Webhook delivery can be retried: {reason}")]
    Retryable {
        /// Sanitized, bounded operational reason.
        reason: String,
    },
    /// A definitive request rejection that should not be retried.
    #[error("Webhook delivery was rejected: {reason}")]
    Terminal {
        /// Sanitized, bounded operational reason.
        reason: String,
    },
}

impl WebhookDeliveryError {
    /// Whether retrying the same execution identifier can make progress.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable { .. })
    }

    /// Returns the sanitized reason suitable for bounded persistence.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Retryable { reason } | Self::Terminal { reason } => reason,
        }
    }
}

/// Bounded, redirect-free HTTP client for arbitrary webhook destinations.
#[derive(Clone, Debug)]
pub struct WebhookClient {
    client: reqwest::Client,
    max_response_bytes: usize,
}

impl WebhookClient {
    /// Builds a bounded, redirect-free webhook client.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS HTTP client cannot be constructed.
    pub fn new(
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(false)
            .user_agent(concat!("silicon-remind/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            max_response_bytes,
        })
    }

    /// Sends one event and treats any successful HTTP response as acceptance.
    ///
    /// # Errors
    ///
    /// Returns a classified delivery error for transport, protocol, or HTTP
    /// failures. Ambiguous outcomes are retried with the same execution UUID.
    /// Recipients should deduplicate by the stable execution UUID.
    pub async fn deliver(
        &self,
        destination: &WebhookDestination,
        event: &ReminderEvent,
        now: DateTime<Utc>,
    ) -> Result<WebhookReceipt, WebhookDeliveryError> {
        let envelope = EventEnvelope::new(event);
        let body =
            serde_json::to_vec(&envelope).map_err(|error| WebhookDeliveryError::Terminal {
                reason: bounded_reason(format!("event serialization failed: {error}")),
            })?;
        let timestamp = now.timestamp().to_string();
        let headers = request_headers(
            event.execution_id,
            &timestamp,
            &body,
            &destination.signing_secret,
        )?;

        let response = self
            .client
            .post(destination.endpoint_url.clone())
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|error| WebhookDeliveryError::Retryable {
                reason: bounded_reason(format!("transport failure: {error}")),
            })?;
        let status = response.status();
        let _response_body = read_bounded(response, self.max_response_bytes).await?;

        if status.is_success() {
            return Ok(WebhookReceipt {
                event_id: event.execution_id,
            });
        }

        let reason = bounded_reason(format!("Webhook returned HTTP {}", status.as_u16()));
        if is_retryable_status(status) {
            Err(WebhookDeliveryError::Retryable { reason })
        } else {
            Err(WebhookDeliveryError::Terminal { reason })
        }
    }
}

#[derive(Debug, Serialize)]
struct EventEnvelope<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    source: &'static str,
    subject: String,
    occurred_at: DateTime<Utc>,
    schema_version: &'static str,
    payload: EventPayload<'a>,
}

impl<'a> EventEnvelope<'a> {
    fn new(event: &'a ReminderEvent) -> Self {
        Self {
            event_type: "remind.schedule.triggered",
            source: "silicon-remind",
            subject: event.schedule_id.to_string(),
            occurred_at: event.scheduled_for,
            schema_version: "1.0",
            payload: EventPayload {
                execution_id: event.execution_id,
                schedule_id: event.schedule_id,
                silicon_id: &event.silicon_id,
                text: &event.text,
                scheduled_for: event.scheduled_for,
                timezone: &event.timezone,
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct EventPayload<'a> {
    execution_id: Uuid,
    schedule_id: Uuid,
    silicon_id: &'a str,
    text: &'a str,
    scheduled_for: DateTime<Utc>,
    timezone: &'a str,
}

type HmacSha256 = Hmac<Sha256>;

fn signature(
    signing_secret: &SecretString,
    execution_id: Uuid,
    timestamp: &str,
    body: &[u8],
) -> Result<String, WebhookDeliveryError> {
    if !signing_secret_is_valid(signing_secret) {
        return Err(WebhookDeliveryError::Terminal {
            reason: bounded_reason("webhook signing credential is invalid"),
        });
    }
    let mut mac =
        HmacSha256::new_from_slice(signing_secret.expose_secret().as_bytes()).map_err(|_| {
            WebhookDeliveryError::Terminal {
                reason: bounded_reason("webhook signing credential is invalid"),
            }
        })?;
    mac.update(execution_id.to_string().as_bytes());
    mac.update(b".");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(format!(
        "v1,{}",
        STANDARD.encode(mac.finalize().into_bytes())
    ))
}

pub(crate) fn signing_secret_is_valid(secret: &SecretString) -> bool {
    let value = secret.expose_secret();
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

pub(crate) fn destination_url_is_allowed(destination: &Url, production: bool) -> bool {
    matches!(destination.scheme(), "https" | "http")
        && (!production || destination.scheme() == "https")
        && destination.host_str().is_some()
        && destination.username().is_empty()
        && destination.password().is_none()
        && destination.fragment().is_none()
}

fn request_headers(
    execution_id: Uuid,
    timestamp: &str,
    body: &[u8],
    signing_secret: &SecretString,
) -> Result<HeaderMap, WebhookDeliveryError> {
    let mut headers = HeaderMap::with_capacity(4);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    insert_header(&mut headers, "idempotency-key", &execution_id.to_string())?;
    insert_header(&mut headers, "webhook-id", &execution_id.to_string())?;
    if !signing_secret.expose_secret().is_empty() {
        let signature = signature(signing_secret, execution_id, timestamp, body)?;
        insert_header(&mut headers, "webhook-timestamp", timestamp)?;
        insert_header(&mut headers, "webhook-signature", &signature)?;
    }
    Ok(headers)
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), WebhookDeliveryError> {
    let value = HeaderValue::from_str(value).map_err(|_| WebhookDeliveryError::Terminal {
        reason: bounded_reason("webhook request metadata is invalid"),
    })?;
    headers.insert(name, value);
    Ok(())
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, WebhookDeliveryError> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(WebhookDeliveryError::Retryable {
            reason: bounded_reason("webhook response exceeded the configured size limit"),
        });
    }

    let mut body = BytesMut::with_capacity(maximum.min(8_192));
    while let Some(chunk) =
        response
            .chunk()
            .await
            .map_err(|error| WebhookDeliveryError::Retryable {
                reason: bounded_reason(format!("failed reading webhook response: {error}")),
            })?
    {
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(WebhookDeliveryError::Retryable {
                reason: bounded_reason("webhook response exceeded the configured size limit"),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.to_vec())
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_EARLY
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    ) || status.is_server_error()
}

fn bounded_reason(reason: impl AsRef<str>) -> String {
    const MAX_REASON_BYTES: usize = 1_000;
    let reason = reason.as_ref();
    if reason.len() <= MAX_REASON_BYTES {
        return reason.to_owned();
    }
    let mut end = MAX_REASON_BYTES;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use url::Url;

    use super::{bounded_reason, destination_url_is_allowed, signature, signing_secret_is_valid};

    fn signing_secret() -> SecretString {
        SecretString::from("v1.abcdefghijklmnopqrstuvwxyz123456")
    }

    #[test]
    fn signature_has_standard_webhooks_format() {
        let signature = signature(
            &signing_secret(),
            uuid::Uuid::nil(),
            "123",
            br#"{"ok":true}"#,
        );
        assert!(signature.is_ok());
        let Ok(signature) = signature else {
            return;
        };
        assert_eq!(signature.len(), 47);
        assert!(signature.starts_with("v1,"));
        assert_eq!(
            STANDARD
                .decode(&signature[3..])
                .map(|bytes| bytes.len())
                .ok(),
            Some(32)
        );
    }

    #[test]
    fn signing_secret_is_bounded_text() {
        assert!(signing_secret_is_valid(&signing_secret()));
        assert!(!signing_secret_is_valid(&SecretString::from(
            "has\ncontrol"
        )));
    }

    #[test]
    fn destination_accepts_arbitrary_webhook_urls() -> anyhow::Result<()> {
        for path in [
            "https://hooks.example.test/custom/path?tenant=one",
            "http://localhost:8787/anything",
        ] {
            assert!(destination_url_is_allowed(&Url::parse(path)?, false));
        }
        assert!(destination_url_is_allowed(
            &Url::parse("https://hooks.example.test/custom")?,
            true
        ));
        assert!(!destination_url_is_allowed(
            &Url::parse("http://hooks.example.test/custom")?,
            true
        ));
        for path in [
            "ftp://hooks.example.test/custom",
            "https://user:pass@hooks.example.test/custom",
            "https://hooks.example.test/custom#fragment",
        ] {
            assert!(!destination_url_is_allowed(&Url::parse(path)?, false));
        }
        Ok(())
    }

    #[test]
    fn persisted_reasons_are_utf8_safe_and_bounded() {
        let input = format!("{}é", "x".repeat(999));
        let reason = bounded_reason(input);
        assert!(reason.len() <= 1_000);
        assert!(reason.is_char_boundary(reason.len()));
    }
}
