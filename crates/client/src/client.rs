//! The Remind API client: immutable configuration, one request per call, no stored session.
use crate::{Error, Mutation, Result, Secret, is_test_environment_key, models};
use reqwest::{Method, RequestBuilder};
use serde::de::DeserializeOwned;
use std::time::Duration;
use url::Url;

/// The production Remind API origin.
pub const DEFAULT_URL: &str = "https://backend.remind.teamofsilicons.com";
/// The API contract this client speaks (`/api/v2`, `X-Remind-API-Version: 2`).
pub const API_VERSION: u32 = 2;
const PREFIX: &str = "/api/v2";
const MAX_BODY: usize = 16 * 1024 * 1024;

/// How a request proves who is calling.
#[derive(Clone, Debug)]
pub(crate) enum Credential {
    /// `Authorization: Bearer <Silicon Accounts access token issued to Remind>`.
    Bearer(Secret),
    /// `Authorization: Proof <sap_… User verification proof for Remind>`.
    Proof(Secret),
}

/// Immutable credentials and transport configuration; no cached session, no refresh loop.
///
/// `with_*` methods return a new client, so one base client can serve many accounts.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    credential: Option<Credential>,
    test_key: Option<Secret>,
    telemetry_enabled: bool,
}

impl Client {
    /// A client for the Remind API at `base_url`, a pathless origin. HTTPS is required; plain
    /// HTTP is accepted only for this machine (localhost, 127.0.0.1, ::1). No network call.
    pub fn new(base_url: &str) -> Result<Self> {
        let base = parse_origin(base_url)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("silicon-remind-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(Error::Transport)?;
        Ok(Self {
            http,
            base,
            credential: None,
            test_key: None,
            telemetry_enabled: true,
        })
    }

    /// The API origin this client calls.
    pub fn base_url(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    /// Signs requests with a Silicon Accounts access token issued to Remind
    /// (`Authorization: Bearer`). Get one with [`crate::accounts::SignIn`].
    pub fn with_session(&self, access_token: Secret) -> Result<Self> {
        let token = access_token.expose().trim();
        if token.is_empty() {
            return Err(Error::Invalid("the access token is empty".into()));
        }
        if token.starts_with("sap_") {
            return Err(Error::Invalid(
                "this is a verification proof, not an access token; use Client::with_proof".into(),
            ));
        }
        let mut next = self.clone();
        next.credential = Some(Credential::Bearer(Secret::new(token)));
        Ok(next)
    }

    /// Signs requests with a User verification proof another app obtained from Silicon
    /// Accounts for Remind (`Authorization: Proof sap_…`). Remind accepts proofs only on the
    /// read routes (`reminders`, `reminder`, `executions`, `silicons`, `me`) with the
    /// `remind.schedules.read` scope, from apps its deployment trusts.
    pub fn with_proof(&self, proof: Secret) -> Result<Self> {
        let proof = proof.expose().trim();
        if !proof.starts_with("sap_") {
            return Err(Error::Invalid(
                "a verification proof starts with sap_; send access tokens with Client::with_session"
                    .into(),
            ));
        }
        let mut next = self.clone();
        next.credential = Some(Credential::Proof(Secret::new(proof)));
        Ok(next)
    }

    /// Selects a test environment by its 32-character key (`X-Remind-Test-Key`). The
    /// credential stays: inside a test environment you are still the account you signed in as.
    pub fn with_test_environment(&self, key: Secret) -> Result<Self> {
        if !is_test_environment_key(key.expose()) {
            return Err(Error::Invalid(
                "a test environment key is 32 letters and digits; get it with `remind env key <id>` or from whoever shared the environment".into(),
            ));
        }
        let mut next = self.clone();
        next.test_key = Some(key);
        Ok(next)
    }

    /// Turns client and request telemetry on or off (on by default).
    #[must_use]
    pub fn with_telemetry(&self, enabled: bool) -> Self {
        let mut next = self.clone();
        next.telemetry_enabled = enabled;
        next
    }

    /// Whether a test environment is selected.
    pub fn is_testing(&self) -> bool {
        self.test_key.is_some()
    }

    /// Whether a credential is attached.
    pub fn has_credential(&self) -> bool {
        self.credential.is_some()
    }

    pub(crate) fn telemetry_enabled(&self) -> bool {
        self.telemetry_enabled
    }

    pub(crate) fn require_test(&self) -> Result<()> {
        if self.is_testing() {
            Ok(())
        } else {
            Err(Error::Invalid(
                "this action is only possible for a test environment; use remind --test <test_id> <command>".into(),
            ))
        }
    }

    pub(crate) fn require_production(&self) -> Result<()> {
        if self.is_testing() {
            Err(Error::Invalid(
                "test environments are managed from production; run this without --test".into(),
            ))
        } else {
            Ok(())
        }
    }

    /// A request to `/api/v2{path}` (or exactly `path` when it starts with `/api/versions` or
    /// `/health`).
    pub(crate) fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let full = if path.starts_with("/health") || path == "/api/versions" {
            path.to_owned()
        } else {
            format!("{PREFIX}{path}")
        };
        let url = self
            .base
            .join(&full)
            .map_err(|_| Error::Invalid("invalid API path".into()))?;
        let mut request = self
            .http
            .request(method, url)
            .header("x-remind-api-version", API_VERSION.to_string());
        if !self.telemetry_enabled {
            request = request.header("x-remind-telemetry", "off");
        }
        match &self.credential {
            Some(Credential::Bearer(token)) => request = request.bearer_auth(token.expose()),
            Some(Credential::Proof(proof)) => {
                request = request.header("authorization", format!("Proof {}", proof.expose()));
            }
            None => {}
        }
        if let Some(key) = &self.test_key
            && !full.starts_with("/health")
        {
            request = request.header("x-remind-test-key", key.expose());
        }
        Ok(request)
    }

    pub(crate) fn mutation(
        &self,
        method: Method,
        path: &str,
        mutation: &Mutation,
    ) -> Result<RequestBuilder> {
        Ok(self
            .request(method, path)?
            .header("idempotency-key", mutation.key()))
    }

    pub(crate) async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let started = std::time::Instant::now();
        let result = self
            .send(request)
            .await
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|_| Error::Decode));
        self.observe(started, result.is_ok()).await;
        result
    }

    pub(crate) async fn empty(&self, request: RequestBuilder) -> Result<()> {
        let started = std::time::Instant::now();
        let result = self.send(request).await.map(|_| ());
        self.observe(started, result.is_ok()).await;
        result
    }

    async fn observe(&self, started: std::time::Instant, success: bool) {
        self.track(&models::TelemetryEvent {
            source: "rust_client".into(),
            event: "request_completed".into(),
            step: "response".into(),
            success,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            status_code: None,
        })
        .await;
    }

    async fn send(&self, request: RequestBuilder) -> Result<Vec<u8>> {
        let mut response = request.send().await.map_err(Error::Transport)?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(Error::Transport)? {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(Error::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        if status.is_success() {
            return Ok(bytes);
        }
        Err(api_error(status.as_u16(), &bytes, retry_after))
    }
}

/// Builds [`Error::Api`] from Remind's error body; a body that is not one still gives a
/// precise error with the HTTP status.
fn api_error(status: u16, body: &[u8], retry_after: Option<u64>) -> Error {
    let envelope: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let error = &envelope["error"];
    let text = |key: &str| error[key].as_str().map(str::to_owned);
    Error::Api {
        status,
        code: text("code").unwrap_or_else(|| "unexpected_response".into()),
        message: text("message").unwrap_or_else(|| {
            format!("Silicon Remind answered HTTP {status} without its error body")
        }),
        hint: text("hint"),
        request_id: text("request_id"),
        retry_after,
    }
}

/// Parses a pathless origin: https anywhere, http only for this machine.
pub(crate) fn parse_origin(raw: &str) -> Result<Url> {
    let base = Url::parse(raw.trim())
        .map_err(|_| Error::Invalid(format!("`{raw}` is not a valid URL")))?;
    if base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || base.port() == Some(0)
        || !(base.scheme() == "https" || base.scheme() == "http" && is_loopback(&base))
        || !matches!(base.path(), "" | "/")
    {
        return Err(Error::Invalid(format!(
            "`{raw}` must be an https origin (plain http only for localhost, 127.0.0.1 or ::1), without credentials, a path, query or fragment"
        )));
    }
    Ok(base)
}

pub(crate) fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}
