//! Signed Silicon Hook delivery adapter.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::BytesMut;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac as _};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::domain::is_valid_global_silicon_id;

/// Resolved Hook endpoint and its write-only signing credential.
#[derive(Clone)]
pub struct HookDestination {
    /// Exact endpoint URL issued by Silicon Hook.
    pub endpoint_url: Url,
    /// Per-endpoint HMAC signing secret.
    pub signing_secret: SecretString,
}

impl std::fmt::Debug for HookDestination {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookDestination")
            .field("endpoint_url", &self.endpoint_url)
            .field("signing_secret", &"[REDACTED]")
            .finish()
    }
}

/// Immutable occurrence snapshot delivered to Silicon Hook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReminderEvent {
    /// Stable occurrence and Hook idempotency identifier.
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

/// Successful durable acceptance from Silicon Hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HookReceipt {
    /// Hook's stable event identifier.
    pub event_id: Uuid,
}

/// Failure category used by the durable retry policy.
#[derive(Debug, Error)]
pub enum HookDeliveryError {
    /// A transient or ambiguous outcome that is safe to retry idempotently.
    #[error("Hook delivery can be retried: {reason}")]
    Retryable {
        /// Sanitized, bounded operational reason.
        reason: String,
    },
    /// A definitive request rejection that should not be retried.
    #[error("Hook delivery was rejected: {reason}")]
    Terminal {
        /// Sanitized, bounded operational reason.
        reason: String,
    },
}

impl HookDeliveryError {
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

/// Bounded, redirect-free HTTP client for Silicon Hook.
#[derive(Clone, Debug)]
pub struct HookClient {
    client: reqwest::Client,
    max_response_bytes: usize,
}

impl HookClient {
    /// Builds a hardened Hook client.
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

    /// Sends one signed event and waits for Hook's durable acceptance.
    ///
    /// # Errors
    ///
    /// Returns a classified delivery error for transport, protocol, or HTTP
    /// failures. Ambiguous outcomes are retryable because the execution UUID is
    /// reused as Hook's idempotency key.
    pub async fn deliver(
        &self,
        destination: &HookDestination,
        event: &ReminderEvent,
        now: DateTime<Utc>,
    ) -> Result<HookReceipt, HookDeliveryError> {
        let envelope = EventEnvelope::new(event);
        let body = serde_json::to_vec(&envelope).map_err(|error| HookDeliveryError::Terminal {
            reason: bounded_reason(format!("event serialization failed: {error}")),
        })?;
        let timestamp = now.timestamp().to_string();
        let signature = signature(&destination.signing_secret, &timestamp, &body)?;
        let headers = request_headers(event.execution_id, &timestamp, &signature)?;

        let response = self
            .client
            .post(destination.endpoint_url.clone())
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|error| HookDeliveryError::Retryable {
                reason: bounded_reason(format!("transport failure: {error}")),
            })?;
        let status = response.status();
        let response_body = read_bounded(response, self.max_response_bytes).await?;

        if status == StatusCode::ACCEPTED {
            let acceptance: Acceptance =
                serde_json::from_slice(&response_body).map_err(|error| {
                    HookDeliveryError::Retryable {
                        reason: bounded_reason(format!(
                            "Hook returned malformed acceptance: {error}"
                        )),
                    }
                })?;
            if acceptance.status != "accepted" {
                return Err(HookDeliveryError::Retryable {
                    reason: bounded_reason("Hook returned an unexpected acceptance status"),
                });
            }
            return Ok(HookReceipt {
                event_id: acceptance.event_id,
            });
        }

        let reason = bounded_reason(format!("Hook returned HTTP {}", status.as_u16()));
        if is_retryable_status(status) {
            Err(HookDeliveryError::Retryable { reason })
        } else {
            Err(HookDeliveryError::Terminal { reason })
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

#[derive(Debug, Deserialize)]
struct Acceptance {
    event_id: Uuid,
    status: String,
}

type HmacSha256 = Hmac<Sha256>;

fn signature(
    signing_secret: &SecretString,
    timestamp: &str,
    body: &[u8],
) -> Result<String, HookDeliveryError> {
    let key = decode_signing_key(signing_secret)?;
    let mut mac = HmacSha256::new_from_slice(&key).map_err(|_| HookDeliveryError::Terminal {
        reason: bounded_reason("Hook signing credential is invalid"),
    })?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(format!("v1={}", hex::encode(mac.finalize().into_bytes())))
}

pub(crate) fn signing_secret_is_valid(secret: &SecretString) -> bool {
    decode_signing_key(secret).is_ok()
}

fn decode_signing_key(secret: &SecretString) -> Result<Zeroizing<Vec<u8>>, HookDeliveryError> {
    let encoded = secret
        .expose_secret()
        .strip_prefix("whsec_")
        .ok_or_else(|| HookDeliveryError::Terminal {
            reason: bounded_reason("Hook signing credential is invalid"),
        })?;
    let decoded = Zeroizing::new(URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        HookDeliveryError::Terminal {
            reason: bounded_reason("Hook signing credential is invalid"),
        }
    })?);
    if decoded.len() != 32 {
        return Err(HookDeliveryError::Terminal {
            reason: bounded_reason("Hook signing credential is invalid"),
        });
    }
    Ok(decoded)
}

pub(crate) fn destination_url_is_allowed(
    destination: &Url,
    base: &Url,
    expected_silicon_id: &str,
) -> bool {
    if destination.scheme() != base.scheme()
        || destination.host_str() != base.host_str()
        || destination.port_or_known_default() != base.port_or_known_default()
        || !destination.username().is_empty()
        || destination.password().is_some()
        || destination.query().is_some()
        || destination.fragment().is_some()
    {
        return false;
    }

    let Some(mut segments) = destination.path_segments().map(Iterator::collect::<Vec<_>>) else {
        return false;
    };
    if segments.last() == Some(&"") {
        segments.pop();
    }
    let route = match segments.as_slice() {
        ["silicon", silicon_id, endpoint_key]
        | ["api", "v1", "silicon", silicon_id, endpoint_key] => Some((*silicon_id, *endpoint_key)),
        _ => None,
    };
    route.is_some_and(|(silicon_id, endpoint_key)| {
        silicon_id == expected_silicon_id
            && is_valid_global_silicon_id(silicon_id)
            && endpoint_key.len() == 6
            && endpoint_key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'A'..=b'F'))
    })
}

fn request_headers(
    execution_id: Uuid,
    timestamp: &str,
    signature: &str,
) -> Result<HeaderMap, HookDeliveryError> {
    let mut headers = HeaderMap::with_capacity(4);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    insert_header(&mut headers, "idempotency-key", &execution_id.to_string())?;
    insert_header(&mut headers, "x-hook-timestamp", timestamp)?;
    insert_header(&mut headers, "x-hook-signature", signature)?;
    Ok(headers)
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), HookDeliveryError> {
    let value = HeaderValue::from_str(value).map_err(|_| HookDeliveryError::Terminal {
        reason: bounded_reason("Hook request metadata is invalid"),
    })?;
    headers.insert(name, value);
    Ok(())
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, HookDeliveryError> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(HookDeliveryError::Retryable {
            reason: bounded_reason("Hook response exceeded the configured size limit"),
        });
    }

    let mut body = BytesMut::with_capacity(maximum.min(8_192));
    while let Some(chunk) =
        response
            .chunk()
            .await
            .map_err(|error| HookDeliveryError::Retryable {
                reason: bounded_reason(format!("failed reading Hook response: {error}")),
            })?
    {
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(HookDeliveryError::Retryable {
                reason: bounded_reason("Hook response exceeded the configured size limit"),
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
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use secrecy::SecretString;
    use url::Url;

    use super::{bounded_reason, destination_url_is_allowed, signature, signing_secret_is_valid};

    fn signing_secret() -> SecretString {
        SecretString::from(format!("whsec_{}", URL_SAFE_NO_PAD.encode([7_u8; 32])))
    }

    #[test]
    fn signature_has_versioned_hex_format() {
        let signature = signature(&signing_secret(), "123", br#"{"ok":true}"#);
        assert!(signature.is_ok());
        let Ok(signature) = signature else {
            return;
        };
        assert_eq!(signature.len(), 67);
        assert!(signature.starts_with("v1="));
        assert!(signature[3..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn signing_secret_must_be_a_canonical_256_bit_hook_secret() {
        assert!(signing_secret_is_valid(&signing_secret()));
        assert!(!signing_secret_is_valid(&SecretString::from(
            "not-a-hook-secret"
        )));
    }

    #[test]
    fn destination_accepts_only_normative_routes_for_the_expected_silicon() -> anyhow::Result<()> {
        let base = Url::parse("https://hook.teamofsilicons.com/api/v1")?;
        for path in [
            "https://hook.teamofsilicons.com/silicon/cos:tos/40AE2F",
            "https://hook.teamofsilicons.com/silicon/cos:tos/40AE2F/",
            "https://hook.teamofsilicons.com/api/v1/silicon/cos:tos/40AE2F",
        ] {
            assert!(destination_url_is_allowed(
                &Url::parse(path)?,
                &base,
                "cos:tos"
            ));
        }
        for path in [
            "https://evil.example/silicon/cos:tos/40AE2F",
            "https://hook.teamofsilicons.com/silicon/cos:tos/40ae2f",
            "https://hook.teamofsilicons.com/silicon/cos:tos/40AE2F/extra",
            "https://hook.teamofsilicons.com/silicon/not-global/40AE2F",
        ] {
            assert!(!destination_url_is_allowed(
                &Url::parse(path)?,
                &base,
                "cos:tos"
            ));
        }
        assert!(!destination_url_is_allowed(
            &Url::parse("https://hook.teamofsilicons.com/silicon/cos:tos/40AE2F")?,
            &base,
            "other:tos"
        ));
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
