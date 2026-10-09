//! 45-day retention: purging archived reminders into the deleted-reminder
//! ledger, disabled subscriptions, and expired idempotency records.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use super::{
    PostgresRepository, append_audit, row_count, validate_worker_id, validate_worker_limit,
};
use crate::infrastructure::postgres::{
    error::RepositoryError,
    models::{ActorType, AuditContext, SchedulePurgeResult},
};

/// The deleted-reminder ledger keeps the newest records up to this bound.
pub(crate) const DELETED_REMINDER_LEDGER_LIMIT: u32 = 100_000;
const DELETED_REMINDER_LEDGER_LOCK_CLASS: i32 = 0x5349_4c49;
const DELETED_REMINDER_LEDGER_LOCK_KEY: i32 = 1;

#[derive(Debug, FromRow)]
struct PurgeScheduleRow {
    id: Uuid,
    org_id: Option<String>,
    owner_principal_id: Uuid,
    owner_uuid: Option<String>,
    owner_id: Option<String>,
    silicon_id: String,
    reminder_text: String,
    schedule_kind: String,
    cron_expression: String,
    timezone: String,
    last_triggered_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    archived_at: DateTime<Utc>,
    purge_after: DateTime<Utc>,
    purge_reason: String,
}

#[derive(Debug, FromRow)]
struct PurgeHookDestinationRow {
    id: Uuid,
    org_id: Option<String>,
    silicon_id: String,
    disabled_at: DateTime<Utc>,
    purge_after: DateTime<Utc>,
}

impl PostgresRepository {
    /// Permanently deletes archived schedules whose 45-day deadline has passed,
    /// with their execution history, in a bounded batch. Each one first gets a
    /// durable deleted-reminder record (what it said, who set it, when it last
    /// triggered); the ledger keeps the newest 100,000 records.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid batch size or a database error.
    pub async fn purge_expired_schedules(
        &self,
        now: DateTime<Utc>,
        limit: u32,
        worker_id: &str,
    ) -> Result<SchedulePurgeResult, RepositoryError> {
        validate_worker_limit(limit)?;
        validate_worker_id(worker_id)?;
        let mut transaction = self.pool.begin().await?;
        // All purge workers serialize capture, ledger insertion, deletion and
        // trimming under one transaction-scoped lock.
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(DELETED_REMINDER_LEDGER_LOCK_CLASS)
            .bind(DELETED_REMINDER_LEDGER_LOCK_KEY)
            .execute(&mut *transaction)
            .await?;
        let candidates = sqlx::query_as::<_, PurgeScheduleRow>(
            "SELECT s.id, s.org_id, s.owner_principal_id, a.uuid AS owner_uuid, \
                    NULLIF(a.public_id, '') AS owner_id, s.silicon_id, \
                    s.reminder_text, s.schedule_kind, s.cron_expression, s.timezone, \
                    last_execution.last_triggered_at, s.created_at, \
                    CASE WHEN s.deleted_at IS NOT NULL THEN s.deleted_at ELSE s.completed_at END \
                        AS archived_at, \
                    s.purge_after, \
                    CASE WHEN s.deleted_at IS NOT NULL THEN 'deleted' ELSE 'completed' END \
                        AS purge_reason \
             FROM schedules s \
             LEFT JOIN account_keys k ON k.storage_id = s.owner_principal_id \
             LEFT JOIN accounts a ON a.uuid = k.account_uuid \
             LEFT JOIN LATERAL (\
                 SELECT max(execution.scheduled_for) AS last_triggered_at \
                 FROM executions execution WHERE execution.schedule_id = s.id\
             ) last_execution ON true \
             WHERE s.purge_after IS NOT NULL AND s.purge_after <= $1 \
             ORDER BY s.purge_after, s.id \
             FOR UPDATE OF s SKIP LOCKED \
             LIMIT $2",
        )
        .bind(now)
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await?;
        let audit = AuditContext {
            actor_type: ActorType::System,
            actor_id: worker_id.to_owned(),
            request_id: None,
        };
        for candidate in &candidates {
            write_deleted_reminder(&mut transaction, candidate, now, &audit).await?;
        }
        let logged = row_count(candidates.len())?;
        let ids = candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        let purged = if ids.is_empty() {
            0
        } else {
            sqlx::query("DELETE FROM schedules WHERE id = ANY($1)")
                .bind(&ids)
                .execute(&mut *transaction)
                .await?
                .rows_affected()
        };
        if purged != logged {
            return Err(RepositoryError::InvalidState);
        }
        let trimmed =
            trim_deleted_reminder_ledger(&mut transaction, DELETED_REMINDER_LEDGER_LIMIT).await?;
        transaction.commit().await?;
        Ok(SchedulePurgeResult {
            purged,
            logged,
            trimmed,
        })
    }

    /// Permanently deletes disabled subscription ciphertext after its 45-day
    /// recovery window, in a bounded multi-worker-safe batch.
    ///
    /// # Errors
    ///
    /// Returns an input error for invalid worker settings or a database error.
    pub async fn purge_expired_hook_destinations(
        &self,
        now: DateTime<Utc>,
        limit: u32,
        worker_id: &str,
    ) -> Result<u64, RepositoryError> {
        validate_worker_limit(limit)?;
        validate_worker_id(worker_id)?;
        let mut transaction = self.pool.begin().await?;
        let candidates = sqlx::query_as::<_, PurgeHookDestinationRow>(
            "SELECT id, org_id, silicon_id, disabled_at, purge_after FROM hook_destinations \
             WHERE purge_after IS NOT NULL AND purge_after <= $1 \
             ORDER BY purge_after, id FOR UPDATE SKIP LOCKED LIMIT $2",
        )
        .bind(now)
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await?;
        let audit = AuditContext {
            actor_type: ActorType::System,
            actor_id: worker_id.to_owned(),
            request_id: None,
        };
        for candidate in &candidates {
            append_audit(
                &mut transaction,
                candidate.org_id.as_deref(),
                &audit,
                "hook_destination.purged",
                "hook_destination",
                Some(candidate.id.to_string()),
                json!({
                    "silicon_id": candidate.silicon_id,
                    "disabled_at": candidate.disabled_at,
                    "purge_after": candidate.purge_after,
                }),
            )
            .await?;
        }
        let ids = candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        let deleted = if ids.is_empty() {
            0
        } else {
            sqlx::query("DELETE FROM hook_destinations WHERE id = ANY($1)")
                .bind(&ids)
                .execute(&mut *transaction)
                .await?
                .rows_affected()
        };
        transaction.commit().await?;
        Ok(deleted)
    }

    /// Deletes expired idempotency responses in a bounded, multi-worker-safe batch.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid batch size or a database error.
    pub async fn purge_expired_idempotency(
        &self,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, RepositoryError> {
        validate_worker_limit(limit)?;
        let result = sqlx::query(
            "WITH expired AS (\
                 SELECT id FROM idempotency_records WHERE expires_at <= $1 \
                 ORDER BY expires_at, id FOR UPDATE SKIP LOCKED LIMIT $2\
             ) \
             DELETE FROM idempotency_records records USING expired \
             WHERE records.id = expired.id",
        )
        .bind(now)
        .bind(i64::from(limit))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

async fn write_deleted_reminder(
    transaction: &mut Transaction<'_, Postgres>,
    candidate: &PurgeScheduleRow,
    purged_at: DateTime<Utc>,
    audit: &AuditContext,
) -> Result<(), RepositoryError> {
    let record_text = deleted_reminder_record_text(candidate, purged_at);
    let ledger_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO deleted_reminders (\
             schedule_id, org_id, owner_principal_id, silicon_id, reminder_text, \
             schedule_kind, cron_expression, timezone, last_triggered_at, \
             created_at, archived_at, purge_after, purged_at, purge_reason, record_text\
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
         RETURNING id",
    )
    .bind(candidate.id)
    .bind(&candidate.org_id)
    .bind(candidate.owner_principal_id)
    .bind(&candidate.silicon_id)
    .bind(&candidate.reminder_text)
    .bind(&candidate.schedule_kind)
    .bind(&candidate.cron_expression)
    .bind(&candidate.timezone)
    .bind(candidate.last_triggered_at)
    .bind(candidate.created_at)
    .bind(candidate.archived_at)
    .bind(candidate.purge_after)
    .bind(purged_at)
    .bind(&candidate.purge_reason)
    .bind(record_text)
    .fetch_one(&mut **transaction)
    .await?;
    append_audit(
        transaction,
        candidate.org_id.as_deref(),
        audit,
        "schedule.purged",
        "schedule",
        Some(candidate.id.to_string()),
        json!({
            "deleted_reminder_id": ledger_id,
            "purge_reason": candidate.purge_reason,
        }),
    )
    .await
}

/// One JSON line per deleted reminder. Version 1.1 adds the owner's Silicon
/// Accounts uuid and current id; `org_id` stays for rows that had one.
fn deleted_reminder_record_text(schedule: &PurgeScheduleRow, purged_at: DateTime<Utc>) -> String {
    json!({
        "schema_version": "1.1",
        "schedule_id": schedule.id,
        "org_id": schedule.org_id,
        "owner_principal_id": schedule.owner_principal_id,
        "owner_uuid": schedule.owner_uuid,
        "owner_id": schedule.owner_id,
        "silicon_id": schedule.silicon_id,
        "reminder_text": schedule.reminder_text,
        "schedule_kind": schedule.schedule_kind,
        "cron_expression": schedule.cron_expression,
        "timezone": schedule.timezone,
        "last_triggered_at": schedule.last_triggered_at,
        "created_at": schedule.created_at,
        "archived_at": schedule.archived_at,
        "purge_after": schedule.purge_after,
        "purged_at": purged_at,
        "purge_reason": schedule.purge_reason,
    })
    .to_string()
}

/// Removes deterministic oldest ledger records beyond the supplied global cap.
pub(crate) async fn trim_deleted_reminder_ledger(
    transaction: &mut Transaction<'_, Postgres>,
    maximum_records: u32,
) -> Result<u64, RepositoryError> {
    let result = sqlx::query(
        "DELETE FROM deleted_reminders ledger \
         USING (SELECT id FROM deleted_reminders ORDER BY id DESC OFFSET $1) expired \
         WHERE ledger.id = expired.id",
    )
    .bind(i64::from(maximum_records))
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected())
}
