//! Sign in to Remind with Silicon Accounts, as Remind's own public client (no secret).
//!
//! - **Carbons** use the device flow: [`SignIn::start_device`] returns a code to show, the
//!   Carbon approves it on the account site from any device, and [`SignIn::wait_for_device`]
//!   polls (honouring `interval` and `slow_down`) until it is approved, denied or expires
//!   (10 minutes).
//! - **Silicons** never see a page. They mint a short-lived token with
//!   `silicon-accounts login --app remind -q` and hand it over; [`SignIn::exchange_slt`]
//!   exchanges it (single use, 2 minutes, bound to Remind).
//! - [`SignIn::refresh`] rotates the refresh token. Presenting a used refresh token ends the
//!   whole sign-in, so refresh one at a time and store the new pair before using it.
//! - [`SignIn::revoke`] ends this sign-in at Silicon Accounts (sign-out).
//!
//! Nothing here reads files or the environment, and token fields are [`Secret`]s whose
//! `Debug` output hides the value.
use crate::client::parse_origin;
use crate::models::{AccountKind, AccountRef};
use crate::{Error, Refusal, Result, Secret};
use serde::{Deserialize, Serialize};
use silicon_accounts_client::{AccountsClient, DevicePoll, TokenResponse};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

/// Production Silicon Accounts.
pub const DEFAULT_ACCOUNTS_URL: &str = "https://accounts.teamofsilicons.com";
/// Remind's app id at Silicon Accounts; also its public `client_id`.
pub const APP_ID: &str = "remind";
/// How a Silicon gets a short-lived token for Remind.
pub const SLT_MINT_COMMAND: &str = "silicon-accounts login --app remind -q";
/// The whole Silicon sign-in, in one line.
pub const SILICON_SIGN_IN: &str =
    "silicon-accounts login --app remind -q | remind login --slt-stdin";
const SLT_GRANT_TYPE: &str = "urn:silicon:params:oauth:grant-type:slt";

/// Seconds since the Unix epoch on this machine.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The signed-in Carbon or Silicon, as Silicon Accounts shares it with Remind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedInAccount {
    /// Permanent Silicon Accounts UUIDv4 (canonical lowercase, hyphenated). Key on this.
    pub uuid: String,
    /// Current public id (`c:ada`, `si:scout`); it can change.
    pub id: String,
    /// Carbon or Silicon.
    pub kind: AccountKind,
    /// Display name (empty when none).
    #[serde(default)]
    pub display_name: String,
    /// Profile photo URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
    /// A Silicon's custodian (the Carbon who looks after it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custodian: Option<AccountRef>,
}

/// A successful sign-in or refresh: Remind's tokens for one account.
#[derive(Clone, Debug)]
pub struct Tokens {
    /// Silicon Accounts access token for Remind (EdDSA JWT, 30 minutes).
    pub access_token: Secret,
    /// Rotating refresh token (`sar_…`). Every refresh returns a new one.
    pub refresh_token: Secret,
    /// When the access token expires (Unix seconds, from this machine's clock).
    pub expires_at: i64,
    /// When the sign-in itself ends (Unix seconds), if Silicon Accounts said.
    pub refresh_expires_at: Option<i64>,
    /// Granted scopes, space-separated.
    pub scope: String,
    /// Who signed in.
    pub account: SignedInAccount,
}

impl Tokens {
    fn from_response(response: &TokenResponse, received_at: i64) -> Result<Self> {
        let unexpected = |what: &str| Error::Accounts {
            status: None,
            code: "unexpected_response".into(),
            message: format!("Silicon Accounts answered the sign-in without {what}."),
            hint: Some("Retry; if it keeps happening, report it with `remind report`.".into()),
            request_id: None,
        };
        let refresh_token = response
            .refresh_token
            .as_ref()
            .ok_or_else(|| unexpected("a refresh token"))?;
        let account = response
            .account
            .as_ref()
            .ok_or_else(|| unexpected("the account"))?;
        let kind = match account.kind.as_str() {
            "silicon" => AccountKind::Silicon,
            _ => AccountKind::Carbon,
        };
        Ok(Self {
            access_token: Secret::new(response.access_token.expose()),
            refresh_token: Secret::new(refresh_token.expose()),
            expires_at: received_at
                .saturating_add(i64::try_from(response.expires_in).unwrap_or(i64::MAX)),
            refresh_expires_at: response
                .refresh_token_expires_at
                .map(|at| at.unix_timestamp()),
            scope: response.scope.clone().unwrap_or_default(),
            account: SignedInAccount {
                uuid: account.uuid.clone(),
                id: account.id.clone(),
                kind,
                display_name: account.display_name.clone(),
                pfp_url: Some(account.pfp_url.clone()).filter(|url| !url.is_empty()),
                custodian: account.custodian.as_ref().map(|custodian| AccountRef {
                    uuid: custodian.uuid.clone(),
                    id: custodian.id.clone(),
                    kind: Some(AccountKind::Carbon),
                }),
            },
        })
    }
}

/// A device sign-in in progress. Show [`DeviceStart::user_code`] and
/// [`DeviceStart::verification_uri`]; the device code itself is never shown.
#[derive(Clone)]
pub struct DeviceStart {
    /// The code the Carbon confirms, e.g. `WDJB-MJHT`.
    pub user_code: String,
    /// Where to approve, e.g. `https://accounts.teamofsilicons.com/device`.
    pub verification_uri: String,
    /// The same page with the code filled in.
    pub verification_uri_complete: Option<String>,
    /// Seconds the code stays valid (600).
    pub expires_in: u64,
    /// Minimum seconds between polls (5).
    pub interval: u64,
    /// When the code stops working (Unix seconds).
    pub expires_at: i64,
    device_code: Secret,
}
impl std::fmt::Debug for DeviceStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceStart")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}
impl DeviceStart {
    /// The page to open in a browser: the one with the code filled in when available.
    pub fn browser_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// One poll of a device sign-in that did not finish it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceProgress {
    /// Not approved yet.
    Pending,
    /// Silicon Accounts asked to poll more slowly; the new interval in seconds.
    SlowDown {
        /// Seconds between polls from now on.
        interval: u64,
    },
    /// A poll failed for a passing reason (network, 5xx, rate limit); polling continues.
    Retrying {
        /// What failed.
        message: String,
    },
}

/// The outcome of a single device poll.
#[derive(Debug)]
#[non_exhaustive]
pub enum DeviceCheck {
    /// Not approved yet; poll again after `interval`.
    Pending,
    /// Polling too fast: add 5 seconds to the interval.
    SlowDown,
    /// Approved: Remind's tokens for the Carbon.
    Approved(Box<Tokens>),
}

/// Signs accounts in to Remind at one Silicon Accounts deployment.
#[derive(Clone, Debug)]
pub struct SignIn {
    client: AccountsClient,
    http: reqwest::Client,
    base: Url,
    app_id: String,
    telemetry: bool,
}

impl SignIn {
    /// Signs in to `app_id` (normally [`APP_ID`]) at the Silicon Accounts deployment at
    /// `accounts_url`: https, or plain http only for this machine. No network call.
    pub fn new(accounts_url: &str, app_id: &str) -> Result<Self> {
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return Err(Error::Invalid(
                "the Silicon Accounts app id is empty".into(),
            ));
        }
        let base = parse_origin(accounts_url).map_err(|_| {
            Error::Invalid(format!(
                "the Silicon Accounts URL `{accounts_url}` must be an https origin (plain http only for localhost, 127.0.0.1 or ::1), such as {DEFAULT_ACCOUNTS_URL}"
            ))
        })?;
        let client = AccountsClient::builder()
            .base_url(base.as_str())
            .user_agent(concat!("silicon-remind-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| Error::accounts(&error))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("silicon-remind-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(Error::Transport)?;
        Ok(Self {
            client,
            http,
            base,
            app_id: app_id.to_owned(),
            telemetry: true,
        })
    }

    /// Turns Silicon Accounts telemetry off for these calls (`X-Accounts-Telemetry: off`).
    #[must_use]
    pub fn with_telemetry(mut self, enabled: bool) -> Self {
        self.telemetry = enabled;
        self.client = self.client.with_telemetry(enabled);
        self
    }

    /// The Silicon Accounts origin, without a trailing slash.
    pub fn accounts_url(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    /// The app id this signs in to.
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// `POST /v1/device/authorize` with `client_id` = the app id: starts a Carbon's device
    /// sign-in. `label` names this device in the Carbon's list of sign-ins.
    pub async fn start_device(&self, label: Option<&str>) -> Result<DeviceStart> {
        let started = now();
        let device = self
            .client
            .app_device_authorize(&self.app_id, None, label)
            .await
            .map_err(|error| {
                if error.is_code("unauthorized_client") {
                    Error::SignInRefused {
                        refusal: Refusal::NotAllowed,
                        message: error.message(),
                        hint: format!(
                            "This Remind's sign-in setup at Silicon Accounts has device sign-in turned off. Silicons sign in with `{}`.",
                            self.silicon_sign_in()
                        ),
                    }
                } else {
                    Error::accounts(&error)
                }
            })?;
        Ok(DeviceStart {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            verification_uri_complete: device.verification_uri_complete.clone(),
            expires_in: device.expires_in,
            interval: device.interval.max(1),
            expires_at: started.saturating_add(i64::try_from(device.expires_in).unwrap_or(600)),
            device_code: Secret::new(device.device_code.expose()),
        })
    }

    /// Polls a device sign-in once. Denied and expired codes are errors
    /// ([`Refusal::DeviceDenied`], [`Refusal::DeviceExpired`]).
    pub async fn poll_device(&self, device: &DeviceStart) -> Result<DeviceCheck> {
        let received_at = now();
        match self
            .client
            .app_device_poll(&self.app_id, device.device_code.expose())
            .await
        {
            Ok(DevicePoll::Pending) => Ok(DeviceCheck::Pending),
            Ok(DevicePoll::SlowDown) => Ok(DeviceCheck::SlowDown),
            Ok(DevicePoll::Denied) => Err(Error::SignInRefused {
                refusal: Refusal::DeviceDenied,
                message: format!(
                    "The sign-in request {} was denied on the account site.",
                    device.user_code
                ),
                hint: "Run `remind login` again if that was a mistake.".into(),
            }),
            Ok(DevicePoll::Expired) => Err(device_expired(device)),
            Ok(DevicePoll::Tokens(tokens)) => Ok(DeviceCheck::Approved(Box::new(
                Tokens::from_response(&tokens, received_at)?,
            ))),
            Ok(_) => Ok(DeviceCheck::Pending),
            Err(error) => Err(Error::accounts(&error)),
        }
    }

    /// Polls until the Carbon approves, honouring `interval` and `slow_down` (5 more seconds
    /// each time), and gives up when the code expires. Passing failures (network, 5xx, rate
    /// limits) are reported through `on_progress` and retried.
    pub async fn wait_for_device(
        &self,
        device: &DeviceStart,
        mut on_progress: impl FnMut(DeviceProgress),
    ) -> Result<Tokens> {
        let mut interval = Duration::from_secs(device.interval.max(1));
        loop {
            if now().saturating_add(i64::try_from(interval.as_secs()).unwrap_or(i64::MAX))
                > device.expires_at
            {
                return Err(device_expired(device));
            }
            tokio::time::sleep(interval).await;
            match self.poll_device(device).await {
                Ok(DeviceCheck::Approved(tokens)) => return Ok(*tokens),
                Ok(DeviceCheck::Pending) => on_progress(DeviceProgress::Pending),
                Ok(DeviceCheck::SlowDown) => {
                    interval += Duration::from_secs(5);
                    on_progress(DeviceProgress::SlowDown {
                        interval: interval.as_secs(),
                    });
                }
                Err(error) if error.is_transient() => on_progress(DeviceProgress::Retrying {
                    message: error.message(),
                }),
                Err(error) => return Err(error),
            }
        }
    }

    /// Exchanges a Silicon's short-lived token (`slt_…`) for Remind's tokens, with Remind's
    /// `client_id` alone. Single use: a refused token is used up too.
    pub async fn exchange_slt(&self, slt: &Secret) -> Result<Tokens> {
        let slt = slt.expose().trim();
        if !slt.starts_with("slt_") {
            return Err(Error::SignInRefused {
                refusal: Refusal::NotAShortLivedToken,
                message: "That is not a Silicon Accounts short-lived token: they start with slt_. Nothing was sent.".into(),
                hint: format!("Mint one for Remind with `{}` and pass it on (for example `{}`).", self.mint_command(), self.silicon_sign_in()),
            });
        }
        let form = [
            ("grant_type", SLT_GRANT_TYPE),
            ("slt", slt),
            ("client_id", self.app_id.as_str()),
        ];
        let received_at = now();
        match self.post_form("token", &form).await? {
            Ok(body) => {
                let response: TokenResponse =
                    serde_json::from_slice(&body).map_err(|_| self.undecodable())?;
                Tokens::from_response(&response, received_at)
            }
            Err(failure) => Err(self.slt_failure(failure)),
        }
    }

    /// Rotates a refresh token. The old one stops working: store the returned pair before
    /// using it, and never refresh the same token twice (that ends the sign-in).
    pub async fn refresh(&self, refresh_token: &Secret) -> Result<Tokens> {
        let received_at = now();
        match self
            .client
            .refresh_app_public_client(&self.app_id, refresh_token.expose())
            .await
        {
            Ok(response) => Tokens::from_response(&response, received_at),
            Err(error) if error.is_code("invalid_grant") => Err(Error::SignInRefused {
                refusal: Refusal::SignInEnded,
                message: error.message(),
                hint: self.sign_in_again(),
            }),
            Err(error) => Err(Error::accounts(&error)),
        }
    }

    /// Ends this sign-in at Silicon Accounts with its refresh token (RFC 7009, `client_id`
    /// alone). Returns whether Silicon Accounts revoked something; an unknown or already
    /// revoked token answers `false`, which still means the sign-in is over.
    pub async fn revoke(&self, refresh_token: &Secret) -> Result<bool> {
        let form = [
            ("token", refresh_token.expose().trim()),
            ("token_type_hint", "refresh_token"),
            ("client_id", self.app_id.as_str()),
        ];
        match self.post_form("revoke", &form).await? {
            Ok(body) => Ok(serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["revoked"].as_bool())
                .unwrap_or(true)),
            Err(failure) => Err(failure.into_error()),
        }
    }

    /// `silicon-accounts login --app <app_id> -q`.
    pub fn mint_command(&self) -> String {
        format!("silicon-accounts login --app {} -q", self.app_id)
    }

    fn silicon_sign_in(&self) -> String {
        format!("{} | remind login --slt-stdin", self.mint_command())
    }

    fn sign_in_again(&self) -> String {
        format!(
            "Sign in again: `remind login` (Carbons) or `{}` (Silicons).",
            self.silicon_sign_in()
        )
    }

    fn undecodable(&self) -> Error {
        Error::Accounts {
            status: None,
            code: "unexpected_response".into(),
            message: format!(
                "Silicon Accounts at {} answered with a body this client does not understand.",
                self.accounts_url()
            ),
            hint: Some("Check ACCOUNTS_URL points at Silicon Accounts.".into()),
            request_id: None,
        }
    }
}

fn device_expired(device: &DeviceStart) -> Error {
    Error::SignInRefused {
        refusal: Refusal::DeviceExpired,
        message: format!(
            "The sign-in code {} expired before it was approved (codes last {} minutes).",
            device.user_code,
            device.expires_in.div_ceil(60)
        ),
        hint: "Run `remind login` again for a new code.".into(),
    }
}

/// A Silicon Accounts error answer: OAuth (`{"error","error_description"}`) or the service's
/// own (`{"error":{"code","message","hint"}}`).
struct AccountsFailure {
    status: u16,
    code: String,
    message: String,
    hint: Option<String>,
    request_id: Option<String>,
}

impl AccountsFailure {
    fn parse(status: u16, body: &[u8], request_id: Option<String>) -> Self {
        let value: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
        let (code, message, hint) = match &value["error"] {
            serde_json::Value::String(code) => (
                code.clone(),
                value["error_description"].as_str().map(str::to_owned),
                None,
            ),
            serde_json::Value::Object(error) => (
                error
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unexpected_response")
                    .to_owned(),
                error
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                error
                    .get("hint")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            ),
            _ => ("unexpected_response".to_owned(), None, None),
        };
        Self {
            status,
            message: message.unwrap_or_else(|| {
                format!("Silicon Accounts answered HTTP {status} ({code}) without a description.")
            }),
            code,
            hint,
            request_id,
        }
    }

    fn into_error(self) -> Error {
        Error::Accounts {
            status: Some(self.status),
            code: self.code,
            message: self.message,
            hint: self.hint,
            request_id: self.request_id,
        }
    }
}

impl SignIn {
    /// POSTs a form to `/v1/oauth/{endpoint}`: the 2xx body, or Silicon Accounts' failure.
    /// Transport failures are errors. Never retried: a refresh or exchange is single use.
    async fn post_form(
        &self,
        endpoint: &str,
        form: &[(&str, &str)],
    ) -> Result<std::result::Result<Vec<u8>, AccountsFailure>> {
        let url = self
            .base
            .join(&format!("v1/oauth/{endpoint}"))
            .map_err(|_| Error::Invalid("invalid Silicon Accounts URL".into()))?;
        let mut request = self
            .http
            .post(url)
            .header("accept", "application/json")
            .form(form);
        if !self.telemetry {
            request = request.header("x-accounts-telemetry", "off");
        }
        let response = request.send().await.map_err(|error| Error::Accounts {
            status: None,
            code: if error.is_timeout() {
                "request_timeout".into()
            } else {
                "connection_failed".into()
            },
            message: format!(
                "Could not reach Silicon Accounts at {}: {error}.",
                self.accounts_url()
            ),
            hint: Some(
                "Check the network and ACCOUNTS_URL, then retry. A short-lived token that never reached Silicon Accounts is still unused."
                    .into(),
            ),
            request_id: None,
        })?;
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = response.bytes().await.map_err(|error| Error::Accounts {
            status: Some(status),
            code: "connection_failed".into(),
            message: format!("The answer from Silicon Accounts was cut off: {error}."),
            hint: Some("Retry.".into()),
            request_id: request_id.clone(),
        })?;
        if (200..300).contains(&status) {
            Ok(Ok(body.to_vec()))
        } else {
            Ok(Err(AccountsFailure::parse(status, &body, request_id)))
        }
    }

    /// Turns a refused short-lived token into a precise error with what to do next.
    fn slt_failure(&self, failure: AccountsFailure) -> Error {
        let mint = self.mint_command();
        match failure.code.as_str() {
            "invalid_grant" => {
                let refusal = slt_refusal(&failure.message);
                let hint = match &refusal {
                    Refusal::SltAlreadyUsed => format!(
                        "Each short-lived token works once. Mint a fresh one with `{mint}` and sign in with it right away."
                    ),
                    Refusal::SltExpired => format!(
                        "Short-lived tokens last 2 minutes. Mint a fresh one with `{mint}` and use it at once, for example `{}`.",
                        self.silicon_sign_in()
                    ),
                    Refusal::SltWrongApp { app } => format!(
                        "It was minted for {}. Mint one for Remind with `{mint}`.",
                        app.as_deref()
                            .map_or_else(|| "another app".to_owned(), |a| format!("'{a}'"))
                    ),
                    Refusal::SltUnknown => format!(
                        "Check it was passed whole, and that it comes from the Silicon Accounts this remind uses ({}). Mint a fresh one with `{mint}`.",
                        self.accounts_url()
                    ),
                    _ => format!("Mint a fresh one with `{mint}`."),
                };
                Error::SignInRefused {
                    refusal,
                    message: failure.message,
                    hint,
                }
            }
            "unauthorized_client" | "invalid_client" => Error::SignInRefused {
                refusal: Refusal::NotAllowed,
                message: failure.message,
                hint: format!(
                    "This Remind's sign-in setup at Silicon Accounts ({}) does not let its command line exchange short-lived tokens. Ask whoever runs this Remind to turn on public_client for '{}'.",
                    self.accounts_url(),
                    self.app_id
                ),
            },
            _ => failure.into_error(),
        }
    }
}

/// Classifies Silicon Accounts' description of a refused short-lived token.
fn slt_refusal(description: &str) -> Refusal {
    let lower = description.to_ascii_lowercase();
    if lower.contains("already used") {
        Refusal::SltAlreadyUsed
    } else if lower.contains("issued for the app") {
        Refusal::SltWrongApp {
            app: description.split('\'').nth(1).map(str::to_owned),
        }
    } else if lower.contains("expired") {
        Refusal::SltExpired
    } else if lower.contains("not known") {
        Refusal::SltUnknown
    } else if lower.contains("must be a short-lived token") {
        Refusal::NotAShortLivedToken
    } else {
        Refusal::Other
    }
}

#[cfg(test)]
mod tests {
    use super::{Refusal, slt_refusal};

    #[test]
    fn refused_short_lived_tokens_are_classified_from_the_description() {
        let cases = [
            (
                "The short-lived token was already used; each one works once. Get a new one.",
                Refusal::SltAlreadyUsed,
            ),
            (
                "The short-lived token expired at 2026-10-10T00:00:00.000Z (they last 120 seconds); get a new one with `silicon-accounts login --app remind`.",
                Refusal::SltExpired,
            ),
            (
                "The short-lived token was issued for the app 'briefcase', not for 'remind'; get one for 'remind' with `silicon-accounts login --app remind`.",
                Refusal::SltWrongApp {
                    app: Some("briefcase".into()),
                },
            ),
            (
                "The short-lived token is not known: it is mistyped or was never issued.",
                Refusal::SltUnknown,
            ),
            (
                "slt must be a short-lived token (it starts with slt_), but this is a refresh token.",
                Refusal::NotAShortLivedToken,
            ),
            (
                "The Silicon's STK was rotated after this short-lived token was issued.",
                Refusal::Other,
            ),
        ];
        for (description, expected) in cases {
            assert_eq!(slt_refusal(description), expected, "{description}");
        }
    }
}
