//! Public schedule and execution reads, and owner-only schedule changes.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{AssertSqlSafe, Postgres, QueryBuilder, Transaction};
use uuid::Uuid;

use crate::domain::{ARCHIVE_RETENTION_DAYS, MAX_SCHEDULE_STATUS_BATCH_SIZE, ScheduleSection};

use super::{
    EXECUTION_COLUMNS, PostgresRepository, Reservation, SCHEDULE_COLUMNS,
    SCHEDULE_RETURNING_COLUMNS, append_audit, complete_idempotency, decode_idempotent_response,
    lifecycle::{fail_schedule_unaccepted_executions, lock_owner_for_write},
    page_from_rows, push_owner_filter, reserve_idempotency, validate_idempotency_context,
    validate_idempotency_operation, validate_owner_keys, validate_public_limit,
    validate_silicon_snapshot,
};
use crate::infrastructure::postgres::{
    error::RepositoryError,
    models::{
        ActorType, AuditContext, BulkScheduleStatusReplacement, CreateSchedule, ExecutionCursor,
        ExecutionRow, IdempotencyContext, IdempotentMutation, ListSchedules, MutableScheduleStatus,
        Page, ScheduleReplacement, ScheduleResponse, ScheduleRow, ScheduleStatusBatchResponse,
        ScheduleStatusResponse, StoredIdempotentResponse,
    },
};

const OWNER_ARCHIVE_FAILURE_REASON: &str = "stopped after reminder archive";

impl PostgresRepository {
    /// Reads one retained schedule the caller may read.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn get_schedule(
        &self,
        read_keys: Option<&[Uuid]>,
        schedule_id: Uuid,
    ) -> Result<Option<ScheduleRow>, RepositoryError> {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules s WHERE s.id = "
        ));
        query
            .push_bind(schedule_id)
            .push(" AND (s.purge_after IS NULL OR s.purge_after > clock_timestamp())");
        push_owner_filter(&mut query, "s.owner_principal_id", read_keys);
        query
            .build_query_as::<ScheduleRow>()
            .fetch_optional(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Reads a bounded set of retained, readable schedules for a status change.
    ///
    /// Rows are UUID-ordered and not filtered by owner, so the application can
    /// tell an invisible id (404) from a visible schedule another Silicon owns
    /// (403). Archived rows are included so their state can be reported after
    /// visibility and ownership.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid id batch or a database error.
    pub async fn get_schedules_for_status_change(
        &self,
        read_keys: Option<&[Uuid]>,
        schedule_ids: &[Uuid],
    ) -> Result<Vec<ScheduleRow>, RepositoryError> {
        validate_schedule_status_ids(schedule_ids)?;
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules s WHERE s.id = ANY("
        ));
        query
            .push_bind(schedule_ids.to_vec())
            .push(") AND (s.purge_after IS NULL OR s.purge_after > clock_timestamp())");
        push_owner_filter(&mut query, "s.owner_principal_id", read_keys);
        query.push(" ORDER BY s.id");
        query
            .build_query_as::<ScheduleRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Lists readable schedules with the stable `(created_at DESC, id DESC)` keyset.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid limit, status or Silicon id, and a
    /// database error when PostgreSQL rejects the query.
    pub async fn list_schedules(
        &self,
        filters: &ListSchedules,
    ) -> Result<Page<ScheduleRow>, RepositoryError> {
        validate_public_limit(filters.limit)?;
        if let Some(status) = filters.status.as_deref() {
            validate_schedule_status(status)?;
        }
        if let Some(silicon_id) = filters.silicon_snapshot.as_deref() {
            validate_silicon_snapshot(silicon_id)?;
        }
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules s WHERE TRUE"
        ));
        match filters.section {
            ScheduleSection::Current => {
                query.push(" AND s.deleted_at IS NULL AND s.status <> 'completed'");
            }
            ScheduleSection::Archived => {
                query.push(
                    " AND (s.deleted_at IS NOT NULL OR s.status = 'completed') \
                     AND s.purge_after > clock_timestamp()",
                );
            }
        }
        push_owner_filter(
            &mut query,
            "s.owner_principal_id",
            filters.read_keys.as_deref(),
        );
        match (&filters.owner_keys, &filters.silicon_snapshot) {
            (Some(keys), Some(snapshot)) => {
                query
                    .push(" AND (s.owner_principal_id = ANY(")
                    .push_bind(keys.clone())
                    .push(") OR s.silicon_id = ")
                    .push_bind(snapshot.clone())
                    .push(')');
            }
            (Some(keys), None) => {
                push_owner_filter(&mut query, "s.owner_principal_id", Some(keys));
            }
            (None, Some(snapshot)) => {
                query
                    .push(" AND s.silicon_id = ")
                    .push_bind(snapshot.clone());
            }
            (None, None) => {}
        }
        if let Some(status) = filters.status.as_deref() {
            query.push(" AND s.status = ").push_bind(status);
        }
        if let Some(cursor) = filters.cursor {
            query
                .push(" AND (s.created_at, s.id) < (")
                .push_bind(cursor.created_at)
                .push(", ")
                .push_bind(cursor.id)
                .push(')');
        }
        query
            .push(" ORDER BY s.created_at DESC, s.id DESC LIMIT ")
            .push_bind(i64::from(filters.limit) + 1);
        let rows = query
            .build_query_as::<ScheduleRow>()
            .fetch_all(&self.pool)
            .await?;
        Ok(page_from_rows(rows, filters.limit))
    }

    /// Counts retained schedules per owner storage key.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn count_schedules_by_owner(
        &self,
        keys: &[Uuid],
    ) -> Result<HashMap<Uuid, i64>, RepositoryError> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT owner_principal_id, count(*) FROM schedules \
             WHERE owner_principal_id = ANY($1) \
               AND (purge_after IS NULL OR purge_after > clock_timestamp()) \
             GROUP BY owner_principal_id",
        )
        .bind(keys)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Reads one readable execution by its stable UUID.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn get_execution(
        &self,
        read_keys: Option<&[Uuid]>,
        execution_id: Uuid,
    ) -> Result<Option<ExecutionRow>, RepositoryError> {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT {EXECUTION_COLUMNS} FROM executions e \
             JOIN schedules s ON s.id = e.schedule_id WHERE e.id = "
        ));
        query
            .push_bind(execution_id)
            .push(" AND (s.purge_after IS NULL OR s.purge_after > clock_timestamp())");
        push_owner_filter(&mut query, "s.owner_principal_id", read_keys);
        query
            .build_query_as::<ExecutionRow>()
            .fetch_optional(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Lists one readable schedule's execution history with keyset pagination.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::NotFound`] when the schedule is not readable,
    /// an input error for an invalid limit, or a database error.
    pub async fn list_executions(
        &self,
        read_keys: Option<&[Uuid]>,
        schedule_id: Uuid,
        cursor: Option<ExecutionCursor>,
        limit: u32,
    ) -> Result<Page<ExecutionRow>, RepositoryError> {
        validate_public_limit(limit)?;
        if self.get_schedule(read_keys, schedule_id).await?.is_none() {
            return Err(RepositoryError::NotFound);
        }
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT {EXECUTION_COLUMNS} FROM executions e WHERE e.schedule_id = "
        ));
        query.push_bind(schedule_id);
        if let Some(cursor) = cursor {
            query
                .push(" AND (e.scheduled_for, e.id) < (")
                .push_bind(cursor.scheduled_for)
                .push(", ")
                .push_bind(cursor.id)
                .push(')');
        }
        query
            .push(" ORDER BY e.scheduled_for DESC, e.id DESC LIMIT ")
            .push_bind(i64::from(limit) + 1);
        let rows = query
            .build_query_as::<ExecutionRow>()
            .fetch_all(&self.pool)
            .await?;
        Ok(page_from_rows(rows, limit))
    }

    /// Finds the exact response for an unexpired idempotency scope without
    /// reserving it, so a committed create or PATCH replays before time-sensitive
    /// validation. The mutation's own reservation stays authoritative.
    ///
    /// # Errors
    ///
    /// Returns an idempotency conflict for a different request hash, an
    /// incomplete error for a malformed record, an input error, or a database error.
    pub async fn find_idempotent_response(
        &self,
        operation: &str,
        target_id: Option<Uuid>,
        context: &IdempotencyContext,
    ) -> Result<Option<StoredIdempotentResponse>, RepositoryError> {
        validate_idempotency_context(context)?;
        validate_idempotency_operation(operation)?;
        let row = sqlx::query_as::<_, super::IdempotencyRow>(
            "SELECT request_hash, state, response_status, response_body \
             FROM idempotency_records \
             WHERE org_id IS NULL AND actor_type = $1 AND actor_id = $2 \
               AND operation = $3 AND target_id IS NOT DISTINCT FROM $4 \
               AND idempotency_key = $5 AND expires_at > clock_timestamp()",
        )
        .bind(context.actor_type.as_str())
        .bind(&context.actor_id)
        .bind(operation)
        .bind(target_id)
        .bind(&context.key)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        if row.request_hash.as_slice() != context.request_hash.as_slice() {
            return Err(RepositoryError::IdempotencyConflict);
        }
        decode_idempotent_response(row).map(Some)
    }

    /// Creates a Silicon-owned schedule and its replay response atomically.
    ///
    /// # Errors
    ///
    /// Returns an idempotency conflict for key reuse with a different request,
    /// [`RepositoryError::SiliconUnavailable`] when the owner's account is no
    /// longer active, an input error, or a database error.
    pub async fn create_schedule_idempotent(
        &self,
        schedule: &CreateSchedule,
        idempotency: &IdempotencyContext,
        audit: &AuditContext,
    ) -> Result<IdempotentMutation<ScheduleRow>, RepositoryError> {
        validate_create_schedule(schedule)?;
        validate_mutating_silicon(idempotency)?;
        let mut transaction = self.pool.begin().await?;
        let reservation =
            reserve_idempotency(&mut transaction, "schedule.create", None, idempotency).await?;
        if let Reservation::Replay {
            status_code,
            response_body,
        } = reservation
        {
            transaction.commit().await?;
            return Ok(IdempotentMutation::Replayed {
                status_code,
                response_body,
            });
        }
        lock_owner_for_write(&mut transaction, schedule.owner_key).await?;
        let keys = super::capacity_keys(&mut transaction, schedule.owner_key).await?;
        let retained: i64 =
            sqlx::query_scalar("SELECT count(*) FROM schedules WHERE owner_principal_id = ANY($1)")
                .bind(&keys)
                .fetch_one(&mut *transaction)
                .await?;
        if retained >= 1000 {
            return Err(RepositoryError::ResourceLimit("reminders"));
        }

        let sql = format!(
            "INSERT INTO schedules (\
                 id, org_id, owner_principal_id, silicon_id, reminder_text, \
                 timezone, schedule_kind, cron_expression, status, next_run_at\
             ) VALUES ($1, NULL, $2, $3, $4, $5, $6, $7, 'active', $8) \
             RETURNING {SCHEDULE_RETURNING_COLUMNS}"
        );
        let row = sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(sql))
            .bind(schedule.id)
            .bind(schedule.owner_key)
            .bind(&schedule.silicon_id)
            .bind(&schedule.text)
            .bind(&schedule.timezone)
            .bind(&schedule.schedule_kind)
            .bind(&schedule.cron)
            .bind(schedule.next_run_at)
            .fetch_one(&mut *transaction)
            .await?;
        append_audit(
            &mut transaction,
            None,
            audit,
            "schedule.created",
            "schedule",
            Some(schedule.id.to_string()),
            json!({
                "owner_uuid": schedule.owner.uuid,
                "owner_silicon_id": schedule.silicon_id,
                "kind": schedule.schedule_kind,
                "version": row.version,
            }),
        )
        .await?;
        let response_body =
            serde_json::to_value(ScheduleResponse::new(&row, Some(&schedule.owner)))?;
        complete_idempotency(&mut transaction, reservation, 201, &response_body).await?;
        transaction.commit().await?;
        Ok(IdempotentMutation::Applied {
            value: row,
            response_body,
        })
    }

    /// Persists a fully merged schedule PATCH with optimistic locking and an
    /// atomic idempotency response.
    ///
    /// # Errors
    ///
    /// Returns not found for a schedule outside the owner's keys, version
    /// conflict for a stale replacement, invalid state for completed schedules,
    /// idempotency conflict for incompatible key reuse, or a database error.
    pub async fn replace_schedule_idempotent(
        &self,
        replacement: &ScheduleReplacement,
        idempotency: &IdempotencyContext,
        audit: &AuditContext,
    ) -> Result<IdempotentMutation<ScheduleRow>, RepositoryError> {
        validate_schedule_replacement(replacement)?;
        validate_mutating_silicon(idempotency)?;
        let mut transaction = self.pool.begin().await?;
        let reservation = reserve_idempotency(
            &mut transaction,
            "schedule.patch",
            Some(replacement.schedule_id),
            idempotency,
        )
        .await?;
        if let Reservation::Replay {
            status_code,
            response_body,
        } = reservation
        {
            transaction.commit().await?;
            return Ok(IdempotentMutation::Replayed {
                status_code,
                response_body,
            });
        }

        // The owner's account row is locked before schedule rows, the same order
        // account lifecycle events use, so the two never deadlock.
        lock_first_owner(&mut transaction, &replacement.owner_keys).await?;
        let lock_sql = format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules s \
             WHERE s.id = $1 AND s.owner_principal_id = ANY($2) AND s.deleted_at IS NULL \
             FOR UPDATE"
        );
        let current = sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(lock_sql))
            .bind(replacement.schedule_id)
            .bind(&replacement.owner_keys)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(RepositoryError::NotFound)?;
        if current.status == "completed" {
            return Err(RepositoryError::InvalidState);
        }
        if current.version != replacement.expected_version {
            return Err(RepositoryError::VersionConflict);
        }

        let update_sql = format!(
            "UPDATE schedules SET \
                 reminder_text = $1, timezone = $2, schedule_kind = $3, \
                 cron_expression = $4, status = $5, next_run_at = $6, \
                 version = version + 1 \
             WHERE id = $7 AND deleted_at IS NULL AND version = $8 \
             RETURNING {SCHEDULE_RETURNING_COLUMNS}"
        );
        let row = sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(update_sql))
            .bind(&replacement.text)
            .bind(&replacement.timezone)
            .bind(&replacement.schedule_kind)
            .bind(&replacement.cron)
            .bind(replacement.status.as_str())
            .bind(replacement.next_run_at)
            .bind(replacement.schedule_id)
            .bind(replacement.expected_version)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(RepositoryError::VersionConflict)?;
        append_audit(
            &mut transaction,
            current.org_id.as_deref(),
            audit,
            "schedule.updated",
            "schedule",
            Some(replacement.schedule_id.to_string()),
            json!({
                "previous_version": current.version,
                "version": row.version,
                "status": row.status,
                "kind": row.schedule_kind,
            }),
        )
        .await?;
        let response_body =
            serde_json::to_value(ScheduleResponse::new(&row, Some(&replacement.owner)))?;
        complete_idempotency(&mut transaction, reservation, 200, &response_body).await?;
        transaction.commit().await?;
        Ok(IdempotentMutation::Applied {
            value: row,
            response_body,
        })
    }

    /// Applies one desired status to a bounded set of the owner's schedules atomically.
    ///
    /// Rows are locked in UUID order; results and the stored response keep API
    /// request order. Schedules already in the desired status are returned
    /// unchanged. Every failure rolls back the whole batch.
    ///
    /// # Errors
    ///
    /// Returns not found when a target is outside the owner's keys, invalid
    /// state for an archived schedule, version conflict for a stale snapshot,
    /// idempotency conflict for incompatible key reuse, or a database error.
    pub async fn replace_schedule_statuses_idempotent(
        &self,
        replacement: &BulkScheduleStatusReplacement,
        idempotency: &IdempotencyContext,
        audit: &AuditContext,
    ) -> Result<IdempotentMutation<Vec<ScheduleRow>>, RepositoryError> {
        validate_bulk_schedule_status_replacement(replacement)?;
        validate_mutating_silicon(idempotency)?;
        let mut transaction = self.pool.begin().await?;
        let reservation =
            reserve_idempotency(&mut transaction, "schedule.bulk_status", None, idempotency)
                .await?;
        if let Reservation::Replay {
            status_code,
            response_body,
        } = reservation
        {
            transaction.commit().await?;
            return Ok(IdempotentMutation::Replayed {
                status_code,
                response_body,
            });
        }
        let rows_by_id = lock_schedule_status_rows(&mut transaction, replacement).await?;
        let rows =
            apply_schedule_status_changes(&mut transaction, replacement, audit, rows_by_id).await?;
        let response_body = serde_json::to_value(ScheduleStatusBatchResponse {
            items: rows.iter().map(ScheduleStatusResponse::from).collect(),
        })?;
        complete_idempotency(&mut transaction, reservation, 200, &response_body).await?;
        transaction.commit().await?;
        Ok(IdempotentMutation::Applied {
            value: rows,
            response_body,
        })
    }

    /// Archives one of the owner's schedules and starts its 45-day retention.
    /// Repeating it, including for an automatically archived one-time reminder,
    /// succeeds without extending retention.
    ///
    /// # Errors
    ///
    /// Returns not found when the schedule is not stored under the owner's keys.
    pub async fn archive_schedule(
        &self,
        owner_keys: &[Uuid],
        schedule_id: Uuid,
        archived_at: DateTime<Utc>,
        audit: &AuditContext,
    ) -> Result<bool, RepositoryError> {
        validate_owner_keys(owner_keys)?;
        let mut transaction = self.pool.begin().await?;
        lock_first_owner(&mut transaction, owner_keys).await?;
        let (status, deleted_at, org_id) =
            sqlx::query_as::<_, (String, Option<DateTime<Utc>>, Option<String>)>(
                "SELECT status, deleted_at, org_id FROM schedules \
                 WHERE owner_principal_id = ANY($1) AND id = $2 \
                   AND (purge_after IS NULL OR purge_after > clock_timestamp()) \
                 FOR UPDATE",
            )
            .bind(owner_keys)
            .bind(schedule_id)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(RepositoryError::NotFound)?;

        let newly_archived = status != "completed" && deleted_at.is_none();
        if newly_archived {
            sqlx::query(
                "UPDATE schedules SET deleted_at = $1, next_run_at = NULL, version = version + 1 \
                 WHERE id = $2",
            )
            .bind(archived_at)
            .bind(schedule_id)
            .execute(&mut *transaction)
            .await?;
        }
        let executions_cancelled = fail_schedule_unaccepted_executions(
            &mut transaction,
            &[schedule_id],
            OWNER_ARCHIVE_FAILURE_REASON,
        )
        .await?;
        if newly_archived {
            append_audit(
                &mut transaction,
                org_id.as_deref(),
                audit,
                "schedule.archived",
                "schedule",
                Some(schedule_id.to_string()),
                json!({
                    "reason": "owner_requested",
                    "retention_days": ARCHIVE_RETENTION_DAYS,
                    "executions_cancelled": executions_cancelled,
                }),
            )
            .await?;
        } else if executions_cancelled > 0 {
            append_audit(
                &mut transaction,
                org_id.as_deref(),
                audit,
                "schedule.delivery_cancelled_after_archive_request",
                "schedule",
                Some(schedule_id.to_string()),
                json!({ "executions_cancelled": executions_cancelled }),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(newly_archived)
    }
}

pub(super) fn validate_create_schedule(schedule: &CreateSchedule) -> Result<(), RepositoryError> {
    validate_silicon_snapshot(&schedule.silicon_id)?;
    validate_schedule_values(
        &schedule.text,
        &schedule.timezone,
        &schedule.schedule_kind,
        &schedule.cron,
    )
}

fn validate_schedule_replacement(replacement: &ScheduleReplacement) -> Result<(), RepositoryError> {
    validate_owner_keys(&replacement.owner_keys)?;
    validate_schedule_values(
        &replacement.text,
        &replacement.timezone,
        &replacement.schedule_kind,
        &replacement.cron,
    )?;
    if replacement.expected_version <= 0 {
        return Err(RepositoryError::InvalidInput(
            "expected schedule version must be positive",
        ));
    }
    Ok(())
}

fn validate_bulk_schedule_status_replacement(
    replacement: &BulkScheduleStatusReplacement,
) -> Result<(), RepositoryError> {
    validate_owner_keys(&replacement.owner_keys)?;
    let schedule_ids = replacement
        .schedules
        .iter()
        .map(|schedule| schedule.id)
        .collect::<Vec<_>>();
    validate_schedule_status_ids(&schedule_ids)?;
    if replacement
        .schedules
        .iter()
        .any(|schedule| schedule.expected_version <= 0)
    {
        return Err(RepositoryError::InvalidInput(
            "expected schedule versions must be positive",
        ));
    }
    if replacement.status == MutableScheduleStatus::Paused
        && replacement
            .schedules
            .iter()
            .any(|schedule| schedule.next_run_at.is_some())
    {
        return Err(RepositoryError::InvalidInput(
            "a paused schedule cannot have a next occurrence",
        ));
    }
    Ok(())
}

fn validate_schedule_status_ids(schedule_ids: &[Uuid]) -> Result<(), RepositoryError> {
    if !(1..=MAX_SCHEDULE_STATUS_BATCH_SIZE).contains(&schedule_ids.len()) {
        return Err(RepositoryError::InvalidInput(
            "schedule status batch must contain 1 to 100 IDs",
        ));
    }
    let unique_ids = schedule_ids.iter().copied().collect::<HashSet<_>>();
    if unique_ids.len() != schedule_ids.len() {
        return Err(RepositoryError::InvalidInput(
            "schedule status batch IDs must be unique",
        ));
    }
    Ok(())
}

fn validate_schedule_values(
    text: &str,
    timezone: &str,
    schedule_kind: &str,
    cron: &str,
) -> Result<(), RepositoryError> {
    if text.trim().is_empty() || text.len() > 100_000 {
        return Err(RepositoryError::InvalidInput(
            "reminder text must contain 1 to 100000 bytes",
        ));
    }
    if timezone.trim().is_empty() || timezone.len() > 255 {
        return Err(RepositoryError::InvalidInput(
            "timezone must be a non-empty identifier",
        ));
    }
    if !matches!(schedule_kind, "one_time" | "recurring") {
        return Err(RepositoryError::InvalidInput(
            "schedule kind must be one_time or recurring",
        ));
    }
    if cron.len() > 1_000 || cron.split_whitespace().count() != 5 {
        return Err(RepositoryError::InvalidInput(
            "cron must contain exactly five fields",
        ));
    }
    Ok(())
}

pub(super) fn validate_schedule_status(status: &str) -> Result<(), RepositoryError> {
    if matches!(status, "active" | "paused" | "completed") {
        Ok(())
    } else {
        Err(RepositoryError::InvalidInput("invalid schedule status"))
    }
}

fn validate_mutating_silicon(idempotency: &IdempotencyContext) -> Result<(), RepositoryError> {
    if idempotency.actor_type == ActorType::Silicon {
        Ok(())
    } else {
        Err(RepositoryError::InvalidInput(
            "only a Silicon changes its reminders",
        ))
    }
}

async fn lock_schedule_status_rows(
    transaction: &mut Transaction<'_, Postgres>,
    replacement: &BulkScheduleStatusReplacement,
) -> Result<HashMap<Uuid, ScheduleRow>, RepositoryError> {
    lock_first_owner(transaction, &replacement.owner_keys).await?;
    let mut lock_ids = replacement
        .schedules
        .iter()
        .map(|schedule| schedule.id)
        .collect::<Vec<_>>();
    lock_ids.sort_unstable();
    let lock_sql = format!(
        "SELECT {SCHEDULE_COLUMNS} FROM schedules s \
         WHERE s.owner_principal_id = ANY($1) AND s.id = ANY($2) \
           AND (s.purge_after IS NULL OR s.purge_after > clock_timestamp()) \
         ORDER BY s.id \
         FOR UPDATE OF s"
    );
    let locked = sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(lock_sql))
        .bind(&replacement.owner_keys)
        .bind(&lock_ids)
        .fetch_all(&mut **transaction)
        .await?;
    if locked.len() != replacement.schedules.len() {
        return Err(RepositoryError::NotFound);
    }
    let rows_by_id = locked
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
    validate_locked_schedule_status_rows(replacement, &rows_by_id)?;
    Ok(rows_by_id)
}

/// Locks the owner's account (every key in `keys` belongs to one account) and
/// refuses the change when that account is no longer active.
async fn lock_first_owner(
    transaction: &mut Transaction<'_, Postgres>,
    keys: &[Uuid],
) -> Result<(), RepositoryError> {
    let key = keys.first().copied().ok_or(RepositoryError::InvalidInput(
        "a change needs the owner's storage keys",
    ))?;
    lock_owner_for_write(transaction, key).await
}

fn validate_locked_schedule_status_rows(
    replacement: &BulkScheduleStatusReplacement,
    rows_by_id: &HashMap<Uuid, ScheduleRow>,
) -> Result<(), RepositoryError> {
    if rows_by_id
        .values()
        .any(|row| row.deleted_at.is_some() || row.status == "completed")
    {
        return Err(RepositoryError::InvalidState);
    }
    if replacement.schedules.iter().any(|change| {
        rows_by_id
            .get(&change.id)
            .is_none_or(|row| row.version != change.expected_version)
    }) {
        return Err(RepositoryError::VersionConflict);
    }
    if replacement.status == MutableScheduleStatus::Active
        && replacement.schedules.iter().any(|change| {
            rows_by_id.get(&change.id).is_some_and(|row| {
                row.status != replacement.status.as_str() && change.next_run_at.is_none()
            })
        })
    {
        return Err(RepositoryError::InvalidInput(
            "a resumed schedule must have a next occurrence",
        ));
    }
    Ok(())
}

async fn apply_schedule_status_changes(
    transaction: &mut Transaction<'_, Postgres>,
    replacement: &BulkScheduleStatusReplacement,
    audit: &AuditContext,
    mut rows_by_id: HashMap<Uuid, ScheduleRow>,
) -> Result<Vec<ScheduleRow>, RepositoryError> {
    let update_sql = format!(
        "UPDATE schedules SET status = $1, next_run_at = $2, version = version + 1 \
         WHERE id = $3 AND owner_principal_id = ANY($4) \
           AND deleted_at IS NULL AND status <> 'completed' AND version = $5 \
         RETURNING {SCHEDULE_RETURNING_COLUMNS}"
    );
    let mut rows = Vec::with_capacity(replacement.schedules.len());
    for change in &replacement.schedules {
        let current = rows_by_id
            .remove(&change.id)
            .ok_or(RepositoryError::NotFound)?;
        if current.status == replacement.status.as_str() {
            rows.push(current);
            continue;
        }
        let row = sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(update_sql.clone()))
            .bind(replacement.status.as_str())
            .bind(change.next_run_at)
            .bind(change.id)
            .bind(&replacement.owner_keys)
            .bind(change.expected_version)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(RepositoryError::VersionConflict)?;
        append_audit(
            transaction,
            current.org_id.as_deref(),
            audit,
            "schedule.status_changed",
            "schedule",
            Some(change.id.to_string()),
            json!({
                "previous_version": current.version,
                "version": row.version,
                "previous_status": current.status,
                "status": row.status,
            }),
        )
        .await?;
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{CreateSchedule, validate_create_schedule};
    use crate::domain::{AccountRef, ActorKind};

    #[test]
    fn create_rejects_an_unknown_schedule_kind() {
        let schedule = CreateSchedule {
            id: Uuid::now_v7(),
            owner_key: Uuid::now_v7(),
            silicon_id: "si:assistant".to_owned(),
            owner: AccountRef {
                uuid: "zQo".to_owned(),
                id: "si:assistant".to_owned(),
                kind: Some(ActorKind::Silicon),
            },
            text: "Prepare report".to_owned(),
            timezone: "Asia/Kolkata".to_owned(),
            schedule_kind: "sometimes".to_owned(),
            cron: "0 9 * * *".to_owned(),
            next_run_at: Utc::now(),
        };
        assert!(validate_create_schedule(&schedule).is_err());
    }
}
