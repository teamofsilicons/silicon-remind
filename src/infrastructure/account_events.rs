//! Silicon Accounts app webhook events, applied exactly once per `event_id`.
//!
//! Data events (`account.id_changed`, `account.updated`,
//! `silicon.custodian_changed`) re-read the account from Silicon Accounts and
//! store its current state, so their arrival order does not matter; when the
//! lookup fails the event's own data is applied with its ordering guard
//! (`occurred_at`, or the account `version`). Sign-out and access events record
//! a revocation cutoff; deletion archives the account's reminders.

use chrono::{DateTime, TimeZone as _, Utc};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use silicon_accounts_client::{WebhookEvent, WebhookPayload};
use uuid::Uuid;

use super::{
    identity::{IdentityStore, Reread},
    postgres::{
        ActorType, AuditContext, NewInternalEvent, OwnerCleanup, append_audit, cleanup_owner_data,
        insert_internal_event_receipt, mark_internal_event_processed, validate_internal_event,
    },
    testing::TestEnvironments,
};
use crate::{domain::is_valid_account_uuid, error::AppError};

/// Receipt source for Silicon Accounts webhook deliveries.
pub const SOURCE: &str = "silicon-accounts";

/// What happened to one delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventOutcome {
    /// Applied now.
    Processed,
    /// The same `event_id` was already applied.
    Duplicate,
    /// Recorded but nothing to do (a type Remind does not use, or a ping).
    Ignored,
}

impl EventOutcome {
    /// Stable wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Processed => "processed",
            Self::Duplicate => "duplicate",
            Self::Ignored => "ignored",
        }
    }
}

/// Applies one verified event. `raw_body` is the exact signed body.
///
/// # Errors
///
/// Returns a database or test-environment error; Silicon Accounts then retries
/// the delivery, and the receipt keeps the retry from applying it twice.
pub async fn apply(
    identity: &IdentityStore,
    tests: Option<&TestEnvironments>,
    event: &WebhookEvent,
    raw_body: &[u8],
) -> Result<EventOutcome, AppError> {
    let subject = subject_of(&event.payload);
    if let Some(uuid) = subject
        && !is_valid_account_uuid(uuid)
    {
        return Err(body_invalid(
            "The event names an account uuid Remind cannot read.",
        ));
    }
    // A signed in-flight pre-cutover delivery cannot recreate the retired subject.
    // Accounts emits fresh reconciliation events under the new UUID at cutover.
    if let Some(uuid) = subject
        && identity.is_retired_uuid(uuid).await?
    {
        return Ok(EventOutcome::Ignored);
    }
    let _: Value = serde_json::from_slice(raw_body)
        .map_err(|_| body_invalid("The signed body is not a JSON object."))?;
    let receipt = NewInternalEvent {
        id: Uuid::now_v7(),
        source: SOURCE.to_owned(),
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        org_id: None,
        subject_id: subject.map(str::to_owned),
        payload: json!({}),
        payload_hash: Sha256::digest(raw_body).into(),
        received_at: Utc::now(),
    };
    validate_internal_event(&receipt)?;
    let occurred_at = occurred_at(event);

    // Work outside the transaction: an authoritative re-read for data events
    // (events can arrive out of order; what Silicon Accounts shows now cannot),
    // and retiring a deleted account's test environments (idempotent; done
    // first so a failure makes Silicon Accounts retry the whole event).
    let reread = match (&event.payload, subject) {
        (
            WebhookPayload::AccountIdChanged(_)
            | WebhookPayload::AccountUpdated(_)
            | WebhookPayload::CustodianChanged(_),
            Some(uuid),
        ) => identity.reread(uuid).await,
        _ => Reread::default(),
    };
    if let (WebhookPayload::AccountDeleted(_), Some(uuid), Some(tests)) =
        (&event.payload, subject, tests)
    {
        tests.retire_owned_by(uuid).await?;
    }

    let mut transaction = identity.pool().begin().await?;
    let Some(row) = insert_internal_event_receipt(&mut transaction, &receipt).await? else {
        transaction.commit().await?;
        return Ok(EventOutcome::Duplicate);
    };
    let audit = AuditContext {
        actor_type: ActorType::Application,
        actor_id: SOURCE.to_owned(),
        request_id: crate::request_context::current_request_id(),
    };
    let outcome = apply_payload(
        &mut transaction,
        event,
        subject,
        &reread,
        occurred_at,
        &audit,
    )
    .await?;
    let processed = mark_internal_event_processed(&mut transaction, row.id, Utc::now()).await?;
    append_audit(
        &mut transaction,
        None,
        &audit,
        "accounts_event.applied",
        "internal_event_receipt",
        Some(processed.id.to_string()),
        json!({
            "event_type": event.event_type,
            "subject_uuid": subject,
            "outcome": outcome.as_str(),
        }),
    )
    .await?;
    transaction.commit().await?;
    // The authoritative re-read is written after the receipt so a duplicate
    // delivery never repeats it; it is an idempotent upsert.
    identity.store_reread(&reread).await?;
    Ok(outcome)
}

type Tx<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

/// Applies one event's effect inside the receipt's transaction.
async fn apply_payload(
    transaction: &mut Tx<'_>,
    event: &WebhookEvent,
    subject: Option<&str>,
    reread: &Reread,
    occurred_at: DateTime<Utc>,
    audit: &AuditContext,
) -> Result<EventOutcome, AppError> {
    let Some(uuid) = subject else {
        if !matches!(event.payload, WebhookPayload::Ping) {
            tracing::info!(
                event.kind = %event.event_type,
                "Silicon Accounts event type is not used by Remind; recorded and acknowledged"
            );
        }
        return Ok(EventOutcome::Ignored);
    };
    match &event.payload {
        // The re-read is written after commit: the lookup settles the id and
        // custodian, the user base read the display name and photo.
        WebhookPayload::AccountIdChanged(_) | WebhookPayload::CustodianChanged(_)
            if reread.summary.is_some() => {}
        WebhookPayload::AccountUpdated(_) if reread.member.is_some() => {}
        WebhookPayload::AccountIdChanged(data) => {
            ensure_row(transaction, uuid, data.kind.map(kind_name)).await?;
            sqlx::query(
                "UPDATE accounts SET public_id = $2, id_observed_at = $3, \
                     updated_at = clock_timestamp() \
                 WHERE uuid = $1 AND (id_observed_at IS NULL OR id_observed_at < $3)",
            )
            .bind(uuid)
            .bind(public_id(&data.new_id))
            .bind(occurred_at)
            .execute(&mut **transaction)
            .await?;
        }
        WebhookPayload::AccountUpdated(data) => {
            if let Some(account) = &data.account {
                update_profile(transaction, uuid, account, occurred_at).await?;
            }
        }
        WebhookPayload::CustodianChanged(data) => {
            if let Some(to) = data
                .to
                .as_ref()
                .filter(|to| is_valid_account_uuid(&to.uuid))
            {
                ensure_row(transaction, uuid, Some("silicon")).await?;
                sqlx::query(
                    "UPDATE accounts SET custodian_uuid = $2, custodian_id = $3, \
                         custodian_observed_at = $4, updated_at = clock_timestamp() \
                     WHERE uuid = $1 AND kind = 'silicon' \
                       AND (custodian_observed_at IS NULL OR custodian_observed_at < $4)",
                )
                .bind(uuid)
                .bind(&to.uuid)
                .bind(public_id(&to.id))
                .bind(occurred_at)
                .execute(&mut **transaction)
                .await?;
            }
        }
        // `app_revoked` is Remind ending one of its own sign-ins (one machine's
        // logout); every other reason ends all of them.
        WebhookPayload::MembershipSignedOut(data)
            if data.reason.as_deref() == Some("app_revoked") =>
        {
            return Ok(EventOutcome::Ignored);
        }
        WebhookPayload::MembershipSignedOut(_) => {
            ensure_row(transaction, uuid, None).await?;
            set_cutoff(transaction, uuid, occurred_at).await?;
        }
        WebhookPayload::MembershipAccessRemoved(_) => {
            remove_access(transaction, uuid, occurred_at, audit).await?;
        }
        WebhookPayload::AccountDeleted(_) => {
            delete_account(transaction, uuid, occurred_at, audit).await?;
        }
        _ => return Ok(EventOutcome::Ignored),
    }
    Ok(EventOutcome::Processed)
}

/// `account.updated` without a lookup: applies the event's data when its
/// account version is newer than the one stored.
async fn update_profile(
    transaction: &mut Tx<'_>,
    uuid: &str,
    account: &silicon_accounts_client::AccountForApp,
    occurred_at: DateTime<Utc>,
) -> Result<(), AppError> {
    ensure_row(transaction, uuid, Some(kind_name(account.kind))).await?;
    let custodian = account
        .custodian
        .as_ref()
        .filter(|custodian| is_valid_account_uuid(&custodian.uuid));
    sqlx::query(
        "UPDATE accounts SET display_name = $2, pfp_url = $3, \
             custodian_uuid = CASE WHEN kind = 'silicon' AND (custodian_observed_at IS NULL OR custodian_observed_at < $7) THEN COALESCE($4, custodian_uuid) \
                                   ELSE custodian_uuid END, \
             custodian_id = CASE WHEN kind = 'silicon' AND (custodian_observed_at IS NULL OR custodian_observed_at < $7) THEN COALESCE($5, custodian_id) \
                                 ELSE custodian_id END, \
             custodian_observed_at = GREATEST(custodian_observed_at, $7), \
             profile_version = $6, updated_at = clock_timestamp() \
         WHERE uuid = $1 AND profile_version < $6",
    )
    .bind(uuid)
    .bind(&account.display_name)
    .bind(&account.pfp_url)
    .bind(custodian.map(|custodian| custodian.uuid.as_str()))
    .bind(custodian.map(|custodian| public_id(&custodian.id)))
    .bind(account.version)
    .bind(occurred_at)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// `membership.access_removed`: tokens up to the event are refused; unless the
/// account already signed in again after it, its reminders stop firing and its
/// pending deliveries end until it signs in again.
async fn remove_access(
    transaction: &mut Tx<'_>,
    uuid: &str,
    occurred_at: DateTime<Utc>,
    audit: &AuditContext,
) -> Result<(), AppError> {
    ensure_row(transaction, uuid, None).await?;
    set_cutoff(transaction, uuid, occurred_at).await?;
    let suspended = sqlx::query(
        "UPDATE accounts SET status = 'access_removed', status_changed_at = $2, \
             updated_at = clock_timestamp() \
         WHERE uuid = $1 AND status = 'active' \
           AND (last_token_iat IS NULL OR last_token_iat <= $2)",
    )
    .bind(uuid)
    .bind(occurred_at)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
        > 0;
    if suspended {
        let keys = keys_in(transaction, uuid).await?;
        cleanup_owner_data(transaction, &keys, Utc::now(), OwnerCleanup::Suspend, audit).await?;
    }
    Ok(())
}

/// `account.deleted`: the account is anonymised and refused from now on; its
/// reminders are archived (45-day retention, then the deleted-reminder
/// ledger), its subscriptions disabled, and every grant and allow-list entry
/// it is part of ends. Other accounts' data is untouched.
pub(crate) async fn delete_account(
    transaction: &mut Tx<'_>,
    uuid: &str,
    occurred_at: DateTime<Utc>,
    audit: &AuditContext,
) -> Result<(), AppError> {
    ensure_row(transaction, uuid, None).await?;
    set_cutoff(transaction, uuid, occurred_at).await?;
    sqlx::query(
        "UPDATE accounts SET status = 'deleted', status_changed_at = $2, public_id = '', \
             display_name = '', pfp_url = '', custodian_uuid = NULL, custodian_id = NULL, updated_at = clock_timestamp() \
         WHERE uuid = $1",
    )
    .bind(uuid)
    .bind(occurred_at)
    .execute(&mut **transaction)
    .await?;
    let keys = keys_in(transaction, uuid).await?;
    cleanup_owner_data(transaction, &keys, Utc::now(), OwnerCleanup::Delete, audit).await?;
    for (table, left, right) in [
        ("reminder_viewers", "owner_uuid", "viewer_uuid"),
        ("silicon_allowances", "silicon_uuid", "allowed_uuid"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET revoked_at = clock_timestamp(), revoked_by_uuid = $1 \
             WHERE revoked_at IS NULL AND ({left} = $1 OR {right} = $1)"
        )))
        .bind(uuid)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

fn subject_of(payload: &WebhookPayload) -> Option<&str> {
    match payload {
        WebhookPayload::AccountIdChanged(data) => Some(&data.uuid),
        WebhookPayload::AccountUpdated(data) => Some(&data.uuid),
        WebhookPayload::AccountDeleted(data) | WebhookPayload::MembershipAccessRemoved(data) => {
            Some(&data.uuid)
        }
        WebhookPayload::MembershipSignedOut(data) => Some(&data.uuid),
        WebhookPayload::CustodianChanged(data) => Some(&data.uuid),
        _ => None,
    }
}

fn occurred_at(event: &WebhookEvent) -> DateTime<Utc> {
    event
        .occurred_at
        .and_then(|at| {
            Utc.timestamp_opt(at.unix_timestamp(), at.nanosecond())
                .single()
        })
        .unwrap_or_else(Utc::now)
}

const fn kind_name(kind: silicon_accounts_client::AccountKind) -> &'static str {
    match kind {
        silicon_accounts_client::AccountKind::Carbon => "carbon",
        silicon_accounts_client::AccountKind::Silicon => "silicon",
    }
}

fn public_id(id: &str) -> &str {
    if crate::domain::is_valid_public_id(id) {
        id
    } else {
        ""
    }
}

async fn ensure_row(
    transaction: &mut Tx<'_>,
    uuid: &str,
    kind: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO accounts (uuid, kind) VALUES ($1, $2) \
         ON CONFLICT (uuid) DO UPDATE SET kind = COALESCE(accounts.kind, EXCLUDED.kind)",
    )
    .bind(uuid)
    .bind(kind)
    .execute(&mut **transaction)
    .await?;
    sqlx::query("SELECT 1 FROM accounts WHERE uuid = $1 FOR UPDATE")
        .bind(uuid)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn set_cutoff(
    transaction: &mut Tx<'_>,
    uuid: &str,
    occurred_at: DateTime<Utc>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE accounts SET revoked_before = GREATEST(COALESCE(revoked_before, $2), $2), \
             updated_at = clock_timestamp() WHERE uuid = $1",
    )
    .bind(uuid)
    .bind(occurred_at)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn keys_in(transaction: &mut Tx<'_>, uuid: &str) -> Result<Vec<Uuid>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT storage_id FROM account_keys WHERE account_uuid = $1 ORDER BY storage_id",
    )
    .bind(uuid)
    .fetch_all(&mut **transaction)
    .await?)
}

fn body_invalid(message: &'static str) -> AppError {
    AppError::described(
        axum::http::StatusCode::BAD_REQUEST,
        "webhook_body_invalid",
        message,
    )
}
