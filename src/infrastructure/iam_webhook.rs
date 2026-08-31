//! Verification for Silicon IAM application webhook requests.

use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac as _};
use http::HeaderMap;
use secrecy::{ExposeSecret as _, SecretString};
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

const EVENT_ID_HEADER: &str = "x-silicon-iam-event-id";
const TIMESTAMP_HEADER: &str = "x-silicon-iam-timestamp";
const KEY_VERSION_HEADER: &str = "x-silicon-iam-key-version";
const SIGNATURE_HEADER: &str = "x-silicon-iam-signature";
const SIGNING_SECRET_PREFIX: &str = "whs_";
const SIGNATURE_PREFIX: &str = "v1=";
const REPLAY_TOLERANCE: Duration = Duration::from_mins(5);

type HmacSha256 = Hmac<Sha256>;

/// Authenticated metadata bound to an exact IAM webhook body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedIamWebhook {
    /// Sender-stable event identifier from the signed request headers.
    pub(crate) event_id: Uuid,
}

/// A generic webhook authentication rejection.
#[derive(Clone, Copy, Debug, Error)]
#[error("Silicon IAM webhook authentication failed")]
pub(crate) struct IamWebhookVerificationError;

/// Version-aware verifier for IAM application webhook signatures.
#[derive(Clone)]
pub(crate) struct IamWebhookVerifier {
    keys: Arc<BTreeMap<i16, SecretString>>,
}

impl IamWebhookVerifier {
    /// Validates IAM webhook credentials while retaining their exact bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when a credential does not have IAM's `whs_` encoding
    /// or its encoded suffix does not contain exactly 256 bits. IAM signs with
    /// the complete credential bytes, so the validated prefix is retained.
    pub(crate) fn from_base64url(
        encoded_keys: &BTreeMap<i16, SecretString>,
    ) -> anyhow::Result<Self> {
        if encoded_keys.is_empty() {
            anyhow::bail!("IAM webhook keyring is empty");
        }

        let mut keys = BTreeMap::new();
        for (version, encoded_secret) in encoded_keys {
            if *version <= 0 {
                anyhow::bail!("IAM webhook key version must be positive");
            }
            let encoded = encoded_secret
                .expose_secret()
                .strip_prefix(SIGNING_SECRET_PREFIX)
                .ok_or_else(|| anyhow::anyhow!("IAM webhook key version {version} is malformed"))?;
            let decoded =
                Zeroizing::new(URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
                    anyhow::anyhow!("IAM webhook key version {version} is malformed")
                })?);
            let _: [u8; 32] = decoded.as_slice().try_into().map_err(|_| {
                anyhow::anyhow!("IAM webhook key version {version} is not 256 bits")
            })?;
            keys.insert(*version, encoded_secret.clone());
        }
        Ok(Self {
            keys: Arc::new(keys),
        })
    }

    /// Authenticates the exact raw body and enforces IAM's five-minute replay
    /// window before any JSON is parsed or state is changed.
    pub(crate) fn verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
        now: DateTime<Utc>,
    ) -> Result<VerifiedIamWebhook, IamWebhookVerificationError> {
        let event_id = unique_header(headers, EVENT_ID_HEADER)?
            .parse::<Uuid>()
            .map_err(|_| IamWebhookVerificationError)?;
        let timestamp_text = unique_header(headers, TIMESTAMP_HEADER)?;
        let timestamp = timestamp_text
            .parse::<i64>()
            .map_err(|_| IamWebhookVerificationError)?;
        if now.timestamp().abs_diff(timestamp) > REPLAY_TOLERANCE.as_secs() {
            return Err(IamWebhookVerificationError);
        }
        let key_version = unique_header(headers, KEY_VERSION_HEADER)?
            .parse::<i16>()
            .map_err(|_| IamWebhookVerificationError)?;
        if key_version <= 0 {
            return Err(IamWebhookVerificationError);
        }
        let key = self
            .keys
            .get(&key_version)
            .ok_or(IamWebhookVerificationError)?;
        let signature = decode_signature(unique_header(headers, SIGNATURE_HEADER)?)?;

        let mut mac = HmacSha256::new_from_slice(key.expose_secret().as_bytes())
            .map_err(|_| IamWebhookVerificationError)?;
        mac.update(timestamp_text.as_bytes());
        mac.update(b".");
        mac.update(raw_body);
        mac.verify_slice(&signature)
            .map_err(|_| IamWebhookVerificationError)?;

        Ok(VerifiedIamWebhook { event_id })
    }
}

impl fmt::Debug for IamWebhookVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IamWebhookVerifier")
            .field("key_versions", &self.keys.keys())
            .finish_non_exhaustive()
    }
}

fn unique_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<&'a str, IamWebhookVerificationError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().ok_or(IamWebhookVerificationError)?;
    if values.next().is_some() {
        return Err(IamWebhookVerificationError);
    }
    value.to_str().map_err(|_| IamWebhookVerificationError)
}

fn decode_signature(value: &str) -> Result<[u8; 32], IamWebhookVerificationError> {
    let encoded = value
        .strip_prefix(SIGNATURE_PREFIX)
        .ok_or(IamWebhookVerificationError)?;
    if encoded.len() != 64
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(IamWebhookVerificationError);
    }
    let mut signature = [0_u8; 32];
    hex::decode_to_slice(encoded, &mut signature).map_err(|_| IamWebhookVerificationError)?;
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use chrono::{TimeZone as _, Utc};
    use hmac::{Hmac, Mac as _};
    use http::{HeaderMap, HeaderValue};
    use secrecy::SecretString;
    use sha2::Sha256;
    use uuid::Uuid;

    use super::{
        EVENT_ID_HEADER, IamWebhookVerifier, KEY_VERSION_HEADER, SIGNATURE_HEADER, TIMESTAMP_HEADER,
    };

    const KEY: [u8; 32] = [0x5a; 32];
    const EVENT_ID: &str = "0198f74d-7ef7-7c9f-95bf-7d403a61e5ca";
    const TIMESTAMP: &str = "1788177600";

    fn verifier() -> anyhow::Result<IamWebhookVerifier> {
        let encoded = URL_SAFE_NO_PAD.encode(KEY);
        IamWebhookVerifier::from_base64url(&BTreeMap::from([(
            7,
            SecretString::from(format!("whs_{encoded}")),
        )]))
    }

    fn now() -> chrono::DateTime<Utc> {
        match Utc.timestamp_opt(1_788_177_600, 0).single() {
            Some(value) => value,
            None => panic!("fixed timestamp must be valid"),
        }
    }

    fn headers(body: &[u8]) -> anyhow::Result<HeaderMap> {
        let encoded = URL_SAFE_NO_PAD.encode(KEY);
        let credential = format!("whs_{encoded}");
        let mut mac = Hmac::<Sha256>::new_from_slice(credential.as_bytes())?;
        mac.update(TIMESTAMP.as_bytes());
        mac.update(b".");
        mac.update(body);
        let signature = format!("v1={}", hex::encode(mac.finalize().into_bytes()));

        let mut headers = HeaderMap::new();
        headers.insert(EVENT_ID_HEADER, HeaderValue::from_static(EVENT_ID));
        headers.insert(TIMESTAMP_HEADER, HeaderValue::from_static(TIMESTAMP));
        headers.insert(KEY_VERSION_HEADER, HeaderValue::from_static("7"));
        headers.insert(SIGNATURE_HEADER, HeaderValue::from_str(&signature)?);
        Ok(headers)
    }

    #[test]
    fn accepts_a_valid_signature_for_the_exact_body() -> anyhow::Result<()> {
        let body = br#"{"event_id":"0198f74d-7ef7-7c9f-95bf-7d403a61e5ca"}"#;
        let verified = verifier()?.verify(&headers(body)?, body, now())?;
        assert_eq!(verified.event_id, Uuid::parse_str(EVENT_ID)?);
        Ok(())
    }

    #[test]
    fn rejects_a_signature_for_different_raw_bytes() -> anyhow::Result<()> {
        let original = br#"{"value":1}"#;
        let changed = br#"{ "value": 1 }"#;
        assert!(
            verifier()?
                .verify(&headers(original)?, changed, now())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn rejects_timestamps_outside_five_minutes() -> anyhow::Result<()> {
        let body = br"{}";
        let stale = now() + chrono::TimeDelta::seconds(301);
        assert!(verifier()?.verify(&headers(body)?, body, stale).is_err());
        Ok(())
    }

    #[test]
    fn rejects_duplicate_security_headers() -> anyhow::Result<()> {
        let body = br"{}";
        let mut headers = headers(body)?;
        headers.append(KEY_VERSION_HEADER, HeaderValue::from_static("7"));
        assert!(verifier()?.verify(&headers, body, now()).is_err());
        Ok(())
    }

    #[test]
    fn rejects_an_unknown_key_version() -> anyhow::Result<()> {
        let body = br"{}";
        let mut headers = headers(body)?;
        headers.insert(KEY_VERSION_HEADER, HeaderValue::from_static("8"));
        assert!(verifier()?.verify(&headers, body, now()).is_err());
        Ok(())
    }

    #[test]
    fn rejects_non_contract_secret_encoding() {
        let result = IamWebhookVerifier::from_base64url(&BTreeMap::from([(
            1,
            SecretString::from(URL_SAFE_NO_PAD.encode(KEY)),
        )]));
        assert!(result.is_err());
    }
}
