//! Concurrent lease-based Silicon Hook delivery and retry policy.

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
        crypto::{EncryptedSecret, SecretCipherKeyring, destination_field_associated_data},
        hook::{
            HookClient, HookDeliveryError, HookDestination, ReminderEvent,
            destination_url_is_allowed,
        },
        postgres::{ExecutionRow, HookDestinationRow, PostgresRepository},
    },
    metrics::Metrics,
};

/// Claims and concurrently delivers one bounded execution batch.
#[derive(Clone, Debug)]
pub struct DeliveryProcessor {
    repository: PostgresRepository,
    hook_client: HookClient,
    encryption: SecretCipherKeyring,
    hook_base_url: Url,
    worker_id: String,
    lease_duration: Duration,
    max_concurrency: u32,
    retry: RetrySettings,
    clock: Arc<dyn Clock>,
    metrics: Metrics,
    testing: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct DeliveryProcessorConfig {
    pub(crate) hook_base_url: Url,
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
        hook_client: HookClient,
        encryption: SecretCipherKeyring,
        config: DeliveryProcessorConfig,
        clock: Arc<dyn Clock>,
        metrics: Metrics,
    ) -> Self {
        Self {
            repository,
            hook_client,
            encryption,
            hook_base_url: config.hook_base_url,
            worker_id: config.worker_id,
            lease_duration: config.lease_duration,
            max_concurrency: config.max_concurrency,
            retry: config.retry,
            clock,
            metrics,
            testing: false,
        }
    }

    /// Reuses delivery policy against an isolated repository.
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
        let destination = match self.resolve_destination(&execution).await {
            Ok(destination) => destination,
            Err(error) => {
                return self.finish_failure(&execution, error).await;
            }
        };
        let attempted_at = self.clock.now();
        if !self
            .repository
            .delivery_lease_is_live(execution.id, &self.worker_id, attempted_at)
            .await?
        {
            tracing::debug!(
                execution.id = %execution.id,
                "skipping delivery whose lease or schedule is no longer live"
            );
            return Ok(());
        }
        let event = ReminderEvent {
            execution_id: execution.id,
            schedule_id: execution.schedule_id,
            silicon_id: execution.silicon_id.clone(),
            text: execution.text.clone(),
            scheduled_for: execution.scheduled_for,
            timezone: execution.timezone.clone(),
        };
        match self
            .hook_client
            .deliver(&destination, &event, attempted_at)
            .await
        {
            Ok(receipt) => {
                self.repository
                    .mark_delivery_succeeded(
                        execution.id,
                        &self.worker_id,
                        receipt.event_id,
                        self.clock.now(),
                    )
                    .await?;
                self.metrics.deliveries_succeeded.inc();
                Ok(())
            }
            Err(error) => self.finish_failure(&execution, error).await,
        }
    }

    async fn resolve_destination(
        &self,
        execution: &ExecutionRow,
    ) -> Result<HookDestination, HookDeliveryError> {
        let row = self
            .repository
            .get_hook_destination(&execution.org_id, &execution.silicon_id)
            .await
            .map_err(|_| retryable("Hook destination lookup failed"))?
            .ok_or_else(|| retryable("Hook destination is not provisioned"))?;
        self.decrypt_destination(&row)
    }

    fn decrypt_destination(
        &self,
        row: &HookDestinationRow,
    ) -> Result<HookDestination, HookDeliveryError> {
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
        let url_aad =
            destination_field_associated_data(&row.org_id, &row.silicon_id, "endpoint_url");
        let secret_aad =
            destination_field_associated_data(&row.org_id, &row.silicon_id, "signing_secret");
        let endpoint_url = self
            .encryption
            .decrypt(&url, &url_aad)
            .map_err(|_| retryable("Hook destination URL could not be decrypted"))?;
        let signing_secret = self
            .encryption
            .decrypt(&secret, &secret_aad)
            .map_err(|_| retryable("Hook signing credential could not be decrypted"))?;
        let endpoint_url = Url::parse(endpoint_url.expose_secret())
            .map_err(|_| terminal("Hook destination URL is malformed"))?;
        if !destination_url_is_allowed(
            &endpoint_url,
            &self.hook_base_url,
            &row.silicon_id,
            self.testing,
        ) {
            return Err(terminal(
                "Hook destination is outside the configured origin",
            ));
        }
        Ok(HookDestination {
            endpoint_url,
            signing_secret,
        })
    }

    async fn finish_failure(
        &self,
        execution: &ExecutionRow,
        error: HookDeliveryError,
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

fn retryable(reason: &'static str) -> HookDeliveryError {
    HookDeliveryError::Retryable {
        reason: reason.to_owned(),
    }
}

fn terminal(reason: &'static str) -> HookDeliveryError {
    HookDeliveryError::Terminal {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::config::RetrySettings;

    use super::retry_delay;

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
