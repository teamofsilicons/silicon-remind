//! Owner lifecycle: the write fence on an owner's account, event receipts, the
//! cleanup that follows a Silicon Accounts lifecycle event, and the drain of
//! resources IAM revoked before the move to Silicon Accounts.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{AssertSqlSafe, FromRow, Postgres, Transaction};
use uuid::Uuid;

use super::{
    PostgresRepository, RECEIPT_COLUMNS, RECEIPT_RETURNING_COLUMNS, append_audit,
    validate_worker_id, validate_worker_limit,
};
use crate::infrastructure::postgres::{
    error::RepositoryError,
    models::{
        ActorType, AuditContext, InternalEventReceiptRow, NewInternalEvent, RevokedResourceCleanup,
    },
};

const IAM_REVOCATION_FAILURE_REASON: &str = "stopped after IAM access revocation";
const ACCESS_REMOVED_FAILURE_REASON: &str = "stopped after the account removed Remind's access";
const ACCOUNT_DELETED_FAILURE_REASON: &str = "stopped after the account was deleted";

/// What an account lifecycle event does to the account's reminders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OwnerCleanup {
    /// The account removed Remind's access: pending deliveries stop; schedules
    /// stay as they are and resume when the account signs in again.
    Suspend,
    /// The account was deleted: schedules are archived (45-day retention, then
    /// the deleted-reminder ledger), pending deliveries stop and subscriptions
    /// are disabled.
    Delete,
}

/// Rows one lifecycle cleanup changed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct OwnerCleanupResult {
    pub(crate) schedules_archived: u64,
    pub(crate) executions_failed: u64,
    pub(crate) destinations_disabled: u64,
}

/// Locks the account behind `owner_key` for the rest of the transaction and
/// refuses the change when it is no longer active. A key no account owns (a
/// legacy row, or any row in a test-environment schema) has no fence here.
///
/// # Errors
///
/// Returns [`RepositoryError::SiliconUnavailable`] for a suspended or deleted
/// account, or a database error.
pub(crate) async fn lock_owner_for_write(
    transaction: &mut Transaction<'_, Postgres>,
    owner_key: Uuid,
) -> Result<(), RepositoryError> {
    let status = sqlx::query_scalar::<_, String>(
        "SELECT a.status FROM account_keys k JOIN accounts a ON a.uuid = k.account_uuid \
         WHERE k.storage_id = $1 FOR SHARE OF a",
    )
    .bind(owner_key)
    .fetch_optional(&mut **transaction)
    .await?;
    match status.as_deref() {
        None | Some("active") => Ok(()),
        Some(_) => Err(RepositoryError::SiliconUnavailable),
    }
}

/// Applies an account lifecycle event to every row stored under the account's keys.
///
/// # Errors
///
/// Returns a database error; the caller's transaction rolls back as a whole.
pub(crate) async fn cleanup_owner_data(
    transaction: &mut Transaction<'_, Postgres>,
    keys: &[Uuid],
    now: DateTime<Utc>,
    cleanup: OwnerCleanup,
    audit: &AuditContext,
) -> Result<OwnerCleanupResult, RepositoryError> {
    let mut result = OwnerCleanupResult::default();
    if keys.is_empty() {
        return Ok(result);
    }
    let reason = match cleanup {
        OwnerCleanup::Suspend => ACCESS_REMOVED_FAILURE_REASON,
        OwnerCleanup::Delete => ACCOUNT_DELETED_FAILURE_REASON,
    };
    if cleanup == OwnerCleanup::Delete {
        let archived: Vec<(Uuid, Option<String>)> = sqlx::query_as(
            "UPDATE schedules SET deleted_at = $1, next_run_at = NULL, version = version + 1 \
             WHERE owner_principal_id = ANY($2) AND deleted_at IS NULL AND status <> 'completed' \
             RETURNING id, org_id",
        )
        .bind(now)
        .bind(keys)
        .fetch_all(&mut **transaction)
        .await?;
        for (id, org_id) in &archived {
            append_audit(
                transaction,
                org_id.as_deref(),
                audit,
                "schedule.archived",
                "schedule",
                Some(id.to_string()),
                json!({"reason": "account_deleted"}),
            )
            .await?;
        }
        result.schedules_archived = u64::try_from(archived.len()).unwrap_or(u64::MAX);
        result.destinations_disabled = sqlx::query(
            "UPDATE hook_destinations SET disabled_at = $1, version = version + 1 \
             WHERE owner_principal_id = ANY($2) AND disabled_at IS NULL",
        )
        .bind(now)
        .bind(keys)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
    }
    result.executions_failed = sqlx::query(
        "UPDATE executions e SET status = 'failed', next_attempt_at = NULL, \
             failure_reason = $2, lease_owner = NULL, lease_expires_at = NULL \
         FROM schedules s \
         WHERE s.id = e.schedule_id AND s.owner_principal_id = ANY($1) \
           AND e.status IN ('pending', 'retrying')",
    )
    .bind(keys)
    .bind(reason)
    .execute(&mut **transaction)
    .await?
    .rows_affected();
    Ok(result)
}

/// Fails the unaccepted executions of the given schedules.
pub(super) async fn fail_schedule_unaccepted_executions(
    transaction: &mut Transaction<'_, Postgres>,
    schedule_ids: &[Uuid],
    failure_reason: &str,
) -> Result<u64, RepositoryError> {
    if schedule_ids.is_empty() {
        return Ok(0);
    }
    sqlx::query(
        "UPDATE executions SET status = 'failed', next_attempt_at = NULL, \
             failure_reason = $2, lease_owner = NULL, lease_expires_at = NULL \
         WHERE schedule_id = ANY($1) AND status IN ('pending', 'retrying')",
    )
    .bind(schedule_ids)
    .bind(failure_reason)
    .execute(&mut **transaction)
    .await
    .map(|result| result.rows_affected())
    .map_err(RepositoryError::from)
}

#[derive(Debug, FromRow)]
struct LegacyRevokedRow {
    id: Uuid,
    org_id: String,
    silicon_id: String,
}

impl PostgresRepository {
    /// Records an authenticated event exactly once by source and event id. A
    /// duplicate with an identical payload returns the original receipt;
    /// reusing an id with different bytes is rejected.
    ///
    /// # Errors
    ///
    /// Returns a receipt conflict, an input error, or a database error.
    pub async fn record_internal_event(
        &self,
        event: &NewInternalEvent,
        audit: &AuditContext,
    ) -> Result<(InternalEventReceiptRow, bool), RepositoryError> {
        validate_internal_event(event)?;
        let mut transaction = self.pool.begin().await?;
        let inserted = insert_internal_event_receipt(&mut transaction, event).await?;
        let was_inserted = inserted.is_some();
        let receipt = if let Some(row) = inserted {
            row
        } else {
            let row = lock_internal_event_receipt(&mut transaction, event).await?;
            if row.payload_hash.as_slice() != event.payload_hash.as_slice() {
                return Err(RepositoryError::EventReceiptConflict);
            }
            row
        };
        if was_inserted {
            append_audit(
                &mut transaction,
                event.org_id.as_deref(),
                audit,
                "internal_event.received",
                "internal_event_receipt",
                Some(receipt.id.to_string()),
                json!({"source": event.source, "event_type": event.event_type}),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok((receipt, was_inserted))
    }

    /// Drains schedules and subscriptions that IAM revoked before the move to
    /// Silicon Accounts (only rows that still carry an organization). After the
    /// cutover no new IAM revocation can appear, so this only finishes work the
    /// IAM-era cleanup left behind.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid worker or limit, or a database error.
    #[allow(
        clippy::too_many_lines,
        reason = "One transaction drains schedules, executions and subscriptions together"
    )]
    pub async fn cleanup_revoked_resources(
        &self,
        now: DateTime<Utc>,
        limit: u32,
        worker_id: &str,
    ) -> Result<RevokedResourceCleanup, RepositoryError> {
        validate_worker_limit(limit)?;
        validate_worker_id(worker_id)?;
        let mut transaction = self.pool.begin().await?;
        let legacy_revoked = "s.org_id IS NOT NULL \
             AND NOT EXISTS (SELECT 1 FROM account_keys k WHERE k.storage_id = s.owner_principal_id) \
             AND NOT EXISTS (SELECT 1 FROM silicon_identities i \
                 JOIN organization_lifecycle o ON o.org_id = i.org_id \
                 WHERE i.org_id = s.org_id AND i.principal_id = s.owner_principal_id \
                   AND i.state = 'active' AND o.state = 'active')";
        let schedules = sqlx::query_as::<_, LegacyRevokedRow>(AssertSqlSafe(format!(
            "SELECT s.id, s.org_id, s.silicon_id FROM schedules s \
             WHERE s.deleted_at IS NULL AND s.status <> 'completed' AND {legacy_revoked} \
             ORDER BY s.id FOR UPDATE OF s SKIP LOCKED LIMIT $1"
        )))
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await?;
        let schedule_ids = schedules.iter().map(|row| row.id).collect::<Vec<_>>();
        let schedules_deleted = if schedule_ids.is_empty() {
            0
        } else {
            sqlx::query(
                "UPDATE schedules SET deleted_at = $1, next_run_at = NULL, version = version + 1 \
                 WHERE id = ANY($2) AND deleted_at IS NULL AND status <> 'completed'",
            )
            .bind(now)
            .bind(&schedule_ids)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
        };
        let executions_failed = sqlx::query(AssertSqlSafe(format!(
            "WITH candidates AS (\
                 SELECT e.id FROM executions e JOIN schedules s ON s.id = e.schedule_id \
                 WHERE e.status IN ('pending', 'retrying') AND {legacy_revoked} \
                 ORDER BY e.id FOR UPDATE OF e SKIP LOCKED LIMIT $1\
             ) \
             UPDATE executions e SET status = 'failed', next_attempt_at = NULL, \
                 failure_reason = $2, lease_owner = NULL, lease_expires_at = NULL \
             FROM candidates WHERE e.id = candidates.id"
        )))
        .bind(i64::from(limit))
        .bind(IAM_REVOCATION_FAILURE_REASON)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        let destinations = sqlx::query_as::<_, LegacyRevokedRow>(
            "WITH candidates AS (\
                 SELECT d.id FROM hook_destinations d \
                 WHERE d.disabled_at IS NULL AND d.org_id IS NOT NULL \
                   AND NOT EXISTS (SELECT 1 FROM account_keys k \
                                   WHERE k.storage_id = d.owner_principal_id) \
                   AND NOT EXISTS (SELECT 1 FROM silicon_identities i \
                       JOIN organization_lifecycle o ON o.org_id = i.org_id \
                       WHERE i.org_id = d.org_id AND i.principal_id = d.owner_principal_id \
                         AND i.state = 'active' AND o.state = 'active') \
                 ORDER BY d.id FOR UPDATE OF d SKIP LOCKED LIMIT $1\
             ) \
             UPDATE hook_destinations d SET disabled_at = $2, version = d.version + 1 \
             FROM candidates WHERE d.id = candidates.id \
             RETURNING d.id, d.org_id, d.silicon_id",
        )
        .bind(i64::from(limit))
        .bind(now)
        .fetch_all(&mut *transaction)
        .await?;
        let audit = AuditContext {
            actor_type: ActorType::System,
            actor_id: worker_id.to_owned(),
            request_id: None,
        };
        for schedule in &schedules {
            append_audit(
                &mut transaction,
                Some(&schedule.org_id),
                &audit,
                "schedule.archived_after_iam_revocation",
                "schedule",
                Some(schedule.id.to_string()),
                json!({ "silicon_id": schedule.silicon_id }),
            )
            .await?;
        }
        for destination in &destinations {
            append_audit(
                &mut transaction,
                Some(&destination.org_id),
                &audit,
                "hook_destination.disabled_after_iam_revocation",
                "hook_destination",
                Some(destination.id.to_string()),
                json!({ "silicon_id": destination.silicon_id }),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(RevokedResourceCleanup {
            schedules_deleted,
            executions_failed,
            destinations_disabled: u64::try_from(destinations.len()).unwrap_or(u64::MAX),
        })
    }
}

pub(crate) fn validate_internal_event(event: &NewInternalEvent) -> Result<(), RepositoryError> {
    if event.source.trim().is_empty() || event.source.len() > 255 {
        return Err(RepositoryError::InvalidInput(
            "internal event source is invalid",
        ));
    }
    if event.event_id.trim().is_empty() || event.event_id.len() > 255 {
        return Err(RepositoryError::InvalidInput(
            "internal event ID is invalid",
        ));
    }
    if event.event_type.trim().is_empty() || event.event_type.len() > 255 {
        return Err(RepositoryError::InvalidInput(
            "internal event type is invalid",
        ));
    }
    if !event.payload.is_object() {
        return Err(RepositoryError::InvalidInput(
            "internal event payload must be a JSON object",
        ));
    }
    Ok(())
}

/// Inserts a receipt, or returns `None` when `(source, event_id)` already exists.
pub(crate) async fn insert_internal_event_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    event: &NewInternalEvent,
) -> Result<Option<InternalEventReceiptRow>, RepositoryError> {
    let sql = format!(
        "INSERT INTO internal_event_receipts (\
             id, source, event_id, event_type, org_id, subject_id, payload, \
             payload_hash, status, next_attempt_at, received_at\
         ) VALUES (\
             $1, $2, $3, $4, $5, $6, $7, $8, 'pending', clock_timestamp(), clock_timestamp()\
         ) \
         ON CONFLICT (source, event_id) DO NOTHING \
         RETURNING {RECEIPT_RETURNING_COLUMNS}"
    );
    sqlx::query_as::<_, InternalEventReceiptRow>(AssertSqlSafe(sql))
        .bind(event.id)
        .bind(&event.source)
        .bind(&event.event_id)
        .bind(&event.event_type)
        .bind(&event.org_id)
        .bind(&event.subject_id)
        .bind(&event.payload)
        .bind(event.payload_hash.as_slice())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(RepositoryError::from)
}

async fn lock_internal_event_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    event: &NewInternalEvent,
) -> Result<InternalEventReceiptRow, RepositoryError> {
    let sql = format!(
        "SELECT {RECEIPT_COLUMNS} FROM internal_event_receipts r \
         WHERE r.source = $1 AND r.event_id = $2 FOR UPDATE"
    );
    sqlx::query_as::<_, InternalEventReceiptRow>(AssertSqlSafe(sql))
        .bind(&event.source)
        .bind(&event.event_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(RepositoryError::InvalidState)
}

/// Marks a pending receipt processed.
pub(crate) async fn mark_internal_event_processed(
    transaction: &mut Transaction<'_, Postgres>,
    receipt_id: Uuid,
    processed_at: DateTime<Utc>,
) -> Result<InternalEventReceiptRow, RepositoryError> {
    let sql = format!(
        "UPDATE internal_event_receipts r SET \
             status = 'processed', processed_at = $1, next_attempt_at = NULL, \
             failure_reason = NULL, lease_owner = NULL, lease_expires_at = NULL \
         WHERE r.id = $2 AND r.status = 'pending' \
         RETURNING {RECEIPT_COLUMNS}"
    );
    sqlx::query_as::<_, InternalEventReceiptRow>(AssertSqlSafe(sql))
        .bind(processed_at)
        .bind(receipt_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(RepositoryError::InvalidState)
}
