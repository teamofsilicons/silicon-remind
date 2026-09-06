use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, FromRow, PgPool, Postgres, QueryBuilder, Transaction};
use uuid::Uuid;

use crate::domain::{
    ARCHIVE_RETENTION_DAYS, MAX_SCHEDULE_STATUS_BATCH_SIZE, ReminderReadScope, ScheduleSection,
    is_valid_iam_label, silicon_id_belongs_to_org,
};

use super::{
    error::RepositoryError,
    models::{
        ActorType, AuditContext, BulkScheduleStatusReplacement, CreateSchedule, DueMaterialization,
        ExecutionCursor, ExecutionRow, HookDestinationRewrap, HookDestinationRow,
        IamLifecycleOutcome, IdempotencyContext, IdempotentMutation, InternalEventReceiptRow,
        ListSchedules, MutableScheduleStatus, NewHookDestination, NewInternalEvent, Page,
        RevokedResourceCleanup, SchedulePurgeResult, ScheduleReplacement, ScheduleResponse,
        ScheduleRow, ScheduleStatusBatchResponse, ScheduleStatusResponse, SiliconIdentityRow,
        StoredIdempotentResponse,
    },
};

const PUBLIC_PAGE_LIMIT: u32 = 100;
const WORKER_BATCH_LIMIT: u32 = 10_000;
const IAM_INLINE_CLEANUP_LIMIT: i64 = 1_000;
const MAX_FAILURE_REASON_BYTES: usize = 4_096;
const IAM_REVOCATION_FAILURE_REASON: &str = "stopped after IAM access revocation";
const OWNER_ARCHIVE_FAILURE_REASON: &str = "stopped after reminder archive";
pub(super) const DELETED_REMINDER_LEDGER_LIMIT: u32 = 100_000;
const DELETED_REMINDER_LEDGER_LOCK_CLASS: i32 = 0x5349_4c49;
const DELETED_REMINDER_LEDGER_LOCK_KEY: i32 = 1;

const SCHEDULE_COLUMNS: &str = "\
    s.id, s.org_id, s.owner_principal_id, s.silicon_id, s.reminder_text AS text, s.timezone, \
    s.schedule_kind, s.cron_expression AS cron, s.status, s.next_run_at, s.version, \
    s.completed_at, s.deleted_at, s.purge_after, s.created_at, s.updated_at";

const SCHEDULE_RETURNING_COLUMNS: &str = "\
    id, org_id, owner_principal_id, silicon_id, reminder_text AS text, timezone, \
    schedule_kind, cron_expression AS cron, status, next_run_at, version, \
    completed_at, deleted_at, purge_after, created_at, updated_at";

const EXECUTION_COLUMNS: &str = "\
    e.id, e.schedule_id, e.org_id, e.silicon_id, e.schedule_version, \
    e.schedule_kind, e.scheduled_for, e.reminder_text AS text, e.timezone, \
    e.status, e.attempt_count, e.next_attempt_at, e.attempted_at, \
    e.delivered_at, e.hook_event_id, e.failure_reason, e.lease_owner, \
    e.lease_expires_at, e.created_at, e.updated_at";

const EXECUTION_RETURNING_COLUMNS: &str = "\
    id, schedule_id, org_id, silicon_id, schedule_version, schedule_kind, \
    scheduled_for, reminder_text AS text, timezone, status, attempt_count, \
    next_attempt_at, attempted_at, delivered_at, hook_event_id, \
    failure_reason, lease_owner, lease_expires_at, created_at, updated_at";

const DESTINATION_COLUMNS: &str = "\
    d.id, d.org_id, d.owner_principal_id, d.silicon_id, d.endpoint_url_ciphertext, \
    d.endpoint_url_nonce, d.signing_secret_ciphertext, d.signing_secret_nonce, \
    d.encryption_key_version, d.version, d.disabled_at, d.purge_after, \
    d.created_at, d.updated_at";

const DESTINATION_RETURNING_COLUMNS: &str = "\
    id, org_id, owner_principal_id, silicon_id, endpoint_url_ciphertext, endpoint_url_nonce, \
    signing_secret_ciphertext, signing_secret_nonce, encryption_key_version, \
    version, disabled_at, purge_after, created_at, updated_at";

const RECEIPT_COLUMNS: &str = "\
    r.id, r.source, r.event_id, r.event_type, r.org_id, r.subject_id, r.payload, \
    r.payload_hash, r.status, r.attempt_count, r.next_attempt_at, r.processed_at, \
    r.failure_reason, r.lease_owner, r.lease_expires_at, r.received_at, r.updated_at";

const RECEIPT_RETURNING_COLUMNS: &str = "\
    id, source, event_id, event_type, org_id, subject_id, payload, payload_hash, \
    status, attempt_count, next_attempt_at, processed_at, failure_reason, \
    lease_owner, lease_expires_at, received_at, updated_at";

/// Cohesive PostgreSQL implementation used by API and worker application ports.
#[derive(Clone, Debug)]
pub struct PostgresRepository {
    pool: PgPool,
}

impl PostgresRepository {
    /// Creates a repository over an existing process-wide pool.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns the underlying pool for readiness checks and composition.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Resolves an active IAM principal into its immutable public Silicon ID.
    ///
    /// # Errors
    ///
    /// Returns an input error for malformed identifiers or a database error.
    pub async fn get_active_silicon_identity(
        &self,
        org_id: &str,
        principal_id: Uuid,
    ) -> Result<Option<SiliconIdentityRow>, RepositoryError> {
        validate_org_id(org_id)?;
        sqlx::query_as::<_, SiliconIdentityRow>(
            "SELECT identity.org_id, identity.principal_id, identity.silicon_id \
             FROM silicon_identities identity \
             JOIN organization_lifecycle organization \
               ON organization.org_id = identity.org_id \
             WHERE identity.org_id = $1 AND identity.principal_id = $2 \
               AND identity.state = 'active' AND organization.state = 'active'",
        )
        .bind(org_id)
        .bind(principal_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(RepositoryError::from)
    }

    /// Resolves the public Silicon identity only when its Hook destination is
    /// currently enabled.
    ///
    /// A principal that has never been provisioned is reported as a missing
    /// webhook. A durable IAM or organization tombstone remains a distinct
    /// unavailable-identity error so revocation cannot be mistaken for a
    /// recoverable configuration omission.
    ///
    /// # Errors
    ///
    /// Returns an input error for malformed identifiers, a lifecycle error for
    /// a revoked identity, or a database error.
    pub async fn get_schedulable_silicon_identity(
        &self,
        org_id: &str,
        principal_id: Uuid,
    ) -> Result<SiliconIdentityRow, RepositoryError> {
        validate_org_id(org_id)?;
        let row = sqlx::query_as::<_, (String, Uuid, Option<String>, String, String, bool)>(
            "SELECT identity.org_id, identity.principal_id, identity.silicon_id, \
                    identity.state, organization.state, \
                    EXISTS (\
                        SELECT 1 FROM hook_destinations destination \
                        WHERE destination.org_id = identity.org_id \
                          AND destination.owner_principal_id = identity.principal_id \
                          AND destination.silicon_id = identity.silicon_id \
                          AND destination.disabled_at IS NULL\
                    ) AS has_destination \
             FROM silicon_identities identity \
             JOIN organization_lifecycle organization \
               ON organization.org_id = identity.org_id \
             WHERE identity.org_id = $1 AND identity.principal_id = $2",
        )
        .bind(org_id)
        .bind(principal_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some((org_id, principal_id, silicon_id, identity_state, org_state, has_destination)) =
            row
        else {
            return Err(RepositoryError::WebhookNotConfigured);
        };
        if identity_state != "active" || org_state != "active" {
            return Err(RepositoryError::SiliconUnavailable);
        }
        let silicon_id = silicon_id.ok_or(RepositoryError::SiliconUnavailable)?;
        if !has_destination {
            return Err(RepositoryError::WebhookNotConfigured);
        }

        Ok(SiliconIdentityRow {
            org_id,
            principal_id,
            silicon_id,
        })
    }

    /// Begins an explicit transaction for the two-stage due-materialization API.
    ///
    /// # Errors
    ///
    /// Returns a database error when a connection cannot be acquired.
    pub async fn begin(&self) -> Result<Transaction<'_, Postgres>, RepositoryError> {
        self.pool.begin().await.map_err(RepositoryError::from)
    }

    /// Reads one retained, publicly visible schedule within an organization.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn get_schedule(
        &self,
        org_id: &str,
        schedule_id: Uuid,
        read_scope: &ReminderReadScope,
    ) -> Result<Option<ScheduleRow>, RepositoryError> {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules s WHERE s.org_id = "
        ));
        query
            .push_bind(org_id)
            .push(" AND s.id = ")
            .push_bind(schedule_id)
            .push(" AND (s.purge_after IS NULL OR s.purge_after > clock_timestamp())");
        push_reminder_read_scope(&mut query, "s.owner_principal_id", read_scope);
        query.push(
            " AND EXISTS (\
                 SELECT 1 FROM silicon_identities identity \
                 JOIN organization_lifecycle organization \
                   ON organization.org_id = identity.org_id \
                 WHERE identity.org_id = s.org_id \
                   AND identity.principal_id = s.owner_principal_id \
                   AND identity.state = 'active' \
                   AND organization.state = 'active'\
             )",
        );

        query
            .build_query_as::<ScheduleRow>()
            .fetch_optional(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Reads a bounded set of retained schedules for desired-status validation.
    ///
    /// The result is UUID-ordered and intentionally does not filter by owner.
    /// This lets the application distinguish an invisible ID from a visible
    /// same-organization schedule owned by another Silicon. Retained archived
    /// rows are included so lifecycle errors can be reported after visibility
    /// and ownership checks.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid organization or ID batch and a
    /// database error when the query fails.
    pub async fn get_schedules_for_status_change(
        &self,
        org_id: &str,
        schedule_ids: &[Uuid],
    ) -> Result<Vec<ScheduleRow>, RepositoryError> {
        validate_org_id(org_id)?;
        validate_schedule_status_ids(schedule_ids)?;

        let sql = format!(
            "SELECT {SCHEDULE_COLUMNS} \
             FROM schedules s \
             JOIN silicon_identities identity \
               ON identity.org_id = s.org_id \
              AND identity.principal_id = s.owner_principal_id \
              AND identity.silicon_id = s.silicon_id \
             JOIN organization_lifecycle organization \
               ON organization.org_id = s.org_id \
             WHERE s.org_id = $1 AND s.id = ANY($2) \
               AND (s.purge_after IS NULL OR s.purge_after > clock_timestamp()) \
               AND identity.state = 'active' AND organization.state = 'active' \
             ORDER BY s.id"
        );
        sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(sql))
            .bind(org_id)
            .bind(schedule_ids)
            .fetch_all(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Lists schedules using the stable `(created_at DESC, id DESC)` keyset.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid limit or status and a database
    /// error when PostgreSQL rejects the query.
    pub async fn list_schedules(
        &self,
        filters: &ListSchedules,
    ) -> Result<Page<ScheduleRow>, RepositoryError> {
        validate_org_id(&filters.org_id)?;
        if let Some(silicon_id) = filters.silicon_id.as_deref() {
            validate_global_silicon_id(silicon_id, &filters.org_id)?;
        }
        validate_public_limit(filters.limit)?;
        if let Some(status) = filters.status.as_deref() {
            validate_schedule_status(status)?;
        }

        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules s \
             WHERE s.org_id = "
        ));
        query.push_bind(&filters.org_id);
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
        push_reminder_read_scope(&mut query, "s.owner_principal_id", &filters.read_scope);
        query.push(
            " AND EXISTS (\
                 SELECT 1 FROM silicon_identities identity \
                 JOIN organization_lifecycle organization \
                   ON organization.org_id = identity.org_id \
                 WHERE identity.org_id = s.org_id \
                   AND identity.principal_id = s.owner_principal_id \
                   AND identity.state = 'active' \
                   AND organization.state = 'active'\
             )",
        );

        if let Some(silicon_id) = filters.silicon_id.as_deref() {
            query.push(" AND s.silicon_id = ").push_bind(silicon_id);
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

    /// Reads a visible execution by organization and stable execution UUID.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn get_execution(
        &self,
        org_id: &str,
        execution_id: Uuid,
        read_scope: &ReminderReadScope,
    ) -> Result<Option<ExecutionRow>, RepositoryError> {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT {EXECUTION_COLUMNS} FROM executions e \
             JOIN schedules s ON s.id = e.schedule_id \
             JOIN silicon_identities identity \
               ON identity.org_id = s.org_id \
              AND identity.principal_id = s.owner_principal_id \
             JOIN organization_lifecycle organization \
               ON organization.org_id = s.org_id \
             WHERE e.org_id = "
        ));
        query
            .push_bind(org_id)
            .push(" AND e.id = ")
            .push_bind(execution_id)
            .push(" AND (s.purge_after IS NULL OR s.purge_after > clock_timestamp())");
        push_reminder_read_scope(&mut query, "s.owner_principal_id", read_scope);
        query.push(" AND identity.state = 'active' AND organization.state = 'active'");

        query
            .build_query_as::<ExecutionRow>()
            .fetch_optional(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Lists one schedule's execution history using keyset pagination.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError::NotFound`] when the schedule is absent from
    /// the organization, an input error for an invalid limit, or a database
    /// error when either query fails.
    pub async fn list_executions(
        &self,
        org_id: &str,
        schedule_id: Uuid,
        read_scope: &ReminderReadScope,
        cursor: Option<ExecutionCursor>,
        limit: u32,
    ) -> Result<Page<ExecutionRow>, RepositoryError> {
        validate_public_limit(limit)?;
        if self
            .get_schedule(org_id, schedule_id, read_scope)
            .await?
            .is_none()
        {
            return Err(RepositoryError::NotFound);
        }

        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT {EXECUTION_COLUMNS} FROM executions e \
             WHERE e.org_id = "
        ));
        query
            .push_bind(org_id)
            .push(" AND e.schedule_id = ")
            .push_bind(schedule_id)
            .push(
                " AND EXISTS (\
                     SELECT 1 FROM schedules schedule \
                     JOIN silicon_identities identity \
                       ON identity.org_id = schedule.org_id \
                      AND identity.principal_id = schedule.owner_principal_id \
                     JOIN organization_lifecycle organization \
                       ON organization.org_id = schedule.org_id \
                     WHERE schedule.id = e.schedule_id \
                       AND identity.state = 'active' \
                       AND organization.state = 'active' \
                       AND (schedule.purge_after IS NULL \
                            OR schedule.purge_after > clock_timestamp())",
            );
        push_reminder_read_scope(&mut query, "schedule.owner_principal_id", read_scope);
        query.push(")");

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
    /// reserving or mutating it.
    ///
    /// This read allows application services to replay a committed create or
    /// PATCH response before time-sensitive domain validation. It does not
    /// replace the transactional reservation performed by the mutation method;
    /// a concurrent miss must still race through that authoritative path.
    ///
    /// # Errors
    ///
    /// Returns an idempotency conflict when the scope exists with a different
    /// request hash, an incomplete error for a malformed committed record, an
    /// input error for an invalid scope, or a database error.
    pub async fn find_idempotent_response(
        &self,
        org_id: &str,
        operation: &str,
        target_id: Option<Uuid>,
        context: &IdempotencyContext,
    ) -> Result<Option<StoredIdempotentResponse>, RepositoryError> {
        validate_idempotency_context(context)?;
        validate_idempotency_operation(operation)?;

        let row = sqlx::query_as::<_, IdempotencyRow>(
            "SELECT request_hash, state, response_status, response_body \
             FROM idempotency_records \
             WHERE org_id = $1 AND actor_type = $2 AND actor_id = $3 \
               AND operation = $4 AND target_id IS NOT DISTINCT FROM $5 \
               AND idempotency_key = $6 AND expires_at > clock_timestamp()",
        )
        .bind(org_id)
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
    /// an input error for inconsistent schedule values, or a database error.
    pub async fn create_schedule_idempotent(
        &self,
        schedule: &CreateSchedule,
        idempotency: &IdempotencyContext,
        audit: &AuditContext,
    ) -> Result<IdempotentMutation<ScheduleRow>, RepositoryError> {
        validate_create_schedule(schedule)?;
        validate_mutating_silicon(idempotency, schedule.owner_principal_id)?;

        let mut transaction = self.pool.begin().await?;
        let reservation = reserve_idempotency(
            &mut transaction,
            &schedule.org_id,
            "schedule.create",
            None,
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

        lock_schedulable_silicon_identity(
            &mut transaction,
            &schedule.org_id,
            schedule.owner_principal_id,
            &schedule.silicon_id,
        )
        .await?;

        let sql = format!(
            "INSERT INTO schedules (\
                 id, org_id, owner_principal_id, silicon_id, reminder_text, \
                 timezone, schedule_kind, cron_expression, status, next_run_at\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active', $9) \
             RETURNING {SCHEDULE_RETURNING_COLUMNS}"
        );
        let row = sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(sql))
            .bind(schedule.id)
            .bind(&schedule.org_id)
            .bind(schedule.owner_principal_id)
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
            Some(&schedule.org_id),
            audit,
            "schedule.created",
            "schedule",
            Some(schedule.id.to_string()),
            json!({
                "owner_silicon_id": schedule.silicon_id,
                "kind": schedule.schedule_kind,
                "version": row.version,
            }),
        )
        .await?;

        let response_body = serde_json::to_value(ScheduleResponse::from(&row))?;
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
    /// Returns not found for the wrong tenant/owner, version conflict for a
    /// stale replacement, invalid state for completed schedules, idempotency
    /// conflict for incompatible key reuse, or a database error.
    pub async fn replace_schedule_idempotent(
        &self,
        replacement: &ScheduleReplacement,
        idempotency: &IdempotencyContext,
        audit: &AuditContext,
    ) -> Result<IdempotentMutation<ScheduleRow>, RepositoryError> {
        validate_schedule_replacement(replacement)?;
        validate_mutating_silicon(idempotency, replacement.owner_principal_id)?;

        let mut transaction = self.pool.begin().await?;
        let reservation = reserve_idempotency(
            &mut transaction,
            &replacement.org_id,
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

        let lock_sql = format!(
            "SELECT {SCHEDULE_COLUMNS} \
             FROM schedules s \
             WHERE s.org_id = $1 AND s.owner_principal_id = $2 AND s.id = $3 \
               AND s.deleted_at IS NULL \
             FOR UPDATE"
        );
        let current = sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(lock_sql))
            .bind(&replacement.org_id)
            .bind(replacement.owner_principal_id)
            .bind(replacement.schedule_id)
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
             WHERE id = $7 AND org_id = $8 AND owner_principal_id = $9 \
               AND deleted_at IS NULL AND version = $10 \
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
            .bind(&replacement.org_id)
            .bind(replacement.owner_principal_id)
            .bind(replacement.expected_version)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(RepositoryError::VersionConflict)?;

        append_audit(
            &mut transaction,
            Some(&replacement.org_id),
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

        let response_body = serde_json::to_value(ScheduleResponse::from(&row))?;
        complete_idempotency(&mut transaction, reservation, 200, &response_body).await?;
        transaction.commit().await?;

        Ok(IdempotentMutation::Applied {
            value: row,
            response_body,
        })
    }

    /// Applies one desired status to a bounded set of schedules atomically.
    ///
    /// Rows are locked in UUID order to prevent caller-controlled lock-order
    /// inversions. The returned values and exact idempotency response retain
    /// API request order. Schedules already in the desired status are returned
    /// without changing their version, timestamp, next occurrence, or audit
    /// history.
    ///
    /// # Errors
    ///
    /// Returns not found when any target is outside the active tenant/IAM
    /// projection or owner scope, invalid state for an archived schedule,
    /// version conflict when any application snapshot is stale, idempotency
    /// conflict for incompatible key reuse, or a database error. Every failure
    /// rolls back the whole batch.
    pub async fn replace_schedule_statuses_idempotent(
        &self,
        replacement: &BulkScheduleStatusReplacement,
        idempotency: &IdempotencyContext,
        audit: &AuditContext,
    ) -> Result<IdempotentMutation<Vec<ScheduleRow>>, RepositoryError> {
        validate_bulk_schedule_status_replacement(replacement)?;
        validate_mutating_silicon(idempotency, replacement.owner_principal_id)?;

        let mut transaction = self.pool.begin().await?;
        let reservation = reserve_idempotency(
            &mut transaction,
            &replacement.org_id,
            "schedule.bulk_status",
            None,
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

    /// Archives an owner-visible schedule and starts its 45-day retention
    /// window. Repeating the operation, including for an automatically
    /// archived one-time reminder, succeeds without extending retention.
    ///
    /// # Errors
    ///
    /// Returns not found when ownership cannot be established or a database
    /// error when the transaction fails.
    pub async fn archive_schedule(
        &self,
        org_id: &str,
        owner_principal_id: Uuid,
        schedule_id: Uuid,
        archived_at: DateTime<Utc>,
        audit: &AuditContext,
    ) -> Result<bool, RepositoryError> {
        let mut transaction = self.pool.begin().await?;
        let (status, deleted_at) = sqlx::query_as::<_, (String, Option<DateTime<Utc>>)>(
            "SELECT status, deleted_at \
             FROM schedules \
             WHERE org_id = $1 AND owner_principal_id = $2 AND id = $3 \
               AND (purge_after IS NULL OR purge_after > clock_timestamp()) \
             FOR UPDATE",
        )
        .bind(org_id)
        .bind(owner_principal_id)
        .bind(schedule_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(RepositoryError::NotFound)?;

        let newly_archived = status != "completed" && deleted_at.is_none();
        if newly_archived {
            sqlx::query(
                "UPDATE schedules SET \
                     deleted_at = $1, next_run_at = NULL, version = version + 1 \
                 WHERE id = $2",
            )
            .bind(archived_at)
            .bind(schedule_id)
            .execute(&mut *transaction)
            .await?;
        }

        let schedule_ids = [schedule_id];
        let executions_cancelled = fail_schedule_unaccepted_executions(
            &mut transaction,
            &schedule_ids,
            OWNER_ARCHIVE_FAILURE_REASON,
        )
        .await?;

        if newly_archived {
            append_audit(
                &mut transaction,
                Some(org_id),
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
                Some(org_id),
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

impl PostgresRepository {
    /// Locks a bounded batch of due schedules in deterministic order.
    ///
    /// The caller must calculate the recurring schedule's first future
    /// occurrence, call [`Self::materialize_locked_occurrence`] for every row,
    /// and then commit the same transaction. Dropping or rolling back the
    /// transaction releases all claims without changing scheduler state.
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
            "SELECT {SCHEDULE_COLUMNS} \
             FROM schedules s \
             JOIN silicon_identities identity \
               ON identity.org_id = s.org_id \
              AND identity.principal_id = s.owner_principal_id \
             JOIN organization_lifecycle organization \
               ON organization.org_id = s.org_id \
             WHERE s.status = 'active' AND s.deleted_at IS NULL \
               AND identity.state = 'active' AND organization.state = 'active' \
               AND s.next_run_at IS NOT NULL AND s.next_run_at <= $1 \
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

    /// Materializes one occurrence from a schedule locked by
    /// [`Self::lock_due_schedules`] and advances scheduler state atomically.
    ///
    /// For recurring schedules, `next_run_at` must be the first occurrence
    /// strictly after `worker_now`. For one-time schedules it must be `None`.
    /// The immutable execution snapshot protects an already-materialized
    /// reminder from later schedule edits.
    ///
    /// # Errors
    ///
    /// Returns invalid input/state when the supplied row is no longer due or
    /// the advancement violates the selected schedule kind, and a database
    /// error when materialization fails.
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
                "SELECT {EXECUTION_COLUMNS} \
                 FROM executions e \
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
                 status = CASE \
                     WHEN schedule_kind = 'one_time' THEN 'completed' \
                     ELSE status \
                 END, \
                 completed_at = CASE \
                     WHEN schedule_kind = 'one_time' THEN $2 \
                     ELSE completed_at \
                 END, \
                 version = version + 1 \
             WHERE id = $3 AND org_id = $4 AND silicon_id = $5 \
               AND status = 'active' AND deleted_at IS NULL \
               AND next_run_at IS NOT DISTINCT FROM $6 AND version = $7 \
             RETURNING version",
        )
        .bind(next_run_at)
        .bind(worker_now)
        .bind(schedule.id)
        .bind(&schedule.org_id)
        .bind(&schedule.silicon_id)
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

    /// Acquires bounded Hook-delivery leases with `FOR UPDATE SKIP LOCKED`.
    /// Expired leases are safely reclaimable; deleted schedules are excluded.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid worker, lease, or batch size and a
    /// database error when claiming fails.
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
                 SELECT e.id \
                 FROM executions e \
                 JOIN schedules s ON s.id = e.schedule_id \
                 JOIN silicon_identities identity \
                   ON identity.org_id = s.org_id \
                  AND identity.principal_id = s.owner_principal_id \
                 JOIN organization_lifecycle organization \
                   ON organization.org_id = s.org_id \
                 WHERE e.status IN ('pending', 'retrying') \
                   AND e.next_attempt_at <= $1 \
                   AND (e.lease_expires_at IS NULL OR e.lease_expires_at <= $1) \
                   AND s.deleted_at IS NULL \
                   AND (s.purge_after IS NULL OR s.purge_after > $1) \
                   AND identity.state = 'active' AND organization.state = 'active' \
                 ORDER BY e.next_attempt_at, e.id \
                 FOR UPDATE OF e SKIP LOCKED \
                 LIMIT $2\
             ) \
             UPDATE executions e SET \
                 status = CASE \
                     WHEN e.attempt_count = 0 THEN 'pending' \
                     ELSE 'retrying' \
                 END, \
                 attempt_count = e.attempt_count + 1, \
                 attempted_at = $1, lease_owner = $3, lease_expires_at = $4 \
             FROM candidates c \
             WHERE e.id = c.id \
             RETURNING {EXECUTION_COLUMNS}"
        );
        let rows = sqlx::query_as::<_, ExecutionRow>(AssertSqlSafe(sql))
            .bind(now)
            .bind(i64::from(limit))
            .bind(worker_id)
            .bind(lease_expires_at)
            .fetch_all(&mut *transaction)
            .await?;

        let audit = AuditContext {
            actor_type: ActorType::System,
            actor_id: worker_id.to_owned(),
            request_id: None,
        };
        for row in &rows {
            append_audit(
                &mut transaction,
                Some(&row.org_id),
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

    /// Revalidates a claimed delivery immediately before outbound I/O.
    ///
    /// The lease must still belong to this worker, and its schedule, IAM
    /// identity, organization, and retention window must all remain live.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid worker identifier or a database
    /// error when the eligibility query fails.
    pub async fn delivery_lease_is_live(
        &self,
        execution_id: Uuid,
        worker_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, RepositoryError> {
        validate_worker_id(worker_id)?;
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (\
                 SELECT 1 FROM executions e \
                 JOIN schedules s ON s.id = e.schedule_id \
                 JOIN silicon_identities identity \
                   ON identity.org_id = s.org_id \
                  AND identity.principal_id = s.owner_principal_id \
                 JOIN organization_lifecycle organization \
                   ON organization.org_id = s.org_id \
                 WHERE e.id = $1 AND e.status IN ('pending', 'retrying') \
                   AND e.lease_owner = $2 AND e.lease_expires_at > $3 \
                   AND s.deleted_at IS NULL \
                   AND (s.purge_after IS NULL OR s.purge_after > $3) \
                   AND identity.state = 'active' \
                   AND organization.state = 'active'\
             )",
        )
        .bind(execution_id)
        .bind(worker_id)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::from)
    }

    /// Marks a leased execution delivered to ingress after a validated Hook `200`.
    /// The historical `hook_event_id` column stores the receipt, which does not prove
    /// signature verification or downstream consumer acknowledgment.
    ///
    /// # Errors
    ///
    /// Returns lease lost when this worker no longer owns a live claim, or a
    /// database error when the transition fails.
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
               AND EXISTS (\
                   SELECT 1 FROM schedules s \
                   WHERE s.id = e.schedule_id AND s.deleted_at IS NULL \
                     AND (s.purge_after IS NULL OR s.purge_after > $1)\
               ) \
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

        let audit = AuditContext {
            actor_type: ActorType::System,
            actor_id: worker_id.to_owned(),
            request_id: None,
        };
        append_audit(
            &mut transaction,
            Some(&execution.org_id),
            &audit,
            "execution.delivered",
            "execution",
            Some(execution.id.to_string()),
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
    /// Returns invalid input for an unbounded reason or non-future retry time,
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
               AND EXISTS (\
                   SELECT 1 FROM schedules s \
                   WHERE s.id = e.schedule_id AND s.deleted_at IS NULL \
                     AND (s.purge_after IS NULL OR s.purge_after > $5)\
               ) \
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
               AND EXISTS (\
                   SELECT 1 FROM schedules s \
                   WHERE s.id = e.schedule_id AND s.deleted_at IS NULL \
                     AND (s.purge_after IS NULL OR s.purge_after > $4)\
               ) \
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

impl PostgresRepository {
    /// Creates or atomically replaces encrypted Hook destination material.
    /// No plaintext endpoint or secret crosses this persistence boundary.
    ///
    /// # Errors
    ///
    /// Returns an input error for empty ciphertext or a non-positive key
    /// version, and a database error when replacement fails.
    pub async fn upsert_hook_destination(
        &self,
        destination: &NewHookDestination,
        audit: &AuditContext,
    ) -> Result<HookDestinationRow, RepositoryError> {
        validate_hook_destination(destination)?;
        let mut transaction = self.pool.begin().await?;
        register_active_silicon_identity(&mut transaction, destination).await?;
        let sql = format!(
            "INSERT INTO hook_destinations (\
                 id, org_id, owner_principal_id, silicon_id, endpoint_url_ciphertext, \
                 endpoint_url_nonce, signing_secret_ciphertext, \
                 signing_secret_nonce, encryption_key_version\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (org_id, owner_principal_id) DO UPDATE SET \
                 endpoint_url_ciphertext = EXCLUDED.endpoint_url_ciphertext, \
                 endpoint_url_nonce = EXCLUDED.endpoint_url_nonce, \
                 signing_secret_ciphertext = EXCLUDED.signing_secret_ciphertext, \
                 signing_secret_nonce = EXCLUDED.signing_secret_nonce, \
                 encryption_key_version = EXCLUDED.encryption_key_version, \
                 disabled_at = NULL, version = hook_destinations.version + 1 \
             RETURNING {DESTINATION_RETURNING_COLUMNS}"
        );
        let row = sqlx::query_as::<_, HookDestinationRow>(AssertSqlSafe(sql))
            .bind(destination.id)
            .bind(&destination.org_id)
            .bind(destination.owner_principal_id)
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
            Some(&destination.org_id),
            audit,
            if row.version == 1 {
                "hook_destination.created"
            } else {
                "hook_destination.replaced"
            },
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

    /// Reads active encrypted destination material for one tenant Silicon.
    ///
    /// # Errors
    ///
    /// Returns a database error when the lookup fails.
    pub async fn get_hook_destination(
        &self,
        org_id: &str,
        silicon_id: &str,
    ) -> Result<Option<HookDestinationRow>, RepositoryError> {
        let sql = format!(
            "SELECT {DESTINATION_COLUMNS} \
             FROM hook_destinations d \
             JOIN silicon_identities identity \
               ON identity.org_id = d.org_id \
              AND identity.principal_id = d.owner_principal_id \
             JOIN organization_lifecycle organization \
               ON organization.org_id = d.org_id \
             WHERE d.org_id = $1 AND d.silicon_id = $2 \
               AND d.disabled_at IS NULL AND identity.state = 'active' \
               AND organization.state = 'active'"
        );
        sqlx::query_as::<_, HookDestinationRow>(AssertSqlSafe(sql))
            .bind(org_id)
            .bind(silicon_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Disables a destination without retaining plaintext or deleting audit
    /// history. Repeating the operation is a no-op.
    ///
    /// # Errors
    ///
    /// Returns not found for an unknown tenant Silicon or a database error.
    pub async fn disable_hook_destination(
        &self,
        org_id: &str,
        silicon_id: &str,
        disabled_at: DateTime<Utc>,
        audit: &AuditContext,
    ) -> Result<bool, RepositoryError> {
        let mut transaction = self.pool.begin().await?;
        let existing = sqlx::query_as::<_, (Uuid, Option<DateTime<Utc>>)>(
            "SELECT id, disabled_at FROM hook_destinations \
             WHERE org_id = $1 AND silicon_id = $2 FOR UPDATE",
        )
        .bind(org_id)
        .bind(silicon_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(RepositoryError::NotFound)?;

        if existing.1.is_some() {
            transaction.commit().await?;
            return Ok(false);
        }
        sqlx::query(
            "UPDATE hook_destinations SET disabled_at = $1, version = version + 1 \
             WHERE id = $2",
        )
        .bind(disabled_at)
        .bind(existing.0)
        .execute(&mut *transaction)
        .await?;
        append_audit(
            &mut transaction,
            Some(org_id),
            audit,
            "hook_destination.disabled",
            "hook_destination",
            Some(existing.0.to_string()),
            json!({ "silicon_id": silicon_id }),
        )
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    /// Lists a bounded active batch still encrypted with a historical key.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid key version or limit, or a
    /// database error.
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
            "SELECT {DESTINATION_COLUMNS} \
             FROM hook_destinations d \
             JOIN silicon_identities identity \
               ON identity.org_id = d.org_id \
              AND identity.principal_id = d.owner_principal_id \
             JOIN organization_lifecycle organization \
               ON organization.org_id = d.org_id \
             WHERE d.disabled_at IS NULL AND d.encryption_key_version <> $1 \
               AND identity.state = 'active' AND organization.state = 'active' \
             ORDER BY d.updated_at, d.id LIMIT $2"
        );
        sqlx::query_as::<_, HookDestinationRow>(AssertSqlSafe(sql))
            .bind(current_key_version)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(RepositoryError::from)
    }

    /// Replaces one destination's ciphertext under optimistic locking.
    ///
    /// # Errors
    ///
    /// Returns an input error for malformed encrypted material or a database
    /// error. A concurrent rotation returns `false` without overwriting it.
    pub async fn rewrap_hook_destination(
        &self,
        rewrap: &HookDestinationRewrap,
        audit: &AuditContext,
    ) -> Result<bool, RepositoryError> {
        validate_destination_rewrap(rewrap)?;
        let mut transaction = self.pool.begin().await?;
        let current = sqlx::query_as::<_, (String, String, i16)>(
            "SELECT org_id, silicon_id, encryption_key_version \
             FROM hook_destinations \
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
            Some(&org_id),
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

    /// Records an authenticated internal event exactly once by source/event ID.
    /// A duplicate with an identical payload returns the original receipt;
    /// reusing the ID with different bytes is rejected.
    ///
    /// # Errors
    ///
    /// Returns a receipt conflict for changed payloads, an input error for an
    /// invalid envelope, or a database error.
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
                json!({
                    "source": event.source,
                    "event_type": event.event_type,
                }),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok((receipt, was_inserted))
    }

    /// Records an authenticated IAM event as a durable processed no-op.
    ///
    /// This is used for valid subscribed IAM event types which do not mutate
    /// Remind state. Exact replay returns the original processed receipt;
    /// changing any envelope field under the same sender event ID conflicts.
    ///
    /// # Errors
    ///
    /// Returns a receipt conflict for a changed replay, invalid state for an
    /// incomplete legacy receipt, input errors for malformed events, or a
    /// database error.
    pub async fn record_processed_internal_event(
        &self,
        event: &NewInternalEvent,
        audit: &AuditContext,
    ) -> Result<(InternalEventReceiptRow, bool), RepositoryError> {
        validate_internal_event(event)?;
        let mut transaction = self.pool.begin().await?;
        let Some(inserted) = insert_internal_event_receipt(&mut transaction, event).await? else {
            let receipt = lock_internal_event_receipt(&mut transaction, event).await?;
            if !receipt_matches_event(&receipt, event) {
                return Err(RepositoryError::EventReceiptConflict);
            }
            if receipt.status != "processed" {
                return Err(RepositoryError::InvalidState);
            }
            transaction.commit().await?;
            return Ok((receipt, false));
        };

        let processed_at = sqlx::query_scalar::<_, DateTime<Utc>>("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await?;
        let receipt =
            mark_internal_event_processed(&mut transaction, inserted.id, processed_at).await?;
        append_audit(
            &mut transaction,
            event.org_id.as_deref(),
            audit,
            "internal_event.processed_noop",
            "internal_event_receipt",
            Some(receipt.id.to_string()),
            json!({
                "source": event.source,
                "event_type": event.event_type,
            }),
        )
        .await?;
        transaction.commit().await?;
        Ok((receipt, true))
    }

    /// Atomically records and applies a validated IAM membership-removal event.
    ///
    /// Supported lifecycle projections are a Silicon
    /// `organization.membership.removed.v1`, scoped to `org_id` plus its IAM
    /// principal UUID, and a disabled `organization.updated.v1`, scoped to
    /// `org_id`. An irreversible lifecycle tombstone is committed before
    /// bounded schedule, execution, and destination cleanup. An exact replay
    /// returns its original receipt without repeating changes.
    ///
    /// # Errors
    ///
    /// Returns an event receipt conflict when a source/event ID is reused with
    /// a different envelope or hash, invalid input for a non-IAM or unsupported
    /// lifecycle event, invalid state for a legacy incomplete duplicate, or a
    /// database error when the transaction cannot commit atomically.
    pub async fn apply_iam_lifecycle_event(
        &self,
        event: &NewInternalEvent,
        audit: &AuditContext,
    ) -> Result<IamLifecycleOutcome, RepositoryError> {
        validate_internal_event(event)?;
        let target = validate_iam_lifecycle_event(event)?;
        let mut transaction = self.pool.begin().await?;
        let Some(receipt) = insert_internal_event_receipt(&mut transaction, event).await? else {
            let outcome = load_iam_lifecycle_replay(&mut transaction, event).await?;
            transaction.commit().await?;
            return Ok(outcome);
        };

        let processed_at = sqlx::query_scalar::<_, DateTime<Utc>>("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await?;
        block_iam_target(&mut transaction, &target, processed_at).await?;
        let schedules_deleted =
            soft_delete_iam_schedules(&mut transaction, &target, processed_at).await?;
        let executions_failed = fail_iam_target_unaccepted_executions(
            &mut transaction,
            &target,
            IAM_INLINE_CLEANUP_LIMIT,
        )
        .await?;
        disable_iam_destinations(&mut transaction, &target, processed_at).await?;
        let processed_receipt =
            mark_internal_event_processed(&mut transaction, receipt.id, processed_at).await?;
        let outcome = IamLifecycleOutcome {
            receipt: processed_receipt,
            replayed: false,
            schedules_deleted,
            executions_failed,
        };
        append_iam_lifecycle_audits(&mut transaction, &target, event, audit, &outcome).await?;

        transaction.commit().await?;
        Ok(outcome)
    }

    /// Applies every revocation in one verified IAM projection atomically with its receipt.
    ///
    /// # Errors
    ///
    /// Returns event-id conflicts, invalid IAM input, or a database failure; all projected revocations share one transaction.
    pub async fn apply_iam_projection(
        &self,
        event: &NewInternalEvent,
        organization_revoked: bool,
        mut principals: Vec<Uuid>,
        audit: &AuditContext,
    ) -> Result<InternalEventReceiptRow, RepositoryError> {
        validate_internal_event(event)?;
        if event.source != "silicon-iam" {
            return Err(RepositoryError::InvalidInput("invalid IAM source"));
        }
        let mut tx = self.pool.begin().await?;
        let Some(receipt) = insert_internal_event_receipt(&mut tx, event).await? else {
            let replay = load_iam_lifecycle_replay(&mut tx, event).await?;
            tx.commit().await?;
            return Ok(replay.receipt);
        };
        let now = sqlx::query_scalar::<_, DateTime<Utc>>("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let mut targets = Vec::new();
        if let Some(org_id) = &event.org_id {
            if !is_valid_iam_label(org_id) {
                return Err(RepositoryError::InvalidInput("invalid IAM organization"));
            }
            if organization_revoked {
                targets.push(IamLifecycleTarget::Organization {
                    org_id: org_id.clone(),
                });
            } else {
                principals.sort_unstable();
                principals.dedup();
                for principal_id in principals {
                    targets.push(IamLifecycleTarget::Silicon {
                        org_id: org_id.clone(),
                        principal_id,
                    });
                }
            }
        }
        for target in targets {
            block_iam_target(&mut tx, &target, now).await?;
            soft_delete_iam_schedules(&mut tx, &target, now).await?;
            fail_iam_target_unaccepted_executions(&mut tx, &target, IAM_INLINE_CLEANUP_LIMIT)
                .await?;
            disable_iam_destinations(&mut tx, &target, now).await?;
            append_audit(
                &mut tx,
                Some(target.org_id()),
                audit,
                "iam.authority_revoked",
                target.resource_type(),
                Some(target.resource_id()),
                json!({"event_id":event.event_id}),
            )
            .await?;
        }
        let receipt = mark_internal_event_processed(&mut tx, receipt.id, now).await?;
        tx.commit().await?;
        Ok(receipt)
    }

    /// Drains resources left behind by bounded IAM webhook cleanup.
    ///
    /// Durable organization/principal tombstones already prevent creation,
    /// materialization, and delivery. This worker pass makes archival and
    /// encrypted-destination retention progress without an unbounded webhook
    /// transaction.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid worker or limit, or a database
    /// error. The schedule, execution, destination, and audit changes commit in
    /// one transaction.
    pub async fn cleanup_revoked_resources(
        &self,
        now: DateTime<Utc>,
        limit: u32,
        worker_id: &str,
    ) -> Result<RevokedResourceCleanup, RepositoryError> {
        validate_worker_limit(limit)?;
        validate_worker_id(worker_id)?;
        let mut transaction = self.pool.begin().await?;
        let (schedules, schedules_deleted, executions_failed) =
            cleanup_revoked_schedules(&mut transaction, now, limit).await?;
        let destinations = cleanup_revoked_destinations(&mut transaction, now, limit).await?;
        let audit = AuditContext {
            actor_type: ActorType::System,
            actor_id: worker_id.to_owned(),
            request_id: None,
        };
        append_revocation_cleanup_audits(&mut transaction, &audit, &schedules, &destinations)
            .await?;
        let destinations_disabled = u64::try_from(destinations.len()).map_err(|_| {
            RepositoryError::InvalidInput("database row count exceeds supported integer range")
        })?;
        transaction.commit().await?;
        Ok(RevokedResourceCleanup {
            schedules_deleted,
            executions_failed,
            destinations_disabled,
        })
    }

    /// Permanently deletes completed/deleted schedules whose 45-day deadline
    /// has elapsed, including cascading execution history, in a bounded batch.
    /// Every deletion receives a durable deleted-reminder record and redacted
    /// audit record before removal. The same transaction preserves only the
    /// newest 100,000 global ledger records.
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
        acquire_retention_advisory_lock(&mut transaction).await?;
        let candidates = select_expired_schedule_snapshots(&mut transaction, now, limit).await?;
        let audit = AuditContext {
            actor_type: ActorType::System,
            actor_id: worker_id.to_owned(),
            request_id: None,
        };
        for candidate in &candidates {
            write_deleted_reminder(&mut transaction, candidate, now, &audit).await?;
        }

        let logged = row_count(candidates.len())?;
        let purged = delete_purged_schedules(&mut transaction, &candidates).await?;
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

    /// Permanently deletes disabled Hook destination ciphertext after its
    /// 45-day recovery window, in a bounded multi-worker-safe batch.
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
            "SELECT id, org_id, silicon_id, disabled_at, purge_after \
             FROM hook_destinations \
             WHERE purge_after IS NOT NULL AND purge_after <= $1 \
             ORDER BY purge_after, id \
             FOR UPDATE SKIP LOCKED LIMIT $2",
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
                Some(&candidate.org_id),
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

    /// Deletes expired idempotency responses in a bounded, multi-worker-safe
    /// batch.
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
                 SELECT id FROM idempotency_records \
                 WHERE expires_at <= $1 \
                 ORDER BY expires_at, id \
                 FOR UPDATE SKIP LOCKED \
                 LIMIT $2\
             ) \
             DELETE FROM idempotency_records records \
             USING expired \
             WHERE records.id = expired.id",
        )
        .bind(now)
        .bind(i64::from(limit))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[derive(Debug, FromRow)]
struct PurgeScheduleRow {
    id: Uuid,
    org_id: String,
    owner_principal_id: Uuid,
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

async fn acquire_retention_advisory_lock(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), RepositoryError> {
    // All purge workers serialize candidate capture, ledger insertion,
    // schedule deletion, and global trimming under one transaction-scoped
    // lock. Taking it before the first table read prevents two workers from
    // observing different ledger bounds during the same sweep.
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(DELETED_REMINDER_LEDGER_LOCK_CLASS)
        .bind(DELETED_REMINDER_LEDGER_LOCK_KEY)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn select_expired_schedule_snapshots(
    transaction: &mut Transaction<'_, Postgres>,
    now: DateTime<Utc>,
    limit: u32,
) -> Result<Vec<PurgeScheduleRow>, RepositoryError> {
    sqlx::query_as::<_, PurgeScheduleRow>(
        "SELECT s.id, s.org_id, s.owner_principal_id, s.silicon_id, \
                s.reminder_text, s.schedule_kind, s.cron_expression, s.timezone, \
                last_execution.last_triggered_at, s.created_at, \
                CASE WHEN s.deleted_at IS NOT NULL THEN s.deleted_at ELSE s.completed_at END \
                    AS archived_at, \
                s.purge_after, \
                CASE WHEN s.deleted_at IS NOT NULL THEN 'deleted' ELSE 'completed' END \
                    AS purge_reason \
         FROM schedules s \
         LEFT JOIN LATERAL ( \
             SELECT max(execution.scheduled_for) AS last_triggered_at \
             FROM executions execution \
             WHERE execution.schedule_id = s.id \
         ) last_execution ON true \
         WHERE s.purge_after IS NOT NULL AND s.purge_after <= $1 \
         ORDER BY s.purge_after, s.id \
         FOR UPDATE OF s SKIP LOCKED \
         LIMIT $2",
    )
    .bind(now)
    .bind(i64::from(limit))
    .fetch_all(&mut **transaction)
    .await
    .map_err(RepositoryError::from)
}

async fn write_deleted_reminder(
    transaction: &mut Transaction<'_, Postgres>,
    candidate: &PurgeScheduleRow,
    purged_at: DateTime<Utc>,
    audit: &AuditContext,
) -> Result<(), RepositoryError> {
    let record_text = deleted_reminder_record_text(candidate, purged_at);
    let ledger_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO deleted_reminders ( \
             schedule_id, org_id, owner_principal_id, silicon_id, reminder_text, \
             schedule_kind, cron_expression, timezone, last_triggered_at, \
             created_at, archived_at, purge_after, purged_at, purge_reason, record_text \
         ) VALUES ( \
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15 \
         ) \
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
        Some(&candidate.org_id),
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

async fn delete_purged_schedules(
    transaction: &mut Transaction<'_, Postgres>,
    candidates: &[PurgeScheduleRow],
) -> Result<u64, RepositoryError> {
    let ids = candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(0);
    }
    sqlx::query("DELETE FROM schedules WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&mut **transaction)
        .await
        .map(|result| result.rows_affected())
        .map_err(RepositoryError::from)
}

fn row_count(count: usize) -> Result<u64, RepositoryError> {
    u64::try_from(count).map_err(|_| {
        RepositoryError::InvalidInput("database row count exceeds supported integer range")
    })
}

fn deleted_reminder_record_text(schedule: &PurgeScheduleRow, purged_at: DateTime<Utc>) -> String {
    json!({
        "schema_version": "1.0",
        "schedule_id": schedule.id,
        "org_id": schedule.org_id,
        "owner_principal_id": schedule.owner_principal_id,
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
pub(super) async fn trim_deleted_reminder_ledger(
    transaction: &mut Transaction<'_, Postgres>,
    maximum_records: u32,
) -> Result<u64, RepositoryError> {
    let result = sqlx::query(
        "DELETE FROM deleted_reminders ledger \
         USING ( \
             SELECT id \
             FROM deleted_reminders \
             ORDER BY id DESC \
             OFFSET $1 \
         ) expired \
         WHERE ledger.id = expired.id",
    )
    .bind(i64::from(maximum_records))
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected())
}

#[derive(Debug, FromRow)]
struct PurgeHookDestinationRow {
    id: Uuid,
    org_id: String,
    silicon_id: String,
    disabled_at: DateTime<Utc>,
    purge_after: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct RevokedScheduleRow {
    id: Uuid,
    org_id: String,
    silicon_id: String,
}

#[derive(Debug, FromRow)]
struct RevokedDestinationRow {
    id: Uuid,
    org_id: String,
    silicon_id: String,
}

#[derive(Clone, Copy, Debug)]
struct MaterializationGeneration {
    scheduled_for: DateTime<Utc>,
    schedule_version: i64,
    schedule_kind: &'static str,
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
            Some(&schedule.org_id),
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
            Some(&schedule.org_id),
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
    let schedule_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT s.id \
         FROM schedules s \
         JOIN executions e ON e.schedule_id = s.id \
         JOIN silicon_identities identity \
           ON identity.org_id = s.org_id \
          AND identity.principal_id = s.owner_principal_id \
         JOIN organization_lifecycle organization \
           ON organization.org_id = s.org_id \
         WHERE e.id = $1 AND s.deleted_at IS NULL \
           AND (s.purge_after IS NULL OR s.purge_after > $2) \
           AND identity.state = 'active' AND organization.state = 'active' \
         FOR UPDATE OF s",
    )
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
    let audit = AuditContext {
        actor_type: ActorType::System,
        actor_id: worker_id.to_owned(),
        request_id: None,
    };
    append_audit(
        transaction,
        Some(&execution.org_id),
        &audit,
        action,
        "execution",
        Some(execution.id.to_string()),
        metadata,
    )
    .await
}

async fn register_active_silicon_identity(
    transaction: &mut Transaction<'_, Postgres>,
    destination: &NewHookDestination,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO organization_lifecycle (org_id, state) \
         VALUES ($1, 'active') \
         ON CONFLICT (org_id) DO NOTHING",
    )
    .bind(&destination.org_id)
    .execute(&mut **transaction)
    .await?;

    let organization_state = sqlx::query_scalar::<_, String>(
        "SELECT state FROM organization_lifecycle WHERE org_id = $1 FOR UPDATE",
    )
    .bind(&destination.org_id)
    .fetch_one(&mut **transaction)
    .await?;
    if organization_state != "active" {
        return Err(RepositoryError::SiliconUnavailable);
    }

    sqlx::query(
        "INSERT INTO silicon_identities (org_id, principal_id, silicon_id, state) \
         VALUES ($1, $2, $3, 'active') \
         ON CONFLICT (org_id, principal_id) DO NOTHING",
    )
    .bind(&destination.org_id)
    .bind(destination.owner_principal_id)
    .bind(&destination.silicon_id)
    .execute(&mut **transaction)
    .await?;

    let binding = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT state, silicon_id FROM silicon_identities \
         WHERE org_id = $1 AND principal_id = $2 FOR UPDATE",
    )
    .bind(&destination.org_id)
    .bind(destination.owner_principal_id)
    .fetch_one(&mut **transaction)
    .await?;
    if binding.0 != "active" || binding.1.as_deref() != Some(destination.silicon_id.as_str()) {
        return Err(RepositoryError::SiliconUnavailable);
    }
    Ok(())
}

fn validate_hook_destination(destination: &NewHookDestination) -> Result<(), RepositoryError> {
    validate_org_id(&destination.org_id)?;
    validate_global_silicon_id(&destination.silicon_id, &destination.org_id)?;
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

fn validate_internal_event(event: &NewInternalEvent) -> Result<(), RepositoryError> {
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

#[derive(Clone, Debug)]
enum IamLifecycleTarget {
    Silicon { org_id: String, principal_id: Uuid },
    Organization { org_id: String },
}

impl IamLifecycleTarget {
    fn org_id(&self) -> &str {
        match self {
            Self::Silicon { org_id, .. } | Self::Organization { org_id } => org_id,
        }
    }

    const fn resource_type(&self) -> &'static str {
        match self {
            Self::Silicon { .. } => "silicon",
            Self::Organization { .. } => "organization",
        }
    }

    fn resource_id(&self) -> String {
        match self {
            Self::Silicon { principal_id, .. } => principal_id.to_string(),
            Self::Organization { org_id } => org_id.clone(),
        }
    }
}

fn validate_iam_lifecycle_event(
    event: &NewInternalEvent,
) -> Result<IamLifecycleTarget, RepositoryError> {
    if event.source != "silicon-iam" {
        return Err(RepositoryError::InvalidInput(
            "IAM lifecycle event source must be silicon-iam",
        ));
    }
    let org_id = event
        .org_id
        .as_deref()
        .ok_or(RepositoryError::InvalidInput(
            "IAM lifecycle event requires an organization",
        ))?;
    validate_org_id(org_id)?;

    match event.event_type.as_str() {
        "organization.membership.removed.v1" => {
            let principal_id = event
                .subject_id
                .as_deref()
                .ok_or(RepositoryError::InvalidInput(
                    "Silicon removal requires a subject",
                ))?
                .parse::<Uuid>()
                .map_err(|_| {
                    RepositoryError::InvalidInput("Silicon removal subject must be a UUID")
                })?;
            Ok(IamLifecycleTarget::Silicon {
                org_id: org_id.to_owned(),
                principal_id,
            })
        }
        "organization.updated.v1" if event.subject_id.is_none() => {
            Ok(IamLifecycleTarget::Organization {
                org_id: org_id.to_owned(),
            })
        }
        "organization.updated.v1" => Err(RepositoryError::InvalidInput(
            "organization removal cannot contain a Silicon subject",
        )),
        _ => Err(RepositoryError::InvalidInput(
            "unsupported IAM lifecycle event type",
        )),
    }
}

fn receipt_matches_event(receipt: &InternalEventReceiptRow, event: &NewInternalEvent) -> bool {
    receipt.source == event.source
        && receipt.event_id == event.event_id
        && receipt.event_type == event.event_type
        && receipt.org_id == event.org_id
        && receipt.subject_id == event.subject_id
        && receipt.payload == event.payload
        && receipt.payload_hash.as_slice() == event.payload_hash.as_slice()
}

async fn insert_internal_event_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    event: &NewInternalEvent,
) -> Result<Option<InternalEventReceiptRow>, RepositoryError> {
    let sql = format!(
        "INSERT INTO internal_event_receipts (\
             id, source, event_id, event_type, org_id, subject_id, payload, \
             payload_hash, status, next_attempt_at, received_at\
         ) VALUES (\
             $1, $2, $3, $4, $5, $6, $7, $8, 'pending', \
             clock_timestamp(), clock_timestamp()\
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

async fn load_iam_lifecycle_replay(
    transaction: &mut Transaction<'_, Postgres>,
    event: &NewInternalEvent,
) -> Result<IamLifecycleOutcome, RepositoryError> {
    let receipt = lock_internal_event_receipt(transaction, event).await?;
    if !receipt_matches_event(&receipt, event) {
        return Err(RepositoryError::EventReceiptConflict);
    }
    if receipt.status != "processed" {
        return Err(RepositoryError::InvalidState);
    }
    Ok(IamLifecycleOutcome {
        receipt,
        replayed: true,
        schedules_deleted: 0,
        executions_failed: 0,
    })
}

async fn cleanup_revoked_schedules(
    transaction: &mut Transaction<'_, Postgres>,
    now: DateTime<Utc>,
    limit: u32,
) -> Result<(Vec<RevokedScheduleRow>, u64, u64), RepositoryError> {
    let schedules = sqlx::query_as::<_, RevokedScheduleRow>(
        "SELECT schedule.id, schedule.org_id, schedule.silicon_id \
         FROM schedules schedule \
         LEFT JOIN organization_lifecycle organization \
           ON organization.org_id = schedule.org_id \
         LEFT JOIN silicon_identities identity \
           ON identity.org_id = schedule.org_id \
          AND identity.principal_id = schedule.owner_principal_id \
         WHERE schedule.deleted_at IS NULL AND schedule.status <> 'completed' \
           AND (\
               organization.state IS DISTINCT FROM 'active' \
               OR identity.state IS DISTINCT FROM 'active'\
           ) \
         ORDER BY schedule.id FOR UPDATE OF schedule SKIP LOCKED LIMIT $1",
    )
    .bind(i64::from(limit))
    .fetch_all(&mut **transaction)
    .await?;
    let schedule_ids = schedules
        .iter()
        .map(|schedule| schedule.id)
        .collect::<Vec<_>>();
    let schedules_deleted = if schedule_ids.is_empty() {
        0
    } else {
        sqlx::query(
            "UPDATE schedules SET \
                 deleted_at = $1, next_run_at = NULL, version = version + 1 \
             WHERE id = ANY($2) AND deleted_at IS NULL AND status <> 'completed'",
        )
        .bind(now)
        .bind(&schedule_ids)
        .execute(&mut **transaction)
        .await?
        .rows_affected()
    };
    let executions_failed =
        fail_revoked_unaccepted_executions(transaction, i64::from(limit)).await?;
    Ok((schedules, schedules_deleted, executions_failed))
}

async fn cleanup_revoked_destinations(
    transaction: &mut Transaction<'_, Postgres>,
    now: DateTime<Utc>,
    limit: u32,
) -> Result<Vec<RevokedDestinationRow>, RepositoryError> {
    sqlx::query_as::<_, RevokedDestinationRow>(
        "WITH candidates AS (\
             SELECT destination.id \
             FROM hook_destinations destination \
             LEFT JOIN organization_lifecycle organization \
               ON organization.org_id = destination.org_id \
             LEFT JOIN silicon_identities identity \
               ON identity.org_id = destination.org_id \
              AND identity.principal_id = destination.owner_principal_id \
             WHERE destination.disabled_at IS NULL \
               AND (\
                   organization.state IS DISTINCT FROM 'active' \
                   OR identity.state IS DISTINCT FROM 'active'\
               ) \
             ORDER BY destination.id \
             FOR UPDATE OF destination SKIP LOCKED LIMIT $1\
         ) \
         UPDATE hook_destinations destination SET \
             disabled_at = $2, version = destination.version + 1 \
         FROM candidates WHERE destination.id = candidates.id \
         RETURNING destination.id, destination.org_id, destination.silicon_id",
    )
    .bind(i64::from(limit))
    .bind(now)
    .fetch_all(&mut **transaction)
    .await
    .map_err(RepositoryError::from)
}

async fn append_revocation_cleanup_audits(
    transaction: &mut Transaction<'_, Postgres>,
    audit: &AuditContext,
    schedules: &[RevokedScheduleRow],
    destinations: &[RevokedDestinationRow],
) -> Result<(), RepositoryError> {
    for schedule in schedules {
        append_audit(
            transaction,
            Some(&schedule.org_id),
            audit,
            "schedule.archived_after_iam_revocation",
            "schedule",
            Some(schedule.id.to_string()),
            json!({ "silicon_id": schedule.silicon_id }),
        )
        .await?;
    }
    for destination in destinations {
        append_audit(
            transaction,
            Some(&destination.org_id),
            audit,
            "hook_destination.disabled_after_iam_revocation",
            "hook_destination",
            Some(destination.id.to_string()),
            json!({ "silicon_id": destination.silicon_id }),
        )
        .await?;
    }
    Ok(())
}

async fn soft_delete_iam_schedules(
    transaction: &mut Transaction<'_, Postgres>,
    target: &IamLifecycleTarget,
    deleted_at: DateTime<Utc>,
) -> Result<u64, RepositoryError> {
    let schedule_ids = match target {
        IamLifecycleTarget::Silicon {
            org_id,
            principal_id,
        } => {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM schedules \
                 WHERE org_id = $1 AND owner_principal_id = $2 \
                   AND deleted_at IS NULL AND status <> 'completed' \
                 ORDER BY id FOR UPDATE SKIP LOCKED LIMIT $3",
            )
            .bind(org_id)
            .bind(principal_id)
            .bind(IAM_INLINE_CLEANUP_LIMIT)
            .fetch_all(&mut **transaction)
            .await?
        }
        IamLifecycleTarget::Organization { org_id } => {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM schedules \
                 WHERE org_id = $1 AND deleted_at IS NULL AND status <> 'completed' \
                 ORDER BY id FOR UPDATE SKIP LOCKED LIMIT $2",
            )
            .bind(org_id)
            .bind(IAM_INLINE_CLEANUP_LIMIT)
            .fetch_all(&mut **transaction)
            .await?
        }
    };

    let deleted = if schedule_ids.is_empty() {
        0
    } else {
        sqlx::query(
            "UPDATE schedules SET \
                 deleted_at = $1, next_run_at = NULL, version = version + 1 \
             WHERE id = ANY($2) AND deleted_at IS NULL AND status <> 'completed'",
        )
        .bind(deleted_at)
        .bind(&schedule_ids)
        .execute(&mut **transaction)
        .await?
        .rows_affected()
    };
    Ok(deleted)
}

async fn block_iam_target(
    transaction: &mut Transaction<'_, Postgres>,
    target: &IamLifecycleTarget,
    revoked_at: DateTime<Utc>,
) -> Result<(), RepositoryError> {
    match target {
        IamLifecycleTarget::Silicon {
            org_id,
            principal_id,
        } => {
            sqlx::query(
                "INSERT INTO organization_lifecycle (org_id, state) \
                 VALUES ($1, 'active') ON CONFLICT (org_id) DO NOTHING",
            )
            .bind(org_id)
            .execute(&mut **transaction)
            .await?;
            sqlx::query(
                "INSERT INTO silicon_identities (\
                     org_id, principal_id, silicon_id, state, revoked_at\
                 ) VALUES ($1, $2, NULL, 'revoked', $3) \
                 ON CONFLICT (org_id, principal_id) DO UPDATE SET \
                     state = 'revoked', revoked_at = LEAST(\
                         COALESCE(silicon_identities.revoked_at, EXCLUDED.revoked_at), \
                         EXCLUDED.revoked_at\
                     ), updated_at = clock_timestamp()",
            )
            .bind(org_id)
            .bind(principal_id)
            .bind(revoked_at)
            .execute(&mut **transaction)
            .await?;
        }
        IamLifecycleTarget::Organization { org_id } => {
            sqlx::query(
                "INSERT INTO organization_lifecycle (\
                     org_id, state, revoked_at\
                 ) VALUES ($1, 'revoked', $2) \
                 ON CONFLICT (org_id) DO UPDATE SET \
                     state = 'revoked', revoked_at = LEAST(\
                         COALESCE(organization_lifecycle.revoked_at, EXCLUDED.revoked_at), \
                         EXCLUDED.revoked_at\
                     ), updated_at = clock_timestamp()",
            )
            .bind(org_id)
            .bind(revoked_at)
            .execute(&mut **transaction)
            .await?;
            sqlx::query(
                "UPDATE silicon_identities SET \
                     state = 'revoked', revoked_at = COALESCE(revoked_at, $1), \
                     updated_at = clock_timestamp() \
                 WHERE org_id = $2 AND state = 'active'",
            )
            .bind(revoked_at)
            .bind(org_id)
            .execute(&mut **transaction)
            .await?;
        }
    }
    Ok(())
}

async fn disable_iam_destinations(
    transaction: &mut Transaction<'_, Postgres>,
    target: &IamLifecycleTarget,
    disabled_at: DateTime<Utc>,
) -> Result<u64, RepositoryError> {
    let result = match target {
        IamLifecycleTarget::Silicon {
            org_id,
            principal_id,
        } => {
            sqlx::query(
                "WITH candidates AS (\
                     SELECT id FROM hook_destinations \
                     WHERE org_id = $2 AND owner_principal_id = $3 \
                       AND disabled_at IS NULL \
                     ORDER BY id FOR UPDATE SKIP LOCKED LIMIT $4\
                 ) \
                 UPDATE hook_destinations destination SET \
                     disabled_at = $1, version = destination.version + 1 \
                 FROM candidates WHERE destination.id = candidates.id",
            )
            .bind(disabled_at)
            .bind(org_id)
            .bind(principal_id)
            .bind(IAM_INLINE_CLEANUP_LIMIT)
            .execute(&mut **transaction)
            .await?
        }
        IamLifecycleTarget::Organization { org_id } => {
            sqlx::query(
                "WITH candidates AS (\
                     SELECT id FROM hook_destinations \
                     WHERE org_id = $2 AND disabled_at IS NULL \
                     ORDER BY id FOR UPDATE SKIP LOCKED LIMIT $3\
                 ) \
                 UPDATE hook_destinations destination SET \
                     disabled_at = $1, version = destination.version + 1 \
                 FROM candidates WHERE destination.id = candidates.id",
            )
            .bind(disabled_at)
            .bind(org_id)
            .bind(IAM_INLINE_CLEANUP_LIMIT)
            .execute(&mut **transaction)
            .await?
        }
    };
    Ok(result.rows_affected())
}

async fn fail_schedule_unaccepted_executions(
    transaction: &mut Transaction<'_, Postgres>,
    schedule_ids: &[Uuid],
    failure_reason: &str,
) -> Result<u64, RepositoryError> {
    if schedule_ids.is_empty() {
        return Ok(0);
    }
    sqlx::query(
        "UPDATE executions SET \
             status = 'failed', next_attempt_at = NULL, \
             failure_reason = $2, \
             lease_owner = NULL, lease_expires_at = NULL \
         WHERE schedule_id = ANY($1) AND status IN ('pending', 'retrying')",
    )
    .bind(schedule_ids)
    .bind(failure_reason)
    .execute(&mut **transaction)
    .await
    .map(|result| result.rows_affected())
    .map_err(RepositoryError::from)
}

async fn fail_iam_target_unaccepted_executions(
    transaction: &mut Transaction<'_, Postgres>,
    target: &IamLifecycleTarget,
    limit: i64,
) -> Result<u64, RepositoryError> {
    let result = match target {
        IamLifecycleTarget::Silicon {
            org_id,
            principal_id,
        } => {
            sqlx::query(
                "WITH candidates AS (\
                     SELECT execution.id \
                     FROM executions execution \
                     JOIN schedules schedule ON schedule.id = execution.schedule_id \
                     WHERE schedule.org_id = $1 \
                       AND schedule.owner_principal_id = $2 \
                       AND execution.status IN ('pending', 'retrying') \
                     ORDER BY execution.id \
                     FOR UPDATE OF execution SKIP LOCKED LIMIT $3\
                 ) \
                 UPDATE executions execution SET \
                     status = 'failed', next_attempt_at = NULL, \
                     failure_reason = $4, lease_owner = NULL, lease_expires_at = NULL \
                 FROM candidates \
                 WHERE execution.id = candidates.id",
            )
            .bind(org_id)
            .bind(principal_id)
            .bind(limit)
            .bind(IAM_REVOCATION_FAILURE_REASON)
            .execute(&mut **transaction)
            .await?
        }
        IamLifecycleTarget::Organization { org_id } => {
            sqlx::query(
                "WITH candidates AS (\
                     SELECT execution.id \
                     FROM executions execution \
                     JOIN schedules schedule ON schedule.id = execution.schedule_id \
                     WHERE schedule.org_id = $1 \
                       AND execution.status IN ('pending', 'retrying') \
                     ORDER BY execution.id \
                     FOR UPDATE OF execution SKIP LOCKED LIMIT $2\
                 ) \
                 UPDATE executions execution SET \
                     status = 'failed', next_attempt_at = NULL, \
                     failure_reason = $3, lease_owner = NULL, lease_expires_at = NULL \
                 FROM candidates \
                 WHERE execution.id = candidates.id",
            )
            .bind(org_id)
            .bind(limit)
            .bind(IAM_REVOCATION_FAILURE_REASON)
            .execute(&mut **transaction)
            .await?
        }
    };
    Ok(result.rows_affected())
}

async fn fail_revoked_unaccepted_executions(
    transaction: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<u64, RepositoryError> {
    sqlx::query(
        "WITH candidates AS (\
             SELECT execution.id \
             FROM executions execution \
             JOIN schedules schedule ON schedule.id = execution.schedule_id \
             LEFT JOIN organization_lifecycle organization \
               ON organization.org_id = schedule.org_id \
             LEFT JOIN silicon_identities identity \
               ON identity.org_id = schedule.org_id \
              AND identity.principal_id = schedule.owner_principal_id \
             WHERE execution.status IN ('pending', 'retrying') \
               AND (\
                   organization.state IS DISTINCT FROM 'active' \
                   OR identity.state IS DISTINCT FROM 'active'\
               ) \
             ORDER BY execution.id \
             FOR UPDATE OF execution SKIP LOCKED LIMIT $1\
         ) \
         UPDATE executions execution SET \
             status = 'failed', next_attempt_at = NULL, \
             failure_reason = $2, lease_owner = NULL, lease_expires_at = NULL \
         FROM candidates \
         WHERE execution.id = candidates.id",
    )
    .bind(limit)
    .bind(IAM_REVOCATION_FAILURE_REASON)
    .execute(&mut **transaction)
    .await
    .map(|result| result.rows_affected())
    .map_err(RepositoryError::from)
}

async fn mark_internal_event_processed(
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

async fn append_iam_lifecycle_audits(
    transaction: &mut Transaction<'_, Postgres>,
    target: &IamLifecycleTarget,
    event: &NewInternalEvent,
    audit: &AuditContext,
    outcome: &IamLifecycleOutcome,
) -> Result<(), RepositoryError> {
    append_audit(
        transaction,
        Some(target.org_id()),
        audit,
        "iam.lifecycle_applied",
        target.resource_type(),
        Some(target.resource_id()),
        json!({
            "event_id": event.event_id,
            "event_type": event.event_type,
            "schedules_deleted": outcome.schedules_deleted,
            "executions_failed": outcome.executions_failed,
        }),
    )
    .await?;
    append_audit(
        transaction,
        Some(target.org_id()),
        audit,
        "internal_event.processed",
        "internal_event_receipt",
        Some(outcome.receipt.id.to_string()),
        json!({
            "source": event.source,
            "event_type": event.event_type,
        }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sqlx::{Postgres, QueryBuilder};
    use uuid::Uuid;

    use crate::domain::ReminderReadScope;

    use super::{
        CreateSchedule, page_from_rows, push_reminder_read_scope, validate_create_schedule,
    };

    #[test]
    fn create_rejects_an_unknown_schedule_kind() {
        let schedule = CreateSchedule {
            id: Uuid::now_v7(),
            org_id: "tos".to_owned(),
            owner_principal_id: Uuid::now_v7(),
            silicon_id: "assistant:tos".to_owned(),
            text: "Prepare report".to_owned(),
            timezone: "Asia/Kolkata".to_owned(),
            schedule_kind: "sometimes".to_owned(),
            cron: "0 9 * * *".to_owned(),
            next_run_at: Utc::now(),
        };

        assert!(validate_create_schedule(&schedule).is_err());
    }

    #[test]
    fn keyset_page_retains_only_the_requested_size() {
        let page = page_from_rows(vec![1_u8, 2, 3], 2);
        assert_eq!(page.items, vec![1, 2]);
        assert!(page.has_more);
    }

    #[test]
    fn silicon_read_scope_adds_no_owner_predicate() {
        let mut query = QueryBuilder::<Postgres>::new("SELECT 1 WHERE TRUE");
        push_reminder_read_scope(
            &mut query,
            "schedule.owner_principal_id",
            &ReminderReadScope::organization(),
        );

        assert_eq!(query.sql(), "SELECT 1 WHERE TRUE");
    }

    #[test]
    fn empty_carbon_scope_is_an_explicit_false_predicate() {
        let mut query = QueryBuilder::<Postgres>::new("SELECT 1 WHERE TRUE");
        push_reminder_read_scope(
            &mut query,
            "schedule.owner_principal_id",
            &ReminderReadScope::silicon_principals(Vec::new()),
        );

        assert_eq!(query.sql(), "SELECT 1 WHERE TRUE AND FALSE");
    }

    #[test]
    fn carbon_owner_predicate_precedes_cursor_order_and_limit() {
        let mut query = QueryBuilder::<Postgres>::new("SELECT 1 WHERE TRUE");
        push_reminder_read_scope(
            &mut query,
            "schedule.owner_principal_id",
            &ReminderReadScope::silicon_principals(vec![Uuid::from_u128(1)]),
        );
        query.push(
            " AND (schedule.created_at, schedule.id) < ($2, $3) \
             ORDER BY schedule.created_at DESC, schedule.id DESC LIMIT $4",
        );

        let sql = query.sql();
        let sql = sql.as_str();
        let owner = sql.find("schedule.owner_principal_id");
        let cursor = sql.find("schedule.created_at, schedule.id");
        let limit = sql.find("LIMIT");
        assert!(
            owner
                .zip(cursor)
                .is_some_and(|(owner, cursor)| owner < cursor)
        );
        assert!(
            cursor
                .zip(limit)
                .is_some_and(|(cursor, limit)| cursor < limit)
        );
    }
}

#[derive(Debug, FromRow)]
struct IdempotencyRow {
    request_hash: Vec<u8>,
    state: String,
    response_status: Option<i16>,
    response_body: Option<Value>,
}

#[derive(Debug)]
enum Reservation {
    New(Uuid),
    Replay {
        status_code: u16,
        response_body: Value,
    },
}

async fn reserve_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    operation: &str,
    target_id: Option<Uuid>,
    context: &IdempotencyContext,
) -> Result<Reservation, RepositoryError> {
    validate_idempotency_context(context)?;

    sqlx::query(
        "DELETE FROM idempotency_records \
         WHERE org_id = $1 AND actor_type = $2 AND actor_id = $3 \
           AND operation = $4 AND target_id IS NOT DISTINCT FROM $5 \
           AND idempotency_key = $6 AND expires_at <= clock_timestamp()",
    )
    .bind(org_id)
    .bind(context.actor_type.as_str())
    .bind(&context.actor_id)
    .bind(operation)
    .bind(target_id)
    .bind(&context.key)
    .execute(&mut **transaction)
    .await?;

    let id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO idempotency_records (\
             id, org_id, actor_type, actor_id, operation, target_id, \
             idempotency_key, request_hash, state, expires_at\
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'reserved', $9) \
         ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(org_id)
    .bind(context.actor_type.as_str())
    .bind(&context.actor_id)
    .bind(operation)
    .bind(target_id)
    .bind(&context.key)
    .bind(context.request_hash.as_slice())
    .bind(context.expires_at)
    .execute(&mut **transaction)
    .await?;

    if inserted.rows_affected() == 1 {
        return Ok(Reservation::New(id));
    }

    let existing = sqlx::query_as::<_, IdempotencyRow>(
        "SELECT request_hash, state, response_status, response_body \
         FROM idempotency_records \
         WHERE org_id = $1 AND actor_type = $2 AND actor_id = $3 \
           AND operation = $4 AND target_id IS NOT DISTINCT FROM $5 \
           AND idempotency_key = $6 \
         FOR UPDATE",
    )
    .bind(org_id)
    .bind(context.actor_type.as_str())
    .bind(&context.actor_id)
    .bind(operation)
    .bind(target_id)
    .bind(&context.key)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(RepositoryError::IdempotencyIncomplete)?;

    if existing.request_hash.as_slice() != context.request_hash.as_slice() {
        return Err(RepositoryError::IdempotencyConflict);
    }
    let response = decode_idempotent_response(existing)?;
    Ok(Reservation::Replay {
        status_code: response.status_code,
        response_body: response.response_body,
    })
}

fn decode_idempotent_response(
    row: IdempotencyRow,
) -> Result<StoredIdempotentResponse, RepositoryError> {
    if row.state != "completed" {
        return Err(RepositoryError::IdempotencyIncomplete);
    }
    let status_code = row
        .response_status
        .and_then(|status| u16::try_from(status).ok())
        .ok_or(RepositoryError::IdempotencyIncomplete)?;
    let response_body = row
        .response_body
        .ok_or(RepositoryError::IdempotencyIncomplete)?;
    Ok(StoredIdempotentResponse {
        status_code,
        response_body,
    })
}

async fn complete_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    reservation: Reservation,
    status_code: i16,
    response_body: &Value,
) -> Result<(), RepositoryError> {
    let Reservation::New(id) = reservation else {
        return Err(RepositoryError::IdempotencyIncomplete);
    };

    let result = sqlx::query(
        "UPDATE idempotency_records SET \
             state = 'completed', response_status = $1, response_body = $2 \
         WHERE id = $3 AND state = 'reserved'",
    )
    .bind(status_code)
    .bind(response_body)
    .bind(id)
    .execute(&mut **transaction)
    .await?;

    if result.rows_affected() != 1 {
        return Err(RepositoryError::IdempotencyIncomplete);
    }
    Ok(())
}

async fn append_audit(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: Option<&str>,
    context: &AuditContext,
    action: &str,
    resource_type: &str,
    resource_id: Option<String>,
    metadata: Value,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO audit_records (\
             id, org_id, actor_type, actor_id, action, resource_type, \
             resource_id, request_id, metadata\
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(Uuid::now_v7())
    .bind(org_id)
    .bind(context.actor_type.as_str())
    .bind(&context.actor_id)
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(&context.request_id)
    .bind(metadata)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn validate_create_schedule(schedule: &CreateSchedule) -> Result<(), RepositoryError> {
    validate_org_id(&schedule.org_id)?;
    validate_global_silicon_id(&schedule.silicon_id, &schedule.org_id)?;
    validate_schedule_values(
        &schedule.text,
        &schedule.timezone,
        &schedule.schedule_kind,
        &schedule.cron,
    )?;
    Ok(())
}

fn validate_schedule_replacement(replacement: &ScheduleReplacement) -> Result<(), RepositoryError> {
    validate_org_id(&replacement.org_id)?;
    validate_global_silicon_id(&replacement.silicon_id, &replacement.org_id)?;
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
    validate_org_id(&replacement.org_id)?;
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

fn validate_mutating_silicon(
    idempotency: &IdempotencyContext,
    owner_principal_id: Uuid,
) -> Result<(), RepositoryError> {
    if idempotency.actor_type != ActorType::Silicon
        || idempotency.actor_id != owner_principal_id.to_string()
    {
        return Err(RepositoryError::InvalidInput(
            "schedule mutation actor must be its owner Silicon",
        ));
    }
    Ok(())
}

async fn lock_schedule_status_rows(
    transaction: &mut Transaction<'_, Postgres>,
    replacement: &BulkScheduleStatusReplacement,
) -> Result<HashMap<Uuid, ScheduleRow>, RepositoryError> {
    // IAM lifecycle writers acquire organization and identity tuples before
    // schedule tuples. Preserve that global ordering to serialize revocation
    // without introducing an inverse-lock deadlock.
    let bound_silicon_id = lock_active_silicon_identity(
        transaction,
        &replacement.org_id,
        replacement.owner_principal_id,
    )
    .await?;

    let mut lock_ids = replacement
        .schedules
        .iter()
        .map(|schedule| schedule.id)
        .collect::<Vec<_>>();
    lock_ids.sort_unstable();
    let lock_sql = format!(
        "SELECT {SCHEDULE_COLUMNS} \
         FROM schedules s \
         WHERE s.org_id = $1 AND s.id = ANY($2) \
           AND (s.purge_after IS NULL OR s.purge_after > clock_timestamp()) \
         ORDER BY s.id \
         FOR UPDATE OF s"
    );
    let locked = sqlx::query_as::<_, ScheduleRow>(AssertSqlSafe(lock_sql))
        .bind(&replacement.org_id)
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
    validate_locked_schedule_status_rows(replacement, &bound_silicon_id, &rows_by_id)?;
    Ok(rows_by_id)
}

fn validate_locked_schedule_status_rows(
    replacement: &BulkScheduleStatusReplacement,
    bound_silicon_id: &str,
    rows_by_id: &HashMap<Uuid, ScheduleRow>,
) -> Result<(), RepositoryError> {
    if rows_by_id.values().any(|row| {
        row.owner_principal_id != replacement.owner_principal_id
            || row.silicon_id != bound_silicon_id
    }) {
        return Err(RepositoryError::NotFound);
    }
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
        "UPDATE schedules SET \
             status = $1, next_run_at = $2, version = version + 1 \
         WHERE id = $3 AND org_id = $4 AND owner_principal_id = $5 \
           AND deleted_at IS NULL AND status <> 'completed' AND version = $6 \
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
            .bind(&replacement.org_id)
            .bind(replacement.owner_principal_id)
            .bind(change.expected_version)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(RepositoryError::VersionConflict)?;
        append_audit(
            transaction,
            Some(&replacement.org_id),
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

async fn lock_active_silicon_identity(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    principal_id: Uuid,
) -> Result<String, RepositoryError> {
    let Some((organization_state, identity_state, silicon_id)) =
        lock_silicon_lifecycle_projection(transaction, org_id, principal_id).await?
    else {
        return Err(RepositoryError::NotFound);
    };
    if organization_state != "active" || identity_state != "active" {
        return Err(RepositoryError::NotFound);
    }
    silicon_id.ok_or(RepositoryError::NotFound)
}

async fn lock_schedulable_silicon_identity(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    principal_id: Uuid,
    silicon_id: &str,
) -> Result<(), RepositoryError> {
    let Some((organization_state, identity_state, bound_silicon_id)) =
        lock_silicon_lifecycle_projection(transaction, org_id, principal_id).await?
    else {
        return Err(RepositoryError::WebhookNotConfigured);
    };
    if identity_state != "active"
        || organization_state != "active"
        || bound_silicon_id.as_deref() != Some(silicon_id)
    {
        return Err(RepositoryError::SiliconUnavailable);
    }

    let destination = sqlx::query_scalar::<_, bool>(
        "SELECT true FROM hook_destinations \
         WHERE org_id = $1 AND owner_principal_id = $2 AND silicon_id = $3 \
           AND disabled_at IS NULL \
         FOR SHARE",
    )
    .bind(org_id)
    .bind(principal_id)
    .bind(silicon_id)
    .fetch_optional(&mut **transaction)
    .await?;
    if destination.is_none() {
        return Err(RepositoryError::WebhookNotConfigured);
    }
    Ok(())
}

async fn lock_silicon_lifecycle_projection(
    transaction: &mut Transaction<'_, Postgres>,
    org_id: &str,
    principal_id: Uuid,
) -> Result<Option<(String, String, Option<String>)>, RepositoryError> {
    let organization_state = sqlx::query_scalar::<_, String>(
        "SELECT state FROM organization_lifecycle WHERE org_id = $1 FOR SHARE",
    )
    .bind(org_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(organization_state) = organization_state else {
        return Ok(None);
    };

    let identity = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT state, silicon_id FROM silicon_identities \
         WHERE org_id = $1 AND principal_id = $2 FOR SHARE",
    )
    .bind(org_id)
    .bind(principal_id)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(identity
        .map(|(identity_state, silicon_id)| (organization_state, identity_state, silicon_id)))
}

fn validate_org_id(value: &str) -> Result<(), RepositoryError> {
    if is_valid_iam_label(value) {
        Ok(())
    } else {
        Err(RepositoryError::InvalidInput(
            "organization ID has an invalid format",
        ))
    }
}

fn validate_global_silicon_id(value: &str, org_id: &str) -> Result<(), RepositoryError> {
    if silicon_id_belongs_to_org(value, org_id) {
        Ok(())
    } else {
        Err(RepositoryError::InvalidInput(
            "global Silicon ID does not match the organization",
        ))
    }
}

fn validate_idempotency_context(context: &IdempotencyContext) -> Result<(), RepositoryError> {
    if context.actor_type == ActorType::System {
        return Err(RepositoryError::InvalidInput(
            "system actors cannot use public idempotency scope",
        ));
    }
    if context.actor_id.trim().is_empty() || context.actor_id.len() > 255 {
        return Err(RepositoryError::InvalidInput(
            "idempotency actor ID is invalid",
        ));
    }
    if !(8..=255).contains(&context.key.len()) {
        return Err(RepositoryError::InvalidInput(
            "idempotency key must contain 8 to 255 bytes",
        ));
    }
    if context.expires_at <= Utc::now() {
        return Err(RepositoryError::InvalidInput(
            "idempotency expiry must be in the future",
        ));
    }
    Ok(())
}

fn validate_idempotency_operation(operation: &str) -> Result<(), RepositoryError> {
    if operation.trim().is_empty() || operation.len() > 255 {
        Err(RepositoryError::InvalidInput(
            "idempotency operation is invalid",
        ))
    } else {
        Ok(())
    }
}

fn validate_schedule_status(status: &str) -> Result<(), RepositoryError> {
    if matches!(status, "active" | "paused" | "completed") {
        Ok(())
    } else {
        Err(RepositoryError::InvalidInput("invalid schedule status"))
    }
}

fn validate_public_limit(limit: u32) -> Result<(), RepositoryError> {
    if (1..=PUBLIC_PAGE_LIMIT).contains(&limit) {
        Ok(())
    } else {
        Err(RepositoryError::InvalidInput(
            "public page limit must be from 1 through 100",
        ))
    }
}

fn push_reminder_read_scope(
    query: &mut QueryBuilder<Postgres>,
    owner_column: &'static str,
    read_scope: &ReminderReadScope,
) {
    match read_scope.silicon_principal_ids() {
        None => {}
        Some([]) => {
            query.push(" AND FALSE");
        }
        Some(principal_ids) => {
            query
                .push(" AND ")
                .push(owner_column)
                .push(" = ANY(")
                .push_bind(principal_ids.to_vec())
                .push(')');
        }
    }
}

fn validate_worker_limit(limit: u32) -> Result<(), RepositoryError> {
    if (1..=WORKER_BATCH_LIMIT).contains(&limit) {
        Ok(())
    } else {
        Err(RepositoryError::InvalidInput(
            "worker batch limit must be from 1 through 10000",
        ))
    }
}

fn validate_worker_id(worker_id: &str) -> Result<(), RepositoryError> {
    if worker_id.trim().is_empty() || worker_id.len() > 255 {
        Err(RepositoryError::InvalidInput("worker ID is invalid"))
    } else {
        Ok(())
    }
}

fn validate_failure_reason(reason: &str) -> Result<(), RepositoryError> {
    if reason.len() > MAX_FAILURE_REASON_BYTES {
        Err(RepositoryError::InvalidInput(
            "failure reason exceeds 4096 bytes",
        ))
    } else {
        Ok(())
    }
}

fn page_from_rows<T>(mut rows: Vec<T>, limit: u32) -> Page<T> {
    let page_size = limit as usize;
    let has_more = rows.len() > page_size;
    rows.truncate(page_size);
    Page {
        items: rows,
        has_more,
    }
}

fn lease_deadline(
    now: DateTime<Utc>,
    duration: Duration,
) -> Result<DateTime<Utc>, RepositoryError> {
    let chrono_duration = chrono::Duration::from_std(duration).map_err(|_| {
        RepositoryError::InvalidInput("lease duration exceeds supported timestamp range")
    })?;
    now.checked_add_signed(chrono_duration)
        .ok_or(RepositoryError::InvalidInput(
            "lease deadline exceeds supported timestamp range",
        ))
}
