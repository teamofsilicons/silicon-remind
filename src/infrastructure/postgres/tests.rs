use std::sync::Arc;

use chrono::{DateTime, Duration, SubsecRound as _, Utc};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ContainerAsync, ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

use crate::{
    application::{ports::Clock, schedules::ScheduleService},
    domain::{Actor, ReminderReadScope, ScheduleSection, ScheduleStatus},
    error::AppError,
};

use super::repository::{DELETED_REMINDER_LEDGER_LIMIT, trim_deleted_reminder_ledger};
use super::{
    ActorType, AuditContext, BulkScheduleStatusReplacement, CreateSchedule, ExecutionRow,
    IamLifecycleOutcome, IdempotencyContext, IdempotentMutation, ListSchedules,
    MutableScheduleStatus, NewHookDestination, NewInternalEvent, PostgresRepository,
    RepositoryError, ScheduleCursor, ScheduleStatusChange, health_check, migrate,
};

// PostgreSQL timestamps retain microseconds. Linux clocks can expose nanoseconds,
// so fixtures compared after a database round trip must use database precision.
fn fixture_now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(6)
}

struct TestDatabase {
    _container: ContainerAsync<Postgres>,
    pool: PgPool,
    repository: PostgresRepository,
}

#[derive(Debug)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

struct LifecycleFixture {
    now: DateTime<Utc>,
    target_principal_id: Uuid,
    target_schedule_id: Uuid,
    completed_schedule_id: Uuid,
    other_schedule_id: Uuid,
    execution_id: Uuid,
    completed_execution_id: Uuid,
    completed_at: DateTime<Utc>,
}

struct VisibilityFixture {
    carbon_scope: ReminderReadScope,
    denied_schedule: Uuid,
    first_allowed_schedule: Uuid,
    second_allowed_schedule: Uuid,
    allowed_execution: Uuid,
    denied_execution: Uuid,
}

struct ExpiredArchiveFixture {
    schedule: Uuid,
    pending_execution: Uuid,
    leased_execution: Uuid,
    created_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
}

struct BulkScheduleStatusFixture {
    owner_principal_id: Uuid,
    active_schedule_id: Uuid,
    paused_schedule_id: Uuid,
    completed_schedule_id: Uuid,
    archived_schedule_id: Uuid,
    execution_id: Uuid,
    execution_next_attempt_at: DateTime<Utc>,
    active_updated_at: DateTime<Utc>,
    paused_updated_at: DateTime<Utc>,
}

#[tokio::test]
async fn reminder_read_scope_is_enforced_before_keyset_pagination() -> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed_visibility_fixture(&database.pool).await?;

    assert_carbon_schedule_pages(&database.repository, &fixture).await?;
    assert_carbon_direct_reads(&database.repository, &fixture).await?;
    assert_empty_and_organization_scopes(&database.repository, &fixture).await?;
    Ok(())
}

async fn seed_visibility_fixture(pool: &PgPool) -> anyhow::Result<VisibilityFixture> {
    let denied_owner = Uuid::now_v7();
    let first_allowed_owner = Uuid::now_v7();
    let second_allowed_owner = Uuid::now_v7();
    let now = fixture_now();
    let denied_schedule = seed_visibility_schedule(
        pool,
        denied_owner,
        "denied:tos",
        "denied newest",
        now - Duration::minutes(1),
    )
    .await?;
    let first_allowed = seed_visibility_schedule(
        pool,
        first_allowed_owner,
        "first:tos",
        "first allowed",
        now - Duration::minutes(2),
    )
    .await?;
    let second_allowed = seed_visibility_schedule(
        pool,
        second_allowed_owner,
        "second:tos",
        "second allowed",
        now - Duration::minutes(3),
    )
    .await?;
    let allowed_execution =
        seed_visibility_execution(pool, first_allowed, "first:tos", now).await?;
    let denied_execution =
        seed_visibility_execution(pool, denied_schedule, "denied:tos", now).await?;

    Ok(VisibilityFixture {
        carbon_scope: ReminderReadScope::silicon_principals(vec![
            first_allowed_owner,
            second_allowed_owner,
        ]),
        denied_schedule,
        first_allowed_schedule: first_allowed,
        second_allowed_schedule: second_allowed,
        allowed_execution,
        denied_execution,
    })
}

async fn assert_carbon_schedule_pages(
    repository: &PostgresRepository,
    fixture: &VisibilityFixture,
) -> anyhow::Result<()> {
    let first_page = repository
        .list_schedules(&visibility_filters(fixture.carbon_scope.clone(), None, 1))
        .await?;
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.items[0].id, fixture.first_allowed_schedule);
    assert!(first_page.has_more);

    let second_page = repository
        .list_schedules(&visibility_filters(
            fixture.carbon_scope.clone(),
            Some(ScheduleCursor {
                created_at: first_page.items[0].created_at,
                id: first_page.items[0].id,
            }),
            1,
        ))
        .await?;
    assert_eq!(second_page.items.len(), 1);
    assert_eq!(second_page.items[0].id, fixture.second_allowed_schedule);
    assert!(!second_page.has_more);
    Ok(())
}

async fn assert_carbon_direct_reads(
    repository: &PostgresRepository,
    fixture: &VisibilityFixture,
) -> anyhow::Result<()> {
    assert!(
        repository
            .get_schedule("tos", fixture.first_allowed_schedule, &fixture.carbon_scope,)
            .await?
            .is_some()
    );
    assert!(
        repository
            .get_schedule("tos", fixture.denied_schedule, &fixture.carbon_scope)
            .await?
            .is_none()
    );
    assert!(
        repository
            .get_execution("tos", fixture.allowed_execution, &fixture.carbon_scope)
            .await?
            .is_some()
    );
    assert!(
        repository
            .get_execution("tos", fixture.denied_execution, &fixture.carbon_scope)
            .await?
            .is_none()
    );
    assert_eq!(
        repository
            .list_executions(
                "tos",
                fixture.first_allowed_schedule,
                &fixture.carbon_scope,
                None,
                20,
            )
            .await?
            .items
            .len(),
        1
    );
    assert!(matches!(
        repository
            .list_executions(
                "tos",
                fixture.denied_schedule,
                &fixture.carbon_scope,
                None,
                20,
            )
            .await,
        Err(RepositoryError::NotFound)
    ));
    Ok(())
}

async fn assert_empty_and_organization_scopes(
    repository: &PostgresRepository,
    fixture: &VisibilityFixture,
) -> anyhow::Result<()> {
    let empty_scope = ReminderReadScope::silicon_principals(Vec::new());
    let empty_page = repository
        .list_schedules(&visibility_filters(empty_scope.clone(), None, 20))
        .await?;
    assert!(empty_page.items.is_empty());
    assert!(
        repository
            .get_schedule("tos", fixture.first_allowed_schedule, &empty_scope)
            .await?
            .is_none()
    );

    let organization_scope = ReminderReadScope::organization();
    let organization_page = repository
        .list_schedules(&visibility_filters(organization_scope.clone(), None, 20))
        .await?;
    assert_eq!(organization_page.items.len(), 3);
    assert!(
        repository
            .get_schedule("tos", fixture.denied_schedule, &organization_scope)
            .await?
            .is_some()
    );
    Ok(())
}

#[tokio::test]
async fn bulk_schedule_status_preserves_order_noops_executions_and_exact_replay()
-> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed_bulk_schedule_status_fixture(&database.pool).await?;
    let mut request_order = vec![fixture.active_schedule_id, fixture.paused_schedule_id];
    request_order.sort_unstable();
    request_order.reverse();

    let (pause, idempotency, response) =
        apply_and_assert_bulk_pause(&database, &fixture, &request_order).await?;
    assert_exact_bulk_status_replay(&database, &pause, &idempotency, &response).await?;
    apply_and_assert_bulk_resume(&database, &fixture, &request_order).await?;
    Ok(())
}

#[tokio::test]
async fn bulk_schedule_status_rolls_back_stale_and_archived_batches() -> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed_bulk_schedule_status_fixture(&database.pool).await?;
    let initial_active = bulk_schedule_state(&database.pool, fixture.active_schedule_id).await?;
    let initial_paused = bulk_schedule_state(&database.pool, fixture.paused_schedule_id).await?;

    let stale = BulkScheduleStatusReplacement {
        org_id: "tos".to_owned(),
        owner_principal_id: fixture.owner_principal_id,
        status: MutableScheduleStatus::Paused,
        schedules: vec![
            ScheduleStatusChange {
                id: fixture.active_schedule_id,
                expected_version: 3,
                next_run_at: None,
            },
            ScheduleStatusChange {
                id: fixture.paused_schedule_id,
                expected_version: 6,
                next_run_at: None,
            },
        ],
    };
    assert!(matches!(
        database
            .repository
            .replace_schedule_statuses_idempotent(
                &stale,
                &bulk_status_idempotency(
                    fixture.owner_principal_id,
                    "bulk-status-stale",
                    [33; 32],
                ),
                &service_audit(),
            )
            .await,
        Err(RepositoryError::VersionConflict)
    ));
    assert_bulk_status_batch_unchanged(&database.pool, &fixture, &initial_active, &initial_paused)
        .await?;

    for (schedule_id, expected_version, key, request_hash) in [
        (
            fixture.archived_schedule_id,
            5,
            "bulk-status-archived",
            [34; 32],
        ),
        (
            fixture.completed_schedule_id,
            2,
            "bulk-status-completed",
            [35; 32],
        ),
    ] {
        let invalid_state = BulkScheduleStatusReplacement {
            org_id: "tos".to_owned(),
            owner_principal_id: fixture.owner_principal_id,
            status: MutableScheduleStatus::Paused,
            schedules: vec![
                ScheduleStatusChange {
                    id: fixture.active_schedule_id,
                    expected_version: 3,
                    next_run_at: None,
                },
                ScheduleStatusChange {
                    id: schedule_id,
                    expected_version,
                    next_run_at: None,
                },
            ],
        };
        assert!(matches!(
            database
                .repository
                .replace_schedule_statuses_idempotent(
                    &invalid_state,
                    &bulk_status_idempotency(fixture.owner_principal_id, key, request_hash,),
                    &service_audit(),
                )
                .await,
            Err(RepositoryError::InvalidState)
        ));
        assert_bulk_status_batch_unchanged(
            &database.pool,
            &fixture,
            &initial_active,
            &initial_paused,
        )
        .await?;
    }

    assert!(bulk_status_audits(&database.pool).await?.is_empty());
    let idempotency_rows = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM idempotency_records WHERE operation = 'schedule.bulk_status'",
    )
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(idempotency_rows, 0);
    Ok(())
}

#[tokio::test]
async fn bulk_schedule_status_uses_deterministic_error_precedence() -> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed_bulk_schedule_status_fixture(&database.pool).await?;
    let foreign_schedule_id = seed_foreign_bulk_status_schedule(&database.pool).await?;
    let expired_schedule_id =
        seed_expired_bulk_status_schedule(&database.pool, fixture.owner_principal_id).await?;
    let service = ScheduleService::new(
        database.repository.clone(),
        Arc::new(FixedClock(fixture_now())),
        std::time::Duration::from_hours(24),
    );
    let actor = Actor::silicon(
        fixture.owner_principal_id.to_string(),
        "tos",
        Uuid::now_v7(),
        1,
    );

    assert!(matches!(
        service
            .update_statuses(
                &actor,
                vec![
                    expired_schedule_id,
                    foreign_schedule_id,
                    fixture.archived_schedule_id,
                ],
                ScheduleStatus::Paused,
                "precedence-missing".to_owned(),
                [41; 32],
            )
            .await,
        Err(AppError::NotFound)
    ));
    assert!(matches!(
        service
            .update_statuses(
                &actor,
                vec![foreign_schedule_id, fixture.archived_schedule_id],
                ScheduleStatus::Paused,
                "precedence-forbidden".to_owned(),
                [42; 32],
            )
            .await,
        Err(AppError::Forbidden)
    ));
    assert!(matches!(
        service
            .update_statuses(
                &actor,
                vec![fixture.archived_schedule_id],
                ScheduleStatus::Paused,
                "precedence-archived".to_owned(),
                [43; 32],
            )
            .await,
        Err(AppError::Conflict { code }) if code == "invalid_schedule_state"
    ));
    Ok(())
}

#[tokio::test]
async fn bulk_pause_serializes_after_scheduler_materialization() -> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    let schedule_id = seed_due_schedule(&database.pool, "bulk-race:tos", now, "recurring").await?;
    let owner_principal_id = schedule_owner(&database.pool, schedule_id).await?;
    let mut scheduler_transaction = database.repository.begin().await?;
    let schedule = database
        .repository
        .lock_due_schedules(&mut scheduler_transaction, now, 100)
        .await?
        .into_iter()
        .find(|row| row.id == schedule_id)
        .ok_or_else(|| anyhow::anyhow!("due schedule was not locked"))?;
    let next_run_at = now + Duration::minutes(1);

    let pause_repository = database.repository.clone();
    let pause_replacement = BulkScheduleStatusReplacement {
        org_id: "tos".to_owned(),
        owner_principal_id,
        status: MutableScheduleStatus::Paused,
        schedules: vec![ScheduleStatusChange {
            id: schedule_id,
            expected_version: schedule.version,
            next_run_at: None,
        }],
    };
    let pause_idempotency =
        bulk_status_idempotency(owner_principal_id, "bulk-race-pause", [44; 32]);
    let mut pause_task = tokio::spawn(async move {
        pause_repository
            .replace_schedule_statuses_idempotent(
                &pause_replacement,
                &pause_idempotency,
                &service_audit(),
            )
            .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut pause_task)
            .await
            .is_err()
    );

    let materialized = database
        .repository
        .materialize_locked_occurrence(
            &mut scheduler_transaction,
            &schedule,
            Uuid::now_v7(),
            now,
            Some(next_run_at),
            &service_audit(),
        )
        .await?;
    scheduler_transaction.commit().await?;

    assert!(matches!(
        pause_task.await?,
        Err(RepositoryError::VersionConflict)
    ));
    let state = bulk_schedule_state(&database.pool, schedule_id).await?;
    assert_eq!(state.0, "active");
    assert_eq!(state.1, Some(next_run_at));
    assert_eq!(state.2, 2);
    assert!(materialized.inserted);
    assert_eq!(materialized.execution.schedule_id, schedule_id);
    Ok(())
}

fn visibility_filters(
    read_scope: ReminderReadScope,
    cursor: Option<ScheduleCursor>,
    limit: u32,
) -> ListSchedules {
    ListSchedules {
        org_id: "tos".to_owned(),
        read_scope,
        silicon_id: None,
        section: ScheduleSection::Current,
        status: None,
        cursor,
        limit,
    }
}

#[derive(Debug, sqlx::FromRow)]
struct DeletedReminderRow {
    id: i64,
    schedule_id: Uuid,
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
    purged_at: DateTime<Utc>,
    purge_reason: String,
    record_text: String,
}

struct PurgeFixture {
    now: DateTime<Utc>,
    archived_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    schedule_id: Uuid,
    owner_principal_id: Uuid,
    reminder_text: &'static str,
    cron_expression: &'static str,
    last_trigger: DateTime<Utc>,
}

#[tokio::test]
async fn iam_silicon_removal_is_atomic_and_replay_safe() -> anyhow::Result<()> {
    let database = test_database().await?;
    health_check(&database.pool).await?;
    let fixture = seed_lifecycle_fixture(&database.pool).await?;
    let event = lifecycle_event(
        "iam-event-1",
        "organization.membership.removed.v1",
        Some(&fixture.target_principal_id.to_string()),
        fixture.now,
    )?;
    let audit = service_audit();

    let outcome = database
        .repository
        .apply_iam_lifecycle_event(&event, &audit)
        .await?;
    assert!(!outcome.replayed);
    assert_eq!(outcome.schedules_deleted, 1);
    assert_eq!(outcome.executions_failed, 2);
    assert_eq!(outcome.receipt.status, "processed");
    assert_silicon_removal_state(&database.pool, &fixture).await?;

    assert_exact_replay(&database.repository, &event, &audit, &outcome).await?;
    assert_hash_conflict(&database.repository, &event, &audit).await;

    let organization_event =
        lifecycle_event("iam-event-2", "organization.updated.v1", None, fixture.now)?;
    let organization_outcome = database
        .repository
        .apply_iam_lifecycle_event(&organization_event, &audit)
        .await?;
    assert_eq!(organization_outcome.schedules_deleted, 1);
    assert_eq!(organization_outcome.executions_failed, 0);
    assert_organization_removal_state(&database.pool, fixture.other_schedule_id).await?;
    Ok(())
}

#[tokio::test]
async fn idempotent_response_lookup_validates_hash_and_expiry() -> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    let request_hash = [3_u8; 32];
    insert_idempotency_response(
        &database.pool,
        "replay-key",
        &request_hash,
        now,
        now + Duration::hours(24),
    )
    .await?;
    let context = IdempotencyContext {
        actor_type: ActorType::Silicon,
        actor_id: "remaining:tos".to_owned(),
        key: "replay-key".to_owned(),
        request_hash,
        expires_at: now + Duration::hours(24),
    };

    let stored = database
        .repository
        .find_idempotent_response("tos", "schedule.create", None, &context)
        .await?
        .ok_or_else(|| anyhow::anyhow!("stored response was not found"))?;
    assert_eq!(stored.status_code, 201);
    assert_eq!(stored.response_body["id"], "stored");

    let mut mismatched = context.clone();
    mismatched.request_hash = [4_u8; 32];
    let mismatch = database
        .repository
        .find_idempotent_response("tos", "schedule.create", None, &mismatched)
        .await;
    assert!(matches!(
        mismatch,
        Err(RepositoryError::IdempotencyConflict)
    ));

    insert_idempotency_response(
        &database.pool,
        "expired-key",
        &request_hash,
        now - Duration::hours(48),
        now - Duration::hours(24),
    )
    .await?;
    let expired_context = IdempotencyContext {
        key: "expired-key".to_owned(),
        ..context
    };
    let expired = database
        .repository
        .find_idempotent_response("tos", "schedule.create", None, &expired_context)
        .await?;
    assert!(expired.is_none());
    Ok(())
}

#[tokio::test]
async fn schedule_purge_logs_full_snapshot_and_trims_deterministic_oldest() -> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed_expired_schedule(&database.pool).await?;
    let result = database
        .repository
        .purge_expired_schedules(fixture.now, 100, "retention-test-worker")
        .await?;
    assert_eq!(result.purged, 1);
    assert_eq!(result.logged, 1);
    assert_eq!(result.trimmed, 0);

    let ledger_id = assert_deleted_reminder_snapshot(&database.pool, &fixture).await?;
    assert_purge_source_removed_and_audited(&database.pool, &fixture, ledger_id).await?;
    assert_trim_keeps_deterministic_newest(&database.pool, &fixture).await?;
    assert_ledger_conflict_preserves_source(&database).await?;
    Ok(())
}

#[tokio::test]
async fn completed_one_time_purge_logs_automatic_archive_snapshot() -> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed_expired_archive(&database.pool).await?;
    let purged_at = fixture_now();
    let result = database
        .repository
        .purge_expired_schedules(purged_at, 100, "completed-purge-test-worker")
        .await?;
    assert_eq!(result.purged, 1);
    assert_eq!(result.logged, 1);

    let ledger = sqlx::query_as::<_, DeletedReminderRow>(
        "SELECT id, schedule_id, org_id, owner_principal_id, silicon_id, \
                reminder_text, schedule_kind, cron_expression, timezone, \
                last_triggered_at, created_at, archived_at, purge_after, purged_at, \
                purge_reason, record_text \
         FROM deleted_reminders WHERE schedule_id = $1",
    )
    .bind(fixture.schedule)
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(ledger.schedule_kind, "one_time");
    assert_eq!(ledger.created_at, fixture.created_at);
    assert_eq!(ledger.archived_at, fixture.completed_at);
    assert_eq!(
        ledger.purge_after,
        fixture.completed_at + Duration::days(crate::domain::ARCHIVE_RETENTION_DAYS)
    );
    assert_eq!(ledger.purged_at, purged_at);
    assert_eq!(ledger.last_triggered_at, Some(fixture.completed_at));
    assert_eq!(ledger.purge_reason, "completed");
    let record = serde_json::from_str::<Value>(&ledger.record_text)?;
    assert_eq!(record["archived_at"], json!(fixture.completed_at));
    assert_eq!(record["purge_reason"], "completed");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM schedules WHERE id = $1")
            .bind(fixture.schedule)
            .fetch_one(&database.pool)
            .await?,
        0
    );
    Ok(())
}

async fn seed_expired_schedule(pool: &PgPool) -> anyhow::Result<PurgeFixture> {
    let now = "2030-01-01T00:00:00Z".parse::<DateTime<Utc>>()?;
    let archived_at = now - Duration::days(45);
    let created_at = archived_at - Duration::days(30);
    let schedule_id = Uuid::now_v7();
    let owner_principal_id = Uuid::now_v7();
    let reminder_text = "Take medication\nwith water";
    let cron_expression = "0 9 * * 1";
    sqlx::query(
        "INSERT INTO schedules ( \
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, deleted_at, created_at, \
             updated_at \
         ) VALUES ($1, 'tos', $2, 'assistant:tos', $3, 'Asia/Kolkata', 'recurring', $4, \
             'paused', NULL, $5, $6, $5)",
    )
    .bind(schedule_id)
    .bind(owner_principal_id)
    .bind(reminder_text)
    .bind(cron_expression)
    .bind(archived_at)
    .bind(created_at)
    .execute(pool)
    .await?;

    let first_trigger = archived_at - Duration::days(2);
    let last_trigger = archived_at - Duration::days(1);
    for scheduled_for in [first_trigger, last_trigger] {
        sqlx::query(
            "INSERT INTO executions ( \
                 id, schedule_id, org_id, silicon_id, schedule_version, schedule_kind, \
                 scheduled_for, reminder_text, timezone, status, attempt_count, \
                 attempted_at, failure_reason, created_at, updated_at \
             ) VALUES ($1, $2, 'tos', 'assistant:tos', 1, 'recurring', $3, $4, \
                 'Asia/Kolkata', 'failed', 1, $3, 'retention fixture', $3, $3)",
        )
        .bind(Uuid::now_v7())
        .bind(schedule_id)
        .bind(scheduled_for)
        .bind(reminder_text)
        .execute(pool)
        .await?;
    }
    Ok(PurgeFixture {
        now,
        archived_at,
        created_at,
        schedule_id,
        owner_principal_id,
        reminder_text,
        cron_expression,
        last_trigger,
    })
}

async fn assert_deleted_reminder_snapshot(
    pool: &PgPool,
    fixture: &PurgeFixture,
) -> anyhow::Result<i64> {
    let ledger = sqlx::query_as::<_, DeletedReminderRow>(
        "SELECT id, schedule_id, org_id, owner_principal_id, silicon_id, \
                reminder_text, schedule_kind, cron_expression, timezone, \
                last_triggered_at, created_at, archived_at, purge_after, purged_at, \
                purge_reason, record_text \
         FROM deleted_reminders WHERE schedule_id = $1",
    )
    .bind(fixture.schedule_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(ledger.schedule_id, fixture.schedule_id);
    assert_eq!(ledger.org_id, "tos");
    assert_eq!(ledger.owner_principal_id, fixture.owner_principal_id);
    assert_eq!(ledger.silicon_id, "assistant:tos");
    assert_eq!(ledger.reminder_text, fixture.reminder_text);
    assert_eq!(ledger.schedule_kind, "recurring");
    assert_eq!(ledger.cron_expression, fixture.cron_expression);
    assert_eq!(ledger.timezone, "Asia/Kolkata");
    assert_eq!(ledger.last_triggered_at, Some(fixture.last_trigger));
    assert_eq!(ledger.created_at, fixture.created_at);
    assert_eq!(ledger.archived_at, fixture.archived_at);
    assert_eq!(ledger.purge_after, fixture.now);
    assert_eq!(ledger.purged_at, fixture.now);
    assert_eq!(ledger.purge_reason, "deleted");
    assert!(!ledger.record_text.contains(['\n', '\r']));
    let record = serde_json::from_str::<Value>(&ledger.record_text)?;
    assert_eq!(
        record,
        json!({
            "schema_version": "1.0",
            "schedule_id": fixture.schedule_id,
            "org_id": "tos",
            "owner_principal_id": fixture.owner_principal_id,
            "silicon_id": "assistant:tos",
            "reminder_text": fixture.reminder_text,
            "schedule_kind": "recurring",
            "cron_expression": fixture.cron_expression,
            "timezone": "Asia/Kolkata",
            "last_triggered_at": fixture.last_trigger,
            "created_at": fixture.created_at,
            "archived_at": fixture.archived_at,
            "purge_after": fixture.now,
            "purged_at": fixture.now,
            "purge_reason": "deleted",
        })
    );
    Ok(ledger.id)
}

async fn assert_purge_source_removed_and_audited(
    pool: &PgPool,
    fixture: &PurgeFixture,
    ledger_id: i64,
) -> anyhow::Result<()> {
    let source_rows = sqlx::query_scalar::<_, i64>(
        "SELECT (SELECT count(*) FROM schedules WHERE id = $1) \
              + (SELECT count(*) FROM executions WHERE schedule_id = $1)",
    )
    .bind(fixture.schedule_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(source_rows, 0);
    let audit_metadata = sqlx::query_scalar::<_, Value>(
        "SELECT metadata FROM audit_records \
         WHERE action = 'schedule.purged' AND resource_id = $1",
    )
    .bind(fixture.schedule_id.to_string())
    .fetch_one(pool)
    .await?;
    assert_eq!(
        audit_metadata,
        json!({
            "deleted_reminder_id": ledger_id,
            "purge_reason": "deleted",
        })
    );
    Ok(())
}

async fn assert_trim_keeps_deterministic_newest(
    pool: &PgPool,
    fixture: &PurgeFixture,
) -> anyhow::Result<()> {
    assert_eq!(DELETED_REMINDER_LEDGER_LIMIT, 100_000);
    let mut newest_schedule_ids = Vec::new();
    for index in 0..4 {
        let trim_schedule_id = Uuid::now_v7();
        newest_schedule_ids.push(trim_schedule_id);
        sqlx::query(
            "INSERT INTO deleted_reminders ( \
                 schedule_id, org_id, owner_principal_id, silicon_id, reminder_text, \
                 schedule_kind, cron_expression, timezone, created_at, archived_at, purge_after, \
                 purged_at, purge_reason, record_text \
             ) VALUES ($1, 'tos', $2, 'assistant:tos', $3, 'one_time', '0 0 * * *', 'UTC', \
                 $4, $5, $6, $7, 'deleted', '{}'::text)",
        )
        .bind(trim_schedule_id)
        .bind(Uuid::now_v7())
        .bind(format!("trim fixture {index}"))
        .bind(fixture.created_at)
        .bind(fixture.archived_at)
        .bind(fixture.now)
        .bind(fixture.now)
        .execute(pool)
        .await?;
    }
    let mut transaction = pool.begin().await?;
    let trimmed = trim_deleted_reminder_ledger(&mut transaction, 3).await?;
    transaction.commit().await?;
    assert_eq!(trimmed, 2);
    let remaining =
        sqlx::query_scalar::<_, Uuid>("SELECT schedule_id FROM deleted_reminders ORDER BY id")
            .fetch_all(pool)
            .await?;
    assert_eq!(remaining, newest_schedule_ids[1..]);
    Ok(())
}

async fn assert_ledger_conflict_preserves_source(database: &TestDatabase) -> anyhow::Result<()> {
    let fixture = seed_expired_schedule(&database.pool).await?;
    sqlx::query(
        "INSERT INTO deleted_reminders ( \
             schedule_id, org_id, owner_principal_id, silicon_id, reminder_text, \
             schedule_kind, cron_expression, timezone, last_triggered_at, created_at, \
             archived_at, purge_after, purged_at, purge_reason, record_text \
         ) VALUES ($1, 'tos', $2, 'assistant:tos', $3, 'recurring', $4, \
             'Asia/Kolkata', $5, $6, $7, $8, $9, 'deleted', '{}'::text)",
    )
    .bind(fixture.schedule_id)
    .bind(fixture.owner_principal_id)
    .bind(fixture.reminder_text)
    .bind(fixture.cron_expression)
    .bind(fixture.last_trigger)
    .bind(fixture.created_at)
    .bind(fixture.archived_at)
    .bind(fixture.now)
    .bind(fixture.now)
    .execute(&database.pool)
    .await?;

    let result = database
        .repository
        .purge_expired_schedules(fixture.now, 100, "retention-test-worker")
        .await;
    assert!(matches!(result, Err(RepositoryError::Database(_))));
    let source_rows = sqlx::query_scalar::<_, i64>(
        "SELECT (SELECT count(*) FROM schedules WHERE id = $1) \
              + (SELECT count(*) FROM executions WHERE schedule_id = $1)",
    )
    .bind(fixture.schedule_id)
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(source_rows, 3);
    let audit_rows = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM audit_records \
         WHERE action = 'schedule.purged' AND resource_id = $1",
    )
    .bind(fixture.schedule_id.to_string())
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(audit_rows, 0);
    Ok(())
}

#[tokio::test]
async fn one_time_materialization_archives_before_delivery_without_extending_retention()
-> anyhow::Result<()> {
    let database = test_database().await?;
    let seed_now = fixture_now();

    let one_time_id =
        seed_due_schedule(&database.pool, "one-time:tos", seed_now, "one_time").await?;
    let worker_now = fixture_now() + Duration::seconds(1);
    let execution =
        materialize_schedule(&database.repository, one_time_id, worker_now, None).await?;
    assert_eq!(execution.schedule_version, 2);
    assert_eq!(execution.schedule_kind, "one_time");
    let before_delivery = schedule_archive_state(&database.pool, one_time_id).await?;
    assert_eq!(before_delivery.0, "completed");
    assert_eq!(before_delivery.1, 2);
    let archived_at = before_delivery
        .2
        .ok_or_else(|| anyhow::anyhow!("one-time schedule was not archived"))?;
    assert_eq!(
        before_delivery.3,
        Some(archived_at + Duration::days(crate::domain::ARCHIVE_RETENTION_DAYS))
    );

    claim_execution(&database.repository, execution.id, worker_now).await?;
    database
        .repository
        .mark_delivery_succeeded(
            execution.id,
            "execution-test-worker",
            Uuid::now_v7(),
            worker_now + Duration::seconds(1),
        )
        .await?;
    assert_eq!(
        schedule_archive_state(&database.pool, one_time_id).await?,
        before_delivery
    );

    let recurring_id =
        seed_due_schedule(&database.pool, "recurring:tos", seed_now, "recurring").await?;
    let recurring = materialize_schedule(
        &database.repository,
        recurring_id,
        worker_now,
        Some(worker_now + Duration::minutes(1)),
    )
    .await?;
    assert_eq!(recurring.schedule_version, 2);
    assert_eq!(recurring.schedule_kind, "recurring");
    claim_execution(&database.repository, recurring.id, worker_now).await?;
    database
        .repository
        .mark_delivery_failed(
            recurring.id,
            "execution-test-worker",
            worker_now + Duration::seconds(1),
            "terminal test failure",
        )
        .await?;
    assert_schedule_state(&database.pool, recurring_id, "active", 2, false).await?;
    Ok(())
}

#[tokio::test]
async fn current_and_archived_sections_are_partitioned_before_pagination() -> anyhow::Result<()> {
    let database = test_database().await?;
    let seed_now = fixture_now();
    let current_id =
        seed_due_schedule(&database.pool, "current-section:tos", seed_now, "recurring").await?;
    let manual_id =
        seed_due_schedule(&database.pool, "manual-archive:tos", seed_now, "recurring").await?;
    let manual_owner = schedule_owner(&database.pool, manual_id).await?;
    let archived_at = fixture_now() + Duration::seconds(1);
    assert!(
        database
            .repository
            .archive_schedule(
                "tos",
                manual_owner,
                manual_id,
                archived_at,
                &service_audit(),
            )
            .await?
    );

    let automatic_id = seed_due_schedule(
        &database.pool,
        "automatic-archive:tos",
        seed_now,
        "one_time",
    )
    .await?;
    let automatic_owner = schedule_owner(&database.pool, automatic_id).await?;
    let worker_now = fixture_now() + Duration::seconds(2);
    let automatic_execution =
        materialize_schedule(&database.repository, automatic_id, worker_now, None).await?;

    assert_schedule_sections(
        &database.repository,
        current_id,
        manual_id,
        automatic_id,
        automatic_execution.id,
    )
    .await?;

    let automatic_before = schedule_archive_state(&database.pool, automatic_id).await?;
    assert!(
        !database
            .repository
            .archive_schedule(
                "tos",
                automatic_owner,
                automatic_id,
                worker_now + Duration::days(1),
                &service_audit(),
            )
            .await?
    );
    assert_eq!(
        schedule_archive_state(&database.pool, automatic_id).await?,
        automatic_before
    );
    assert_cancelled_execution(&database.pool, automatic_execution.id).await?;
    Ok(())
}

async fn assert_schedule_sections(
    repository: &PostgresRepository,
    current_id: Uuid,
    manual_id: Uuid,
    automatic_id: Uuid,
    automatic_execution_id: Uuid,
) -> anyhow::Result<()> {
    let read_scope = ReminderReadScope::organization();
    let current = repository
        .list_schedules(&ListSchedules {
            org_id: "tos".to_owned(),
            read_scope: read_scope.clone(),
            silicon_id: None,
            section: ScheduleSection::Current,
            status: None,
            cursor: None,
            limit: 100,
        })
        .await?;
    assert_eq!(current.items.len(), 1);
    assert_eq!(current.items[0].id, current_id);

    let archived = repository
        .list_schedules(&ListSchedules {
            org_id: "tos".to_owned(),
            read_scope: read_scope.clone(),
            silicon_id: None,
            section: ScheduleSection::Archived,
            status: None,
            cursor: None,
            limit: 100,
        })
        .await?;
    let archived_ids = archived
        .items
        .iter()
        .map(|schedule| schedule.id)
        .collect::<Vec<_>>();
    assert_eq!(archived_ids.len(), 2);
    assert!(archived_ids.contains(&manual_id));
    assert!(archived_ids.contains(&automatic_id));
    assert!(
        repository
            .get_schedule("tos", manual_id, &read_scope)
            .await?
            .is_some()
    );
    assert_eq!(
        repository
            .list_executions("tos", automatic_id, &read_scope, None, 100,)
            .await?
            .items[0]
            .id,
        automatic_execution_id
    );
    Ok(())
}

#[tokio::test]
async fn manual_archive_cancels_unaccepted_work_without_extending_retention() -> anyhow::Result<()>
{
    let database = test_database().await?;
    let now = fixture_now();
    let schedule_id =
        seed_due_schedule(&database.pool, "archive-cancel:tos", now, "recurring").await?;
    let execution_ids =
        seed_unaccepted_archive_executions(&database.pool, schedule_id, now).await?;
    let owner = schedule_owner(&database.pool, schedule_id).await?;
    let archived_at = now + Duration::seconds(1);

    assert!(
        database
            .repository
            .archive_schedule("tos", owner, schedule_id, archived_at, &service_audit())
            .await?
    );
    assert_manual_archive_cancellation(&database.pool, schedule_id, &execution_ids).await?;
    assert!(
        !database
            .repository
            .delivery_lease_is_live(execution_ids[1], "archive-test-worker", archived_at)
            .await?
    );

    let original_state = manual_archive_state(&database.pool, schedule_id).await?;
    assert!(
        !database
            .repository
            .archive_schedule(
                "tos",
                owner,
                schedule_id,
                archived_at + Duration::days(1),
                &service_audit(),
            )
            .await?
    );
    assert_eq!(
        manual_archive_state(&database.pool, schedule_id).await?,
        original_state
    );
    Ok(())
}

#[tokio::test]
async fn expired_archive_is_hidden_and_cannot_resume_delivery() -> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed_expired_archive(&database.pool).await?;
    let read_scope = ReminderReadScope::organization();

    let archived = database
        .repository
        .list_schedules(&ListSchedules {
            org_id: "tos".to_owned(),
            read_scope: read_scope.clone(),
            silicon_id: None,
            section: ScheduleSection::Archived,
            status: None,
            cursor: None,
            limit: 100,
        })
        .await?;
    assert!(archived.items.is_empty());
    assert!(
        database
            .repository
            .get_schedule("tos", fixture.schedule, &read_scope)
            .await?
            .is_none()
    );
    assert!(
        database
            .repository
            .get_execution("tos", fixture.pending_execution, &read_scope)
            .await?
            .is_none()
    );
    assert!(matches!(
        database
            .repository
            .list_executions("tos", fixture.schedule, &read_scope, None, 100)
            .await,
        Err(RepositoryError::NotFound)
    ));

    let now = fixture_now();
    assert!(
        database
            .repository
            .claim_deliveries(
                "expiry-test-worker",
                now,
                std::time::Duration::from_secs(300),
                100,
            )
            .await?
            .is_empty()
    );
    assert!(
        !database
            .repository
            .delivery_lease_is_live(fixture.leased_execution, "expired-lease-worker", now,)
            .await?
    );
    assert!(matches!(
        database
            .repository
            .mark_delivery_failed(
                fixture.leased_execution,
                "expired-lease-worker",
                now,
                "must remain expired",
            )
            .await,
        Err(RepositoryError::LeaseLost)
    ));
    Ok(())
}

async fn seed_unaccepted_archive_executions(
    pool: &PgPool,
    schedule_id: Uuid,
    now: DateTime<Utc>,
) -> anyhow::Result<[Uuid; 2]> {
    let pending_id = Uuid::now_v7();
    let retrying_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO executions (\
             id, schedule_id, org_id, silicon_id, schedule_version, schedule_kind, \
             scheduled_for, reminder_text, timezone, status, attempt_count, \
             next_attempt_at, attempted_at, failure_reason, lease_owner, lease_expires_at\
         ) VALUES \
             ($1, $3, 'tos', 'archive-cancel:tos', 1, 'recurring', $4, \
              'pending archive', 'UTC', 'pending', 0, $6, NULL, NULL, NULL, NULL), \
             ($2, $3, 'tos', 'archive-cancel:tos', 1, 'recurring', $5, \
              'retrying archive', 'UTC', 'retrying', 1, $6, $4, \
              'temporary failure', 'archive-test-worker', $7)",
    )
    .bind(pending_id)
    .bind(retrying_id)
    .bind(schedule_id)
    .bind(now - Duration::minutes(3))
    .bind(now - Duration::minutes(2))
    .bind(now - Duration::minutes(1))
    .bind(now + Duration::minutes(5))
    .execute(pool)
    .await?;
    Ok([pending_id, retrying_id])
}

async fn assert_manual_archive_cancellation(
    pool: &PgPool,
    schedule_id: Uuid,
    execution_ids: &[Uuid; 2],
) -> anyhow::Result<()> {
    let states = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Option<DateTime<Utc>>,
            Option<String>,
            Option<DateTime<Utc>>,
            Option<String>,
        ),
    >(
        "SELECT id, status, next_attempt_at, lease_owner, lease_expires_at, failure_reason \
         FROM executions WHERE id = ANY($1) ORDER BY id",
    )
    .bind(execution_ids.as_slice())
    .fetch_all(pool)
    .await?;
    assert_eq!(states.len(), 2);
    for (_, status, next_attempt_at, lease_owner, lease_expires_at, failure_reason) in states {
        assert_eq!(status, "failed");
        assert!(next_attempt_at.is_none());
        assert!(lease_owner.is_none());
        assert!(lease_expires_at.is_none());
        assert_eq!(
            failure_reason.as_deref(),
            Some("stopped after reminder archive")
        );
    }
    let metadata = sqlx::query_scalar::<_, Value>(
        "SELECT metadata FROM audit_records \
         WHERE action = 'schedule.archived' AND resource_id = $1",
    )
    .bind(schedule_id.to_string())
    .fetch_one(pool)
    .await?;
    assert_eq!(metadata["executions_cancelled"], 2);
    Ok(())
}

async fn assert_cancelled_execution(pool: &PgPool, execution_id: Uuid) -> anyhow::Result<()> {
    let (status, next_attempt_at, lease_owner, failure_reason) = sqlx::query_as::<
        _,
        (
            String,
            Option<DateTime<Utc>>,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT status, next_attempt_at, lease_owner, failure_reason \
         FROM executions WHERE id = $1",
    )
    .bind(execution_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(status, "failed");
    assert!(next_attempt_at.is_none());
    assert!(lease_owner.is_none());
    assert_eq!(
        failure_reason.as_deref(),
        Some("stopped after reminder archive")
    );
    Ok(())
}

async fn manual_archive_state(
    pool: &PgPool,
    schedule_id: Uuid,
) -> anyhow::Result<(i64, DateTime<Utc>, DateTime<Utc>)> {
    sqlx::query_as("SELECT version, deleted_at, purge_after FROM schedules WHERE id = $1")
        .bind(schedule_id)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
}

async fn seed_expired_archive(pool: &PgPool) -> anyhow::Result<ExpiredArchiveFixture> {
    let now = fixture_now();
    let completed_at = now - Duration::days(46);
    let created_at = completed_at - Duration::days(1);
    let owner_principal_id = Uuid::now_v7();
    let schedule_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO organization_lifecycle (org_id, state) \
         VALUES ('tos', 'active') ON CONFLICT (org_id) DO NOTHING",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO silicon_identities (org_id, principal_id, silicon_id, state) \
         VALUES ('tos', $1, 'expired-archive:tos', 'active')",
    )
    .bind(owner_principal_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, completed_at, \
             created_at, updated_at\
         ) VALUES (\
             $1, 'tos', $2, 'expired-archive:tos', 'expired archive', 'UTC', \
             'one_time', '* * * * *', 'completed', NULL, $3, $4, $3\
         )",
    )
    .bind(schedule_id)
    .bind(owner_principal_id)
    .bind(completed_at)
    .bind(created_at)
    .execute(pool)
    .await?;

    let pending_execution_id = Uuid::now_v7();
    let leased_execution_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO executions (\
             id, schedule_id, org_id, silicon_id, schedule_version, schedule_kind, \
             scheduled_for, reminder_text, timezone, status, attempt_count, \
             next_attempt_at, attempted_at, failure_reason, lease_owner, lease_expires_at\
         ) VALUES \
             ($1, $3, 'tos', 'expired-archive:tos', 2, 'one_time', $4, \
              'expired pending', 'UTC', 'pending', 0, $6, NULL, NULL, NULL, NULL), \
             ($2, $3, 'tos', 'expired-archive:tos', 2, 'one_time', $5, \
              'expired leased', 'UTC', 'retrying', 1, $6, $5, \
              'temporary failure', 'expired-lease-worker', $7)",
    )
    .bind(pending_execution_id)
    .bind(leased_execution_id)
    .bind(schedule_id)
    .bind(completed_at)
    .bind(completed_at - Duration::minutes(1))
    .bind(now - Duration::minutes(1))
    .bind(now + Duration::minutes(5))
    .execute(pool)
    .await?;
    Ok(ExpiredArchiveFixture {
        schedule: schedule_id,
        pending_execution: pending_execution_id,
        leased_execution: leased_execution_id,
        created_at,
        completed_at,
    })
}

#[tokio::test]
async fn principal_binding_preserves_public_id_and_revocation_tombstone() -> anyhow::Result<()> {
    let database = test_database().await?;
    let principal_id = Uuid::now_v7();
    let audit = service_audit();
    assert!(matches!(
        database
            .repository
            .get_schedulable_silicon_identity("tos", Uuid::now_v7())
            .await,
        Err(RepositoryError::SiliconUnavailable)
    ));
    let destination = NewHookDestination {
        id: Uuid::now_v7(),
        org_id: "tos".to_owned(),
        owner_principal_id: principal_id,
        silicon_id: "assistant:tos".to_owned(),
        endpoint_url_ciphertext: vec![1],
        endpoint_url_nonce: [2; 12],
        signing_secret_ciphertext: vec![3],
        signing_secret_nonce: [4; 12],
        encryption_key_version: 1,
    };
    database
        .repository
        .upsert_hook_destination(&destination, &audit)
        .await?;
    let binding = database
        .repository
        .get_active_silicon_identity("tos", principal_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("active Silicon binding was not persisted"))?;
    assert_eq!(binding.silicon_id, "assistant:tos");
    assert_eq!(
        database
            .repository
            .get_schedulable_silicon_identity("tos", principal_id)
            .await?
            .silicon_id,
        "assistant:tos"
    );

    let now = fixture_now();
    let schedule = CreateSchedule {
        id: Uuid::now_v7(),
        org_id: "tos".to_owned(),
        owner_principal_id: principal_id,
        silicon_id: "assistant:tos".to_owned(),
        text: "Prepare report".to_owned(),
        timezone: "UTC".to_owned(),
        schedule_kind: "one_time".to_owned(),
        cron: "0 9 * * *".to_owned(),
        next_run_at: now + Duration::hours(1),
    };
    let idempotency = IdempotencyContext {
        actor_type: ActorType::Silicon,
        actor_id: principal_id.to_string(),
        key: "binding-create-1".to_owned(),
        request_hash: [7; 32],
        expires_at: now + Duration::hours(24),
    };
    database
        .repository
        .create_schedule_idempotent(&schedule, &idempotency, &audit)
        .await?;
    assert_disabled_destination_allows_new_creation_but_not_replay(
        &database,
        &schedule,
        &idempotency,
        destination,
        &audit,
        now,
    )
    .await?;

    assert_revocation_tombstone_blocks_creation(
        &database,
        schedule,
        idempotency,
        &audit,
        now,
        principal_id,
    )
    .await?;

    assert_maximum_identifier_binding(&database.repository, &audit).await?;
    Ok(())
}

async fn assert_revocation_tombstone_blocks_creation(
    database: &TestDatabase,
    mut schedule: CreateSchedule,
    idempotency: IdempotencyContext,
    audit: &AuditContext,
    now: DateTime<Utc>,
    principal_id: Uuid,
) -> anyhow::Result<()> {
    let removal = lifecycle_event(
        "binding-removal",
        "organization.membership.removed.v1",
        Some(&principal_id.to_string()),
        now,
    )?;
    database
        .repository
        .apply_iam_lifecycle_event(&removal, audit)
        .await?;
    assert!(
        database
            .repository
            .get_active_silicon_identity("tos", principal_id)
            .await?
            .is_none()
    );
    assert!(
        database
            .repository
            .get_hook_destination("tos", "assistant:tos")
            .await?
            .is_none()
    );

    schedule.id = Uuid::now_v7();
    let blocked_idempotency = IdempotencyContext {
        key: "binding-create-2".to_owned(),
        request_hash: [8; 32],
        ..idempotency
    };
    let blocked = database
        .repository
        .create_schedule_idempotent(&schedule, &blocked_idempotency, audit)
        .await;
    assert!(matches!(blocked, Err(RepositoryError::SiliconUnavailable)));
    Ok(())
}

async fn assert_disabled_destination_allows_new_creation_but_not_replay(
    database: &TestDatabase,
    schedule: &CreateSchedule,
    idempotency: &IdempotencyContext,
    mut destination: NewHookDestination,
    audit: &AuditContext,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    database
        .repository
        .disable_hook_destination("tos", "assistant:tos", now, audit)
        .await?;
    let replay = database
        .repository
        .create_schedule_idempotent(schedule, idempotency, audit)
        .await?;
    assert!(matches!(replay, IdempotentMutation::Replayed { .. }));
    assert!(
        database
            .repository
            .get_schedulable_silicon_identity("tos", schedule.owner_principal_id)
            .await
            .is_ok()
    );

    let mut disabled_schedule = schedule.clone();
    disabled_schedule.id = Uuid::now_v7();
    let disabled_idempotency = IdempotencyContext {
        key: "binding-disabled-1".to_owned(),
        request_hash: [9; 32],
        ..idempotency.clone()
    };
    assert!(
        database
            .repository
            .create_schedule_idempotent(&disabled_schedule, &disabled_idempotency, audit)
            .await
            .is_ok()
    );

    destination.id = Uuid::now_v7();
    database
        .repository
        .upsert_hook_destination(&destination, audit)
        .await?;
    Ok(())
}

async fn assert_maximum_identifier_binding(
    repository: &PostgresRepository,
    audit: &AuditContext,
) -> anyhow::Result<()> {
    let org_id = "o".repeat(50);
    let silicon_id = format!("{}:{org_id}", "s".repeat(50));
    let principal_id = Uuid::now_v7();
    repository
        .upsert_hook_destination(
            &NewHookDestination {
                id: Uuid::now_v7(),
                org_id: org_id.clone(),
                owner_principal_id: principal_id,
                silicon_id: silicon_id.clone(),
                endpoint_url_ciphertext: vec![5],
                endpoint_url_nonce: [6; 12],
                signing_secret_ciphertext: vec![7],
                signing_secret_nonce: [8; 12],
                encryption_key_version: 1,
            },
            audit,
        )
        .await?;
    let binding = repository
        .get_active_silicon_identity(&org_id, principal_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("maximum-length Silicon binding was not persisted"))?;
    assert_eq!(binding.silicon_id, silicon_id);
    Ok(())
}

async fn test_database() -> anyhow::Result<TestDatabase> {
    let container = Postgres::default().with_tag("17-alpine").start().await?;
    let host = container.get_host().await?;
    let port = container.get_host_port_ipv4(5432).await?;
    let database_url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    migrate(&pool).await?;
    let repository = PostgresRepository::new(pool.clone());
    Ok(TestDatabase {
        _container: container,
        pool,
        repository,
    })
}

async fn apply_and_assert_bulk_pause(
    database: &TestDatabase,
    fixture: &BulkScheduleStatusFixture,
    request_order: &[Uuid],
) -> anyhow::Result<(BulkScheduleStatusReplacement, IdempotencyContext, Value)> {
    let replacement = BulkScheduleStatusReplacement {
        org_id: "tos".to_owned(),
        owner_principal_id: fixture.owner_principal_id,
        status: MutableScheduleStatus::Paused,
        schedules: request_order
            .iter()
            .map(|id| ScheduleStatusChange {
                id: *id,
                expected_version: if *id == fixture.active_schedule_id {
                    3
                } else {
                    7
                },
                next_run_at: None,
            })
            .collect(),
    };
    let idempotency =
        bulk_status_idempotency(fixture.owner_principal_id, "bulk-status-pause", [31; 32]);
    let (rows, response) = match database
        .repository
        .replace_schedule_statuses_idempotent(&replacement, &idempotency, &service_audit())
        .await?
    {
        IdempotentMutation::Applied {
            value,
            response_body,
        } => (value, response_body),
        IdempotentMutation::Replayed { .. } => {
            anyhow::bail!("the first bulk status request unexpectedly replayed")
        }
    };
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        request_order
    );
    assert_eq!(status_response_ids(&response)?, request_order);

    let changed = rows
        .iter()
        .find(|row| row.id == fixture.active_schedule_id)
        .ok_or_else(|| anyhow::anyhow!("changed schedule missing from response"))?;
    assert_eq!(changed.status, "paused");
    assert_eq!(changed.version, 4);
    assert!(changed.next_run_at.is_none());
    assert!(changed.updated_at > fixture.active_updated_at);

    let unchanged = rows
        .iter()
        .find(|row| row.id == fixture.paused_schedule_id)
        .ok_or_else(|| anyhow::anyhow!("no-op schedule missing from response"))?;
    assert_eq!(unchanged.status, "paused");
    assert_eq!(unchanged.version, 7);
    assert!(unchanged.next_run_at.is_none());
    assert_eq!(unchanged.updated_at, fixture.paused_updated_at);

    assert_materialized_execution_unchanged(database, fixture).await?;
    let audits = bulk_status_audits(&database.pool).await?;
    let expected_resource_id = fixture.active_schedule_id.to_string();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].0.as_deref(), Some(expected_resource_id.as_str()));
    assert_eq!(audits[0].1["previous_status"], "active");
    assert_eq!(audits[0].1["status"], "paused");
    Ok((replacement, idempotency, response))
}

async fn assert_materialized_execution_unchanged(
    database: &TestDatabase,
    fixture: &BulkScheduleStatusFixture,
) -> anyhow::Result<()> {
    let execution = sqlx::query_as::<_, (String, i64, Option<DateTime<Utc>>)>(
        "SELECT status, schedule_version, next_attempt_at FROM executions WHERE id = $1",
    )
    .bind(fixture.execution_id)
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(execution.0, "pending");
    assert_eq!(execution.1, 3);
    assert_eq!(execution.2, Some(fixture.execution_next_attempt_at));
    Ok(())
}

async fn assert_exact_bulk_status_replay(
    database: &TestDatabase,
    replacement: &BulkScheduleStatusReplacement,
    idempotency: &IdempotencyContext,
    expected_response: &Value,
) -> anyhow::Result<()> {
    match database
        .repository
        .replace_schedule_statuses_idempotent(replacement, idempotency, &service_audit())
        .await?
    {
        IdempotentMutation::Replayed {
            status_code,
            response_body,
        } => {
            assert_eq!(status_code, 200);
            assert_eq!(&response_body, expected_response);
        }
        IdempotentMutation::Applied { .. } => {
            anyhow::bail!("an exact bulk status retry did not replay")
        }
    }
    let conflicting = IdempotencyContext {
        request_hash: [99; 32],
        ..idempotency.clone()
    };
    assert!(matches!(
        database
            .repository
            .replace_schedule_statuses_idempotent(replacement, &conflicting, &service_audit(),)
            .await,
        Err(RepositoryError::IdempotencyConflict)
    ));
    assert_eq!(bulk_status_audits(&database.pool).await?.len(), 1);
    Ok(())
}

async fn apply_and_assert_bulk_resume(
    database: &TestDatabase,
    fixture: &BulkScheduleStatusFixture,
    request_order: &[Uuid],
) -> anyhow::Result<()> {
    let shared_next_run_at = fixture_now() + Duration::hours(6);
    let replacement = BulkScheduleStatusReplacement {
        org_id: "tos".to_owned(),
        owner_principal_id: fixture.owner_principal_id,
        status: MutableScheduleStatus::Active,
        schedules: request_order
            .iter()
            .map(|id| ScheduleStatusChange {
                id: *id,
                expected_version: if *id == fixture.active_schedule_id {
                    4
                } else {
                    7
                },
                next_run_at: Some(shared_next_run_at),
            })
            .collect(),
    };
    let rows = match database
        .repository
        .replace_schedule_statuses_idempotent(
            &replacement,
            &bulk_status_idempotency(fixture.owner_principal_id, "bulk-status-resume", [32; 32]),
            &service_audit(),
        )
        .await?
    {
        IdempotentMutation::Applied { value, .. } => value,
        IdempotentMutation::Replayed { .. } => {
            anyhow::bail!("the first resume request unexpectedly replayed")
        }
    };
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        request_order
    );
    assert!(rows.iter().all(|row| row.status == "active"));
    assert!(
        rows.iter()
            .all(|row| row.next_run_at == Some(shared_next_run_at))
    );
    assert_eq!(bulk_status_audits(&database.pool).await?.len(), 3);
    Ok(())
}

async fn seed_bulk_schedule_status_fixture(
    pool: &PgPool,
) -> anyhow::Result<BulkScheduleStatusFixture> {
    let now = fixture_now();
    let created_at = now - Duration::days(2);
    let completed_at = now - Duration::hours(4);
    let archived_at = now - Duration::hours(1);
    let fixture = BulkScheduleStatusFixture {
        owner_principal_id: Uuid::now_v7(),
        active_schedule_id: Uuid::now_v7(),
        paused_schedule_id: Uuid::now_v7(),
        completed_schedule_id: Uuid::now_v7(),
        archived_schedule_id: Uuid::now_v7(),
        execution_id: Uuid::now_v7(),
        execution_next_attempt_at: now + Duration::minutes(10),
        active_updated_at: now - Duration::hours(3),
        paused_updated_at: now - Duration::hours(2),
    };

    sqlx::query("INSERT INTO organization_lifecycle (org_id, state) VALUES ('tos', 'active')")
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO silicon_identities (org_id, principal_id, silicon_id, state) \
         VALUES ('tos', $1, 'bulk-status:tos', 'active')",
    )
    .bind(fixture.owner_principal_id)
    .execute(pool)
    .await?;
    seed_bulk_status_schedule_rows(pool, &fixture, now, created_at, completed_at, archived_at)
        .await?;
    sqlx::query(
        "INSERT INTO executions (\
             id, schedule_id, org_id, silicon_id, schedule_version, schedule_kind, \
             scheduled_for, reminder_text, timezone, status, next_attempt_at\
         ) VALUES (\
             $1, $2, 'tos', 'bulk-status:tos', 3, 'recurring', $3, \
             'already materialized', 'UTC', 'pending', $4\
         )",
    )
    .bind(fixture.execution_id)
    .bind(fixture.active_schedule_id)
    .bind(now)
    .bind(fixture.execution_next_attempt_at)
    .execute(pool)
    .await?;
    Ok(fixture)
}

async fn seed_bulk_status_schedule_rows(
    pool: &PgPool,
    fixture: &BulkScheduleStatusFixture,
    now: DateTime<Utc>,
    created_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
    archived_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, version, \
             created_at, updated_at\
         ) VALUES (\
             $1, 'tos', $2, 'bulk-status:tos', 'active bulk reminder', 'UTC', \
             'recurring', '*/5 * * * *', 'active', $3, 3, $4, $5\
         )",
    )
    .bind(fixture.active_schedule_id)
    .bind(fixture.owner_principal_id)
    .bind(now + Duration::hours(1))
    .bind(created_at)
    .bind(fixture.active_updated_at)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, version, \
             created_at, updated_at\
         ) VALUES (\
             $1, 'tos', $2, 'bulk-status:tos', 'paused bulk reminder', 'UTC', \
             'recurring', '*/10 * * * *', 'paused', NULL, 7, $3, $4\
         )",
    )
    .bind(fixture.paused_schedule_id)
    .bind(fixture.owner_principal_id)
    .bind(created_at)
    .bind(fixture.paused_updated_at)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, version, \
             completed_at, created_at, updated_at\
         ) VALUES (\
             $1, 'tos', $2, 'bulk-status:tos', 'completed bulk reminder', 'UTC', \
             'one_time', '0 9 * * *', 'completed', NULL, 2, $3, $4, $3\
         )",
    )
    .bind(fixture.completed_schedule_id)
    .bind(fixture.owner_principal_id)
    .bind(completed_at)
    .bind(created_at)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, version, \
             deleted_at, created_at, updated_at\
         ) VALUES (\
             $1, 'tos', $2, 'bulk-status:tos', 'archived bulk reminder', 'UTC', \
             'recurring', '0 12 * * *', 'active', NULL, 5, $3, $4, $3\
         )",
    )
    .bind(fixture.archived_schedule_id)
    .bind(fixture.owner_principal_id)
    .bind(archived_at)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_foreign_bulk_status_schedule(pool: &PgPool) -> anyhow::Result<Uuid> {
    let owner_principal_id = Uuid::now_v7();
    let schedule_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO silicon_identities (org_id, principal_id, silicon_id, state) \
         VALUES ('tos', $1, 'bulk-foreign:tos', 'active')",
    )
    .bind(owner_principal_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at\
         ) VALUES (\
             $1, 'tos', $2, 'bulk-foreign:tos', 'foreign bulk reminder', 'UTC', \
             'recurring', '0 8 * * *', 'active', $3\
         )",
    )
    .bind(schedule_id)
    .bind(owner_principal_id)
    .bind(fixture_now() + Duration::hours(1))
    .execute(pool)
    .await?;
    Ok(schedule_id)
}

async fn seed_expired_bulk_status_schedule(
    pool: &PgPool,
    owner_principal_id: Uuid,
) -> anyhow::Result<Uuid> {
    let schedule_id = Uuid::now_v7();
    let now = fixture_now();
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, deleted_at, created_at\
         ) VALUES (\
             $1, 'tos', $2, 'bulk-status:tos', 'expired bulk reminder', 'UTC', \
             'recurring', '0 7 * * *', 'paused', NULL, $3, $4\
         )",
    )
    .bind(schedule_id)
    .bind(owner_principal_id)
    .bind(now - Duration::days(46))
    .bind(now - Duration::days(60))
    .execute(pool)
    .await?;
    Ok(schedule_id)
}

fn bulk_status_idempotency(
    owner_principal_id: Uuid,
    key: &str,
    request_hash: [u8; 32],
) -> IdempotencyContext {
    IdempotencyContext {
        actor_type: ActorType::Silicon,
        actor_id: owner_principal_id.to_string(),
        key: key.to_owned(),
        request_hash,
        expires_at: fixture_now() + Duration::hours(24),
    }
}

fn status_response_ids(response: &Value) -> anyhow::Result<Vec<Uuid>> {
    response
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("bulk status response did not contain an items array"))?
        .iter()
        .map(|item| {
            let object = item
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("bulk status response item was not an object"))?;
            let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
            keys.sort_unstable();
            anyhow::ensure!(
                keys == ["id", "next_run_at", "status", "updated_at"],
                "bulk status response item had an unexpected shape: {keys:?}"
            );
            let id = object.get("id").and_then(Value::as_str).ok_or_else(|| {
                anyhow::anyhow!("bulk status response item did not contain an id")
            })?;
            Uuid::parse_str(id).map_err(Into::into)
        })
        .collect()
}

async fn bulk_status_audits(pool: &PgPool) -> anyhow::Result<Vec<(Option<String>, Value)>> {
    sqlx::query_as(
        "SELECT resource_id, metadata FROM audit_records \
         WHERE action = 'schedule.status_changed' ORDER BY occurred_at, id",
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

type BulkScheduleState = (String, Option<DateTime<Utc>>, i64, DateTime<Utc>);

async fn bulk_schedule_state(pool: &PgPool, id: Uuid) -> anyhow::Result<BulkScheduleState> {
    sqlx::query_as("SELECT status, next_run_at, version, updated_at FROM schedules WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
}

async fn assert_bulk_status_batch_unchanged(
    pool: &PgPool,
    fixture: &BulkScheduleStatusFixture,
    initial_active: &BulkScheduleState,
    initial_paused: &BulkScheduleState,
) -> anyhow::Result<()> {
    assert_eq!(
        &bulk_schedule_state(pool, fixture.active_schedule_id).await?,
        initial_active
    );
    assert_eq!(
        &bulk_schedule_state(pool, fixture.paused_schedule_id).await?,
        initial_paused
    );
    Ok(())
}

async fn seed_visibility_schedule(
    pool: &PgPool,
    owner_principal_id: Uuid,
    silicon_id: &str,
    text: &str,
    created_at: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let schedule_id = Uuid::now_v7();
    let next_run_at = created_at + Duration::hours(2);
    sqlx::query(
        "INSERT INTO organization_lifecycle (org_id, state) \
         VALUES ('tos', 'active') ON CONFLICT (org_id) DO NOTHING",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO silicon_identities (org_id, principal_id, silicon_id, state) \
         VALUES ('tos', $1, $2, 'active')",
    )
    .bind(owner_principal_id)
    .bind(silicon_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, \
             timezone, schedule_kind, cron_expression, status, next_run_at, \
             created_at, updated_at\
         ) VALUES (\
             $1, 'tos', $2, $3, $4, 'UTC', 'one_time', '0 9 * * *', \
             'active', $5, $6, $6\
         )",
    )
    .bind(schedule_id)
    .bind(owner_principal_id)
    .bind(silicon_id)
    .bind(text)
    .bind(next_run_at)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(schedule_id)
}

async fn seed_visibility_execution(
    pool: &PgPool,
    schedule_id: Uuid,
    silicon_id: &str,
    scheduled_for: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let execution_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO executions (\
             id, schedule_id, org_id, silicon_id, schedule_version, \
             schedule_kind, scheduled_for, reminder_text, timezone, status, \
             next_attempt_at\
         ) VALUES (\
             $1, $2, 'tos', $3, 1, 'one_time', $4, 'visibility test', \
             'UTC', 'pending', $4\
         )",
    )
    .bind(execution_id)
    .bind(schedule_id)
    .bind(silicon_id)
    .bind(scheduled_for)
    .execute(pool)
    .await?;
    Ok(execution_id)
}

async fn seed_due_schedule(
    pool: &PgPool,
    silicon_id: &str,
    now: DateTime<Utc>,
    schedule_kind: &str,
) -> anyhow::Result<Uuid> {
    let schedule_id = Uuid::now_v7();
    let owner_principal_id = Uuid::now_v7();
    let due_at = now - Duration::minutes(1);
    anyhow::ensure!(
        matches!(schedule_kind, "one_time" | "recurring"),
        "unsupported test schedule kind: {schedule_kind}"
    );
    sqlx::query(
        "INSERT INTO organization_lifecycle (org_id, state) \
         VALUES ('tos', 'active') ON CONFLICT (org_id) DO NOTHING",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO silicon_identities (org_id, principal_id, silicon_id, state) \
         VALUES ('tos', $1, $2, 'active')",
    )
    .bind(owner_principal_id)
    .bind(silicon_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, \
             timezone, schedule_kind, cron_expression, status, next_run_at\
         ) VALUES ($1, 'tos', $2, $3, 'generation test', 'UTC', $4, \
                   '* * * * *', 'active', $5)",
    )
    .bind(schedule_id)
    .bind(owner_principal_id)
    .bind(silicon_id)
    .bind(schedule_kind)
    .bind(due_at)
    .execute(pool)
    .await?;
    Ok(schedule_id)
}

async fn materialize_schedule(
    repository: &PostgresRepository,
    schedule_id: Uuid,
    now: DateTime<Utc>,
    next_run_at: Option<DateTime<Utc>>,
) -> anyhow::Result<ExecutionRow> {
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

async fn claim_execution(
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

async fn schedule_archive_state(
    pool: &PgPool,
    schedule_id: Uuid,
) -> anyhow::Result<(String, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> {
    sqlx::query_as(
        "SELECT status, version, completed_at, purge_after \
         FROM schedules WHERE id = $1",
    )
    .bind(schedule_id)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

async fn schedule_owner(pool: &PgPool, schedule_id: Uuid) -> anyhow::Result<Uuid> {
    sqlx::query_scalar("SELECT owner_principal_id FROM schedules WHERE id = $1")
        .bind(schedule_id)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
}

async fn assert_schedule_state(
    pool: &PgPool,
    schedule_id: Uuid,
    expected_status: &str,
    expected_version: i64,
    expects_completion: bool,
) -> anyhow::Result<()> {
    let (status, version, completed_at) =
        sqlx::query_as::<_, (String, i64, Option<DateTime<Utc>>)>(
            "SELECT status, version, completed_at FROM schedules WHERE id = $1",
        )
        .bind(schedule_id)
        .fetch_one(pool)
        .await?;
    assert_eq!(status, expected_status);
    assert_eq!(version, expected_version);
    assert_eq!(completed_at.is_some(), expects_completion);
    Ok(())
}

async fn seed_lifecycle_fixture(pool: &PgPool) -> anyhow::Result<LifecycleFixture> {
    let now = fixture_now();
    let target_schedule_id = Uuid::now_v7();
    let completed_schedule_id = Uuid::now_v7();
    let other_schedule_id = Uuid::now_v7();
    let target_principal_id = Uuid::now_v7();
    let other_principal_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, \
             timezone, schedule_kind, cron_expression, status, next_run_at\
         ) VALUES \
             ($1, 'tos', $4, 'removed:tos', 'target', 'UTC', 'recurring', '* * * * *', \
              'active', $3), \
             ($2, 'tos', $5, 'remaining:tos', 'other', 'UTC', 'recurring', '* * * * *', \
              'active', $3)",
    )
    .bind(target_schedule_id)
    .bind(other_schedule_id)
    .bind(now - Duration::minutes(1))
    .bind(target_principal_id)
    .bind(other_principal_id)
    .execute(pool)
    .await?;

    let completed_at = now - Duration::hours(1);
    sqlx::query(
        "INSERT INTO schedules (\
             id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, completed_at, \
             created_at, updated_at\
         ) VALUES (\
             $1, 'tos', $2, 'removed:tos', 'completed target', 'UTC', \
             'one_time', '* * * * *', 'completed', NULL, $3, $4, $3\
         )",
    )
    .bind(completed_schedule_id)
    .bind(target_principal_id)
    .bind(completed_at)
    .bind(completed_at - Duration::hours(1))
    .execute(pool)
    .await?;

    let execution_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO executions (\
             id, schedule_id, org_id, silicon_id, schedule_version, \
             schedule_kind, scheduled_for, reminder_text, timezone, status, \
             attempt_count, next_attempt_at, attempted_at, lease_owner, \
             lease_expires_at\
         ) VALUES (\
             $1, $2, 'tos', 'removed:tos', 1, 'recurring', $3, 'target', \
             'UTC', 'retrying', 1, $3, $3, 'delivery-worker', $4\
         )",
    )
    .bind(execution_id)
    .bind(target_schedule_id)
    .bind(now)
    .bind(now + Duration::minutes(5))
    .execute(pool)
    .await?;
    let completed_execution_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO executions (\
             id, schedule_id, org_id, silicon_id, schedule_version, schedule_kind, \
             scheduled_for, reminder_text, timezone, status, next_attempt_at\
         ) VALUES (\
             $1, $2, 'tos', 'removed:tos', 2, 'one_time', $3, \
             'completed target', 'UTC', 'pending', $3\
         )",
    )
    .bind(completed_execution_id)
    .bind(completed_schedule_id)
    .bind(completed_at)
    .execute(pool)
    .await?;
    Ok(LifecycleFixture {
        now,
        target_principal_id,
        target_schedule_id,
        completed_schedule_id,
        other_schedule_id,
        execution_id,
        completed_execution_id,
        completed_at,
    })
}

fn lifecycle_event(
    event_id: &str,
    event_type: &str,
    subject_id: Option<&str>,
    received_at: DateTime<Utc>,
) -> anyhow::Result<NewInternalEvent> {
    let payload = json!({
        "event_id": event_id,
        "event_type": event_type,
        "schema_version": "1.0",
        "org_id": "tos",
        "silicon_id": subject_id,
    });
    let payload_hash = Sha256::digest(serde_json::to_vec(&payload)?).into();
    Ok(NewInternalEvent {
        id: Uuid::now_v7(),
        source: "silicon-iam".to_owned(),
        event_id: event_id.to_owned(),
        event_type: event_type.to_owned(),
        org_id: Some("tos".to_owned()),
        subject_id: subject_id.map(str::to_owned),
        payload,
        payload_hash,
        received_at,
    })
}

fn service_audit() -> AuditContext {
    AuditContext {
        actor_type: ActorType::Service,
        actor_id: "internal-api".to_owned(),
        request_id: Some("request-1".to_owned()),
    }
}

async fn assert_silicon_removal_state(
    pool: &PgPool,
    fixture: &LifecycleFixture,
) -> anyhow::Result<()> {
    let target_state = sqlx::query_as::<_, (Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
        "SELECT deleted_at, next_run_at FROM schedules WHERE id = $1",
    )
    .bind(fixture.target_schedule_id)
    .fetch_one(pool)
    .await?;
    assert!(target_state.0.is_some());
    assert!(target_state.1.is_none());

    let other_deleted = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT deleted_at FROM schedules WHERE id = $1",
    )
    .bind(fixture.other_schedule_id)
    .fetch_one(pool)
    .await?;
    assert!(other_deleted.is_none());

    let execution_state = sqlx::query_as::<
        _,
        (
            String,
            Option<DateTime<Utc>>,
            Option<String>,
            Option<DateTime<Utc>>,
        ),
    >(
        "SELECT status, next_attempt_at, lease_owner, lease_expires_at \
         FROM executions WHERE id = $1",
    )
    .bind(fixture.execution_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(execution_state.0, "failed");
    assert!(execution_state.1.is_none());
    assert!(execution_state.2.is_none());
    assert!(execution_state.3.is_none());

    let completed_state =
        sqlx::query_as::<_, (Option<DateTime<Utc>>, DateTime<Utc>, DateTime<Utc>, String)>(
            "SELECT schedule.deleted_at, schedule.completed_at, schedule.purge_after, \
                execution.status \
         FROM schedules schedule \
         JOIN executions execution ON execution.schedule_id = schedule.id \
         WHERE schedule.id = $1 AND execution.id = $2",
        )
        .bind(fixture.completed_schedule_id)
        .bind(fixture.completed_execution_id)
        .fetch_one(pool)
        .await?;
    assert!(completed_state.0.is_none());
    assert_eq!(completed_state.1, fixture.completed_at);
    assert_eq!(
        completed_state.2,
        fixture.completed_at + Duration::days(crate::domain::ARCHIVE_RETENTION_DAYS)
    );
    assert_eq!(completed_state.3, "failed");
    Ok(())
}

async fn assert_exact_replay(
    repository: &PostgresRepository,
    event: &NewInternalEvent,
    audit: &AuditContext,
    original: &IamLifecycleOutcome,
) -> anyhow::Result<()> {
    let mut replay = event.clone();
    replay.id = Uuid::now_v7();
    replay.received_at += Duration::seconds(1);
    let outcome = repository.apply_iam_lifecycle_event(&replay, audit).await?;
    assert!(outcome.replayed);
    assert_eq!(outcome.receipt.id, original.receipt.id);
    assert_eq!(outcome.schedules_deleted, 0);
    assert_eq!(outcome.executions_failed, 0);
    Ok(())
}

async fn assert_hash_conflict(
    repository: &PostgresRepository,
    event: &NewInternalEvent,
    audit: &AuditContext,
) {
    let mut conflicting = event.clone();
    conflicting.id = Uuid::now_v7();
    conflicting.payload_hash = [0_u8; 32];
    let result = repository
        .apply_iam_lifecycle_event(&conflicting, audit)
        .await;
    assert!(matches!(result, Err(RepositoryError::EventReceiptConflict)));
}

async fn assert_organization_removal_state(pool: &PgPool, schedule_id: Uuid) -> anyhow::Result<()> {
    let deleted_at = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT deleted_at FROM schedules WHERE id = $1",
    )
    .bind(schedule_id)
    .fetch_one(pool)
    .await?;
    assert!(deleted_at.is_some());

    let audit_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM audit_records WHERE action LIKE 'iam.%' \
         OR action = 'internal_event.processed'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(audit_count, 4);
    Ok(())
}

async fn insert_idempotency_response(
    pool: &PgPool,
    key: &str,
    request_hash: &[u8; 32],
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO idempotency_records (\
             id, org_id, actor_type, actor_id, operation, target_id, \
             idempotency_key, request_hash, state, response_status, \
             response_body, created_at, expires_at\
         ) VALUES (\
             $1, 'tos', 'silicon', 'remaining:tos', 'schedule.create', NULL, \
             $2, $3, 'completed', 201, '{\"id\":\"stored\"}'::jsonb, $4, $5\
         )",
    )
    .bind(Uuid::now_v7())
    .bind(key)
    .bind(request_hash.as_slice())
    .bind(created_at)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}
