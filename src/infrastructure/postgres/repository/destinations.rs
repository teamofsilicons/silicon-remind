//! Encrypted webhook subscriptions owned by Silicons.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::AssertSqlSafe;
use uuid::Uuid;

use super::{
    DESTINATION_COLUMNS, DESTINATION_RETURNING_COLUMNS, PostgresRepository, append_audit,
    lifecycle::lock_owner_for_write, validate_owner_keys, validate_silicon_snapshot,
    validate_worker_limit,
};
use crate::infrastructure::postgres::{
    error::RepositoryError,
    models::{AuditContext, HookDestinationRewrap, HookDestinationRow, NewHookDestination},
};

impl PostgresRepository {
    /// Creates one encrypted webhook subscription (associated-data version 2).
    /// No plaintext endpoint or secret crosses this boundary. A Silicon may have
    /// up to 20 active subscriptions per environment.
    ///
    /// # Errors
    ///
    /// Returns an input error for empty ciphertext or a non-positive key
    /// version, [`RepositoryError::SiliconUnavailable`] for an inactive owner,
    /// or a database error.
    pub async fn upsert_hook_destination(
        &self,
        destination: &NewHookDestination,
        audit: &AuditContext,
    ) -> Result<HookDestinationRow, RepositoryError> {
        validate_hook_destination(destination)?;
        let mut transaction = self.pool.begin().await?;
        lock_owner_for_write(&mut transaction, destination.owner_key).await?;
        let keys = super::capacity_keys(&mut transaction, destination.owner_key).await?;
        let active: i64 = sqlx::query_scalar("SELECT count(*) FROM hook_destinations WHERE owner_principal_id = ANY($1) AND disabled_at IS NULL")
            .bind(&keys).fetch_one(&mut *transaction).await?;
        if active >= 20 {
            return Err(RepositoryError::ResourceLimit("subscriptions"));
        }

        let sql = format!(
            "INSERT INTO hook_destinations (\
                 id, org_id, owner_principal_id, silicon_id, endpoint_url_ciphertext, \
                 endpoint_url_nonce, signing_secret_ciphertext, \
                 signing_secret_nonce, encryption_key_version, aad_version\
             ) VALUES ($1, NULL, $2, $3, $4, $5, $6, $7, $8, 2) \
             RETURNING {DESTINATION_RETURNING_COLUMNS}"
        );
        let row = sqlx::query_as::<_, HookDestinationRow>(AssertSqlSafe(sql))
            .bind(destination.id)
            .bind(destination.owner_key)
            .bind(&destination.silicon_id)
            .bind(&destination.endpoint_url_ciphertext)
            .bind(destination.endpoint_url_nonce.as_slice())
            .bind(&destination.signing_secret_ciphertext)
            .bind(destination.signing_secret_nonce.as_slice())
            .bind(destination.encryption_key_version)
            .fetch_one(&mut *transaction)
            .await?;
        append_audit(
            &mut transaction,
            None,
            audit,
            "hook_destination.created",
            "hook_destination",
            Some(row.id.to_string()),
            json!({
                "silicon_id": destination.silicon_id,
                "encryption_key_version": destination.encryption_key_version,
                "version": row.version,
            }),
        )
        .await?;
        transaction.commit().await?;
        Ok(row)
    }

    /// Reads the first active subscription stored under the given keys, for the
    /// legacy single-subscription endpoint.
    ///
    /// # Errors
    ///
    /// Returns a database error when the lookup fails.
    pub async fn get_hook_destination(
        &self,
        owner_keys: &[Uuid],
    ) -> Result<Option<HookDestinationRow>, RepositoryError> {
        Ok(self
            .get_hook_destinations(owner_keys)
            .await?
            .into_iter()
            .next())
    }

    /// Reads every active subscription stored under the given keys, oldest first.
    /// An empty result is valid: subscriptions are optional.
    ///
    /// # Errors
    ///
    /// Returns a database error when the lookup fails.
    pub async fn get_hook_destinations(
        &self,
        owner_keys: &[Uuid],
    ) -> Result<Vec<HookDestinationRow>, RepositoryError> {
        let sql = format!(
            "SELECT {DESTINATION_COLUMNS} FROM hook_destinations d \
             WHERE d.owner_principal_id = ANY($1) AND d.disabled_at IS NULL \
             ORDER BY d.created_at, d.id"
        );
        sqlx::query_as::<_, HookDestinationRow>(AssertSqlSafe(sql))
            .bind(owner_keys)
            .fetch_all(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Alias for the subscription management API.
    ///
    /// # Errors
    ///
    /// Returns a database error when the lookup fails.
    pub async fn list_hook_destinations(
        &self,
        owner_keys: &[Uuid],
    ) -> Result<Vec<HookDestinationRow>, RepositoryError> {
        self.get_hook_destinations(owner_keys).await
    }

    /// Every active subscription of the account that owns `owner_key`: one
    /// account may hold several storage keys (its own and linked legacy keys),
    /// and a reminder goes to all of its subscriptions. In a test-environment
    /// schema, where no account map exists, this is the key's own subscriptions.
    ///
    /// # Errors
    ///
    /// Returns a database error when the lookup fails.
    pub async fn destinations_for_owner_account(
        &self,
        owner_key: Uuid,
    ) -> Result<Vec<HookDestinationRow>, RepositoryError> {
        let sql = format!(
            "SELECT {DESTINATION_COLUMNS} FROM hook_destinations d \
             WHERE d.disabled_at IS NULL AND d.owner_principal_id IN (\
                 SELECT $1::uuid \
                 UNION SELECT sibling.storage_id FROM account_keys own \
                 JOIN account_keys sibling ON sibling.account_uuid = own.account_uuid \
                 WHERE own.storage_id = $1) \
             ORDER BY d.created_at, d.id"
        );
        sqlx::query_as::<_, HookDestinationRow>(AssertSqlSafe(sql))
            .bind(owner_key)
            .fetch_all(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Disables every active subscription stored under the given keys. Repeating
    /// it is a no-op.
    ///
    /// # Errors
    ///
    /// Returns a database error when the update fails.
    pub async fn disable_hook_destination(
        &self,
        owner_keys: &[Uuid],
        disabled_at: DateTime<Utc>,
        audit: &AuditContext,
    ) -> Result<bool, RepositoryError> {
        validate_owner_keys(owner_keys)?;
        let mut transaction = self.pool.begin().await?;
        let disabled: Vec<(Uuid, Option<String>)> = sqlx::query_as(
            "UPDATE hook_destinations SET disabled_at = $1, version = version + 1 \
             WHERE owner_principal_id = ANY($2) AND disabled_at IS NULL \
             RETURNING id, org_id",
        )
        .bind(disabled_at)
        .bind(owner_keys)
        .fetch_all(&mut *transaction)
        .await?;
        for (id, org_id) in &disabled {
            append_audit(
                &mut transaction,
                org_id.as_deref(),
                audit,
                "hook_destination.disabled",
                "hook_destination",
                Some(id.to_string()),
                json!({}),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(!disabled.is_empty())
    }

    /// Disables one subscription stored under the given keys.
    ///
    /// # Errors
    ///
    /// Returns not found when no such subscription belongs to the keys, or a
    /// database error.
    pub async fn disable_hook_destination_by_id(
        &self,
        owner_keys: &[Uuid],
        destination_id: Uuid,
        disabled_at: DateTime<Utc>,
        audit: &AuditContext,
    ) -> Result<bool, RepositoryError> {
        validate_owner_keys(owner_keys)?;
        let mut transaction = self.pool.begin().await?;
        let (id, org_id) = sqlx::query_as::<_, (Uuid, Option<String>)>(
            "SELECT id, org_id FROM hook_destinations \
             WHERE id = $1 AND owner_principal_id = ANY($2) FOR UPDATE",
        )
        .bind(destination_id)
        .bind(owner_keys)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(RepositoryError::NotFound)?;
        let changed = sqlx::query(
            "UPDATE hook_destinations SET disabled_at = $1, version = version + 1 \
             WHERE id = $2 AND disabled_at IS NULL",
        )
        .bind(disabled_at)
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            > 0;
        if changed {
            append_audit(
                &mut transaction,
                org_id.as_deref(),
                audit,
                "hook_destination.disabled",
                "hook_destination",
                Some(destination_id.to_string()),
                json!({}),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    /// Lists a bounded batch of active subscriptions still encrypted with a
    /// historical key.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid key version or limit, or a database error.
    pub async fn list_hook_destinations_for_rewrap(
        &self,
        current_key_version: i16,
        limit: u32,
    ) -> Result<Vec<HookDestinationRow>, RepositoryError> {
        if current_key_version <= 0 {
            return Err(RepositoryError::InvalidInput(
                "encryption key version must be positive",
            ));
        }
        validate_worker_limit(limit)?;
        let sql = format!(
            "SELECT {DESTINATION_COLUMNS} FROM hook_destinations d \
             WHERE d.disabled_at IS NULL AND d.encryption_key_version <> $1 \
             ORDER BY d.updated_at, d.id LIMIT $2"
        );
        sqlx::query_as::<_, HookDestinationRow>(AssertSqlSafe(sql))
            .bind(current_key_version)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Replaces one subscription's ciphertext under optimistic locking. The
    /// associated-data version is unchanged.
    ///
    /// # Errors
    ///
    /// Returns an input error for malformed encrypted material or a database
    /// error. A concurrent change returns `false` without overwriting it.
    pub async fn rewrap_hook_destination(
        &self,
        rewrap: &HookDestinationRewrap,
        audit: &AuditContext,
    ) -> Result<bool, RepositoryError> {
        validate_destination_rewrap(rewrap)?;
        let mut transaction = self.pool.begin().await?;
        let current = sqlx::query_as::<_, (Option<String>, String, i16)>(
            "SELECT org_id, silicon_id, encryption_key_version FROM hook_destinations \
             WHERE id = $1 AND version = $2 AND disabled_at IS NULL FOR UPDATE",
        )
        .bind(rewrap.id)
        .bind(rewrap.expected_version)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((org_id, silicon_id, previous_key_version)) = current else {
            transaction.commit().await?;
            return Ok(false);
        };
        let result = sqlx::query(
            "UPDATE hook_destinations SET \
                 endpoint_url_ciphertext = $1, endpoint_url_nonce = $2, \
                 signing_secret_ciphertext = $3, signing_secret_nonce = $4, \
                 encryption_key_version = $5, version = version + 1 \
             WHERE id = $6 AND version = $7 AND disabled_at IS NULL",
        )
        .bind(&rewrap.endpoint_url_ciphertext)
        .bind(rewrap.endpoint_url_nonce.as_slice())
        .bind(&rewrap.signing_secret_ciphertext)
        .bind(rewrap.signing_secret_nonce.as_slice())
        .bind(rewrap.encryption_key_version)
        .bind(rewrap.id)
        .bind(rewrap.expected_version)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            transaction.commit().await?;
            return Ok(false);
        }
        append_audit(
            &mut transaction,
            org_id.as_deref(),
            audit,
            "hook_destination.rewrapped",
            "hook_destination",
            Some(rewrap.id.to_string()),
            json!({
                "silicon_id": silicon_id,
                "previous_key_version": previous_key_version,
                "encryption_key_version": rewrap.encryption_key_version,
            }),
        )
        .await?;
        transaction.commit().await?;
        Ok(true)
    }
}

fn validate_hook_destination(destination: &NewHookDestination) -> Result<(), RepositoryError> {
    validate_silicon_snapshot(&destination.silicon_id)?;
    if destination.endpoint_url_ciphertext.is_empty()
        || destination.signing_secret_ciphertext.is_empty()
    {
        return Err(RepositoryError::InvalidInput(
            "encrypted destination values cannot be empty",
        ));
    }
    if destination.encryption_key_version <= 0 {
        return Err(RepositoryError::InvalidInput(
            "encryption key version must be positive",
        ));
    }
    Ok(())
}

fn validate_destination_rewrap(rewrap: &HookDestinationRewrap) -> Result<(), RepositoryError> {
    if rewrap.expected_version <= 0 {
        return Err(RepositoryError::InvalidInput(
            "expected destination version must be positive",
        ));
    }
    if rewrap.endpoint_url_ciphertext.is_empty() || rewrap.signing_secret_ciphertext.is_empty() {
        return Err(RepositoryError::InvalidInput(
            "encrypted destination values cannot be empty",
        ));
    }
    if rewrap.encryption_key_version <= 0 {
        return Err(RepositoryError::InvalidInput(
            "encryption key version must be positive",
        ));
    }
    Ok(())
}
