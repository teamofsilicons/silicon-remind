//! Due-schedule materialization and lease-based webhook delivery state.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, Postgres, Transaction};
use uuid::Uuid;

use crate::domain::ARCHIVE_RETENTION_DAYS;

use super::{
    EXECUTION_COLUMNS, EXECUTION_RETURNING_COLUMNS, OWNER_IS_LIVE, PostgresRepository,
    SCHEDULE_COLUMNS, append_audit, lease_deadline, validate_failure_reason, validate_worker_id,
    validate_worker_limit,
};
use crate::infrastructure::postgres::{
    error::RepositoryError,
    models::{ActorType, AuditContext, DueMaterialization, ExecutionRow, ScheduleRow},
};

#[derive(Clone, Copy, Debug)]
struct MaterializationGeneration {
    scheduled_for: DateTime<Utc>,
    schedule_version: i64,
    schedule_kind: &'static str,
}

impl PostgresRepository {
    /// Locks a bounded batch of due schedules whose owner may receive reminders.
    ///
    /// The caller computes each recurring schedule's next occurrence, calls
    /// [`Self::materialize_locked_occurrence`] for every row, then commits the
    /// same transaction. Rolling back releases every claim unchanged.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid batch size or a database error.
    pub async fn lock_due_schedules(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<ScheduleRow>, RepositoryError> {
        validate_worker_limit(limit)?;
        let sql = format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules s \
             WHERE s.status = 'active' AND s.deleted_at IS NULL \
               AND s.next_run_at IS NOT NULL AND s.next_run_at <= $1 \
               AND {OWNER_IS_LIVE} \
             ORDER BY s.next_run_at, s.id \
             FOR UPDATE OF s SKIP LOCKED \
             LIMIT $2"
        );
        sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(sql))
            .bind(now)
            .bind(i64::from(limit))
            .fetch_all(&mut **transaction)
            .await
            .map_err(RepositoryError::from)
    }

    /// Materializes one occurrence of a schedule locked by
    /// [`Self::lock_due_schedules`] and advances the schedule atomically.
    ///
    /// For recurring schedules `next_run_at` must be the first occurrence
    /// strictly after `worker_now`; for one-time schedules it must be `None`.
    ///
    /// # Errors
    ///
    /// Returns invalid input/state when the row is no longer due or the
    /// advancement violates the schedule kind, and a database error otherwise.
    pub async fn materialize_locked_occurrence(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        schedule: &ScheduleRow,
        execution_id: Uuid,
        worker_now: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
        audit: &AuditContext,
    ) -> Result<DueMaterialization, RepositoryError> {
        let generation = materialization_generation(schedule, worker_now, next_run_at)?;
        let insert_sql = format!(
            "INSERT INTO executions (\
                 id, schedule_id, org_id, silicon_id, schedule_version, \
                 schedule_kind, scheduled_for, reminder_text, timezone, \
                 status, next_attempt_at\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'pending', $10) \
             ON CONFLICT (schedule_id, scheduled_for) DO NOTHING \
             RETURNING {EXECUTION_RETURNING_COLUMNS}"
        );
        let inserted_row = sqlx::query_as::<_, ExecutionRow>(AssertSqlSafe(insert_sql))
            .bind(execution_id)
            .bind(schedule.id)
            .bind(&schedule.org_id)
            .bind(&schedule.silicon_id)
            .bind(generation.schedule_version)
            .bind(generation.schedule_kind)
            .bind(generation.scheduled_for)
            .bind(&schedule.text)
            .bind(&schedule.timezone)
            .bind(worker_now)
            .fetch_optional(&mut **transaction)
            .await?;
        let inserted = inserted_row.is_some();
        let execution = if let Some(row) = inserted_row {
            row
        } else {
            let select_sql = format!(
                "SELECT {EXECUTION_COLUMNS} FROM executions e \
                 WHERE e.schedule_id = $1 AND e.scheduled_for = $2"
            );
            sqlx::query_as::<_, ExecutionRow>(AssertSqlSafe(select_sql))
                .bind(schedule.id)
                .bind(generation.scheduled_for)
                .fetch_optional(&mut **transaction)
                .await?
                .ok_or(RepositoryError::InvalidState)?
        };
        if execution.schedule_version != generation.schedule_version
            || execution.schedule_kind != generation.schedule_kind
        {
            return Err(RepositoryError::InvalidState);
        }

        let advanced_version = sqlx::query_scalar::<_, i64>(
            "UPDATE schedules SET \
                 next_run_at = $1, \
                 status = CASE WHEN schedule_kind = 'one_time' THEN 'completed' ELSE status END, \
                 completed_at = CASE WHEN schedule_kind = 'one_time' THEN $2 \
                                     ELSE completed_at END, \
                 version = version + 1 \
             WHERE id = $3 AND owner_principal_id = $4 \
               AND status = 'active' AND deleted_at IS NULL \
               AND next_run_at IS NOT DISTINCT FROM $5 AND version = $6 \
             RETURNING version",
        )
        .bind(next_run_at)
        .bind(worker_now)
        .bind(schedule.id)
        .bind(schedule.owner_principal_id)
        .bind(generation.scheduled_for)
        .bind(schedule.version)
        .fetch_optional(&mut **transaction)
        .await?;
        if advanced_version != Some(generation.schedule_version) {
            return Err(RepositoryError::InvalidState);
        }
        append_materialization_audits(
            transaction,
            schedule,
            &execution,
            generation,
            worker_now,
            inserted,
            audit,
        )
        .await?;
        Ok(DueMaterialization {
            execution,
            inserted,
        })
    }

    /// Acquires bounded delivery leases with `FOR UPDATE SKIP LOCKED`. Expired
    /// leases are reclaimable; archived schedules and owners who may no longer
    /// receive reminders are excluded.
    ///
    /// # Errors
    ///
    /// Returns an input error for invalid worker settings or a database error.
    pub async fn claim_deliveries(
        &self,
        worker_id: &str,
        now: DateTime<Utc>,
        lease_duration: Duration,
        limit: u32,
    ) -> Result<Vec<ExecutionRow>, RepositoryError> {
        validate_worker_id(worker_id)?;
        validate_worker_limit(limit)?;
        let lease_expires_at = lease_deadline(now, lease_duration)?;
        if lease_expires_at <= now {
            return Err(RepositoryError::InvalidInput(
                "delivery lease duration must be positive",
            ));
        }
        let mut transaction = self.pool.begin().await?;
        let sql = format!(
            "WITH candidates AS (\
                 SELECT e.id FROM executions e \
                 JOIN schedules s ON s.id = e.schedule_id \
                 WHERE e.status IN ('pending', 'retrying') \
                   AND e.next_attempt_at <= $1 \
                   AND (e.lease_expires_at IS NULL OR e.lease_expires_at <= $1) \
                   AND s.deleted_at IS NULL \
                   AND (s.purge_after IS NULL OR s.purge_after > $1) \
                   AND {OWNER_IS_LIVE} \
                 ORDER BY e.next_attempt_at, e.id \
                 FOR UPDATE OF e SKIP LOCKED \
                 LIMIT $2\
             ) \
             UPDATE executions e SET \
                 status = CASE WHEN e.attempt_count = 0 THEN 'pending' ELSE 'retrying' END, \
                 attempt_count = e.attempt_count + 1, \
                 attempted_at = $1, lease_owner = $3, lease_expires_at = $4 \
             FROM candidates c WHERE e.id = c.id \
             RETURNING {EXECUTION_COLUMNS}"
        );
        let rows = sqlx::query_as::<_, ExecutionRow>(AssertSqlSafe(sql))
            .bind(now)
            .bind(i64::from(limit))
            .bind(worker_id)
            .bind(lease_expires_at)
            .fetch_all(&mut *transaction)
            .await?;
        let audit = system_audit(worker_id);
        for row in &rows {
            append_audit(
                &mut transaction,
                row.org_id.as_deref(),
                &audit,
                "execution.delivery_claimed",
                "execution",
                Some(row.id.to_string()),
                json!({
                    "attempt_count": row.attempt_count,
                    "lease_expires_at": row.lease_expires_at,
                }),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(rows)
    }

    /// Revalidates a claimed delivery immediately before outbound I/O: the lease
    /// must still be this worker's, and the schedule, its owner and its
    /// retention window must all still be live.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid worker id or a database error.
    pub async fn delivery_lease_is_live(
        &self,
        execution_id: Uuid,
        worker_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, RepositoryError> {
        validate_worker_id(worker_id)?;
        let sql = format!(
            "SELECT EXISTS (\
                 SELECT 1 FROM executions e JOIN schedules s ON s.id = e.schedule_id \
                 WHERE e.id = $1 AND e.status IN ('pending', 'retrying') \
                   AND e.lease_owner = $2 AND e.lease_expires_at > $3 \
                   AND s.deleted_at IS NULL \
                   AND (s.purge_after IS NULL OR s.purge_after > $3) \
                   AND {OWNER_IS_LIVE}\
             )"
        );
        sqlx::query_scalar::<_, bool>(AssertSqlSafe(sql))
            .bind(execution_id)
            .bind(worker_id)
            .bind(now)
            .fetch_one(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// The owner storage key of the schedule behind an execution.
    ///
    /// # Errors
    ///
    /// Returns not found for an unknown execution or a database error.
    pub async fn execution_owner_key(&self, execution_id: Uuid) -> Result<Uuid, RepositoryError> {
        sqlx::query_scalar(
            "SELECT s.owner_principal_id FROM executions e \
             JOIN schedules s ON s.id = e.schedule_id WHERE e.id = $1",
        )
        .bind(execution_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(RepositoryError::NotFound)
    }

    /// Marks a leased execution delivered after a validated `2xx`. The historical
    /// `hook_event_id` column stores the receipt.
    ///
    /// # Errors
    ///
    /// Returns lease lost when this worker no longer owns a live claim, or a
    /// database error.
    pub async fn mark_delivery_succeeded(
        &self,
        execution_id: Uuid,
        worker_id: &str,
        hook_event_id: Uuid,
        accepted_at: DateTime<Utc>,
    ) -> Result<ExecutionRow, RepositoryError> {
        validate_worker_id(worker_id)?;
        let mut transaction = self.pool.begin().await?;
        lock_live_delivery_schedule(&mut transaction, execution_id, accepted_at).await?;
        let sql = format!(
            "UPDATE executions e SET \
                 status = 'delivered', delivered_at = $1, hook_event_id = $2, \
                 next_attempt_at = NULL, failure_reason = NULL, \
                 lease_owner = NULL, lease_expires_at = NULL \
             WHERE e.id = $3 AND e.status IN ('pending', 'retrying') \
               AND e.lease_owner = $4 AND e.lease_expires_at > $1 \
               AND EXISTS (SELECT 1 FROM schedules s WHERE s.id = e.schedule_id \
                   AND s.deleted_at IS NULL AND (s.purge_after IS NULL OR s.purge_after > $1)) \
             RETURNING {EXECUTION_COLUMNS}"
        );
        let execution = sqlx::query_as::<_, ExecutionRow>(AssertSqlSafe(sql))
            .bind(accepted_at)
            .bind(hook_event_id)
            .bind(execution_id)
            .bind(worker_id)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(RepositoryError::LeaseLost)?;
        append_worker_execution_audit(
            &mut transaction,
            &execution,
            worker_id,
            "execution.delivered",
            json!({
                "attempt_count": execution.attempt_count,
                "hook_event_id": hook_event_id,
            }),
        )
        .await?;
        transaction.commit().await?;
        Ok(execution)
    }

    /// Releases a live delivery lease into the retry queue.
    ///
    /// # Errors
    ///
    /// Returns invalid input for an unbounded reason or a non-future retry time,
    /// lease lost for a stale owner, or a database error.
    pub async fn mark_delivery_retrying(
        &self,
        execution_id: Uuid,
        worker_id: &str,
        failed_at: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
        failure_reason: &str,
    ) -> Result<ExecutionRow, RepositoryError> {
        validate_worker_id(worker_id)?;
        validate_failure_reason(failure_reason)?;
        if next_attempt_at <= failed_at {
            return Err(RepositoryError::InvalidInput(
                "retry time must be after the failed attempt",
            ));
        }
        let mut transaction = self.pool.begin().await?;
        lock_live_delivery_schedule(&mut transaction, execution_id, failed_at).await?;
        let sql = format!(
            "UPDATE executions e SET \
                 status = 'retrying', next_attempt_at = $1, failure_reason = $2, \
                 lease_owner = NULL, lease_expires_at = NULL \
             WHERE e.id = $3 AND e.status IN ('pending', 'retrying') \
               AND e.lease_owner = $4 AND e.lease_expires_at > $5 \
               AND EXISTS (SELECT 1 FROM schedules s WHERE s.id = e.schedule_id \
                   AND s.deleted_at IS NULL AND (s.purge_after IS NULL OR s.purge_after > $5)) \
             RETURNING {EXECUTION_COLUMNS}"
        );
        let execution = sqlx::query_as::<_, ExecutionRow>(AssertSqlSafe(sql))
            .bind(next_attempt_at)
            .bind(failure_reason)
            .bind(execution_id)
            .bind(worker_id)
            .bind(failed_at)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(RepositoryError::LeaseLost)?;
        append_worker_execution_audit(
            &mut transaction,
            &execution,
            worker_id,
            "execution.delivery_retry_scheduled",
            json!({
                "attempt_count": execution.attempt_count,
                "next_attempt_at": next_attempt_at,
            }),
        )
        .await?;
        transaction.commit().await?;
        Ok(execution)
    }

    /// Marks a live delivery lease terminally failed.
    ///
    /// # Errors
    ///
    /// Returns invalid input for an unbounded reason, lease lost for a stale
    /// owner, or a database error.
    pub async fn mark_delivery_failed(
        &self,
        execution_id: Uuid,
        worker_id: &str,
        failed_at: DateTime<Utc>,
        failure_reason: &str,
    ) -> Result<ExecutionRow, RepositoryError> {
        validate_worker_id(worker_id)?;
        validate_failure_reason(failure_reason)?;
        let mut transaction = self.pool.begin().await?;
        lock_live_delivery_schedule(&mut transaction, execution_id, failed_at).await?;
        let sql = format!(
            "UPDATE executions e SET \
                 status = 'failed', next_attempt_at = NULL, failure_reason = $1, \
                 lease_owner = NULL, lease_expires_at = NULL \
             WHERE e.id = $2 AND e.status IN ('pending', 'retrying') \
               AND e.lease_owner = $3 AND e.lease_expires_at > $4 \
               AND EXISTS (SELECT 1 FROM schedules s WHERE s.id = e.schedule_id \
                   AND s.deleted_at IS NULL AND (s.purge_after IS NULL OR s.purge_after > $4)) \
             RETURNING {EXECUTION_COLUMNS}"
        );
        let execution = sqlx::query_as::<_, ExecutionRow>(AssertSqlSafe(sql))
            .bind(failure_reason)
            .bind(execution_id)
            .bind(worker_id)
            .bind(failed_at)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(RepositoryError::LeaseLost)?;
        append_worker_execution_audit(
            &mut transaction,
            &execution,
            worker_id,
            "execution.delivery_failed",
            json!({ "attempt_count": execution.attempt_count }),
        )
        .await?;
        transaction.commit().await?;
        Ok(execution)
    }
}

fn system_audit(worker_id: &str) -> AuditContext {
    AuditContext {
        actor_type: ActorType::System,
        actor_id: worker_id.to_owned(),
        request_id: None,
    }
}

async fn append_materialization_audits(
    transaction: &mut Transaction<'_, Postgres>,
    schedule: &ScheduleRow,
    execution: &ExecutionRow,
    generation: MaterializationGeneration,
    worker_now: DateTime<Utc>,
    inserted: bool,
    audit: &AuditContext,
) -> Result<(), RepositoryError> {
    if inserted {
        append_audit(
            transaction,
            schedule.org_id.as_deref(),
            audit,
            "execution.materialized",
            "execution",
            Some(execution.id.to_string()),
            json!({
                "schedule_id": schedule.id,
                "schedule_version": generation.schedule_version,
                "schedule_kind": generation.schedule_kind,
                "scheduled_for": generation.scheduled_for,
                "coalesced": schedule.schedule_kind == "recurring"
                    && generation.scheduled_for < worker_now,
            }),
        )
        .await?;
    }
    if generation.schedule_kind == "one_time" {
        append_audit(
            transaction,
            schedule.org_id.as_deref(),
            audit,
            "schedule.archived",
            "schedule",
            Some(schedule.id.to_string()),
            json!({
                "execution_id": execution.id,
                "reason": "one_time_triggered",
                "retention_days": ARCHIVE_RETENTION_DAYS,
                "scheduled_for": generation.scheduled_for,
            }),
        )
        .await?;
    }
    Ok(())
}

fn materialization_generation(
    schedule: &ScheduleRow,
    worker_now: DateTime<Utc>,
    next_run_at: Option<DateTime<Utc>>,
) -> Result<MaterializationGeneration, RepositoryError> {
    if schedule.status != "active" || schedule.deleted_at.is_some() {
        return Err(RepositoryError::InvalidState);
    }
    let scheduled_for = schedule.next_run_at.ok_or(RepositoryError::InvalidState)?;
    if scheduled_for > worker_now {
        return Err(RepositoryError::InvalidInput(
            "a future schedule cannot be materialized",
        ));
    }
    let schedule_kind = match schedule.schedule_kind.as_str() {
        "recurring" => {
            let next = next_run_at.ok_or(RepositoryError::InvalidInput(
                "a recurring materialization requires its next occurrence",
            ))?;
            if next <= worker_now {
                return Err(RepositoryError::InvalidInput(
                    "the next recurring occurrence must be after worker time",
                ));
            }
            "recurring"
        }
        "one_time" if next_run_at.is_none() => "one_time",
        "one_time" => {
            return Err(RepositoryError::InvalidInput(
                "a one-time materialization must clear next_run_at",
            ));
        }
        _ => return Err(RepositoryError::InvalidState),
    };
    let schedule_version = schedule
        .version
        .checked_add(1)
        .ok_or(RepositoryError::InvalidState)?;
    Ok(MaterializationGeneration {
        scheduled_for,
        schedule_version,
        schedule_kind,
    })
}

async fn lock_live_delivery_schedule(
    transaction: &mut Transaction<'_, Postgres>,
    execution_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), RepositoryError> {
    let sql = format!(
        "SELECT s.id FROM schedules s JOIN executions e ON e.schedule_id = s.id \
         WHERE e.id = $1 AND s.deleted_at IS NULL \
           AND (s.purge_after IS NULL OR s.purge_after > $2) \
           AND {OWNER_IS_LIVE} \
         FOR UPDATE OF s"
    );
    let schedule_id = sqlx::query_scalar::<_, Uuid>(AssertSqlSafe(sql))
        .bind(execution_id)
        .bind(now)
        .fetch_optional(&mut **transaction)
        .await?;
    if schedule_id.is_none() {
        return Err(RepositoryError::LeaseLost);
    }
    Ok(())
}

async fn append_worker_execution_audit(
    transaction: &mut Transaction<'_, Postgres>,
    execution: &ExecutionRow,
    worker_id: &str,
    action: &str,
    metadata: Value,
) -> Result<(), RepositoryError> {
    append_audit(
        transaction,
        execution.org_id.as_deref(),
        &system_audit(worker_id),
        action,
        "execution",
        Some(execution.id.to_string()),
        metadata,
    )
    .await
}
