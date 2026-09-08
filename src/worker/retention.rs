//! Bounded 45-day schedule and idempotency cleanup.

use chrono::{DateTime, Utc};

use crate::infrastructure::{
    crypto::{EncryptedSecret, SecretCipherKeyring, destination_field_associated_data},
    postgres::{
        ActorType, AuditContext, HookDestinationRewrap, HookDestinationRow, PostgresRepository,
    },
};

/// Rows permanently removed during one retention sweep.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionResult {
    /// Completed/deleted schedules removed with cascading execution history.
    pub schedules: u64,
    /// Deleted-reminder ledger records written before schedule removal.
    pub deleted_reminders_logged: u64,
    /// Oldest deleted-reminder records removed to maintain the rolling bound.
    pub deleted_reminders_trimmed: u64,
    /// Expired idempotency replay records removed.
    pub idempotency_records: u64,
    /// Disabled encrypted webhook destinations removed.
    pub hook_destinations: u64,
    /// Active destinations migrated to the current encryption key.
    pub destinations_rewrapped: u64,
}

/// Runs one bounded retention sweep.
///
/// # Errors
///
/// Returns a PostgreSQL or repository-policy error. Each cleanup category uses
/// its own transaction, so a later failure does not undo an earlier committed
/// category.
pub async fn sweep(
    repository: &PostgresRepository,
    encryption: &SecretCipherKeyring,
    worker_id: &str,
    now: DateTime<Utc>,
    limit: u32,
) -> anyhow::Result<RetentionResult> {
    let destinations_rewrapped =
        rewrap_destinations(repository, encryption, worker_id, limit).await?;
    let schedule_purge = repository
        .purge_expired_schedules(now, limit, worker_id)
        .await?;
    let hook_destinations = repository
        .purge_expired_hook_destinations(now, limit, worker_id)
        .await?;
    let idempotency_records = repository.purge_expired_idempotency(now, limit).await?;
    Ok(RetentionResult {
        schedules: schedule_purge.purged,
        deleted_reminders_logged: schedule_purge.logged,
        deleted_reminders_trimmed: schedule_purge.trimmed,
        idempotency_records,
        hook_destinations,
        destinations_rewrapped,
    })
}

async fn rewrap_destinations(
    repository: &PostgresRepository,
    encryption: &SecretCipherKeyring,
    worker_id: &str,
    limit: u32,
) -> anyhow::Result<u64> {
    let rows = repository
        .list_hook_destinations_for_rewrap(encryption.current_version(), limit)
        .await?;
    let audit = AuditContext {
        actor_type: ActorType::System,
        actor_id: worker_id.to_owned(),
        request_id: None,
    };
    let mut changed = 0_u64;
    for row in rows {
        let rewrap = reencrypt_destination(encryption, &row)?;
        if repository.rewrap_hook_destination(&rewrap, &audit).await? {
            changed = changed.saturating_add(1);
        }
    }
    Ok(changed)
}

fn reencrypt_destination(
    encryption: &SecretCipherKeyring,
    row: &HookDestinationRow,
) -> anyhow::Result<HookDestinationRewrap> {
    let url_aad = destination_field_associated_data(&row.org_id, &row.silicon_id, "endpoint_url");
    let secret_aad =
        destination_field_associated_data(&row.org_id, &row.silicon_id, "signing_secret");
    let endpoint_url = encryption.decrypt(
        &EncryptedSecret {
            key_version: row.encryption_key_version,
            nonce: row.endpoint_url_nonce.clone(),
            ciphertext: row.endpoint_url_ciphertext.clone(),
        },
        &url_aad,
    )?;
    let signing_secret = encryption.decrypt(
        &EncryptedSecret {
            key_version: row.encryption_key_version,
            nonce: row.signing_secret_nonce.clone(),
            ciphertext: row.signing_secret_ciphertext.clone(),
        },
        &secret_aad,
    )?;
    let endpoint_url = encryption.encrypt(&endpoint_url, &url_aad)?;
    let signing_secret = encryption.encrypt(&signing_secret, &secret_aad)?;
    anyhow::ensure!(
        endpoint_url.key_version == signing_secret.key_version,
        "active encryption key changed during destination rewrap"
    );
    Ok(HookDestinationRewrap {
        id: row.id,
        expected_version: row.version,
        endpoint_url_ciphertext: endpoint_url.ciphertext,
        endpoint_url_nonce: nonce(&endpoint_url.nonce)?,
        signing_secret_ciphertext: signing_secret.ciphertext,
        signing_secret_nonce: nonce(&signing_secret.nonce)?,
        encryption_key_version: endpoint_url.key_version,
    })
}

fn nonce(value: &[u8]) -> anyhow::Result<[u8; 12]> {
    value
        .try_into()
        .map_err(|_| anyhow::anyhow!("encrypted destination nonce has an invalid length"))
}
