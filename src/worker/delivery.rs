//! Concurrent lease-based configured webhook receiver delivery and retry policy.

use std::{sync::Arc, time::Duration};

use secrecy::ExposeSecret as _;
use sha2::{Digest as _, Sha256};
use tokio::task::JoinSet;
use url::Url;
use uuid::Uuid;

use crate::{
    application::ports::Clock,
    config::RetrySettings,
    infrastructure::{
        crypto::{EncryptedSecret, SecretCipherKeyring, stored_destination_associated_data},
        identity::IdentityStore,
        postgres::{ExecutionRow, HookDestinationRow, PostgresRepository},
        webhook::{
            ReminderEvent, WebhookClient, WebhookDeliveryError, WebhookDestination,
            destination_url_is_allowed,
        },
    },
    metrics::Metrics,
};

/// Claims and concurrently delivers one bounded execution batch.
#[derive(Clone, Debug)]
pub struct DeliveryProcessor {
    repository: PostgresRepository,
    identity: IdentityStore,
    webhook_client: WebhookClient,
    ting: Option<crate::infrastructure::ting::Ting>,
    encryption: SecretCipherKeyring,
    worker_id: String,
    lease_duration: Duration,
    max_concurrency: u32,
    retry: RetrySettings,
    clock: Arc<dyn Clock>,
    metrics: Metrics,
    production: bool,
    testing: bool,
    test_webhook_urls: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct DeliveryProcessorConfig {
    pub(crate) production: bool,
    pub(crate) worker_id: String,
    pub(crate) lease_duration: Duration,
    pub(crate) max_concurrency: u32,
    pub(crate) retry: RetrySettings,
}

impl DeliveryProcessor {
    /// Creates a delivery stage from validated process settings.
    #[must_use]
    pub(crate) fn new(
        repository: PostgresRepository,
        identity: IdentityStore,
        webhook_client: WebhookClient,
        encryption: SecretCipherKeyring,
        config: DeliveryProcessorConfig,
        clock: Arc<dyn Clock>,
        metrics: Metrics,
    ) -> Self {
        Self {
            repository,
            identity,
            webhook_client,
            ting: None,
            encryption,
            worker_id: config.worker_id,
            lease_duration: config.lease_duration,
            max_concurrency: config.max_concurrency,
            retry: config.retry,
            clock,
            metrics,
            production: config.production,
            testing: false,
            test_webhook_urls: Vec::new(),
        }
    }

    pub(crate) fn with_ting(mut self, ting: Option<crate::infrastructure::ting::Ting>) -> Self {
        self.ting = ting;
        self
    }

    pub(crate) fn with_test_destinations(mut self, urls: Vec<String>) -> Self {
        self.test_webhook_urls = urls;
        self
    }

    /// Reuses delivery policy against a test environment's repository.
    pub(crate) fn with_repository(&self, repository: PostgresRepository) -> Self {
        let mut delivery = self.clone();
        delivery.repository = repository;
        delivery.testing = true;
        delivery
    }

    /// Returns this process's maximum simultaneous outbound deliveries.
    #[must_use]
    pub(crate) const fn max_concurrency(&self) -> u32 {
        self.max_concurrency
    }

    /// Claims and drains one delivery batch, awaiting every acquired lease.
    ///
    /// # Errors
    ///
    /// Returns the first persistence/task error after all claimed executions
    /// have been processed, avoiding abandoned leases within this process.
    pub async fn run_once(&self) -> anyhow::Result<usize> {
        let executions = self
            .repository
            .claim_deliveries(
                &self.worker_id,
                self.clock.now(),
                self.lease_duration,
                self.max_concurrency,
            )
            .await?;
        let claimed = executions.len();
        let mut tasks = JoinSet::new();
        for execution in executions {
            let processor = self.clone();
            tasks.spawn(async move { processor.deliver_one(execution).await });
        }

        let mut first_error = None;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error.into());
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(claimed),
        }
    }

    async fn deliver_one(&self, execution: ExecutionRow) -> anyhow::Result<()> {
        let (key, owner) = match self.resolve_owner(&execution).await {
            Ok(resolved) => resolved,
            Err(error) => return self.finish_failure(&execution, error).await,
        };
        let attempted_at = self.clock.now();
        if !self
            .repository
            .delivery_lease_is_live(execution.id, &self.worker_id, attempted_at)
            .await?
        {
            return Ok(());
        }
        let event = ReminderEvent {
            execution_id: execution.id,
            schedule_id: execution.schedule_id,
            silicon_id: owner
                .as_ref()
                .map(|account| &account.id)
                .filter(|id| !id.is_empty())
                .unwrap_or(&execution.silicon_id)
                .clone(),
            silicon_uuid: owner.map(|account| account.uuid),
            text: execution.text.clone(),
            scheduled_for: execution.scheduled_for,
            timezone: execution.timezone.clone(),
        };
        let (ting, hooks) = tokio::join!(
            async {
                if execution.ting_status == "pending" {
                    self.deliver_ting(&event).await
                } else {
                    Ok(execution.ting_status.as_str())
                }
            },
            async {
                if execution.webhook_status == "pending" {
                    self.deliver_webhooks(key, &event, attempted_at).await
                } else {
                    Ok(execution.webhook_status.as_str())
                }
            }
        );
        self.finish_channels(&execution, ting, hooks).await
    }

    async fn finish_channels(
        &self,
        execution: &ExecutionRow,
        ting: Result<&str, WebhookDeliveryError>,
        hooks: Result<&str, WebhookDeliveryError>,
    ) -> anyhow::Result<()> {
        let last_attempt = execution.attempt_count >= i32::from(self.retry.max_attempts);
        let ting_status = channel_status(&ting, last_attempt);
        let webhook_status = channel_status(&hooks, last_attempt);
        self.repository
            .record_channels(
                execution.id,
                &self.worker_id,
                ting_status,
                webhook_status,
                self.clock.now(),
            )
            .await?;

        // A terminal failure in one channel must never suppress retries for the other.
        if ting_status == "pending" || webhook_status == "pending" {
            let reason = [&ting, &hooks]
                .into_iter()
                .filter_map(|result| result.as_ref().err())
                .find(|error| error.is_retryable())
                .map_or("Delivery is pending", WebhookDeliveryError::reason);
            return self
                .finish_failure(
                    execution,
                    WebhookDeliveryError::Retryable {
                        reason: reason.to_owned(),
                    },
                )
                .await;
        }
        if ting_status == "failed" || webhook_status == "failed" {
            let reason = ting
                .as_ref()
                .err()
                .or(hooks.as_ref().err())
                .map_or("A delivery channel failed", WebhookDeliveryError::reason);
            return self
                .finish_failure(
                    execution,
                    WebhookDeliveryError::Terminal {
                        reason: reason.to_owned(),
                    },
                )
                .await;
        }
        if ting_status == "unsubscribed" && webhook_status == "none" {
            return self
                .finish_failure(
                    execution,
                    terminal("Ting has no Remind subscription and there are no webhooks"),
                )
                .await;
        }
        self.repository
            .mark_delivery_succeeded(
                execution.id,
                &self.worker_id,
                (webhook_status == "delivered").then_some(execution.id),
                self.clock.now(),
            )
            .await?;
        self.metrics.deliveries_succeeded.inc();
        Ok(())
    }

    async fn deliver_ting(
        &self,
        event: &ReminderEvent,
    ) -> Result<&'static str, WebhookDeliveryError> {
        if self.testing {
            return Ok("simulated");
        }
        let Some(ting) = &self.ting else {
            return Ok("disabled");
        };
        let Some(recipient) = &event.silicon_uuid else {
            return Ok("unsubscribed");
        };
        let result = ting.send(recipient, event).await?;
        if result == "unsubscribed" {
            let enrolled: bool = sqlx::query_scalar(
                "SELECT ting_registered_at IS NOT NULL FROM accounts WHERE uuid=$1",
            )
            .bind(recipient)
            .fetch_one(self.identity.pool())
            .await
            .map_err(|_| retryable("Ting registration lookup failed"))?;
            if !enrolled {
                return Err(retryable(
                    "Ting is waiting for the Silicon to open Remind or enable notifications",
                ));
            }
        }
        Ok(result)
    }

    async fn deliver_webhooks(
        &self,
        key: Uuid,
        event: &ReminderEvent,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<&'static str, WebhookDeliveryError> {
        let rows = self
            .repository
            .destinations_for_owner_account(key)
            .await
            .map_err(|_| retryable("Webhook destination lookup failed"))?;
        if rows.is_empty() {
            return Ok("none");
        }
        let mut failure = None;
        let mut delivered = false;
        for row in &rows {
            let result: Result<(), WebhookDeliveryError> = async {
                let destination = self.decrypt_destination(row)?;
                if self.testing
                    && !self
                        .test_webhook_urls
                        .iter()
                        .any(|url| url == destination.endpoint_url.as_str())
                {
                    return Ok(());
                }
                self.webhook_client.deliver(&destination, event, at).await?;
                delivered = true;
                Ok(())
            }
            .await;
            if let Err(error) = result
                && (failure.is_none() || !error.is_retryable())
            {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(if delivered { "delivered" } else { "simulated" })
    }

    /// Resolve the stable recipient and check production lifecycle state even for a sandbox copy.
    async fn resolve_owner(
        &self,
        execution: &ExecutionRow,
    ) -> Result<(Uuid, Option<crate::domain::AccountRef>), WebhookDeliveryError> {
        let key = self
            .repository
            .execution_owner_key(execution.id)
            .await
            .map_err(|_| retryable("Reminder owner lookup failed"))?;
        let owner = self
            .identity
            .owners_by_keys(&[key])
            .await
            .map_err(|_| retryable("Reminder owner lookup failed"))?
            .remove(&key);
        if let Some(owner) = &owner
            && self
                .identity
                .find(&owner.uuid)
                .await
                .map_err(|_| retryable("Reminder owner lookup failed"))?
                .is_none_or(|row| row.status != "active")
        {
            return Err(terminal("Reminder owner is no longer active"));
        }
        Ok((key, owner))
    }

    fn decrypt_destination(
        &self,
        row: &HookDestinationRow,
    ) -> Result<WebhookDestination, WebhookDeliveryError> {
        let key_version = row.encryption_key_version;
        let url = EncryptedSecret {
            key_version,
            nonce: row.endpoint_url_nonce.clone(),
            ciphertext: row.endpoint_url_ciphertext.clone(),
        };
        let secret = EncryptedSecret {
            key_version,
            nonce: row.signing_secret_nonce.clone(),
            ciphertext: row.signing_secret_ciphertext.clone(),
        };
        let aad = |field| {
            stored_destination_associated_data(
                row.aad_version,
                row.org_id.as_deref(),
                &row.silicon_id,
                row.id,
                row.owner_principal_id,
                field,
            )
            .map_err(|_| terminal("Webhook destination has unusable associated data"))
        };
        let url_aad = aad("endpoint_url")?;
        let secret_aad = aad("signing_secret")?;
        let endpoint_url = self
            .encryption
            .decrypt(&url, &url_aad)
            .map_err(|_| retryable("Webhook destination URL could not be decrypted"))?;
        let signing_secret = self
            .encryption
            .decrypt(&secret, &secret_aad)
            .map_err(|_| retryable("Webhook signing credential could not be decrypted"))?;
        let endpoint_url = Url::parse(endpoint_url.expose_secret())
            .map_err(|_| terminal("Webhook destination URL is malformed"))?;
        if !destination_url_is_allowed(&endpoint_url, self.production && !self.testing) {
            return Err(terminal("Webhook destination URL is not allowed"));
        }
        Ok(WebhookDestination {
            endpoint_url,
            signing_secret,
        })
    }

    async fn finish_failure(
        &self,
        execution: &ExecutionRow,
        error: WebhookDeliveryError,
    ) -> anyhow::Result<()> {
        let failed_at = self.clock.now();
        let attempt = u16::try_from(execution.attempt_count).unwrap_or(u16::MAX);
        if error.is_retryable() && attempt < self.retry.max_attempts {
            let delay = retry_delay(&self.retry, execution.id, attempt);
            let delay = chrono::Duration::from_std(delay)?;
            let next_attempt_at = failed_at
                .checked_add_signed(delay)
                .ok_or_else(|| anyhow::anyhow!("retry timestamp overflow"))?;
            self.repository
                .mark_delivery_retrying(
                    execution.id,
                    &self.worker_id,
                    failed_at,
                    next_attempt_at,
                    error.reason(),
                )
                .await?;
            self.metrics.deliveries_retried.inc();
        } else {
            self.repository
                .mark_delivery_failed(execution.id, &self.worker_id, failed_at, error.reason())
                .await?;
            self.metrics.deliveries_failed.inc();
        }
        Ok(())
    }
}

/// Calculates capped exponential delay with deterministic execution jitter.
#[must_use]
pub fn retry_delay(policy: &RetrySettings, execution_id: Uuid, attempt: u16) -> Duration {
    let exponent = u32::from(attempt.saturating_sub(1).min(31));
    let multiplier = 1_u128 << exponent;
    let maximum_ms = policy.max_delay.as_millis();
    let base_ms = policy.base_delay.as_millis();
    let delay_ms = base_ms.saturating_mul(multiplier).min(maximum_ms);
    let jitter_window = delay_ms.saturating_mul(u128::from(policy.jitter_percent)) / 100;
    let mut hasher = Sha256::new();
    hasher.update(execution_id.as_bytes());
    hasher.update(attempt.to_be_bytes());
    let digest = hasher.finalize();
    let entropy = u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]);
    let jitter = if jitter_window == 0 {
        0
    } else {
        u128::from(entropy) % jitter_window.saturating_add(1)
    };
    let total_ms = delay_ms.saturating_add(jitter).min(maximum_ms);
    Duration::from_millis(u64::try_from(total_ms).unwrap_or(u64::MAX))
}

fn channel_status<'a>(
    result: &Result<&'a str, WebhookDeliveryError>,
    last_attempt: bool,
) -> &'a str {
    match result {
        Ok(status) => status,
        Err(error) if error.is_retryable() && !last_attempt => "pending",
        Err(_) => "failed",
    }
}

fn retryable(reason: &'static str) -> WebhookDeliveryError {
    WebhookDeliveryError::Retryable {
        reason: reason.to_owned(),
    }
}

fn terminal(reason: &'static str) -> WebhookDeliveryError {
    WebhookDeliveryError::Terminal {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use chrono::Utc;
    use secrecy::SecretString;
    use uuid::Uuid;

    use crate::{
        application::ports::SystemClock,
        config::RetrySettings,
        infrastructure::{
            crypto::{
                SecretCipherKeyring, destination_field_associated_data,
                destination_field_associated_data_v2,
            },
            postgres::{HookDestinationRow, PostgresRepository},
            webhook::{WebhookClient, WebhookDeliveryError},
        },
        metrics::Metrics,
    };

    use super::{DeliveryProcessor, DeliveryProcessorConfig, retry_delay};

    #[tokio::test]
    async fn a_test_environment_cannot_deliver_for_an_owner_removed_in_production()
    -> anyhow::Result<()> {
        let production_db = crate::test_support::TestPostgres::start().await?;
        let environment_db = crate::test_support::TestPostgres::start().await?;
        let production_pool = production_db.pool(2).await?;
        let environment_pool = environment_db.pool(2).await?;
        let key = Uuid::now_v7();
        for pool in [&production_pool, &environment_pool] {
            crate::infrastructure::postgres::migrate(pool).await?;
            sqlx::query("INSERT INTO accounts(uuid,kind,public_id,status,looked_up_at) VALUES('Owner','silicon','si:owner','active',clock_timestamp())").execute(pool).await?;
            sqlx::query("INSERT INTO account_keys(storage_id,account_uuid,origin) VALUES($1,'Owner','accounts')").bind(key).execute(pool).await?;
        }
        let schedule = Uuid::now_v7();
        let execution_id = Uuid::now_v7();
        sqlx::query("INSERT INTO schedules(id,org_id,owner_principal_id,silicon_id,reminder_text,schedule_kind,cron_expression) VALUES($1,NULL,$2,'si:owner','fixture','one_time','* * * * *')")
            .bind(schedule).bind(key).execute(&environment_pool).await?;
        sqlx::query("INSERT INTO executions(id,schedule_id,org_id,silicon_id,schedule_version,schedule_kind,scheduled_for,reminder_text,timezone,next_attempt_at) VALUES($1,$2,NULL,'si:owner',1,'one_time',clock_timestamp(),'fixture','UTC',clock_timestamp())")
            .bind(execution_id).bind(schedule).execute(&environment_pool).await?;
        let execution =
            sqlx::query_as("SELECT *, reminder_text AS text FROM executions WHERE id=$1")
                .bind(execution_id)
                .fetch_one(&environment_pool)
                .await?;
        let encryption = SecretCipherKeyring::from_base64url(
            1,
            &BTreeMap::from([(1, SecretString::from(URL_SAFE_NO_PAD.encode([7; 32])))]),
        )?;
        let processor = DeliveryProcessor::new(
            PostgresRepository::new(production_pool.clone()),
            crate::test_support::identity_store(production_pool.clone(), "http://127.0.0.1:9")?,
            WebhookClient::new(Duration::from_secs(1), Duration::from_secs(1), 1024)?,
            encryption,
            DeliveryProcessorConfig {
                production: false,
                worker_id: "removed-owner".into(),
                lease_duration: Duration::from_secs(60),
                max_concurrency: 1,
                retry: policy(),
            },
            Arc::new(SystemClock),
            Metrics::new(),
        )
        .with_repository(PostgresRepository::new(environment_pool.clone()));
        assert!(processor.resolve_owner(&execution).await.is_ok());
        for status in ["access_removed", "deleted"] {
            sqlx::query("UPDATE accounts SET status=$1 WHERE uuid='Owner'")
                .bind(status)
                .execute(&production_pool)
                .await?;
            assert!(matches!(
                processor.resolve_owner(&execution).await,
                Err(WebhookDeliveryError::Terminal { .. })
            ));
        }
        let still_active: String =
            sqlx::query_scalar("SELECT status FROM accounts WHERE uuid='Owner'")
                .fetch_one(&environment_pool)
                .await?;
        assert_eq!(
            still_active, "active",
            "the sandbox copy cannot override production lifecycle state"
        );
        Ok(())
    }

    #[tokio::test]
    async fn decrypted_destination_respects_development_production_and_sandbox_policy()
    -> anyhow::Result<()> {
        let encryption = SecretCipherKeyring::from_base64url(
            1,
            &BTreeMap::from([(1, SecretString::from(URL_SAFE_NO_PAD.encode([7; 32])))]),
        )?;
        let pool =
            sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://localhost/unused")?;
        let repository = PostgresRepository::new(pool.clone());
        let mut processor = DeliveryProcessor::new(
            repository,
            crate::test_support::identity_store(pool, "http://127.0.0.1:9")?,
            WebhookClient::new(Duration::from_secs(1), Duration::from_secs(1), 1024)?,
            encryption.clone(),
            DeliveryProcessorConfig {
                production: false,
                worker_id: "policy-test".into(),
                lease_duration: Duration::from_secs(60),
                max_concurrency: 1,
                retry: policy(),
            },
            Arc::new(SystemClock),
            Metrics::new(),
        );
        let id = Uuid::from_u128(5);
        let owner = Uuid::from_u128(6);
        for (scheme, aad_version) in [("http", 1_i16), ("https", 1), ("http", 2), ("https", 2)] {
            let endpoint = format!("{scheme}://127.0.0.1:8787/reminders");
            let aad = |field| {
                if aad_version == 1 {
                    destination_field_associated_data("test", "si:clock", field)
                } else {
                    destination_field_associated_data_v2(id, owner, field)
                }
            };
            let url =
                encryption.encrypt(&SecretString::from(endpoint.clone()), &aad("endpoint_url"))?;
            let secret = encryption.encrypt(
                &SecretString::from("fixture-signing-secret"),
                &aad("signing_secret"),
            )?;
            let now = Utc::now();
            let row = HookDestinationRow {
                id,
                org_id: (aad_version == 1).then(|| "test".to_owned()),
                owner_principal_id: owner,
                silicon_id: "si:clock".into(),
                endpoint_url_ciphertext: url.ciphertext,
                endpoint_url_nonce: url.nonce,
                signing_secret_ciphertext: secret.ciphertext,
                signing_secret_nonce: secret.nonce,
                encryption_key_version: 1,
                version: 1,
                disabled_at: None,
                purge_after: None,
                created_at: now,
                updated_at: now,
                aad_version,
            };
            for production in [false, true] {
                for testing in [false, true] {
                    processor.production = production;
                    processor.testing = testing;
                    let result = processor.decrypt_destination(&row);
                    if production && !testing {
                        assert!(matches!(result, Err(WebhookDeliveryError::Terminal { .. })));
                    } else {
                        assert_eq!(result?.endpoint_url.as_str(), endpoint);
                    }
                }
            }
        }
        Ok(())
    }

    fn policy() -> RetrySettings {
        RetrySettings {
            max_attempts: 8,
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(3_600),
            jitter_percent: 20,
        }
    }

    #[test]
    fn retry_delay_is_deterministic_and_capped() {
        let execution_id = uuid::Uuid::from_u128(42);
        let first = retry_delay(&policy(), execution_id, 1);
        assert_eq!(first, retry_delay(&policy(), execution_id, 1));
        assert!(first >= Duration::from_secs(30));
        assert!(retry_delay(&policy(), execution_id, 100) <= Duration::from_secs(3_600));
    }

    #[test]
    fn retries_grow_before_the_cap() {
        let execution_id = uuid::Uuid::from_u128(7);
        assert!(retry_delay(&policy(), execution_id, 2) > retry_delay(&policy(), execution_id, 1));
    }
}
