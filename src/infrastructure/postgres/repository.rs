//! The PostgreSQL repository shared by the API and the worker.
//!
//! Ownership is expressed with storage keys: every row stores the
//! Remind-private key of its owner, and `account_keys` maps keys to Silicon
//! Accounts accounts. Reads filter by the storage keys the caller may read;
//! changes require one of the caller's own keys.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Transaction};
use uuid::Uuid;

use crate::domain::is_valid_global_silicon_id;

use super::{
    error::RepositoryError,
    models::{ActorType, AuditContext, IdempotencyContext, Page, StoredIdempotentResponse},
};

mod delivery;
mod destinations;
mod lifecycle;
mod retention;
mod schedules;

pub(crate) use lifecycle::{
    OwnerCleanup, cleanup_owner_data, insert_internal_event_receipt, mark_internal_event_processed,
    validate_internal_event,
};
#[cfg(test)]
pub(super) use retention::{DELETED_REMINDER_LEDGER_LIMIT, trim_deleted_reminder_ledger};

const PUBLIC_PAGE_LIMIT: u32 = 100;
const WORKER_BATCH_LIMIT: u32 = 10_000;
const MAX_FAILURE_REASON_BYTES: usize = 4_096;

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
    d.created_at, d.updated_at, d.aad_version";

const DESTINATION_RETURNING_COLUMNS: &str = "\
    id, org_id, owner_principal_id, silicon_id, endpoint_url_ciphertext, endpoint_url_nonce, \
    signing_secret_ciphertext, signing_secret_nonce, encryption_key_version, \
    version, disabled_at, purge_after, created_at, updated_at, aad_version";

const RECEIPT_COLUMNS: &str = "\
    r.id, r.source, r.event_id, r.event_type, r.org_id, r.subject_id, r.payload, \
    r.payload_hash, r.status, r.attempt_count, r.next_attempt_at, r.processed_at, \
    r.failure_reason, r.lease_owner, r.lease_expires_at, r.received_at, r.updated_at";

const RECEIPT_RETURNING_COLUMNS: &str = "\
    id, source, event_id, event_type, org_id, subject_id, payload, payload_hash, \
    status, attempt_count, next_attempt_at, processed_at, failure_reason, \
    lease_owner, lease_expires_at, received_at, updated_at";

/// SQL predicate: the owner of schedule `s` may still receive reminders.
///
/// A key that belongs to an account follows that account's status (suspended
/// or deleted accounts receive nothing). A key no account owns yet is a row
/// written before the move to Silicon Accounts: it keeps the IAM-era lifecycle
/// recorded at cutover, so unmapped legacy reminders keep firing. A key with
/// neither is a row in a test-environment schema, where identity lives in the
/// production database; test environments have their own lifecycle.
pub(super) const OWNER_IS_LIVE: &str = "(CASE \
    WHEN EXISTS (SELECT 1 FROM account_keys k WHERE k.storage_id = s.owner_principal_id) \
    THEN EXISTS (SELECT 1 FROM account_keys k JOIN accounts a ON a.uuid = k.account_uuid \
                 WHERE k.storage_id = s.owner_principal_id AND a.status = 'active') \
    WHEN s.org_id IS NOT NULL \
    THEN EXISTS (SELECT 1 FROM silicon_identities i \
                 JOIN organization_lifecycle o ON o.org_id = i.org_id \
                 WHERE i.org_id = s.org_id AND i.principal_id = s.owner_principal_id \
                   AND i.state = 'active' AND o.state = 'active') \
    ELSE true END)";

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

    /// Begins an explicit transaction for the two-stage due-materialization API.
    ///
    /// # Errors
    ///
    /// Returns a database error when a connection cannot be acquired.
    pub async fn begin(&self) -> Result<Transaction<'_, Postgres>, RepositoryError> {
        self.pool.begin().await.map_err(RepositoryError::from)
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

/// Idempotency records are scoped to the account (`org_id` stays NULL).
async fn reserve_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    operation: &str,
    target_id: Option<Uuid>,
    context: &IdempotencyContext,
) -> Result<Reservation, RepositoryError> {
    validate_idempotency_context(context)?;
    sqlx::query(
        "DELETE FROM idempotency_records \
         WHERE org_id IS NULL AND actor_type = $1 AND actor_id = $2 \
           AND operation = $3 AND target_id IS NOT DISTINCT FROM $4 \
           AND idempotency_key = $5 AND expires_at <= clock_timestamp()",
    )
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
         ) VALUES ($1, NULL, $2, $3, $4, $5, $6, $7, 'reserved', $8) \
         ON CONFLICT DO NOTHING",
    )
    .bind(id)
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
         WHERE org_id IS NULL AND actor_type = $1 AND actor_id = $2 \
           AND operation = $3 AND target_id IS NOT DISTINCT FROM $4 \
           AND idempotency_key = $5 \
         FOR UPDATE",
    )
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

/// Appends one audit record. `org_id` is set only for rows that still carry an
/// organization from before the move to Silicon Accounts.
pub(crate) async fn append_audit(
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

fn validate_silicon_snapshot(value: &str) -> Result<(), RepositoryError> {
    if is_valid_global_silicon_id(value) {
        Ok(())
    } else {
        Err(RepositoryError::InvalidInput("invalid Silicon id"))
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

fn validate_owner_keys(keys: &[Uuid]) -> Result<(), RepositoryError> {
    if keys.is_empty() || keys.len() > 1_000 {
        return Err(RepositoryError::InvalidInput(
            "a change needs 1 to 1000 owner storage keys",
        ));
    }
    Ok(())
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

/// Restricts a query to rows stored under the given keys. `None` adds nothing
/// (a test environment's key holder reads every row); an empty list matches
/// nothing.
fn push_owner_filter(
    query: &mut QueryBuilder<Postgres>,
    owner_column: &'static str,
    keys: Option<&[Uuid]>,
) {
    match keys {
        None => {}
        Some([]) => {
            query.push(" AND FALSE");
        }
        Some(keys) => {
            query
                .push(" AND ")
                .push(owner_column)
                .push(" = ANY(")
                .push_bind(keys.to_vec())
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

fn row_count(count: usize) -> Result<u64, RepositoryError> {
    u64::try_from(count).map_err(|_| {
        RepositoryError::InvalidInput("database row count exceeds supported integer range")
    })
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

/// Serializes creation across all storage keys belonging to an account.
async fn capacity_keys(
    transaction: &mut Transaction<'_, Postgres>,
    key: Uuid,
) -> Result<Vec<Uuid>, RepositoryError> {
    let uuid: Option<String> =
        sqlx::query_scalar("SELECT account_uuid FROM account_keys WHERE storage_id = $1")
            .bind(key)
            .fetch_optional(&mut **transaction)
            .await?;
    let identity = uuid.clone().unwrap_or_else(|| key.to_string());
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("remind.capacity:{identity}"))
        .execute(&mut **transaction)
        .await?;
    match uuid {
        Some(uuid) => Ok(sqlx::query_scalar(
            "SELECT storage_id FROM account_keys WHERE account_uuid = $1",
        )
        .bind(uuid)
        .fetch_all(&mut **transaction)
        .await?),
        None => Ok(vec![key]),
    }
}

#[cfg(test)]
mod tests {
    use sqlx::{Postgres, QueryBuilder};
    use uuid::Uuid;

    use super::{page_from_rows, push_owner_filter};

    #[test]
    fn keyset_page_retains_only_the_requested_size() {
        let page = page_from_rows(vec![1_u8, 2, 3], 2);
        assert_eq!(page.items, vec![1, 2]);
        assert!(page.has_more);
    }

    #[test]
    fn everything_scope_adds_no_owner_predicate() {
        let mut query = QueryBuilder::<Postgres>::new("SELECT 1 WHERE TRUE");
        push_owner_filter(&mut query, "s.owner_principal_id", None);
        assert_eq!(query.sql(), "SELECT 1 WHERE TRUE");
    }

    #[test]
    fn empty_scope_is_an_explicit_false_predicate() {
        let mut query = QueryBuilder::<Postgres>::new("SELECT 1 WHERE TRUE");
        push_owner_filter(&mut query, "s.owner_principal_id", Some(&[]));
        assert_eq!(query.sql(), "SELECT 1 WHERE TRUE AND FALSE");
    }

    #[test]
    fn owner_predicate_precedes_cursor_order_and_limit() {
        let mut query = QueryBuilder::<Postgres>::new("SELECT 1 WHERE TRUE");
        push_owner_filter(
            &mut query,
            "s.owner_principal_id",
            Some(&[Uuid::from_u128(1)]),
        );
        query.push(" AND (s.created_at, s.id) < ($2, $3) ORDER BY s.created_at DESC LIMIT $4");
        let sql = query.sql();
        let sql = sql.as_str();
        let owner = sql.find("s.owner_principal_id");
        let cursor = sql.find("s.created_at, s.id");
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
