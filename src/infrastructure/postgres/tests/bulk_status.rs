//! Atomic desired-status changes over several of a Silicon's reminders.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    FixedClock, TestDatabase, actor, fixture_now, seed_account, seed_due_schedule, service_audit,
    test_database,
};
use crate::{
    application::schedules::ScheduleService,
    domain::{ActorKind, ScheduleStatus},
    error::AppError,
    infrastructure::postgres::{
        ActorType, BulkScheduleStatusReplacement, IdempotencyContext, IdempotentMutation,
        MutableScheduleStatus, RepositoryError, ScheduleStatusChange,
    },
};

struct Fixture {
    owner_key: Uuid,
    active: Uuid,
    paused: Uuid,
    completed: Uuid,
    archived: Uuid,
    execution: Uuid,
    execution_next_attempt_at: DateTime<Utc>,
    active_updated_at: DateTime<Utc>,
    paused_updated_at: DateTime<Utc>,
}

async fn seed(pool: &PgPool) -> anyhow::Result<Fixture> {
    let now = fixture_now();
    let created_at = now - Duration::days(2);
    let owner_key = seed_account(
        pool,
        "Bulk",
        ActorKind::Silicon,
        "si:bulk-status",
        Some(("Ada", "c:ada")),
    )
    .await?;
    let fixture = Fixture {
        owner_key,
        active: Uuid::now_v7(),
        paused: Uuid::now_v7(),
        completed: Uuid::now_v7(),
        archived: Uuid::now_v7(),
        execution: Uuid::now_v7(),
        execution_next_attempt_at: now + Duration::minutes(10),
        active_updated_at: now - Duration::hours(3),
        paused_updated_at: now - Duration::hours(2),
    };
    for (id, status, version, next_run_at, completed_at, deleted_at, updated_at, kind) in [
        (
            fixture.active,
            "active",
            3,
            Some(now + Duration::hours(1)),
            None,
            None,
            fixture.active_updated_at,
            "recurring",
        ),
        (
            fixture.paused,
            "paused",
            7,
            None,
            None,
            None,
            fixture.paused_updated_at,
            "recurring",
        ),
        (
            fixture.completed,
            "completed",
            2,
            None,
            Some(now - Duration::hours(4)),
            None,
            now - Duration::hours(4),
            "one_time",
        ),
        (
            fixture.archived,
            "active",
            5,
            None,
            None,
            Some(now - Duration::hours(1)),
            now - Duration::hours(1),
            "recurring",
        ),
    ] {
        sqlx::query(
            "INSERT INTO schedules (id, org_id, owner_principal_id, silicon_id, reminder_text, \
                 timezone, schedule_kind, cron_expression, status, next_run_at, version, \
                 completed_at, deleted_at, created_at, updated_at) \
             VALUES ($1, NULL, $2, 'si:bulk-status', 'bulk reminder', 'UTC', $3, '*/5 * * * *', \
                 $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(owner_key)
        .bind(kind)
        .bind(status)
        .bind(next_run_at)
        .bind(version)
        .bind(completed_at)
        .bind(deleted_at)
        .bind(created_at)
        .bind(updated_at)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        "INSERT INTO executions (id, schedule_id, org_id, silicon_id, schedule_version, \
             schedule_kind, scheduled_for, reminder_text, timezone, status, next_attempt_at) \
         VALUES ($1, $2, NULL, 'si:bulk-status', 3, 'recurring', $3, 'already materialized', \
             'UTC', 'pending', $4)",
    )
    .bind(fixture.execution)
    .bind(fixture.active)
    .bind(now)
    .bind(fixture.execution_next_attempt_at)
    .execute(pool)
    .await?;
    Ok(fixture)
}

fn idempotency(key: &str, request_hash: [u8; 32]) -> IdempotencyContext {
    IdempotencyContext {
        actor_type: ActorType::Silicon,
        actor_id: "Bulk".to_owned(),
        key: key.to_owned(),
        request_hash,
        expires_at: fixture_now() + Duration::hours(24),
    }
}

fn replacement(
    fixture: &Fixture,
    status: MutableScheduleStatus,
    changes: Vec<(Uuid, i64, Option<DateTime<Utc>>)>,
) -> BulkScheduleStatusReplacement {
    BulkScheduleStatusReplacement {
        owner_keys: vec![fixture.owner_key],
        status,
        schedules: changes
            .into_iter()
            .map(|(id, expected_version, next_run_at)| ScheduleStatusChange {
                id,
                expected_version,
                next_run_at,
            })
            .collect(),
    }
}

type State = (String, Option<DateTime<Utc>>, i64, DateTime<Utc>);

async fn state(pool: &PgPool, id: Uuid) -> anyhow::Result<State> {
    Ok(sqlx::query_as(
        "SELECT status, next_run_at, version, updated_at FROM schedules WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?)
}

async fn status_audits(pool: &PgPool) -> anyhow::Result<Vec<(Option<String>, Value)>> {
    Ok(sqlx::query_as(
        "SELECT resource_id, metadata FROM audit_records \
         WHERE action = 'schedule.status_changed' ORDER BY occurred_at, id",
    )
    .fetch_all(pool)
    .await?)
}

async fn apply(
    database: &TestDatabase,
    replacement: &BulkScheduleStatusReplacement,
    context: &IdempotencyContext,
) -> Result<IdempotentMutation<Vec<crate::infrastructure::postgres::ScheduleRow>>, RepositoryError>
{
    database
        .repository
        .replace_schedule_statuses_idempotent(replacement, context, &service_audit())
        .await
}

#[tokio::test]
async fn bulk_status_preserves_order_noops_executions_and_exact_replay() -> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed(&database.pool).await?;
    let mut order = vec![fixture.active, fixture.paused];
    order.sort_unstable();
    order.reverse();
    let versions = |id: &Uuid| if *id == fixture.active { 3 } else { 7 };

    let pause = replacement(
        &fixture,
        MutableScheduleStatus::Paused,
        order.iter().map(|id| (*id, versions(id), None)).collect(),
    );
    let context = idempotency("bulk-status-pause", [31; 32]);
    let IdempotentMutation::Applied {
        value: rows,
        response_body,
    } = apply(&database, &pause, &context).await?
    else {
        anyhow::bail!("the first bulk status request replayed");
    };
    assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), order);
    let changed = rows
        .iter()
        .find(|row| row.id == fixture.active)
        .ok_or_else(|| anyhow::anyhow!("missing"))?;
    assert_eq!((changed.status.as_str(), changed.version), ("paused", 4));
    assert!(changed.next_run_at.is_none() && changed.updated_at > fixture.active_updated_at);
    let untouched = rows
        .iter()
        .find(|row| row.id == fixture.paused)
        .ok_or_else(|| anyhow::anyhow!("missing"))?;
    assert_eq!(
        (untouched.version, untouched.updated_at),
        (7, fixture.paused_updated_at)
    );
    let execution: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT status, schedule_version, next_attempt_at FROM executions WHERE id = $1",
    )
    .bind(fixture.execution)
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(
        execution,
        (
            "pending".to_owned(),
            3,
            Some(fixture.execution_next_attempt_at)
        )
    );
    assert_eq!(status_audits(&database.pool).await?.len(), 1);

    match apply(&database, &pause, &context).await? {
        IdempotentMutation::Replayed {
            status_code,
            response_body: replayed,
        } => {
            assert_eq!(status_code, 200);
            assert_eq!(replayed, response_body);
        }
        IdempotentMutation::Applied { .. } => anyhow::bail!("an exact retry did not replay"),
    }
    let conflicting = IdempotencyContext {
        request_hash: [99; 32],
        ..context
    };
    assert!(matches!(
        apply(&database, &pause, &conflicting).await,
        Err(RepositoryError::IdempotencyConflict)
    ));

    let resume_at = fixture_now() + Duration::hours(6);
    let resume = replacement(
        &fixture,
        MutableScheduleStatus::Active,
        order
            .iter()
            .map(|id| {
                (
                    *id,
                    if *id == fixture.active { 4 } else { 7 },
                    Some(resume_at),
                )
            })
            .collect(),
    );
    let IdempotentMutation::Applied { value: rows, .. } = apply(
        &database,
        &resume,
        &idempotency("bulk-status-resume", [32; 32]),
    )
    .await?
    else {
        anyhow::bail!("the resume replayed");
    };
    assert!(
        rows.iter()
            .all(|row| row.status == "active" && row.next_run_at == Some(resume_at))
    );
    assert_eq!(status_audits(&database.pool).await?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn bulk_status_rolls_back_stale_archived_and_foreign_batches() -> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed(&database.pool).await?;
    let before = (
        state(&database.pool, fixture.active).await?,
        state(&database.pool, fixture.paused).await?,
    );

    let stale = replacement(
        &fixture,
        MutableScheduleStatus::Paused,
        vec![(fixture.active, 3, None), (fixture.paused, 6, None)],
    );
    let outcome = apply(&database, &stale, &idempotency("bulk-stale", [33; 32])).await;
    assert!(
        matches!(outcome, Err(RepositoryError::VersionConflict)),
        "{outcome:?}"
    );
    for (target, version, key) in [
        (fixture.archived, 5, "bulk-archived"),
        (fixture.completed, 2, "bulk-completed"),
    ] {
        let invalid = replacement(
            &fixture,
            MutableScheduleStatus::Paused,
            vec![(fixture.active, 3, None), (target, version, None)],
        );
        assert!(matches!(
            apply(&database, &invalid, &idempotency(key, [34; 32])).await,
            Err(RepositoryError::InvalidState)
        ));
    }
    // Keys that are not the owner's never reach its schedules.
    let foreign = BulkScheduleStatusReplacement {
        owner_keys: vec![Uuid::now_v7()],
        ..replacement(
            &fixture,
            MutableScheduleStatus::Paused,
            vec![(fixture.active, 3, None)],
        )
    };
    assert!(matches!(
        apply(&database, &foreign, &idempotency("bulk-foreign", [35; 32])).await,
        Err(RepositoryError::NotFound)
    ));

    assert_eq!(
        (
            state(&database.pool, fixture.active).await?,
            state(&database.pool, fixture.paused).await?
        ),
        before
    );
    assert!(status_audits(&database.pool).await?.is_empty());
    let reservations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_records WHERE operation = 'schedule.bulk_status'",
    )
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(reservations, 0);
    Ok(())
}

#[tokio::test]
async fn bulk_status_errors_follow_visibility_then_ownership_then_state() -> anyhow::Result<()> {
    let database = test_database().await?;
    let fixture = seed(&database.pool).await?;
    let now = fixture_now();
    // A sibling (same custodian) is visible but not owned; an outsider is invisible.
    let sibling_key = seed_account(
        &database.pool,
        "Sib",
        ActorKind::Silicon,
        "si:sibling",
        Some(("Ada", "c:ada")),
    )
    .await?;
    let sibling_schedule = super::seed_schedule(
        &database.pool,
        sibling_key,
        "si:sibling",
        "sibling",
        "recurring",
        "active",
        Some(now + Duration::hours(1)),
        now,
    )
    .await?;
    let (_, outsider_schedule) =
        seed_due_schedule(&database.pool, "Out", "si:outsider", now, "recurring").await?;
    let service = ScheduleService::new(
        database.repository.clone(),
        database.identity.clone(),
        Arc::new(FixedClock(now)),
        std::time::Duration::from_hours(24),
    );
    let owner = actor(
        &database.identity,
        "Bulk",
        ActorKind::Silicon,
        "si:bulk-status",
    )
    .await?;
    let run = |ids: Vec<Uuid>, key: &'static str| {
        let service = service.clone();
        let owner = owner.clone();
        async move {
            service
                .update_statuses(
                    &owner,
                    ids,
                    ScheduleStatus::Paused,
                    key.to_owned(),
                    [41; 32],
                )
                .await
        }
    };
    let missing = run(
        vec![outsider_schedule, sibling_schedule, fixture.archived],
        "precedence-missing",
    )
    .await;
    assert!(matches!(missing, Err(AppError::NotFound)), "{missing:?}");
    assert!(matches!(
        run(vec![sibling_schedule, fixture.archived], "precedence-forbidden").await,
        Err(error) if error.code() == "not_reminder_owner"
    ));
    assert!(matches!(
        run(vec![fixture.archived], "precedence-archived").await,
        Err(AppError::Conflict { code }) if code == "invalid_schedule_state"
    ));
    // The custodian reads but never changes its Silicons' reminders.
    let custodian = actor(&database.identity, "Ada", ActorKind::Carbon, "c:ada").await?;
    assert!(matches!(
        service.update_statuses(&custodian, vec![fixture.active], ScheduleStatus::Paused, "precedence-custodian".to_owned(), [42; 32]).await,
        Err(error) if error.code() == "silicon_only"
    ));
    Ok(())
}

#[tokio::test]
async fn bulk_pause_serializes_after_scheduler_materialization() -> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    let (owner_key, schedule_id) =
        seed_due_schedule(&database.pool, "Race", "si:bulk-race", now, "recurring").await?;
    let mut scheduler = database.repository.begin().await?;
    let schedule = database
        .repository
        .lock_due_schedules(&mut scheduler, now, 100)
        .await?
        .into_iter()
        .find(|row| row.id == schedule_id)
        .ok_or_else(|| anyhow::anyhow!("due schedule was not locked"))?;
    let next_run_at = now + Duration::minutes(1);
    let repository = database.repository.clone();
    let pause = BulkScheduleStatusReplacement {
        owner_keys: vec![owner_key],
        status: MutableScheduleStatus::Paused,
        schedules: vec![ScheduleStatusChange {
            id: schedule_id,
            expected_version: schedule.version,
            next_run_at: None,
        }],
    };
    let context = IdempotencyContext {
        actor_id: "Race".to_owned(),
        ..idempotency("race-pause", [44; 32])
    };
    let mut task = tokio::spawn(async move {
        repository
            .replace_schedule_statuses_idempotent(&pause, &context, &service_audit())
            .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut task)
            .await
            .is_err()
    );
    let materialized = database
        .repository
        .materialize_locked_occurrence(
            &mut scheduler,
            &schedule,
            Uuid::now_v7(),
            now,
            Some(next_run_at),
            &service_audit(),
        )
        .await?;
    scheduler.commit().await?;
    assert!(matches!(task.await?, Err(RepositoryError::VersionConflict)));
    let (status, next, version, _) = state(&database.pool, schedule_id).await?;
    assert_eq!(
        (status.as_str(), next, version),
        ("active", Some(next_run_at), 2)
    );
    assert!(materialized.inserted);
    Ok(())
}
