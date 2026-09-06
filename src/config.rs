//! Typed, validated runtime configuration loaded from `REMIND_*` variables.

use std::{
    collections::BTreeMap,
    env, fmt,
    net::SocketAddr,
    num::{NonZeroU32, NonZeroUsize},
    str::FromStr,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use thiserror::Error;
use url::Url;

const MAX_KEYRING_KEYS: usize = 16;
const MAX_WORKER_BATCH_SIZE: usize = 10_000;
const MIN_PRODUCTION_SECRET_BYTES: usize = 32;

/// Fully validated settings shared by the API and worker composition roots.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Deployment environment and its fail-fast safety policy.
    pub environment: RuntimeEnvironment,
    /// HTTP listener and request-boundary policy.
    pub server: ServerSettings,
    /// Runtime PostgreSQL connection pool.
    pub database: DatabaseSettings,
    /// Dedicated shared testing database; absent disables test environments.
    pub testing_database: Option<DatabaseSettings>,
    /// Silicon IAM token-introspection client.
    pub iam: IamSettings,
    /// Credential accepted by Remind's internal API.
    pub internal_api: InternalApiSettings,
    /// Versioned data-encryption keys.
    pub encryption: EncryptionSettings,
    /// Silicon Hook delivery client.
    pub hook: HookSettings,
    /// Durable worker polling and lease policy.
    pub worker: WorkerSettings,
    /// Delivery retry policy.
    pub retry: RetrySettings,
    /// Schedule, execution, and idempotency retention policy.
    pub retention: RetentionSettings,
    /// Tracing filter directive.
    pub log_filter: String,
}

/// Isolated settings for the privileged, one-shot migration process.
#[derive(Clone, Debug)]
pub struct MigrationSettings {
    /// Deployment environment and its database transport policy.
    pub environment: RuntimeEnvironment,
    /// Privileged migration database connection.
    pub database: DatabaseSettings,
    /// Privileged shared test-database connection, when configured.
    pub testing_database: Option<DatabaseSettings>,
    /// Tracing filter directive.
    pub log_filter: String,
}

/// Deployment environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEnvironment {
    /// Local developer process.
    Development,
    /// Automated test process.
    Test,
    /// Deployed production process.
    Production,
}

impl RuntimeEnvironment {
    /// Returns the canonical lowercase environment name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Test => "test",
            Self::Production => "production",
        }
    }
}

impl fmt::Display for RuntimeEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RuntimeEnvironment {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "production" | "prod" => Ok(Self::Production),
            _ => Err("must be development, test, or production".to_owned()),
        }
    }
}

/// HTTP listener and request-boundary settings.
#[derive(Clone, Debug)]
pub struct ServerSettings {
    /// Address on which the API listens.
    pub bind_addr: SocketAddr,
    /// Canonical externally visible API URL.
    pub public_base_url: Url,
    /// Maximum time allowed for one HTTP request.
    pub request_timeout: Duration,
    /// Maximum accepted HTTP request body size.
    pub max_body_bytes: usize,
    /// Maximum requests admitted concurrently by one API process.
    pub max_concurrent_requests: NonZeroUsize,
    /// Deadline for graceful process shutdown.
    pub shutdown_timeout: Duration,
}

/// PostgreSQL pool and session policy.
#[derive(Clone, Debug)]
pub struct DatabaseSettings {
    /// Secret-bearing PostgreSQL connection URL.
    pub url: SecretString,
    /// Maximum open connections per process.
    pub max_connections: NonZeroU32,
    /// Minimum idle connections per process.
    pub min_connections: u32,
    /// Pool acquisition deadline.
    pub acquire_timeout: Duration,
    /// Optional per-statement deadline; runtime settings require a value while
    /// the isolated migrator may delegate to the database.
    pub statement_timeout: Option<Duration>,
}

/// IAM's authenticated opaque-token introspection client.
#[derive(Clone, Debug)]
pub struct IamSettings {
    /// IAM service origin used by the official client.
    pub base_url: Url,
    /// HTTP Basic username registered for Remind.
    pub app_id: String,
    /// HTTP Basic password registered for Remind.
    pub app_secret: SecretString,
    /// End-to-end IAM request deadline.
    pub request_timeout: Duration,
    /// Retained application-webhook signing secrets keyed by IAM version.
    pub webhook_keys: BTreeMap<i16, SecretString>,
}

/// Authentication policy for service-only Remind endpoints.
#[derive(Clone, Debug)]
pub struct InternalApiSettings {
    /// Opaque bearer credential accepted by the internal API.
    pub bearer_token: SecretString,
}

/// Versioned AES-256 keyring used for encrypted Hook destination secrets.
#[derive(Clone, Debug)]
pub struct EncryptionSettings {
    /// Version used for newly encrypted values.
    pub current_version: i16,
    /// Current and retained decryption keys by positive version.
    pub keys: BTreeMap<i16, SecretString>,
}

impl EncryptionSettings {
    /// Returns the encoded active key.
    ///
    /// Configuration validation guarantees this value is present. Returning an
    /// option keeps that invariant explicit at infrastructure boundaries rather
    /// than introducing an infallible indexing panic.
    #[must_use]
    pub fn current_key(&self) -> Option<&SecretString> {
        self.keys.get(&self.current_version)
    }

    /// Returns an encoded historical key by version.
    #[must_use]
    pub fn key(&self, version: i16) -> Option<&SecretString> {
        self.keys.get(&version)
    }

    /// Decodes a configured key into fixed-size secret material.
    ///
    /// The loader has already validated every key, so `None` only indicates an
    /// unknown version or a violated in-memory invariant.
    #[must_use]
    pub fn decoded_key(&self, version: i16) -> Option<SecretBox<[u8; 32]>> {
        let encoded = self.key(version)?;
        let decoded = URL_SAFE_NO_PAD.decode(encoded.expose_secret()).ok()?;
        let key = <[u8; 32]>::try_from(decoded.as_slice()).ok()?;
        Some(SecretBox::new(Box::new(key)))
    }

    /// Decodes the active encryption key into fixed-size secret material.
    #[must_use]
    pub fn decoded_current_key(&self) -> Option<SecretBox<[u8; 32]>> {
        self.decoded_key(self.current_version)
    }
}

/// Silicon Hook outbound-delivery policy.
#[derive(Clone, Debug)]
pub struct HookSettings {
    /// Allowed Hook service base URL used to validate stored destinations.
    pub base_url: Url,
    /// TCP/TLS connection establishment deadline.
    pub connect_timeout: Duration,
    /// End-to-end Hook request deadline.
    pub request_timeout: Duration,
    /// Maximum accepted Hook response body size.
    pub max_response_bytes: usize,
}

/// Scheduler and delivery worker coordination policy.
#[derive(Clone, Debug)]
pub struct WorkerSettings {
    /// Address on which the worker exposes operational HTTP endpoints.
    pub operational_bind_addr: SocketAddr,
    /// Maximum records claimed by one worker query.
    pub batch_size: NonZeroUsize,
    /// Maximum Hook deliveries attempted concurrently by one worker process.
    pub max_delivery_concurrency: NonZeroUsize,
    /// Delay between idle worker polls.
    pub poll_interval: Duration,
    /// Duration for which a delivery claim is owned.
    pub lease_duration: Duration,
}

/// Delivery retry policy.
#[derive(Clone, Debug)]
pub struct RetrySettings {
    /// Maximum total delivery attempts, including the initial attempt.
    pub max_attempts: u16,
    /// Base delay before exponential backoff is applied.
    pub base_delay: Duration,
    /// Upper bound for an individual retry delay.
    pub max_delay: Duration,
    /// Maximum deterministic jitter as a percentage of the computed delay.
    pub jitter_percent: u8,
}

/// Data-retention and cleanup policy.
#[derive(Clone, Debug)]
pub struct RetentionSettings {
    /// Time idempotency responses remain replayable.
    pub idempotency_retention: Duration,
    /// Delay between retention sweeps.
    pub sweep_interval: Duration,
    /// Maximum records deleted by one retention transaction.
    pub batch_size: NonZeroUsize,
}

/// Configuration loading or validation failure.
#[derive(Debug, Error)]
pub enum SettingsError {
    /// A required variable is absent or empty.
    #[error("required environment variable {0} is missing")]
    Missing(&'static str),
    /// A variable cannot be parsed or violates a safety invariant.
    #[error("invalid environment variable {name}: {reason}")]
    Invalid {
        /// Environment variable name.
        name: &'static str,
        /// Redacted reason which never includes the supplied value.
        reason: String,
    },
}

impl Settings {
    /// Loads and validates API/worker settings from process environment.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when a required value is absent, malformed, or
    /// unsafe for the selected environment.
    pub fn from_env() -> Result<Self, SettingsError> {
        Self::from_source(&ProcessEnvironment)
    }

    fn from_source(source: &impl ConfigSource) -> Result<Self, SettingsError> {
        let environment = parse_or(source, "REMIND_ENVIRONMENT", "development")?;
        let server = ServerSettings {
            bind_addr: parse_or(source, "REMIND_BIND_ADDR", "127.0.0.1:8080")?,
            public_base_url: parse_or(source, "REMIND_PUBLIC_BASE_URL", "http://127.0.0.1:8080")?,
            request_timeout: duration_secs(source, "REMIND_REQUEST_TIMEOUT_SECONDS", 15)?,
            max_body_bytes: positive_size(source, "REMIND_MAX_BODY_BYTES", 1_048_576)?,
            max_concurrent_requests: parse_or(source, "REMIND_MAX_CONCURRENT_REQUESTS", "1024")?,
            shutdown_timeout: duration_secs(source, "REMIND_SHUTDOWN_TIMEOUT_SECONDS", 30)?,
        };

        let database = database_settings(source, DatabaseProfile::Runtime)?;
        let testing_database = testing_database_settings(source, &database, environment)?;
        let iam = IamSettings {
            base_url: parse_or(
                source,
                "REMIND_IAM_BASE_URL",
                "https://backend.iam.teamofsilicons.com",
            )?,
            app_id: required(source, "REMIND_IAM_APP_ID")?,
            app_secret: required_secret(source, "REMIND_IAM_APP_SECRET")?,
            request_timeout: duration_millis(source, "REMIND_IAM_REQUEST_TIMEOUT_MS", 3_000)?,
            webhook_keys: iam_webhook_keyring(source)?,
        };
        let internal_api = InternalApiSettings {
            bearer_token: required_secret(source, "REMIND_INTERNAL_API_TOKEN")?,
        };
        let encryption = encryption_settings(source)?;
        let hook = HookSettings {
            base_url: parse_or(
                source,
                "REMIND_HOOK_BASE_URL",
                "http://127.0.0.1:8082/api/v1",
            )?,
            connect_timeout: duration_millis(source, "REMIND_HOOK_CONNECT_TIMEOUT_MS", 2_000)?,
            request_timeout: duration_millis(source, "REMIND_HOOK_REQUEST_TIMEOUT_MS", 10_000)?,
            max_response_bytes: positive_size(source, "REMIND_HOOK_MAX_RESPONSE_BYTES", 65_536)?,
        };
        let worker = WorkerSettings {
            operational_bind_addr: parse_or(
                source,
                "REMIND_WORKER_OPERATIONAL_BIND_ADDR",
                "127.0.0.1:9090",
            )?,
            batch_size: parse_or(source, "REMIND_WORKER_BATCH_SIZE", "100")?,
            max_delivery_concurrency: parse_or(
                source,
                "REMIND_WORKER_MAX_DELIVERY_CONCURRENCY",
                "16",
            )?,
            poll_interval: duration_millis(source, "REMIND_WORKER_POLL_INTERVAL_MS", 500)?,
            lease_duration: duration_secs(source, "REMIND_WORKER_LEASE_SECONDS", 60)?,
        };
        let retry = RetrySettings {
            max_attempts: parse_or(source, "REMIND_RETRY_MAX_ATTEMPTS", "8")?,
            base_delay: duration_secs(source, "REMIND_RETRY_BASE_DELAY_SECONDS", 30)?,
            max_delay: duration_secs(source, "REMIND_RETRY_MAX_DELAY_SECONDS", 3_600)?,
            jitter_percent: parse_or(source, "REMIND_RETRY_JITTER_PERCENT", "20")?,
        };
        let retention = RetentionSettings {
            idempotency_retention: duration_hours(
                source,
                "REMIND_IDEMPOTENCY_RETENTION_HOURS",
                24,
            )?,
            sweep_interval: duration_secs(source, "REMIND_RETENTION_SWEEP_INTERVAL_SECONDS", 60)?,
            batch_size: parse_or(source, "REMIND_RETENTION_BATCH_SIZE", "500")?,
        };

        let settings = Self {
            environment,
            server,
            database,
            testing_database,
            iam,
            internal_api,
            encryption,
            hook,
            worker,
            retry,
            retention,
            log_filter: value_or(
                source,
                "REMIND_LOG_FILTER",
                "silicon_remind=info,tower_http=info",
            ),
        };
        validate_cross_field_policy(&settings)?;
        Ok(settings)
    }
}

impl MigrationSettings {
    /// Loads the isolated migration settings from process environment.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the migrator database URL or pool policy
    /// is missing, malformed, or insecure for production.
    pub fn from_env() -> Result<Self, SettingsError> {
        Self::from_source(&ProcessEnvironment)
    }

    fn from_source(source: &impl ConfigSource) -> Result<Self, SettingsError> {
        let environment = parse_or(source, "REMIND_ENVIRONMENT", "development")?;
        let database = database_settings(source, DatabaseProfile::Migrator)?;
        validate_database_transport(environment, &database, "REMIND_MIGRATOR_DATABASE_URL")?;

        let testing_database = optional(source, "REMIND_TEST_MIGRATOR_DATABASE_URL").map(|url| {
            let mut testing = database.clone();
            testing.url = SecretString::from(url);
            testing
        });
        if let Some(testing) = &testing_database {
            validate_database_url(testing, "REMIND_TEST_MIGRATOR_DATABASE_URL")?;
            validate_database_transport(environment, testing, "REMIND_TEST_MIGRATOR_DATABASE_URL")?;
            if Url::parse(testing.url.expose_secret())
                .ok()
                .map(|u| u.path().to_owned())
                == Url::parse(database.url.expose_secret())
                    .ok()
                    .map(|u| u.path().to_owned())
            {
                return Err(invalid(
                    "REMIND_TEST_MIGRATOR_DATABASE_URL",
                    "must name a different database from production",
                ));
            }
        }
        Ok(Self {
            environment,
            database,
            testing_database,
            log_filter: value_or(source, "REMIND_LOG_FILTER", "silicon_remind=info"),
        })
    }
}

#[derive(Clone, Copy)]
enum DatabaseProfile {
    Runtime,
    Migrator,
}

fn database_settings(
    source: &impl ConfigSource,
    profile: DatabaseProfile,
) -> Result<DatabaseSettings, SettingsError> {
    let (
        url_name,
        max_name,
        acquire_name,
        statement_name,
        max_default,
        acquire_default,
        statement_default,
    ) = match profile {
        DatabaseProfile::Runtime => (
            "REMIND_DATABASE_URL",
            "REMIND_DATABASE_MAX_CONNECTIONS",
            "REMIND_DATABASE_ACQUIRE_TIMEOUT_SECONDS",
            "REMIND_DATABASE_STATEMENT_TIMEOUT_SECONDS",
            "16",
            3,
            10,
        ),
        DatabaseProfile::Migrator => (
            "REMIND_MIGRATOR_DATABASE_URL",
            "REMIND_MIGRATOR_DATABASE_MAX_CONNECTIONS",
            "REMIND_MIGRATOR_DATABASE_ACQUIRE_TIMEOUT_SECONDS",
            "REMIND_MIGRATOR_DATABASE_STATEMENT_TIMEOUT_SECONDS",
            "2",
            10,
            120,
        ),
    };
    let min_connections = match profile {
        DatabaseProfile::Runtime => parse_or(source, "REMIND_DATABASE_MIN_CONNECTIONS", "1")?,
        DatabaseProfile::Migrator => 0,
    };
    let settings = DatabaseSettings {
        url: required_secret(source, url_name)?,
        max_connections: parse_or(source, max_name, max_default)?,
        min_connections,
        acquire_timeout: duration_secs(source, acquire_name, acquire_default)?,
        statement_timeout: optional_duration_secs(source, statement_name, statement_default)?,
    };
    if settings.min_connections >= settings.max_connections.get() {
        return Err(invalid(
            match profile {
                DatabaseProfile::Runtime => "REMIND_DATABASE_MIN_CONNECTIONS",
                DatabaseProfile::Migrator => "REMIND_MIGRATOR_DATABASE_MAX_CONNECTIONS",
            },
            "minimum connections must be lower than maximum connections",
        ));
    }
    validate_database_url(&settings, url_name)?;
    Ok(settings)
}

fn encryption_settings(source: &impl ConfigSource) -> Result<EncryptionSettings, SettingsError> {
    const CURRENT: &str = "REMIND_ENCRYPTION_CURRENT_VERSION";
    const KEYRING: &str = "REMIND_ENCRYPTION_KEYRING";

    let current_version: i16 = parse_required(source, CURRENT)?;
    if current_version <= 0 {
        return Err(invalid(CURRENT, "must be a positive small integer"));
    }

    let encoded = required(source, KEYRING)?;
    let raw = serde_json::from_str::<UniqueStringMap>(&encoded)
        .map_err(|_| invalid(KEYRING, "must be a JSON object of unique key versions"))?;
    let mut keys = BTreeMap::new();
    for (version, encoded_key) in raw.0 {
        let version = version
            .parse::<i16>()
            .map_err(|_| invalid(KEYRING, "key versions must be positive small integers"))?;
        if version <= 0 || keys.contains_key(&version) {
            return Err(invalid(
                KEYRING,
                "key versions must be unique positive small integers",
            ));
        }
        let key = SecretString::from(encoded_key);
        validate_encoded_key(KEYRING, &key)?;
        keys.insert(version, key);
    }
    if !keys.contains_key(&current_version) {
        return Err(invalid(
            CURRENT,
            "current version must exist in the configured keyring",
        ));
    }

    Ok(EncryptionSettings {
        current_version,
        keys,
    })
}

fn iam_webhook_keyring(
    source: &impl ConfigSource,
) -> Result<BTreeMap<i16, SecretString>, SettingsError> {
    const KEYRING: &str = "REMIND_IAM_WEBHOOK_KEYRING";

    let encoded = required(source, KEYRING)?;
    let raw = serde_json::from_str::<UniqueStringMap>(&encoded)
        .map_err(|_| invalid(KEYRING, "must be a JSON object of unique key versions"))?;
    let mut keys = BTreeMap::new();
    for (version, encoded_key) in raw.0 {
        let version = version
            .parse::<i16>()
            .map_err(|_| invalid(KEYRING, "key versions must be positive small integers"))?;
        if version <= 0 || keys.contains_key(&version) {
            return Err(invalid(
                KEYRING,
                "key versions must be unique positive small integers",
            ));
        }
        let key = SecretString::from(encoded_key);
        validate_iam_webhook_key(KEYRING, &key)?;
        keys.insert(version, key);
    }
    if keys.is_empty() {
        return Err(invalid(KEYRING, "must contain at least one key version"));
    }
    Ok(keys)
}

fn validate_cross_field_policy(settings: &Settings) -> Result<(), SettingsError> {
    let Settings {
        environment,
        server,
        database,
        iam,
        internal_api,
        hook,
        worker,
        retry,
        retention,
        ..
    } = settings;
    validate_http_url("REMIND_PUBLIC_BASE_URL", &server.public_base_url)?;
    validate_http_url("REMIND_IAM_BASE_URL", &iam.base_url)?;
    validate_http_url("REMIND_HOOK_BASE_URL", &hook.base_url)?;

    if iam.app_id.len() > 255 {
        return Err(invalid("REMIND_IAM_APP_ID", "must be at most 255 bytes"));
    }
    if worker.batch_size.get() > MAX_WORKER_BATCH_SIZE {
        return Err(invalid("REMIND_WORKER_BATCH_SIZE", "must be at most 10000"));
    }
    if worker.max_delivery_concurrency.get() > MAX_WORKER_BATCH_SIZE {
        return Err(invalid(
            "REMIND_WORKER_MAX_DELIVERY_CONCURRENCY",
            "must be at most 10000",
        ));
    }
    if retention.batch_size.get() > MAX_WORKER_BATCH_SIZE {
        return Err(invalid(
            "REMIND_RETENTION_BATCH_SIZE",
            "must be at most 10000",
        ));
    }
    if worker.max_delivery_concurrency > worker.batch_size {
        return Err(invalid(
            "REMIND_WORKER_MAX_DELIVERY_CONCURRENCY",
            "must be less than or equal to the worker batch size",
        ));
    }
    let statement_timeout = database.statement_timeout.ok_or_else(|| {
        invalid(
            "REMIND_DATABASE_STATEMENT_TIMEOUT_SECONDS",
            "must be enabled so delivery work has a bounded lease safety budget",
        )
    })?;
    let database_budget = database
        .acquire_timeout
        .checked_add(statement_timeout)
        .and_then(|duration| duration.checked_mul(2))
        .ok_or_else(|| {
            invalid(
                "REMIND_WORKER_LEASE_SECONDS",
                "delivery database safety budget is too large",
            )
        })?;
    let minimum_lease = hook
        .request_timeout
        .checked_add(database_budget)
        .and_then(|duration| duration.checked_add(worker.poll_interval))
        .and_then(|duration| duration.checked_add(Duration::from_secs(5)))
        .ok_or_else(|| {
            invalid(
                "REMIND_WORKER_LEASE_SECONDS",
                "delivery safety budget is too large",
            )
        })?;
    if worker.lease_duration <= minimum_lease {
        return Err(invalid(
            "REMIND_WORKER_LEASE_SECONDS",
            "must exceed the Hook timeout, two database operation budgets, the poll interval, and a five-second safety margin",
        ));
    }
    if retry.max_attempts == 0 {
        return Err(invalid(
            "REMIND_RETRY_MAX_ATTEMPTS",
            "must be greater than zero",
        ));
    }
    if retry.max_delay < retry.base_delay {
        return Err(invalid(
            "REMIND_RETRY_MAX_DELAY_SECONDS",
            "must be greater than or equal to the base retry delay",
        ));
    }
    if retry.jitter_percent > 100 {
        return Err(invalid(
            "REMIND_RETRY_JITTER_PERCENT",
            "must be between 0 and 100",
        ));
    }

    if *environment != RuntimeEnvironment::Production {
        return Ok(());
    }

    validate_database_transport(*environment, database, "REMIND_DATABASE_URL")?;
    require_https("REMIND_PUBLIC_BASE_URL", &server.public_base_url)?;
    require_https("REMIND_IAM_BASE_URL", &iam.base_url)?;
    require_https("REMIND_HOOK_BASE_URL", &hook.base_url)?;
    validate_secret_strength("REMIND_IAM_APP_SECRET", &iam.app_secret)?;
    validate_secret_strength("REMIND_INTERNAL_API_TOKEN", &internal_api.bearer_token)?;
    Ok(())
}

fn validate_database_url(
    settings: &DatabaseSettings,
    name: &'static str,
) -> Result<(), SettingsError> {
    let url = Url::parse(settings.url.expose_secret())
        .map_err(|_| invalid(name, "must be a valid PostgreSQL URL"))?;
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        return Err(invalid(name, "must use postgres:// or postgresql://"));
    }
    if url.host_str().is_none() {
        return Err(invalid(name, "must include a database host"));
    }
    Ok(())
}

fn validate_database_transport(
    environment: RuntimeEnvironment,
    settings: &DatabaseSettings,
    name: &'static str,
) -> Result<(), SettingsError> {
    if environment != RuntimeEnvironment::Production {
        return Ok(());
    }
    let url = Url::parse(settings.url.expose_secret())
        .map_err(|_| invalid(name, "must be a valid PostgreSQL URL"))?;
    let ssl_mode = url
        .query_pairs()
        .find_map(|(key, value)| (key == "sslmode").then_some(value.into_owned()));
    if ssl_mode.as_deref() != Some("verify-full") {
        return Err(invalid(name, "must set sslmode=verify-full in production"));
    }
    Ok(())
}

fn validate_http_url(name: &'static str, url: &Url) -> Result<(), SettingsError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(name, "must use http or https"));
    }
    if url.host_str().is_none() {
        return Err(invalid(name, "must include a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(name, "must not contain embedded credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid(name, "must not contain a query or fragment"));
    }
    Ok(())
}

fn require_https(name: &'static str, url: &Url) -> Result<(), SettingsError> {
    if url.scheme() != "https" {
        return Err(invalid(name, "must use https in production"));
    }
    Ok(())
}

fn validate_secret_strength(
    name: &'static str,
    secret: &SecretString,
) -> Result<(), SettingsError> {
    if secret.expose_secret().len() < MIN_PRODUCTION_SECRET_BYTES {
        return Err(invalid(
            name,
            format!("must contain at least {MIN_PRODUCTION_SECRET_BYTES} bytes in production"),
        ));
    }
    Ok(())
}

fn validate_encoded_key(name: &'static str, key: &SecretString) -> Result<(), SettingsError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(key.expose_secret())
        .map_err(|_| invalid(name, "every key must be unpadded base64url"))?;
    if decoded.len() != 32 {
        return Err(invalid(name, "every key must decode to exactly 32 bytes"));
    }
    Ok(())
}

fn validate_iam_webhook_key(name: &'static str, key: &SecretString) -> Result<(), SettingsError> {
    if !key.expose_secret().starts_with("whs_") {
        return Err(invalid(
            name,
            "every Application key must use the whs_ prefix",
        ));
    }
    silicon_iam_client::WebhookSecret::new(key.expose_secret()).map_err(|_| {
        invalid(
            name,
            "every key must be an official IAM webhook signing credential",
        )
    })?;
    Ok(())
}

trait ConfigSource {
    fn get(&self, name: &'static str) -> Option<String>;
}

struct ProcessEnvironment;

impl ConfigSource for ProcessEnvironment {
    fn get(&self, name: &'static str) -> Option<String> {
        env::var(name).ok()
    }
}

struct UniqueStringMap(BTreeMap<String, String>);

impl<'de> Deserialize<'de> for UniqueStringMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(UniqueStringMapVisitor)
    }
}

struct UniqueStringMapVisitor;

impl<'de> Visitor<'de> for UniqueStringMapVisitor {
    type Value = UniqueStringMap;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object with unique string keys and string values")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();
        while let Some((key, value)) = map.next_entry::<String, String>()? {
            if values.insert(key, value).is_some() {
                return Err(serde::de::Error::custom("duplicate key version"));
            }
            if values.len() > MAX_KEYRING_KEYS {
                return Err(serde::de::Error::custom("too many key versions"));
            }
        }
        Ok(UniqueStringMap(values))
    }
}

fn required(source: &impl ConfigSource, name: &'static str) -> Result<String, SettingsError> {
    optional(source, name).ok_or(SettingsError::Missing(name))
}

fn required_secret(
    source: &impl ConfigSource,
    name: &'static str,
) -> Result<SecretString, SettingsError> {
    required(source, name).map(SecretString::from)
}

fn optional(source: &impl ConfigSource, name: &'static str) -> Option<String> {
    source
        .get(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn value_or(source: &impl ConfigSource, name: &'static str, default: &str) -> String {
    optional(source, name).unwrap_or_else(|| default.to_owned())
}

fn parse_required<T>(source: &impl ConfigSource, name: &'static str) -> Result<T, SettingsError>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    let value = required(source, name)?;
    parse(name, &value)
}

fn parse_or<T>(
    source: &impl ConfigSource,
    name: &'static str,
    default: &str,
) -> Result<T, SettingsError>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    let value = value_or(source, name, default);
    parse(name, &value)
}

fn parse<T>(name: &'static str, value: &str) -> Result<T, SettingsError>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|error| invalid(name, error.to_string()))
}

fn positive_size(
    source: &impl ConfigSource,
    name: &'static str,
    default: usize,
) -> Result<usize, SettingsError> {
    let value = parse_or(source, name, &default.to_string())?;
    if value == 0 {
        return Err(invalid(name, "must be greater than zero"));
    }
    Ok(value)
}

fn duration_millis(
    source: &impl ConfigSource,
    name: &'static str,
    default: u64,
) -> Result<Duration, SettingsError> {
    let value = parse_or(source, name, &default.to_string())?;
    if value == 0 {
        return Err(invalid(name, "must be greater than zero"));
    }
    Ok(Duration::from_millis(value))
}

fn duration_secs(
    source: &impl ConfigSource,
    name: &'static str,
    default: u64,
) -> Result<Duration, SettingsError> {
    let value = parse_or(source, name, &default.to_string())?;
    if value == 0 {
        return Err(invalid(name, "must be greater than zero"));
    }
    Ok(Duration::from_secs(value))
}

fn optional_duration_secs(
    source: &impl ConfigSource,
    name: &'static str,
    default: u64,
) -> Result<Option<Duration>, SettingsError> {
    let value = parse_or(source, name, &default.to_string())?;
    Ok((value != 0).then(|| Duration::from_secs(value)))
}

fn duration_hours(
    source: &impl ConfigSource,
    name: &'static str,
    default: u64,
) -> Result<Duration, SettingsError> {
    scaled_duration(source, name, default, 3_600)
}

fn scaled_duration(
    source: &impl ConfigSource,
    name: &'static str,
    default: u64,
    scale: u64,
) -> Result<Duration, SettingsError> {
    let value: u64 = parse_or(source, name, &default.to_string())?;
    let seconds = value
        .checked_mul(scale)
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| invalid(name, "must be a positive duration within range"))?;
    Ok(Duration::from_secs(seconds))
}

fn invalid(name: &'static str, reason: impl Into<String>) -> SettingsError {
    SettingsError::Invalid {
        name,
        reason: reason.into(),
    }
}

fn testing_database_settings(
    source: &impl ConfigSource,
    database: &DatabaseSettings,
    environment: RuntimeEnvironment,
) -> Result<Option<DatabaseSettings>, SettingsError> {
    let testing_database = optional(source, "REMIND_TEST_DATABASE_URL").map(|url| {
        let mut testing = database.clone();
        testing.url = SecretString::from(url);
        testing
    });
    if let Some(testing) = &testing_database {
        validate_database_url(testing, "REMIND_TEST_DATABASE_URL")?;
        validate_database_transport(environment, testing, "REMIND_TEST_DATABASE_URL")?;
        if Url::parse(testing.url.expose_secret())
            .ok()
            .map(|u| u.path().to_owned())
            == Url::parse(database.url.expose_secret())
                .ok()
                .map(|u| u.path().to_owned())
        {
            return Err(invalid(
                "REMIND_TEST_DATABASE_URL",
                "must use a separate database from production",
            ));
        }
    }
    Ok(testing_database)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, net::SocketAddr};

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::{ConfigSource, MigrationSettings, RuntimeEnvironment, Settings, SettingsError};

    struct MapSource(BTreeMap<&'static str, String>);

    impl ConfigSource for MapSource {
        fn get(&self, name: &'static str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    fn valid_source() -> MapSource {
        let key = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let webhook_key = URL_SAFE_NO_PAD.encode([11_u8; 32]);
        MapSource(BTreeMap::from([
            (
                "REMIND_DATABASE_URL",
                "postgres://remind:remind@127.0.0.1:5432/remind".to_owned(),
            ),
            ("REMIND_IAM_APP_ID", "silicon-remind".to_owned()),
            (
                "REMIND_IAM_APP_SECRET",
                "development-iam-application-secret".to_owned(),
            ),
            (
                "REMIND_INTERNAL_API_TOKEN",
                "development-internal-service-token".to_owned(),
            ),
            ("REMIND_ENCRYPTION_CURRENT_VERSION", "1".to_owned()),
            ("REMIND_ENCRYPTION_KEYRING", format!(r#"{{"1":"{key}"}}"#)),
            (
                "REMIND_IAM_WEBHOOK_KEYRING",
                format!(r#"{{"1":"whs_{webhook_key}"}}"#),
            ),
        ]))
    }

    #[test]
    fn runtime_environment_is_case_insensitive() {
        assert_eq!(
            "PrOd".parse::<RuntimeEnvironment>(),
            Ok(RuntimeEnvironment::Production)
        );
    }

    #[test]
    fn loads_typed_defaults_without_exposing_secrets() -> Result<(), SettingsError> {
        let settings = Settings::from_source(&valid_source())?;

        assert_eq!(settings.environment, RuntimeEnvironment::Development);
        assert_eq!(
            settings.worker.operational_bind_addr,
            SocketAddr::from(([127, 0, 0, 1], 9090))
        );
        assert_eq!(settings.worker.max_delivery_concurrency.get(), 16);
        assert_eq!(settings.retry.max_attempts, 8);
        assert!(settings.encryption.current_key().is_some());

        let debug = format!("{settings:?}");
        assert!(!debug.contains("development-iam-application-secret"));
        assert!(!debug.contains("development-internal-service-token"));
        Ok(())
    }

    #[test]
    fn rejects_an_encryption_key_with_the_wrong_length() {
        let mut source = valid_source();
        let short_key = URL_SAFE_NO_PAD.encode([1_u8; 31]);
        source.0.insert(
            "REMIND_ENCRYPTION_KEYRING",
            format!(r#"{{"1":"{short_key}"}}"#),
        );

        assert!(matches!(
            Settings::from_source(&source),
            Err(SettingsError::Invalid {
                name: "REMIND_ENCRYPTION_KEYRING",
                ..
            })
        ));
    }

    #[test]
    fn rejects_an_iam_webhook_key_without_the_contract_prefix() {
        let mut source = valid_source();
        let key = URL_SAFE_NO_PAD.encode([9_u8; 32]);
        source
            .0
            .insert("REMIND_IAM_WEBHOOK_KEYRING", format!(r#"{{"1":"{key}"}}"#));

        assert!(matches!(
            Settings::from_source(&source),
            Err(SettingsError::Invalid {
                name: "REMIND_IAM_WEBHOOK_KEYRING",
                ..
            })
        ));
    }

    #[test]
    fn rejects_insecure_hook_transport_in_production() {
        let mut source = valid_source();
        source
            .0
            .insert("REMIND_ENVIRONMENT", "production".to_owned());
        source.0.insert(
            "REMIND_DATABASE_URL",
            "postgres://remind:remind@db.example/remind?sslmode=verify-full".to_owned(),
        );
        source.0.insert(
            "REMIND_PUBLIC_BASE_URL",
            "https://remind.example".to_owned(),
        );
        source.0.insert(
            "REMIND_IAM_BASE_URL",
            "https://iam.example/api/v1/auth/tokens/introspect".to_owned(),
        );
        source.0.insert(
            "REMIND_IAM_APP_SECRET",
            "a-production-iam-secret-that-is-long-enough".to_owned(),
        );
        source.0.insert(
            "REMIND_INTERNAL_API_TOKEN",
            "a-production-internal-token-that-is-long-enough".to_owned(),
        );

        assert!(matches!(
            Settings::from_source(&source),
            Err(SettingsError::Invalid {
                name: "REMIND_HOOK_BASE_URL",
                ..
            })
        ));
    }

    #[test]
    fn rejects_retry_cap_below_base_delay() {
        let mut source = valid_source();
        source
            .0
            .insert("REMIND_RETRY_BASE_DELAY_SECONDS", "31".to_owned());
        source
            .0
            .insert("REMIND_RETRY_MAX_DELAY_SECONDS", "30".to_owned());

        assert!(matches!(
            Settings::from_source(&source),
            Err(SettingsError::Invalid {
                name: "REMIND_RETRY_MAX_DELAY_SECONDS",
                ..
            })
        ));
    }

    #[test]
    fn rejects_delivery_concurrency_above_claim_batch() {
        let mut source = valid_source();
        source.0.insert("REMIND_WORKER_BATCH_SIZE", "8".to_owned());
        source
            .0
            .insert("REMIND_WORKER_MAX_DELIVERY_CONCURRENCY", "9".to_owned());

        assert!(matches!(
            Settings::from_source(&source),
            Err(SettingsError::Invalid {
                name: "REMIND_WORKER_MAX_DELIVERY_CONCURRENCY",
                ..
            })
        ));
    }

    #[test]
    fn rejects_worker_and_retention_batches_above_repository_limit() {
        for variable in [
            "REMIND_WORKER_BATCH_SIZE",
            "REMIND_WORKER_MAX_DELIVERY_CONCURRENCY",
            "REMIND_RETENTION_BATCH_SIZE",
        ] {
            let mut source = valid_source();
            source.0.insert(variable, "10001".to_owned());
            if variable == "REMIND_WORKER_MAX_DELIVERY_CONCURRENCY" {
                source
                    .0
                    .insert("REMIND_WORKER_BATCH_SIZE", "10000".to_owned());
            }

            assert!(matches!(
                Settings::from_source(&source),
                Err(SettingsError::Invalid { name, .. }) if name == variable
            ));
        }
    }

    #[test]
    fn rejects_delivery_lease_without_full_outbound_safety_budget() {
        let mut source = valid_source();
        source
            .0
            .insert("REMIND_WORKER_LEASE_SECONDS", "41".to_owned());

        assert!(matches!(
            Settings::from_source(&source),
            Err(SettingsError::Invalid {
                name: "REMIND_WORKER_LEASE_SECONDS",
                ..
            })
        ));
    }

    #[test]
    fn migration_settings_use_only_the_migrator_database() -> Result<(), SettingsError> {
        let source = MapSource(BTreeMap::from([(
            "REMIND_MIGRATOR_DATABASE_URL",
            "postgres://migrator:migrator@127.0.0.1:5432/remind".to_owned(),
        )]));

        let settings = MigrationSettings::from_source(&source)?;
        assert_eq!(settings.database.max_connections.get(), 2);
        assert_eq!(settings.database.min_connections, 0);
        Ok(())
    }
}
