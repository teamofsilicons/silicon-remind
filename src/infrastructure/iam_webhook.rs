//! Official Silicon IAM exact-byte webhook verification.

use chrono::{DateTime, Utc};
use http::HeaderMap;
use secrecy::{ExposeSecret as _, SecretString};
use silicon_iam_client::{VerifiedWebhook, WebhookSecret, WebhookSecretKeyring, WebhookVerifier};
use std::{collections::BTreeMap, fmt, sync::Arc};

/// Cloneable verifier sharing retained signing-key versions.
#[derive(Clone)]
pub struct IamWebhookVerifier(Arc<WebhookVerifier>);

impl IamWebhookVerifier {
    /// Builds a verifier from caller-supplied IAM secrets, without decoding their bytes.
    pub fn from_keys(keys: &BTreeMap<i16, SecretString>) -> anyhow::Result<Self> {
        let mut iter = keys.iter();
        let (version, secret) = iter
            .next()
            .ok_or_else(|| anyhow::anyhow!("IAM webhook keyring is empty"))?;
        let mut keyring = WebhookSecretKeyring::new(
            i64::from(*version),
            WebhookSecret::new(secret.expose_secret())?,
        )?;
        for (version, secret) in iter {
            keyring.insert(
                i64::from(*version),
                WebhookSecret::new(secret.expose_secret())?,
            )?;
        }
        Ok(Self(Arc::new(WebhookVerifier::new(keyring))))
    }

    /// Verifies signatures, replay age, event identity, and envelope through the SDK.
    pub(crate) fn verify(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        _now: DateTime<Utc>,
    ) -> Result<VerifiedWebhook, silicon_iam_client::WebhookError> {
        self.0.verify(headers, body)
    }
}

impl fmt::Debug for IamWebhookVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IamWebhookVerifier")
            .finish_non_exhaustive()
    }
}
