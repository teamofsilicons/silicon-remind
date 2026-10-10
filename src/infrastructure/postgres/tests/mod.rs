//! Database tests for the repository, the identity store and account events.
//!
//! Each test gets its own database (`REMIND_TEST_POSTGRES_URL`, or a container).
//! Accounts are seeded directly with a fresh `looked_up_at`, so no test needs
//! Silicon Accounts to answer lookups.

use chrono::{DateTime, Duration, SubsecRound as _, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{ActorType, AuditContext, PostgresRepository, migrate};
use crate::{
    application::ports::Clock,
    domain::{Actor, ActorKind},
    infrastructure::identity::IdentityStore,
};

mod accounts;
mod bulk_status;
mod capacity;
mod lifecycle;
mod links;
mod migrations;
mod retention;
mod visibility;

pub(super) struct TestDatabase {
    _database: crate::test_support::TestPostgres,
    pub(super) pool: PgPool,
    pub(super) repository: PostgresRepository,
    pub(super) identity: IdentityStore,
}

pub(super) async fn test_database() -> anyhow::Result<TestDatabase> {
    let database = crate::test_support::TestPostgres::start().await?;
    let pool = database.pool(5).await?;
    migrate(&pool).await?;
    Ok(TestDatabase {
        _database: database,
        repository: PostgresRepository::new(pool.clone()),
        // Port 9 refuses connections: any unexpected Silicon Accounts call fails fast.
        identity: crate::test_support::identity_store(pool.clone(), "http://127.0.0.1:9")?,
        pool,
    })
}

// PostgreSQL keeps microseconds; compare fixtures at that precision.
pub(super) fn fixture_now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(6)
}

#[derive(Debug)]
pub(super) struct FixedClock(pub(super) DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

pub(super) fn service_audit() -> AuditContext {
    AuditContext {
        actor_type: ActorType::Service,
        actor_id: "repository-test".to_owned(),
        request_id: Some("req_repository_test".to_owned()),
    }
}

/// Seeds an active account (freshly looked up) with its primary storage key.
pub(super) async fn seed_account(
    pool: &PgPool,
    uuid: &str,
    kind: ActorKind,
    id: &str,
    custodian: Option<(&str, &str)>,
) -> anyhow::Result<Uuid> {
    sqlx::query(
        "INSERT INTO accounts (uuid, kind, public_id, display_name, custodian_uuid, custodian_id, \
             status, looked_up_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'active', clock_timestamp())",
    )
    .bind(uuid)
    .bind(kind.as_str())
    .bind(id)
    .bind(format!("{id} display"))
    .bind(custodian.map(|(uuid, _)| uuid))
    .bind(custodian.map(|(_, id)| id))
    .execute(pool)
    .await?;
    let key = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO account_keys (storage_id, account_uuid, origin) VALUES ($1, $2, 'accounts')",
    )
    .bind(key)
    .bind(uuid)
    .execute(pool)
    .await?;
    Ok(key)
}

/// Seeds an account-owned schedule (no organization).
#[allow(
    clippy::too_many_arguments,
    reason = "explicit fixture columns read clearly"
)]
pub(super) async fn seed_schedule(
    pool: &PgPool,
    owner_key: Uuid,
    silicon_id: &str,
    text: &str,
    kind: &str,
    status: &str,
    next_run_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let schedule_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, created_at, updated_at\
         ) VALUES ($1, NULL, $2, $3, $4, 'UTC', $5, '* * * * *', $6, $7, $8, $8)",
    )
    .bind(schedule_id)
    .bind(owner_key)
    .bind(silicon_id)
    .bind(text)
    .bind(kind)
    .bind(status)
    .bind(next_run_at)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(schedule_id)
}

/// Seeds an account and a schedule that is due one minute before `now`.
pub(super) async fn seed_due_schedule(
    pool: &PgPool,
    uuid: &str,
    silicon_id: &str,
    now: DateTime<Utc>,
    kind: &str,
) -> anyhow::Result<(Uuid, Uuid)> {
    let key = seed_account(pool, uuid, ActorKind::Silicon, silicon_id, None).await?;
    let schedule = seed_schedule(
        pool,
        key,
        silicon_id,
        "generation test",
        kind,
        "active",
        Some(now - Duration::minutes(1)),
        now - Duration::hours(1),
    )
    .await?;
    Ok((key, schedule))
}

/// Seeds a pending execution of a schedule.
pub(super) async fn seed_execution(
    pool: &PgPool,
    schedule_id: Uuid,
    silicon_id: &str,
    scheduled_for: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let execution_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO executions (\
             id, schedule_id, org_id, silicon_id, schedule_version, schedule_kind, \
             scheduled_for, reminder_text, timezone, status, next_attempt_at\
         ) VALUES ($1, $2, NULL, $3, 1, 'one_time', $4, 'execution fixture', 'UTC', \
             'pending', $4)",
    )
    .bind(execution_id)
    .bind(schedule_id)
    .bind(silicon_id)
    .bind(scheduled_for)
    .execute(pool)
    .await?;
    Ok(execution_id)
}

/// Resolves an actor exactly as a request with a fresh access token would.
pub(super) async fn actor(
    identity: &IdentityStore,
    uuid: &str,
    kind: ActorKind,
    id: &str,
) -> anyhow::Result<Actor> {
    let claims = claims(uuid, kind, id, Utc::now().timestamp())?;
    Ok(identity.resolve_bearer(&claims).await?)
}

/// Access-token claims as Silicon Accounts issues them to Remind.
pub(super) fn claims(
    uuid: &str,
    kind: ActorKind,
    id: &str,
    iat: i64,
) -> anyhow::Result<silicon_accounts_client::Claims> {
    Ok(serde_json::from_value(json!({
        "iss": "http://127.0.0.1:9",
        "sub": uuid,
        "aud": "remind",
        "exp": iat + 1800,
        "iat": iat,
        "kind": kind.as_str(),
        "id": id,
        "mid": format!("remind:{uuid}"),
        "fid": "family-1",
    }))?)
}

/// A signed-and-parsed Silicon Accounts webhook event for `account_events::apply`.
pub(super) fn event(
    event_id: &str,
    event_type: &str,
    occurred_at: DateTime<Utc>,
    data: &Value,
) -> anyhow::Result<(silicon_accounts_client::WebhookEvent, Vec<u8>)> {
    let body = serde_json::to_vec(&json!({
        "app_id": "remind",
        "data": data,
        "event_id": event_id,
        "occurred_at": occurred_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "silicon": null,
        "type": event_type,
    }))?;
    Ok((silicon_accounts_client::parse_webhook(&body)?, body))
}

pub(super) async fn materialize_schedule(
    repository: &PostgresRepository,
    schedule_id: Uuid,
    now: DateTime<Utc>,
    next_run_at: Option<DateTime<Utc>>,
) -> anyhow::Result<super::ExecutionRow> {
    let mut transaction = repository.begin().await?;
    let schedule = repository
        .lock_due_schedules(&mut transaction, now, 100)
        .await?
        .into_iter()
        .find(|schedule| schedule.id == schedule_id)
        .ok_or_else(|| anyhow::anyhow!("due schedule was not locked"))?;
    let outcome = repository
        .materialize_locked_occurrence(
            &mut transaction,
            &schedule,
            Uuid::now_v7(),
            now,
            next_run_at,
            &service_audit(),
        )
        .await?;
    transaction.commit().await?;
    assert!(outcome.inserted);
    Ok(outcome.execution)
}

pub(super) async fn claim_execution(
    repository: &PostgresRepository,
    execution_id: Uuid,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let claimed = repository
        .claim_deliveries(
            "execution-test-worker",
            now,
            std::time::Duration::from_secs(300),
            100,
        )
        .await?;
    anyhow::ensure!(
        claimed.iter().any(|execution| execution.id == execution_id),
        "materialized execution was not claimed"
    );
    Ok(())
}

pub(super) async fn due_ids(
    repository: &PostgresRepository,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Uuid>> {
    let mut transaction = repository.begin().await?;
    let ids = repository
        .lock_due_schedules(&mut transaction, now, 100)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect();
    transaction.rollback().await?;
    Ok(ids)
}
