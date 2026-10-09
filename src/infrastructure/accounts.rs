//! Silicon Accounts through the official `silicon-accounts-client`.
//!
//! Access tokens are verified locally against a cached JWKS (refetched, at most
//! every 30 seconds, when a token names an unknown key). Introspection, User
//! verification proofs and account lookups go to Silicon Accounts with Remind's
//! app credentials and are cached briefly; lookups also stay inside Remind's
//! own budget, below the 600 a minute Silicon Accounts allows per app.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use secrecy::{ExposeSecret as _, SecretString};
use sha2::{Digest as _, Sha256};
use silicon_accounts_client::{
    AccountSummary, AccountsClient, Claims, Error as ClientError, Jwks, ProofVerification,
    TokenError, ValidProof, VerifyOptions,
};
use tokio::sync::{Mutex, RwLock};

use crate::config::AccountsSettings;

/// A JWKS older than this is fetched again before the next verification.
const JWKS_MAX_AGE: Duration = Duration::from_secs(3_600);
/// A token naming an unknown key refetches the JWKS at most this often.
const JWKS_REFETCH_INTERVAL: Duration = Duration::from_secs(30);
/// Introspection answers are reused for at most this long.
const INTROSPECTION_TTL: Duration = Duration::from_secs(30);
/// Proof verifications are reused for at most this long, and never past expiry.
const PROOF_TTL: Duration = Duration::from_secs(30);
/// Lookups Remind allows itself per minute (Silicon Accounts allows 600).
const LOOKUP_BUDGET_PER_MINUTE: usize = 500;
/// A failed lookup of one account is not repeated sooner than this.
const LOOKUP_FAILURE_BACKOFF: Duration = Duration::from_secs(60);
/// Upper bound on every in-memory cache.
const MAX_CACHE_ENTRIES: usize = 10_000;

/// Why a Silicon Accounts operation did not produce an answer.
#[derive(Debug, thiserror::Error)]
pub enum AccountsError {
    /// The credential is invalid; `message` says exactly why.
    #[error("{code}: {message}")]
    Rejected {
        /// Stable machine-readable code (for example `token_expired`).
        code: &'static str,
        /// What is wrong and what to do.
        message: String,
    },
    /// Silicon Accounts could not be reached or answered unexpectedly.
    #[error("Silicon Accounts is unavailable: {0}")]
    Unavailable(String),
    /// Remind's lookup budget for this minute is spent.
    #[error("the Silicon Accounts lookup budget for this minute is spent")]
    RateLimited,
}

/// Cheap-to-clone handle on Silicon Accounts for one app (Remind).
#[derive(Clone)]
pub struct AccountsGateway {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for AccountsGateway {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountsGateway")
            .field("app_id", &self.inner.app_id)
            .field("issuer", &self.inner.issuer)
            .finish_non_exhaustive()
    }
}

struct Inner {
    client: AccountsClient,
    app_id: String,
    app_secret: SecretString,
    issuer: String,
    verify: VerifyOptions,
    jwks: RwLock<Option<CachedJwks>>,
    jwks_fetch: Mutex<Option<Instant>>,
    introspections: Mutex<HashMap<[u8; 32], (bool, Instant)>>,
    proofs: Mutex<ProofCache>,
    lookups: Mutex<LookupState>,
}

/// Proof verifications by token digest, with the instant they stop being reused.
type ProofCache = HashMap<[u8; 32], (Option<Arc<ValidProof>>, Instant)>;

struct CachedJwks {
    jwks: Arc<Jwks>,
    fetched_at: Instant,
}

#[derive(Default)]
struct LookupState {
    window: VecDeque<Instant>,
    failures: HashMap<String, Instant>,
}

impl AccountsGateway {
    /// Builds the gateway from validated settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot be built for the configured origin.
    pub fn new(settings: &AccountsSettings) -> anyhow::Result<Self> {
        let client = AccountsClient::builder()
            .base_url(settings.api_url.as_str().trim_end_matches('/'))
            .timeout(settings.request_timeout)
            .connect_timeout(settings.request_timeout)
            .user_agent(concat!("silicon-remind/", env!("CARGO_PKG_VERSION")))
            .telemetry(false)
            .max_retries(1)
            .build()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let issuer = settings.url.as_str().trim_end_matches('/').to_owned();
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                app_id: settings.app_id.clone(),
                app_secret: settings.app_secret.clone(),
                verify: VerifyOptions::for_app(&settings.app_id).with_issuer(issuer.clone()),
                issuer,
                jwks: RwLock::new(None),
                jwks_fetch: Mutex::new(None),
                introspections: Mutex::new(HashMap::new()),
                proofs: Mutex::new(HashMap::new()),
                lookups: Mutex::new(LookupState::default()),
            }),
        })
    }

    /// Remind's app id.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.inner.app_id
    }

    /// The issuer access tokens must carry (the public Silicon Accounts origin).
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.inner.issuer
    }

    /// Verifies an access token locally: `EdDSA` signature by a published key,
    /// `exp`/`nbf`, `aud` = Remind's app id and `iss` = the configured origin.
    ///
    /// # Errors
    ///
    /// Returns [`AccountsError::Rejected`] with a precise reason for an invalid
    /// token, or [`AccountsError::Unavailable`] when no JWKS could be fetched.
    pub async fn verify_access_token(&self, token: &str) -> Result<Claims, AccountsError> {
        let jwks = self.jwks(false).await?;
        match silicon_accounts_client::verify_access_token(&jwks, token, &self.inner.verify) {
            Ok(claims) => Ok(claims),
            Err(ClientError::Token(TokenError::UnknownKey { .. })) => {
                let refreshed = self.jwks(true).await?;
                silicon_accounts_client::verify_access_token(&refreshed, token, &self.inner.verify)
                    .map_err(|error| self.rejected(&error))
            }
            Err(error) => Err(self.rejected(&error)),
        }
    }

    /// Asks Silicon Accounts whether an access token is still active (revocation
    /// shows here at once, unlike local verification).
    ///
    /// # Errors
    ///
    /// Returns [`AccountsError::Unavailable`] when Silicon Accounts cannot answer.
    pub async fn token_is_active(&self, token: &str) -> Result<bool, AccountsError> {
        let key = digest(token);
        if let Some((active, at)) = self.inner.introspections.lock().await.get(&key)
            && at.elapsed() < INTROSPECTION_TTL
        {
            return Ok(*active);
        }
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        let introspection = app
            .introspect(token)
            .await
            .map_err(|error| unavailable(&error))?;
        let audience_matches = introspection
            .client_id
            .as_deref()
            .is_none_or(|client_id| client_id == self.inner.app_id);
        let active = introspection.active && audience_matches;
        let mut cache = self.inner.introspections.lock().await;
        bound(&mut cache, |(_, at)| at.elapsed() < INTROSPECTION_TTL);
        cache.insert(key, (active, Instant::now()));
        Ok(active)
    }

    /// Verifies a proof sent to Remind (`POST /v1/proofs/verify`). `None` means
    /// the proof is not valid for Remind right now.
    ///
    /// # Errors
    ///
    /// Returns [`AccountsError::Unavailable`] when Silicon Accounts cannot answer.
    pub async fn verify_proof(
        &self,
        token: &str,
    ) -> Result<Option<Arc<ValidProof>>, AccountsError> {
        let key = digest(token);
        if let Some((proof, until)) = self.inner.proofs.lock().await.get(&key)
            && Instant::now() < *until
        {
            return Ok(proof.clone());
        }
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        let verification = app
            .verify_proof(token)
            .await
            .map_err(|error| unavailable(&error))?;
        let (proof, until) = match verification {
            ProofVerification::Valid(proof) => {
                let remaining = proof.expires_at.unix_timestamp() - unix_now();
                let ttl = Duration::from_secs(u64::try_from(remaining.max(0)).unwrap_or(0))
                    .min(PROOF_TTL);
                (Some(Arc::new(*proof)), Instant::now() + ttl)
            }
            _ => (None, Instant::now() + PROOF_TTL),
        };
        let mut cache = self.inner.proofs.lock().await;
        bound(&mut cache, |(_, until)| Instant::now() < *until);
        cache.insert(key, (proof.clone(), until));
        Ok(proof)
    }

    /// Looks an account up by uuid (`GET /v1/accounts/{uuid}`). `None` for 404.
    ///
    /// # Errors
    ///
    /// Returns [`AccountsError::RateLimited`] when Remind's budget is spent or
    /// [`AccountsError::Unavailable`] when Silicon Accounts cannot answer.
    pub async fn lookup(&self, uuid: &str) -> Result<Option<AccountSummary>, AccountsError> {
        self.lookup_with(uuid, false).await
    }

    /// Looks an account up by its current public id (`GET /v1/accounts/by-id/{id}`).
    /// `None` when no account has that id now.
    ///
    /// # Errors
    ///
    /// Returns [`AccountsError::RateLimited`] or [`AccountsError::Unavailable`].
    pub async fn lookup_by_id(&self, id: &str) -> Result<Option<AccountSummary>, AccountsError> {
        self.lookup_with(id, true).await
    }

    async fn lookup_with(
        &self,
        key: &str,
        by_id: bool,
    ) -> Result<Option<AccountSummary>, AccountsError> {
        {
            let mut state = self.inner.lookups.lock().await;
            let now = Instant::now();
            if state
                .failures
                .get(key)
                .is_some_and(|failed_at| now.duration_since(*failed_at) < LOOKUP_FAILURE_BACKOFF)
            {
                return Err(AccountsError::Unavailable(format!(
                    "the last lookup of {key} failed less than a minute ago"
                )));
            }
            while state
                .window
                .front()
                .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
            {
                state.window.pop_front();
            }
            if state.window.len() >= LOOKUP_BUDGET_PER_MINUTE {
                return Err(AccountsError::RateLimited);
            }
            state.window.push_back(now);
        }
        let app = self
            .inner
            .client
            .as_app(&self.inner.app_id, self.inner.app_secret.expose_secret());
        let result = if by_id {
            app.lookup_by_id(key).await
        } else {
            app.lookup(key).await
        };
        match result {
            Ok(summary) => Ok(Some(summary)),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => {
                let mut state = self.inner.lookups.lock().await;
                bound(&mut state.failures, |failed_at| {
                    failed_at.elapsed() < LOOKUP_FAILURE_BACKOFF
                });
                state.failures.insert(key.to_owned(), Instant::now());
                Err(unavailable(&error))
            }
        }
    }

    async fn jwks(&self, unknown_key: bool) -> Result<Arc<Jwks>, AccountsError> {
        if !unknown_key
            && let Some(cached) = self.inner.jwks.read().await.as_ref()
            && cached.fetched_at.elapsed() < JWKS_MAX_AGE
        {
            return Ok(cached.jwks.clone());
        }
        let mut last_fetch = self.inner.jwks_fetch.lock().await;
        if let Some(cached) = self.inner.jwks.read().await.as_ref() {
            let refetched_recently =
                last_fetch.is_some_and(|at| at.elapsed() < JWKS_REFETCH_INTERVAL);
            let fresh = cached.fetched_at.elapsed() < JWKS_MAX_AGE;
            if (unknown_key && refetched_recently) || (!unknown_key && fresh) {
                return Ok(cached.jwks.clone());
            }
        }
        *last_fetch = Some(Instant::now());
        match self.inner.client.jwks().await {
            Ok(jwks) => {
                let jwks = Arc::new(jwks);
                *self.inner.jwks.write().await = Some(CachedJwks {
                    jwks: jwks.clone(),
                    fetched_at: Instant::now(),
                });
                Ok(jwks)
            }
            Err(error) => {
                if let Some(cached) = self.inner.jwks.read().await.as_ref() {
                    tracing::warn!(
                        error.code = error.code(),
                        "Silicon Accounts JWKS refresh failed; keeping the keys already fetched"
                    );
                    return Ok(cached.jwks.clone());
                }
                Err(unavailable(&error))
            }
        }
    }

    fn rejected(&self, error: &ClientError) -> AccountsError {
        let ClientError::Token(token) = error else {
            return AccountsError::Rejected {
                code: "token_invalid",
                message: format!(
                    "The access token could not be verified: {}",
                    error.message()
                ),
            };
        };
        let message = match token {
            TokenError::UnknownKey { .. } => "The access token was signed with a key Silicon Accounts does not publish. Sign in again to get a new token.".to_owned(),
            TokenError::Expired { .. } => "The access token expired; access tokens live 30 minutes. Refresh it with the refresh token and retry.".to_owned(),
            TokenError::WrongAudience { .. } => format!(
                "The access token was issued to a different app; Remind accepts only access tokens issued to `{}`. An app acting for an account must send a User verification proof instead.",
                self.inner.app_id
            ),
            TokenError::WrongIssuer { .. } => format!(
                "The access token was not issued by {}, the Silicon Accounts instance Remind trusts.",
                self.inner.issuer
            ),
            other => other.message(),
        };
        AccountsError::Rejected {
            code: token.code(),
            message,
        }
    }
}

fn unavailable(error: &ClientError) -> AccountsError {
    AccountsError::Unavailable(format!("{} ({})", error.message(), error.code()))
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.trim().as_bytes()).into()
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Keeps a cache below its bound: drops stale entries first, then everything.
fn bound<K, V>(cache: &mut HashMap<K, V>, keep: impl Fn(&V) -> bool) {
    if cache.len() >= MAX_CACHE_ENTRIES {
        cache.retain(|_, value| keep(value));
        if cache.len() >= MAX_CACHE_ENTRIES {
            cache.clear();
        }
    }
}
