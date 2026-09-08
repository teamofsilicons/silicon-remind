//! Stateless client for the public Silicon Remind API.
//!
//! Construct a client with a service URL, then attach a bearer and organization.
//! `with_test_environment` selects isolated storage without changing any method.
//! Session persistence and token refresh decisions belong to the caller.
use reqwest::{Method, RequestBuilder};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{fmt, time::Duration};
use url::Url;
use uuid::Uuid;

pub mod models;
pub mod updates;

/// Secret material with explicit exposure and redacted debug formatting.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(SecretString);
impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }
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

/// Stable public failure, with no response body or credentials in transport errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{message} ({code}; HTTP {status})")]
    Api {
        status: u16,
        code: String,
        message: String,
        request_id: Option<String>,
        retry_after: Option<u64>,
    },
    #[error("could not reach Silicon Remind: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("invalid client input: {0}")]
    Invalid(String),
    #[error("Silicon Remind returned an incompatible response")]
    Decode,
    #[error("Silicon Remind response exceeded the 16 MiB limit")]
    ResponseTooLarge,
}
pub type Result<T> = std::result::Result<T, Error>;

/// One logical mutation's reusable idempotency key.
#[derive(Clone, Debug)]
pub struct Mutation(String);
impl Mutation {
    pub fn new() -> Self {
        Self(Uuid::now_v7().to_string())
    }
    pub fn with_key(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if !(16..=255).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid(
                "idempotency key must contain 16–255 visible ASCII characters".into(),
            ));
        }
        Ok(Self(key))
    }
    pub fn key(&self) -> &str {
        &self.0
    }
}
impl Default for Mutation {
    fn default() -> Self {
        Self::new()
    }
}

/// Immutable credentials and transport configuration; no cached session or refresh loop.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    bearer: Option<Secret>,
    org: Option<String>,
    test_key: Option<Secret>,
    auto_update: bool,
}
impl Client {
    /// Uses HTTPS, permitting HTTP only for literal loopback development addresses.
    pub fn new(base_url: &str) -> Result<Self> {
        let base =
            Url::parse(base_url).map_err(|_| Error::Invalid("invalid service URL".into()))?;
        let loopback = match base.host() {
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.port() == Some(0)
            || !(base.scheme() == "https" || base.scheme() == "http" && loopback)
            || !matches!(base.path(), "" | "/")
        {
            return Err(Error::Invalid("service URL must be an HTTPS origin (HTTP loopback is allowed), without credentials, a path, query or fragment".into()));
        }
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
            bearer: None,
            org: None,
            test_key: None,
            auto_update: true,
        })
    }
    /// Attaches an application access token and requested organization.
    pub fn with_session(&self, bearer: Secret, org: impl Into<String>) -> Result<Self> {
        let org = org.into();
        if !(3..=50).contains(&org.len())
            || !org
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        {
            return Err(Error::Invalid("invalid organization handle".into()));
        }
        let mut next = self.clone();
        next.bearer = Some(bearer);
        next.org = Some(org);
        Ok(next)
    }
    /// Selects a sandbox by its root key. Its public UUID is not a credential.
    pub fn with_test_environment(&self, key: Secret) -> Result<Self> {
        if key.expose().len() != 32 || !key.expose().bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(Error::Invalid(
                "test key must contain exactly 32 alphanumeric characters".into(),
            ));
        }
        let mut next = self.clone();
        next.test_key = Some(key);
        next.bearer = None;
        next.org = None;
        Ok(next)
    }
    /// Configure the selected sandbox's test-only IAM Application credential.
    pub async fn configure_environment_iam(&self, secret: &Secret) -> Result<()> {
        self.require_test()?;
        self.empty(
            self.request(Method::PUT, "/api/v1/testing-environment/iam")?
                .json(&serde_json::json!({"iam_app_secret":secret})),
        )
        .await
    }

    /// Disables or enables best-effort hourly dependency maintenance.
    pub fn auto_update(mut self, enabled: bool) -> Self {
        self.auto_update = enabled;
        self
    }
    pub fn is_testing(&self) -> bool {
        self.test_key.is_some()
    }
    pub async fn health(&self, ready: bool) -> Result<models::Health> {
        self.json(self.request(
            Method::GET,
            if ready {
                "/health/ready"
            } else {
                "/health/live"
            },
        )?)
        .await
    }
    pub async fn login(&self, slt: &Secret, mutation: &Mutation) -> Result<models::Session> {
        self.json(
            self.mutation(Method::POST, "/api/v1/auth/login", mutation)?
                .json(&serde_json::json!({"slt":slt})),
        )
        .await
    }
    pub async fn refresh(&self, token: &Secret, mutation: &Mutation) -> Result<models::Session> {
        self.json(
            self.mutation(Method::POST, "/api/v1/auth/refresh", mutation)?
                .json(&serde_json::json!({"refresh_token":token})),
        )
        .await
    }
    pub async fn logout(&self, token: &Secret, mutation: &Mutation) -> Result<()> {
        self.empty(
            self.mutation(Method::POST, "/api/v1/auth/logout", mutation)?
                .json(&serde_json::json!({"token":token})),
        )
        .await
    }
    pub async fn me(&self) -> Result<models::Identity> {
        self.json(self.request(Method::GET, "/api/v1/auth/me")?)
            .await
    }
    pub async fn create_reminder(
        &self,
        input: &models::CreateScheduleRequest,
        mutation: &Mutation,
    ) -> Result<models::ScheduleResponse> {
        self.json(
            self.mutation(Method::POST, "/api/v1/schedules", mutation)?
                .json(input),
        )
        .await
    }
    pub async fn reminders(
        &self,
        query: &models::ListSchedules,
    ) -> Result<models::Page<models::ScheduleResponse>> {
        self.json(self.request(Method::GET, "/api/v1/schedules")?.query(query))
            .await
    }
    pub async fn reminder(&self, id: Uuid) -> Result<models::ScheduleResponse> {
        self.json(self.request(Method::GET, &format!("/api/v1/schedules/{id}"))?)
            .await
    }
    pub async fn update_reminder(
        &self,
        id: Uuid,
        input: &models::PatchScheduleRequest,
        mutation: &Mutation,
    ) -> Result<models::ScheduleResponse> {
        self.json(
            self.mutation(Method::PATCH, &format!("/api/v1/schedules/{id}"), mutation)?
                .json(input),
        )
        .await
    }
    pub async fn set_status(
        &self,
        ids: Vec<Uuid>,
        status: models::ScheduleStatus,
        mutation: &Mutation,
    ) -> Result<models::StatusBatch> {
        self.json(
            self.mutation(Method::PATCH, "/api/v1/schedules", mutation)?
                .json(&models::BulkScheduleStatusRequest {
                    schedule_ids: ids,
                    status,
                }),
        )
        .await
    }
    pub async fn archive_reminder(&self, id: Uuid) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/api/v1/schedules/{id}"))?)
            .await
    }
    pub async fn executions(
        &self,
        id: Uuid,
        paging: &models::Paging,
    ) -> Result<models::Page<models::ExecutionResponse>> {
        self.json(
            self.request(Method::GET, &format!("/api/v1/schedules/{id}/executions"))?
                .query(paging),
        )
        .await
    }
    pub async fn configure_webhook(
        &self,
        input: &models::Destination,
    ) -> Result<models::DestinationReceipt> {
        self.json(self.request(Method::POST, "/api/v1/webhooks")?.json(input))
            .await
    }
    /// Add an independent webhook subscription. Multiple subscriptions may be
    /// active for one Silicon; an empty set is valid.
    pub async fn subscribe_webhook(
        &self,
        input: &models::Destination,
    ) -> Result<models::DestinationReceipt> {
        self.json(self.request(Method::POST, "/api/v1/webhooks")?.json(input))
            .await
    }
    /// List every active webhook subscription for the authenticated Silicon.
    pub async fn webhooks(&self) -> Result<models::WebhookSubscriptions> {
        self.json(self.request(Method::GET, "/api/v1/webhooks")?)
            .await
    }
    /// Disable one webhook subscription by registry UUID.
    pub async fn unsubscribe_webhook(&self, id: Uuid) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/api/v1/webhooks/{id}"))?)
            .await
    }
    pub async fn webhook(&self) -> Result<models::DestinationInfo> {
        self.json(self.request(Method::GET, "/api/v1/webhook")?)
            .await
    }
    pub async fn disable_webhook(&self) -> Result<()> {
        self.empty(self.request(Method::DELETE, "/api/v1/webhook")?)
            .await
    }
    pub async fn silicons(
        &self,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<models::Page<models::Silicon>> {
        let mut query = vec![("limit", limit.to_string())];
        if let Some(after) = after {
            query.push(("after", after.to_string()));
        }
        self.json(self.request(Method::GET, "/api/v1/silicons")?.query(&query))
            .await
    }
    pub async fn create_environment(
        &self,
        input: &models::CreateEnvironment,
    ) -> Result<models::EnvironmentCreated> {
        self.require_production()?;
        self.json(
            self.request(Method::POST, "/api/v1/test-environments")?
                .json(input),
        )
        .await
    }
    pub async fn environments(
        &self,
        include_deleted: bool,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<models::Page<models::TestEnvironment>> {
        self.require_production()?;
        let mut query = vec![
            ("include_deleted", include_deleted.to_string()),
            ("limit", limit.to_string()),
        ];
        if let Some(after) = after {
            query.push(("after", after.to_string()));
        }
        self.json(
            self.request(Method::GET, "/api/v1/test-environments")?
                .query(&query),
        )
        .await
    }
    pub async fn environment(&self, id: Uuid) -> Result<models::TestEnvironment> {
        self.require_production()?;
        self.json(self.request(Method::GET, &format!("/api/v1/test-environments/{id}"))?)
            .await
    }
    pub async fn environment_key(&self, id: Uuid) -> Result<models::EnvironmentKey> {
        self.require_production()?;
        self.json(self.request(Method::GET, &format!("/api/v1/test-environments/{id}/key"))?)
            .await
    }
    pub async fn rotate_environment_key(&self, id: Uuid) -> Result<models::EnvironmentKey> {
        self.require_production()?;
        self.json(self.request(
            Method::POST,
            &format!("/api/v1/test-environments/{id}/key-rotations"),
        )?)
        .await
    }
    pub async fn restore_environment(&self, id: Uuid) -> Result<models::EnvironmentKey> {
        self.require_production()?;
        self.json(self.request(
            Method::POST,
            &format!("/api/v1/test-environments/{id}/restorations"),
        )?)
        .await
    }
    pub async fn delete_environment(&self, id: Uuid) -> Result<()> {
        self.require_production()?;
        self.empty(self.request(Method::DELETE, &format!("/api/v1/test-environments/{id}"))?)
            .await
    }
    pub async fn current_environment(&self) -> Result<models::TestEnvironment> {
        self.require_test()?;
        self.json(self.request(Method::GET, "/api/v1/testing-environment")?)
            .await
    }
    pub async fn clean_environment(&self) -> Result<()> {
        self.require_test()?;
        self.empty(self.request(Method::POST, "/api/v1/testing-environment/cleanings")?)
            .await
    }
    fn require_test(&self) -> Result<()> {
        if self.is_testing() {
            Ok(())
        } else {
            Err(Error::Invalid("this action is only possible for a test environment; use remind --test <test_id> <command>".into()))
        }
    }
    fn require_production(&self) -> Result<()> {
        if self.is_testing() {
            Err(Error::Invalid(
                "manage environments using your production organization session; omit --test"
                    .into(),
            ))
        } else {
            Ok(())
        }
    }
    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = self
            .base
            .join(path)
            .map_err(|_| Error::Invalid("invalid API path".into()))?;
        let mut request = self.http.request(method, url);
        if let Some(token) = &self.bearer {
            request = request.bearer_auth(token.expose());
        }
        if let Some(org) = &self.org {
            request = request.header("x-org-id", org);
        }
        if let Some(key) = &self.test_key
            && path.starts_with("/api/v1/")
        {
            request = request.header("x-remind-test-key", key.expose());
        }
        Ok(request)
    }
    fn mutation(&self, method: Method, path: &str, mutation: &Mutation) -> Result<RequestBuilder> {
        Ok(self
            .request(method, path)?
            .header("idempotency-key", mutation.key()))
    }
    async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let result = self
            .send(request)
            .await
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|_| Error::Decode));
        if self.auto_update {
            updates::client_maintenance().await;
        }
        result
    }
    async fn empty(&self, request: RequestBuilder) -> Result<()> {
        let result = self.send(request).await.map(|_| ());
        if self.auto_update {
            updates::client_maintenance().await;
        }
        result
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
            if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err(Error::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let envelope: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| Error::Decode)?;
            let error = &envelope["error"];
            return Err(Error::Api {
                status: status.as_u16(),
                code: error["code"].as_str().unwrap_or("unknown_error").into(),
                message: error["message"].as_str().unwrap_or("request failed").into(),
                request_id: error["request_id"].as_str().map(str::to_owned),
                retry_after,
            });
        }
        Ok(bytes)
    }
}
