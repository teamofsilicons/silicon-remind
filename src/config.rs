//! Typed, validated runtime configuration loaded from `REMIND_*` variables.

use std::{
    collections::{BTreeMap, BTreeSet},
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

/// Production Silicon Accounts origin, used when `ACCOUNTS_URL` is unset.
pub const DEFAULT_ACCOUNTS_URL: &str = "https://accounts.teamofsilicons.com";
/// Remind's app id at Silicon Accounts, used when `REMIND_APP_ID` is unset.
pub const DEFAULT_APP_ID: &str = "remind";
/// Proof scope that lets another app read reminders for an account.
pub const SCHEDULES_READ_SCOPE: &str = "remind.schedules.read";
/// Every proof scope Remind honours.
pub const PROOF_SCOPES: &[&str] = &[SCHEDULES_READ_SCOPE];

/// Variables from the Silicon IAM and Honeycomb era that Remind now ignores.
pub const RETIRED_VARIABLES: &[&str] = &[
    "REMIND_IAM_BASE_URL",
    "REMIND_IAM_APP_ID",
    "REMIND_IAM_APP_SECRET",
    "REMIND_IAM_REQUEST_TIMEOUT_MS",
    "REMIND_IAM_WEBHOOK_KEYRING",
    "REMIND_INTERNAL_API_TOKEN",
    "REMIND_HONEYCOMB_SERVICE_TOKEN",
    "REMIND_HONEYCOMB_BASE_URL",
];

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
    /// Silicon Accounts: token verification, lookups, proofs and the app webhook.
    pub accounts: AccountsSettings,
    /// Versioned data-encryption keys.
    pub encryption: EncryptionSettings,
    /// Generic outbound webhook delivery client.
    pub webhook: WebhookSettings,
    /// Durable worker polling and lease policy.
    pub worker: WorkerSettings,
    /// Exact test receiver URLs explicitly allowed to perform external test delivery.
    pub test_webhook_urls: Vec<String>,
    /// Postmark server credential; absent disables production bug submission.
    pub postmark_server_token: Option<SecretString>,
    /// Space Station telemetry, enabled unless explicitly opted out.
    pub telemetry_enabled: bool,
    /// Write-only key for the dedicated Remind production event table.
    pub telemetry_table_key: Option<SecretString>,
    /// Private spool directory for the official Space Station Rust client.
    pub telemetry_home: std::path::PathBuf,
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

/// Silicon Accounts, Remind's only identity provider.
#[derive(Clone, Debug)]
pub struct AccountsSettings {
    /// Public Silicon Accounts origin (`ACCOUNTS_URL`). Access tokens carry it as `iss`.
    pub url: Url,
    /// Server-to-server origin (`ACCOUNTS_API_URL`, defaults to `url`).
    pub api_url: Url,
    /// Remind's app id (`REMIND_APP_ID`); access tokens must carry it as `aud`.
    pub app_id: String,
    /// Remind's app secret (`REMIND_APP_SECRET`), used only server to server.
    pub app_secret: SecretString,
    /// App webhook signing secrets (`REMIND_ACCOUNTS_WEBHOOK_SECRET`, comma separated
    /// while a rotation overlaps). A delivery is accepted when any of them verifies.
    pub webhook_secrets: Vec<SecretString>,
    /// Deadline for one call to Silicon Accounts (`REMIND_ACCOUNTS_REQUEST_TIMEOUT_MS`).
    pub request_timeout: Duration,
    /// How long a looked-up profile and custodian stay fresh
    /// (`REMIND_ACCOUNTS_LOOKUP_TTL_SECONDS`); webhooks update them sooner.
    pub lookup_ttl: Duration,
    /// Which apps may send User verification proofs for which scope (`REMIND_PROOF_ISSUERS`).
    pub proof_issuers: ProofIssuers,
}

/// Per-scope allow-list of apps whose User verification proofs Remind accepts.
///
/// Parsed from comma-separated `scope=app_id` pairs; a scope may repeat to allow
/// several apps. Empty (the default) means no proof is accepted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProofIssuers(BTreeMap<String, BTreeSet<String>>);

impl ProofIssuers {
    /// Returns whether `app_id` may send proofs carrying `scope`.
    #[must_use]
    pub fn allows(&self, scope: &str, app_id: &str) -> bool {
        self.0.get(scope).is_some_and(|apps| apps.contains(app_id))
    }

    /// Returns whether no issuer is configured at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Parses `scope=app_id[,scope=app_id...]`.
    ///
    /// # Errors
    ///
    /// Returns a precise reason for an unknown scope or a malformed pair.
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut issuers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for pair in value
            .split(',')
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
        {
            let Some((scope, app_id)) = pair.split_once('=') else {
                return Err(format!(
                    "`{pair}` is not a scope=app_id pair, for example remind.schedules.read=interface"
                ));
            };
            let (scope, app_id) = (scope.trim(), app_id.trim());
            if !PROOF_SCOPES.contains(&scope) {
                return Err(format!(
                    "Remind honours no proof scope `{scope}`; the scopes it honours are: {}",
                    PROOF_SCOPES.join(", ")
                ));
            }
            if !is_app_id(app_id) {
                return Err(format!(
                    "`{app_id}` is not an app id (lowercase letters, digits, - and _, starting with a letter)"
                ));
            }
            issuers
                .entry(scope.to_owned())
                .or_default()
                .insert(app_id.to_owned());
        }
        Ok(Self(issuers))
    }
}

fn is_app_id(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

/// Versioned AES-256 keyring used for encrypted webhook destination secrets.
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

/// Generic outbound webhook-delivery policy.
#[derive(Clone, Debug)]
pub struct WebhookSettings {
    /// TCP/TLS connection establishment deadline.
    pub connect_timeout: Duration,
    /// End-to-end webhook request deadline.
    pub request_timeout: Duration,
    /// Maximum accepted webhook response body size.
    pub max_response_bytes: usize,
}

/// Scheduler and delivery worker coordination policy.
#[derive(Clone, Debug)]
pub struct WorkerSettings {
    /// Address on which the worker exposes operational HTTP endpoints.
    pub operational_bind_addr: SocketAddr,
    /// Maximum records claimed by one worker query.
    pub batch_size: NonZeroUsize,
    /// Maximum webhook deliveries attempted concurrently by one worker process.
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

    #[allow(
        clippy::too_many_lines,
        reason = "Typed environment settings are assembled in one place"
    )]
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
        let accounts = accounts_settings(source, environment)?;
        let encryption = encryption_settings(source)?;
        let webhook = WebhookSettings {
            connect_timeout: duration_millis(source, "REMIND_WEBHOOK_CONNECT_TIMEOUT_MS", 2_000)?,
            request_timeout: duration_millis(source, "REMIND_WEBHOOK_REQUEST_TIMEOUT_MS", 10_000)?,
            max_response_bytes: positive_size(source, "REMIND_WEBHOOK_MAX_RESPONSE_BYTES", 65_536)?,
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
            accounts,
            encryption,
            webhook,
            worker,
            telemetry_enabled: parse_or(source, "REMIND_TELEMETRY_ENABLED", "true")?,
            telemetry_table_key: optional(source, "REMIND_TELEMETRY_TABLE_KEY")
                .map(SecretString::from),
            telemetry_home: value_or(source, "REMIND_TELEMETRY_HOME", "/var/lib/remind/telemetry")
                .into(),
            postmark_server_token: optional(source, "REMIND_POSTMARK_SERVER_TOKEN")
                .map(SecretString::from),
            test_webhook_urls: value_or(source, "REMIND_TEST_WEBHOOK_URLS", "")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
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

#[cfg(test)]
impl Settings {
    /// Settings for in-process tests: development mode, telemetry off, the
    /// given Silicon Accounts origin and database.
    pub(crate) fn for_tests(accounts_url: &str, database_url: &str) -> Result<Self, SettingsError> {
        struct Values(BTreeMap<&'static str, String>);
        impl ConfigSource for Values {
            fn get(&self, name: &'static str) -> Option<String> {
                self.0.get(name).cloned()
            }
        }
        let key = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        Self::from_source(&Values(BTreeMap::from([
            ("REMIND_DATABASE_URL", database_url.to_owned()),
            ("ACCOUNTS_URL", accounts_url.to_owned()),
            ("REMIND_APP_SECRET", "sa_app_remind_test_secret".to_owned()),
            (
                "REMIND_ACCOUNTS_WEBHOOK_SECRET",
                crate::test_support::TEST_WEBHOOK_SECRET.to_owned(),
            ),
            (
                "REMIND_PROOF_ISSUERS",
                "remind.schedules.read=interface".to_owned(),
            ),
            ("REMIND_ENCRYPTION_CURRENT_VERSION", "1".to_owned()),
            ("REMIND_ENCRYPTION_KEYRING", format!(r#"{{"1":"{key}"}}"#)),
            ("REMIND_TELEMETRY_ENABLED", "false".to_owned()),
        ])))
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

fn accounts_settings(
    source: &impl ConfigSource,
    environment: RuntimeEnvironment,
) -> Result<AccountsSettings, SettingsError> {
    let url = accounts_origin(
        source,
        "ACCOUNTS_URL",
        Some(DEFAULT_ACCOUNTS_URL),
        environment,
    )?;
    let api_url = if optional(source, "ACCOUNTS_API_URL").is_some() {
        accounts_origin(source, "ACCOUNTS_API_URL", None, environment)?
    } else {
        url.clone()
    };
    let app_id = value_or(source, "REMIND_APP_ID", DEFAULT_APP_ID);
    if !is_app_id(&app_id) {
        return Err(invalid(
            "REMIND_APP_ID",
            "must be an app id: 1 to 80 lowercase letters, digits, - or _, starting with a letter",
        ));
    }
    let app_secret = required_secret(source, "REMIND_APP_SECRET")?;
    let webhook_secrets = accounts_webhook_secrets(source)?;
    let proof_issuers = ProofIssuers::parse(&value_or(source, "REMIND_PROOF_ISSUERS", ""))
        .map_err(|reason| invalid("REMIND_PROOF_ISSUERS", reason))?;
    if environment == RuntimeEnvironment::Production {
        validate_secret_strength("REMIND_APP_SECRET", &app_secret)?;
    }
    Ok(AccountsSettings {
        url,
        api_url,
        app_id,
        app_secret,
        webhook_secrets,
        request_timeout: duration_millis(source, "REMIND_ACCOUNTS_REQUEST_TIMEOUT_MS", 3_000)?,
        lookup_ttl: duration_secs(source, "REMIND_ACCOUNTS_LOOKUP_TTL_SECONDS", 900)?,
        proof_issuers,
    })
}

/// Parses a Silicon Accounts origin: https, or http only for this machine.
fn accounts_origin(
    source: &impl ConfigSource,
    name: &'static str,
    default: Option<&str>,
    environment: RuntimeEnvironment,
) -> Result<Url, SettingsError> {
    let raw = match default {
        Some(default) => value_or(source, name, default),
        None => required(source, name)?,
    };
    let url = Url::parse(&raw).map_err(|_| {
        invalid(
            name,
            "must be an absolute URL such as https://accounts.teamofsilicons.com",
        )
    })?;
    validate_http_url(name, &url)?;
    if !matches!(url.path(), "" | "/") {
        return Err(invalid(
            name,
            "must be an origin without a path, such as https://accounts.teamofsilicons.com",
        ));
    }
    if url.scheme() == "http" && !is_loopback(&url) {
        return Err(invalid(
            name,
            "may use http only for this machine (localhost, 127.0.0.1 or ::1); use https",
        ));
    }
    if environment == RuntimeEnvironment::Production {
        require_https(name, &url)?;
    }
    Ok(url)
}

/// Returns whether a URL names this machine.
#[must_use]
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost" || domain.ends_with(".localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

fn accounts_webhook_secrets(
    source: &impl ConfigSource,
) -> Result<Vec<SecretString>, SettingsError> {
    const NAME: &str = "REMIND_ACCOUNTS_WEBHOOK_SECRET";
    let raw = required(source, NAME)?;
    let secrets: Vec<SecretString> = raw
        .split(',')
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .map(|secret| SecretString::from(secret.to_owned()))
        .collect();
    if secrets.is_empty() {
        return Err(SettingsError::Missing(NAME));
    }
    if secrets.len() > 4 {
        return Err(invalid(
            NAME,
            "at most four secrets may overlap during a rotation",
        ));
    }
    if secrets.iter().any(|secret| {
        let value = secret.expose_secret();
        !value.starts_with("whsec_")
            || value.len() < 16
            || !value.bytes().all(|byte| byte.is_ascii_graphic())
    }) {
        return Err(invalid(
            NAME,
            "every secret must be the whsec_... value Silicon Accounts showed when Remind's webhook was set",
        ));
    }
    Ok(secrets)
}

/// Accounts settings for operator tooling that only looks accounts up
/// (`remind-migrate link-identities`). `None` when `REMIND_APP_SECRET` is
/// unset: the tool then works offline.
///
/// # Errors
///
/// Returns a redacted error for a malformed origin or app id.
pub fn lookup_settings_from_env(
    environment: RuntimeEnvironment,
) -> Result<Option<AccountsSettings>, SettingsError> {
    let source = ProcessEnvironment;
    if optional(&source, "REMIND_APP_SECRET").is_none() {
        return Ok(None);
    }
    let url = accounts_origin(
        &source,
        "ACCOUNTS_URL",
        Some(DEFAULT_ACCOUNTS_URL),
        environment,
    )?;
    let api_url = if optional(&source, "ACCOUNTS_API_URL").is_some() {
        accounts_origin(&source, "ACCOUNTS_API_URL", None, environment)?
    } else {
        url.clone()
    };
    let app_id = value_or(&source, "REMIND_APP_ID", DEFAULT_APP_ID);
    if !is_app_id(&app_id) {
        return Err(invalid("REMIND_APP_ID", "must be an app id"));
    }
    Ok(Some(AccountsSettings {
        url,
        api_url,
        app_id,
        app_secret: required_secret(&source, "REMIND_APP_SECRET")?,
        webhook_secrets: Vec::new(),
        request_timeout: duration_millis(&source, "REMIND_ACCOUNTS_REQUEST_TIMEOUT_MS", 3_000)?,
        lookup_ttl: Duration::from_mins(15),
        proof_issuers: ProofIssuers::default(),
    }))
}

/// Lists the retired IAM and Honeycomb variables still present in this process's environment.
#[must_use]
pub fn retired_variables_present() -> Vec<&'static str> {
    RETIRED_VARIABLES
        .iter()
        .copied()
        .filter(|name| env::var_os(name).is_some())
        .collect()
}

fn validate_cross_field_policy(settings: &Settings) -> Result<(), SettingsError> {
    let Settings {
        environment,
        server,
        database,
        webhook,
        worker,
        retry,
        retention,
        ..
    } = settings;
    validate_http_url("REMIND_PUBLIC_BASE_URL", &server.public_base_url)?;

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
    let minimum_lease = webhook
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
            "must exceed the webhook timeout, two database operation budgets, the poll interval, and a five-second safety margin",
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

    use super::{
        ConfigSource, MigrationSettings, ProofIssuers, RuntimeEnvironment, Settings, SettingsError,
    };

    struct MapSource(BTreeMap<&'static str, String>);

    impl ConfigSource for MapSource {
        fn get(&self, name: &'static str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    fn valid_source() -> MapSource {
        let key = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        MapSource(BTreeMap::from([
            (
                "REMIND_DATABASE_URL",
                "postgres://remind:remind@127.0.0.1:5432/remind".to_owned(),
            ),
            (
                "REMIND_APP_SECRET",
                "development-accounts-app-secret".to_owned(),
            ),
            (
                "REMIND_ACCOUNTS_WEBHOOK_SECRET",
                "whsec_development-webhook-secret".to_owned(),
            ),
            ("REMIND_ENCRYPTION_CURRENT_VERSION", "1".to_owned()),
            ("REMIND_ENCRYPTION_KEYRING", format!(r#"{{"1":"{key}"}}"#)),
        ]))
    }

    fn rejected(source: &MapSource) -> Option<(&'static str, String)> {
        match Settings::from_source(source) {
            Err(SettingsError::Invalid { name, reason }) => Some((name, reason)),
            Err(SettingsError::Missing(name)) => Some((name, "missing".to_owned())),
            Ok(_) => None,
        }
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

        assert_eq!(
            settings.accounts.url.as_str(),
            "https://accounts.teamofsilicons.com/"
        );
        assert_eq!(settings.accounts.api_url, settings.accounts.url);
        assert_eq!(settings.accounts.app_id, "remind");
        assert_eq!(settings.accounts.webhook_secrets.len(), 1);
        assert!(settings.accounts.proof_issuers.is_empty());

        let debug = format!("{settings:?}");
        assert!(!debug.contains("development-accounts-app-secret"));
        assert!(!debug.contains("whsec_development-webhook-secret"));
        Ok(())
    }

    #[test]
    fn accounts_app_secret_and_webhook_secret_are_required() {
        for name in ["REMIND_APP_SECRET", "REMIND_ACCOUNTS_WEBHOOK_SECRET"] {
            let mut source = valid_source();
            source.0.remove(name);
            assert!(matches!(
                Settings::from_source(&source),
                Err(SettingsError::Missing(missing)) if missing == name
            ));
        }
    }

    #[test]
    fn accounts_urls_allow_http_only_for_this_machine() -> Result<(), SettingsError> {
        for local in [
            "http://localhost:9590",
            "http://127.0.0.1:9589",
            "http://[::1]:9590",
        ] {
            let mut source = valid_source();
            source.0.insert("ACCOUNTS_URL", local.to_owned());
            let settings = Settings::from_source(&source)?;
            assert_eq!(settings.accounts.url.scheme(), "http");
        }
        let mut source = valid_source();
        source
            .0
            .insert("ACCOUNTS_URL", "http://accounts.example".to_owned());
        assert_eq!(
            rejected(&source),
            Some((
                "ACCOUNTS_URL",
                "may use http only for this machine (localhost, 127.0.0.1 or ::1); use https"
                    .to_owned()
            ))
        );
        let mut source = valid_source();
        source
            .0
            .insert("ACCOUNTS_URL", "https://accounts.example/v1".to_owned());
        assert_eq!(
            rejected(&source).map(|(name, _)| name),
            Some("ACCOUNTS_URL")
        );

        let mut source = valid_source();
        source
            .0
            .insert("ACCOUNTS_URL", "http://localhost:9590".to_owned());
        source
            .0
            .insert("ACCOUNTS_API_URL", "http://127.0.0.1:9589".to_owned());
        let settings = Settings::from_source(&source)?;
        assert_eq!(settings.accounts.api_url.as_str(), "http://127.0.0.1:9589/");
        Ok(())
    }

    #[test]
    fn webhook_secrets_need_the_whsec_prefix_and_may_overlap() -> Result<(), SettingsError> {
        let mut source = valid_source();
        source.0.insert(
            "REMIND_ACCOUNTS_WEBHOOK_SECRET",
            "whsec_old-secret-value-1, whsec_new-secret-value-2".to_owned(),
        );
        assert_eq!(
            Settings::from_source(&source)?
                .accounts
                .webhook_secrets
                .len(),
            2
        );
        source.0.insert(
            "REMIND_ACCOUNTS_WEBHOOK_SECRET",
            "whs_legacy_iam_key_value".to_owned(),
        );
        assert_eq!(
            rejected(&source).map(|(name, _)| name),
            Some("REMIND_ACCOUNTS_WEBHOOK_SECRET")
        );
        Ok(())
    }

    #[test]
    fn proof_issuers_are_parsed_per_scope() {
        let issuers =
            ProofIssuers::parse("remind.schedules.read=interface, remind.schedules.read=glass");
        assert!(issuers.as_ref().is_ok_and(|issuers| {
            issuers.allows("remind.schedules.read", "interface")
                && issuers.allows("remind.schedules.read", "glass")
                && !issuers.allows("remind.schedules.read", "dm")
                && !issuers.allows("remind.schedules.write", "interface")
        }));
        assert!(ProofIssuers::parse("").is_ok_and(|issuers| issuers.is_empty()));
        assert_eq!(
            ProofIssuers::parse("remind.schedules.write=interface"),
            Err("Remind honours no proof scope `remind.schedules.write`; the scopes it honours are: remind.schedules.read".to_owned())
        );
        assert!(ProofIssuers::parse("interface").is_err());
        assert!(ProofIssuers::parse("remind.schedules.read=Interface!").is_err());
    }

    #[test]
    fn app_id_defaults_to_remind_and_is_validated() {
        let mut source = valid_source();
        source.0.insert("REMIND_APP_ID", "Remind App".to_owned());
        assert_eq!(
            rejected(&source).map(|(name, _)| name),
            Some("REMIND_APP_ID")
        );
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
    fn production_settings_do_not_require_a_hook_service() {
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
            "REMIND_APP_SECRET",
            "a-production-accounts-secret-that-is-long-enough".to_owned(),
        );
        assert!(Settings::from_source(&source).is_ok());

        source
            .0
            .insert("REMIND_APP_SECRET", "short-secret".to_owned());
        assert_eq!(
            rejected(&source).map(|(name, _)| name),
            Some("REMIND_APP_SECRET")
        );
        source.0.insert(
            "REMIND_APP_SECRET",
            "a-production-accounts-secret-that-is-long-enough".to_owned(),
        );
        source
            .0
            .insert("ACCOUNTS_URL", "http://localhost:9590".to_owned());
        assert_eq!(
            rejected(&source),
            Some(("ACCOUNTS_URL", "must use https in production".to_owned()))
        );
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
