//! Stateless Rust client for Silicon Remind, signed in with Silicon Accounts.
//!
//! Two parts, both stateless:
//!
//! - [`accounts::SignIn`] signs a Carbon or a Silicon in to Remind with Silicon Accounts, as
//!   Remind's own public client (no secret): the device flow for Carbons, a short-lived token
//!   (`slt_…`) for Silicons, refresh and sign-out. It returns [`accounts::Tokens`]; where they
//!   live is the caller's decision.
//! - [`Client`] calls the Remind API (contract 2, `/api/v2`) with a Silicon Accounts access
//!   token ([`Client::with_session`]), or, for another app reading on an account's behalf, a
//!   User verification proof ([`Client::with_proof`]). [`Client::with_test_environment`]
//!   selects a test environment's isolated data without changing any method.
//!
//! ```no_run
//! # async fn example() -> silicon_remind_client::Result<()> {
//! use silicon_remind_client::{Client, Mutation, Secret, accounts::SignIn, models};
//!
//! // A Silicon got a short-lived token with `silicon-accounts login --app remind -q`.
//! let sign_in = SignIn::new(silicon_remind_client::accounts::DEFAULT_ACCOUNTS_URL, "remind")?;
//! let tokens = sign_in.exchange_slt(&Secret::new("slt_…")).await?;
//!
//! let remind = Client::new(silicon_remind_client::DEFAULT_URL)?.with_session(tokens.access_token)?;
//! let reminder = remind
//!     .create_reminder(
//!         &models::CreateScheduleRequest {
//!             text: "Review the build".into(),
//!             kind: models::ScheduleKind::Recurring,
//!             timezone: "Asia/Kolkata".into(),
//!             cron: "0 9 * * MON-FRI".into(),
//!         },
//!         &Mutation::new(),
//!     )
//!     .await?;
//! println!("created {}", reminder.id);
//! # Ok(()) }
//! ```
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

pub mod accounts;
mod api;
mod client;
mod environments;
mod error;
pub mod models;

pub use client::{API_VERSION, Client, DEFAULT_URL};
pub use error::{Error, Refusal, Result};

/// Secret material with explicit exposure and redacted `Debug` output.
///
/// `Serialize` deliberately writes the value (for request bodies and caller-owned secure
/// storage); never serialize secrets into ordinary logs.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(SecretString);
impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }
    /// The secret value. Call only where it must be sent or stored.
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.expose())
    }
}
impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.expose() == other.expose()
    }
}

/// One logical mutation's reusable idempotency key.
///
/// Reuse the same `Mutation` when retrying the exact same create, edit or status change, so a
/// retry after an uncertain response never applies it twice.
#[derive(Clone, Debug)]
pub struct Mutation(String);
impl Mutation {
    /// A new unique key.
    pub fn new() -> Self {
        Self(Uuid::now_v7().to_string())
    }
    /// Adopts a caller-chosen key (16 to 255 visible ASCII characters).
    pub fn with_key(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if !(16..=255).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid(
                "an idempotency key must contain 16 to 255 visible ASCII characters".into(),
            ));
        }
        Ok(Self(key))
    }
    /// The key sent as `Idempotency-Key`.
    pub fn key(&self) -> &str {
        &self.0
    }
}
impl Default for Mutation {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a value is a test environment key: 32 ASCII letters and digits.
pub fn is_test_environment_key(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_alphanumeric())
}
