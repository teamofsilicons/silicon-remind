//! Archive, 45-day retention, the deleted-reminder ledger and materialization.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    claim_execution, fixture_now, materialize_schedule, seed_account, seed_due_schedule,
    service_audit, test_database,
};
use crate::{
    domain::{ARCHIVE_RETENTION_DAYS, ActorKind, ScheduleSection},
    infrastructure::postgres::{
        ListSchedules, RepositoryError,
        repository::{DELETED_REMINDER_LEDGER_LIMIT, trim_deleted_reminder_ledger},
    },
};

struct Purge {
    now: DateTime<Utc>,
    archived_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    schedule_id: Uuid,
    owner_key: Uuid,
    last_trigger: DateTime<Utc>,
}

const TEXT: &str = "Take medication\nwith water";

/// An archived schedule whose retention ended exactly at `now`, owned by si:assistant (uuid Asst).
async fn seed_expired(pool: &PgPool, owner_key: Uuid) -> anyhow::Result<Purge> {
    let now = "2030-01-01T00:00:00Z".parse::<DateTime<Utc>>()?;
    let archived_at = now - Duration::days(45);
    let created_at = archived_at - Duration::days(30);
    let schedule_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO schedules (id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, deleted_at, created_at, updated_at) \
         VALUES ($1, NULL, $2, 'si:assistant', $3, 'Asia/Kolkata', 'recurring', '0 9 * * 1', \
             'paused', NULL, $4, $5, $4)",
    )
    .bind(schedule_id)
    .bind(owner_key)
    .bind(TEXT)
    .bind(archived_at)
    .bind(created_at)
    .execute(pool)
    .await?;
    let last_trigger = archived_at - Duration::days(1);
    for scheduled_for in [archived_at - Duration::days(2), last_trigger] {
        sqlx::query(
            "INSERT INTO executions (id, schedule_id, org_id, silicon_id, schedule_version, \
                 schedule_kind, scheduled_for, reminder_text, timezone, status, attempt_count, \
                 attempted_at, failure_reason, created_at, updated_at) \
             VALUES ($1, $2, NULL, 'si:assistant', 1, 'recurring', $3, $4, 'Asia/Kolkata', \
                 'failed', 1, $3, 'retention fixture', $3, $3)",
        )
        .bind(Uuid::now_v7())
        .bind(schedule_id)
        .bind(scheduled_for)
        .bind(TEXT)
        .execute(pool)
        .await?;
    }
    Ok(Purge {
        now,
        archived_at,
        created_at,
        schedule_id,
        owner_key,
        last_trigger,
    })
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One ordered scenario covers the snapshot, the trim and the conflict rollback"
)]
async fn purge_logs_who_set_it_and_trims_the_oldest() -> anyhow::Result<()> {
    let database = test_database().await?;
    let owner_key = seed_account(
        &database.pool,
        "Asst",
        ActorKind::Silicon,
        "si:assistant",
        None,
    )
    .await?;
    let fixture = seed_expired(&database.pool, owner_key).await?;
    // The Silicon changed its id after setting the reminder: the ledger keeps both.
    sqlx::query("UPDATE accounts SET public_id = 'si:assistant-two' WHERE uuid = 'Asst'")
        .execute(&database.pool)
        .await?;
    let result = database
        .repository
        .purge_expired_schedules(fixture.now, 100, "retention-test-worker")
        .await?;
    assert_eq!((result.purged, result.logged, result.trimmed), (1, 1, 0));

    let (ledger_id, org_id, owner, silicon_id, purge_after, record): (
        i64,
        Option<String>,
        Uuid,
        String,
        DateTime<Utc>,
        String,
    ) = sqlx::query_as(
        "SELECT id, org_id, owner_principal_id, silicon_id, purge_after, record_text \
             FROM deleted_reminders WHERE schedule_id = $1",
    )
    .bind(fixture.schedule_id)
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(
        (org_id, owner, silicon_id.as_str(), purge_after),
        (None, fixture.owner_key, "si:assistant", fixture.now)
    );
    assert!(!record.contains(['\n', '\r']));
    assert_eq!(
        serde_json::from_str::<Value>(&record)?,
        json!({
            "schema_version": "1.1",
            "schedule_id": fixture.schedule_id,
            "org_id": null,
            "owner_principal_id": fixture.owner_key,
            "owner_uuid": "Asst",
            "owner_id": "si:assistant-two",
            "silicon_id": "si:assistant",
            "reminder_text": TEXT,
            "schedule_kind": "recurring",
            "cron_expression": "0 9 * * 1",
            "timezone": "Asia/Kolkata",
            "last_triggered_at": fixture.last_trigger,
            "created_at": fixture.created_at,
            "archived_at": fixture.archived_at,
            "purge_after": fixture.now,
            "purged_at": fixture.now,
            "purge_reason": "deleted",
        })
    );
    let remaining: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM schedules WHERE id = $1) + (SELECT count(*) FROM executions WHERE schedule_id = $1)",
    )
    .bind(fixture.schedule_id)
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(remaining, 0);
    let audit: Value = sqlx::query_scalar(
        "SELECT metadata FROM audit_records WHERE action = 'schedule.purged' AND resource_id = $1",
    )
    .bind(fixture.schedule_id.to_string())
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(
        audit,
        json!({"deleted_reminder_id": ledger_id, "purge_reason": "deleted"})
    );

    // Trimming keeps the deterministic newest records.
    assert_eq!(DELETED_REMINDER_LEDGER_LIMIT, 100_000);
    let mut newest = Vec::new();
    for index in 0..4 {
        let id = Uuid::now_v7();
        newest.push(id);
        sqlx::query(
            "INSERT INTO deleted_reminders (schedule_id, org_id, owner_principal_id, silicon_id, \
                 reminder_text, schedule_kind, cron_expression, timezone, created_at, archived_at, \
                 purge_after, purged_at, purge_reason, record_text) \
             VALUES ($1, NULL, $2, 'si:assistant', $3, 'one_time', '0 0 * * *', 'UTC', $4, $5, \
                 $6, $6, 'deleted', '{}'::text)",
        )
        .bind(id)
        .bind(Uuid::now_v7())
        .bind(format!("trim fixture {index}"))
        .bind(fixture.created_at)
        .bind(fixture.archived_at)
        .bind(fixture.now)
        .execute(&database.pool)
        .await?;
    }
    let mut transaction = database.pool.begin().await?;
    assert_eq!(trim_deleted_reminder_ledger(&mut transaction, 3).await?, 2);
    transaction.commit().await?;
    let kept: Vec<Uuid> =
        sqlx::query_scalar("SELECT schedule_id FROM deleted_reminders ORDER BY id")
            .fetch_all(&database.pool)
            .await?;
    assert_eq!(kept, newest[1..]);

    // A ledger conflict rolls back the purge and keeps the source rows.
    let conflict = seed_expired(&database.pool, owner_key).await?;
    sqlx::query(
        "INSERT INTO deleted_reminders (schedule_id, org_id, owner_principal_id, silicon_id, \
             reminder_text, schedule_kind, cron_expression, timezone, created_at, archived_at, \
             purge_after, purged_at, purge_reason, record_text) \
         VALUES ($1, NULL, $2, 'si:assistant', 'x', 'recurring', '0 9 * * 1', 'UTC', $3, $4, $5, \
             $5, 'deleted', '{}'::text)",
    )
    .bind(conflict.schedule_id)
    .bind(owner_key)
    .bind(conflict.created_at)
    .bind(conflict.archived_at)
    .bind(conflict.now)
    .execute(&database.pool)
    .await?;
    assert!(matches!(
        database
            .repository
            .purge_expired_schedules(conflict.now, 100, "retention-test-worker")
            .await,
        Err(RepositoryError::Database(_))
    ));
    let kept_rows: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM schedules WHERE id = $1) + (SELECT count(*) FROM executions WHERE schedule_id = $1)",
    )
    .bind(conflict.schedule_id)
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(kept_rows, 3);
    Ok(())
}

#[tokio::test]
async fn one_time_materialization_archives_before_delivery_and_purges_as_completed()
-> anyhow::Result<()> {
    let database = test_database().await?;
    let seed_now = fixture_now();
    let (_, one_time) =
        seed_due_schedule(&database.pool, "Once", "si:one-time", seed_now, "one_time").await?;
    let worker_now = seed_now + Duration::seconds(1);
    let execution = materialize_schedule(&database.repository, one_time, worker_now, None).await?;
    assert_eq!(
        (execution.schedule_version, execution.schedule_kind.as_str()),
        (2, "one_time")
    );
    let pool = database.pool.clone();
    let state = move |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (String, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
                "SELECT status, version, completed_at, purge_after FROM schedules WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
        }
    };
    let before = state(one_time).await?;
    let completed_at = before.2.ok_or_else(|| anyhow::anyhow!("not archived"))?;
    assert_eq!((before.0.as_str(), before.1), ("completed", 2));
    assert_eq!(
        before.3,
        Some(completed_at + Duration::days(ARCHIVE_RETENTION_DAYS))
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
    assert_eq!(state(one_time).await?, before);

    let purged_at = completed_at + Duration::days(ARCHIVE_RETENTION_DAYS) + Duration::seconds(1);
    let result = database
        .repository
        .purge_expired_schedules(purged_at, 100, "completed-purge")
        .await?;
    assert_eq!((result.purged, result.logged), (1, 1));
    let (reason, archived_at): (String, DateTime<Utc>) = sqlx::query_as(
        "SELECT purge_reason, archived_at FROM deleted_reminders WHERE schedule_id = $1",
    )
    .bind(one_time)
    .fetch_one(&database.pool)
    .await?;
    assert_eq!((reason.as_str(), archived_at), ("completed", completed_at));
    Ok(())
}

#[tokio::test]
async fn sections_partition_current_and_archived_and_archive_cancels_pending_work()
-> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    let (current_key, current) =
        seed_due_schedule(&database.pool, "Cur", "si:current", now, "recurring").await?;
    let (manual_key, manual) =
        seed_due_schedule(&database.pool, "Man", "si:manual", now, "recurring").await?;
    let pending = super::seed_execution(
        &database.pool,
        manual,
        "si:manual",
        now - Duration::minutes(3),
    )
    .await?;
    let archived_at = now + Duration::seconds(1);
    assert!(
        database
            .repository
            .archive_schedule(&[manual_key], manual, archived_at, &service_audit())
            .await?
    );
    // Repeating the archive neither extends retention nor fails.
    let first = sqlx::query_as::<_, (i64, DateTime<Utc>)>(
        "SELECT version, purge_after FROM schedules WHERE id = $1",
    )
    .bind(manual)
    .fetch_one(&database.pool)
    .await?;
    assert!(
        !database
            .repository
            .archive_schedule(
                &[manual_key],
                manual,
                archived_at + Duration::days(1),
                &service_audit()
            )
            .await?
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, DateTime<Utc>)>(
            "SELECT version, purge_after FROM schedules WHERE id = $1"
        )
        .bind(manual)
        .fetch_one(&database.pool)
        .await?,
        first
    );
    // Another account's keys cannot archive it.
    assert!(matches!(
        database
            .repository
            .archive_schedule(&[current_key], manual, archived_at, &service_audit())
            .await,
        Err(RepositoryError::NotFound)
    ));
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, failure_reason FROM executions WHERE id = $1")
            .bind(pending)
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(
        (status.as_str(), reason.as_deref()),
        ("failed", Some("stopped after reminder archive"))
    );

    let list = |section| ListSchedules {
        read_keys: Some(vec![current_key, manual_key]),
        owner_keys: None,
        silicon_snapshot: None,
        section,
        status: None,
        cursor: None,
        limit: 100,
    };
    let current_page = database
        .repository
        .list_schedules(&list(ScheduleSection::Current))
        .await?;
    assert_eq!(
        current_page.items.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![current]
    );
    let archived_page = database
        .repository
        .list_schedules(&list(ScheduleSection::Archived))
        .await?;
    assert_eq!(
        archived_page.items.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![manual]
    );
    Ok(())
}

#[tokio::test]
async fn expired_archives_are_hidden_and_cannot_resume_delivery() -> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    let key = seed_account(
        &database.pool,
        "Exp",
        ActorKind::Silicon,
        "si:expired",
        None,
    )
    .await?;
    let completed_at = now - Duration::days(46);
    let schedule = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO schedules (id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at, completed_at, created_at, updated_at) \
         VALUES ($1, NULL, $2, 'si:expired', 'expired archive', 'UTC', 'one_time', '* * * * *', \
             'completed', NULL, $3, $4, $3)",
    )
    .bind(schedule)
    .bind(key)
    .bind(completed_at)
    .bind(completed_at - Duration::days(1))
    .execute(&database.pool)
    .await?;
    let leased = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO executions (id, schedule_id, org_id, silicon_id, schedule_version, schedule_kind, \
             scheduled_for, reminder_text, timezone, status, attempt_count, next_attempt_at, \
             attempted_at, failure_reason, lease_owner, lease_expires_at) \
         VALUES ($1, $2, NULL, 'si:expired', 2, 'one_time', $3, 'expired leased', 'UTC', 'retrying', \
             1, $4, $3, 'temporary failure', 'expired-lease-worker', $5)",
    )
    .bind(leased)
    .bind(schedule)
    .bind(completed_at)
    .bind(now - Duration::minutes(1))
    .bind(now + Duration::minutes(5))
    .execute(&database.pool)
    .await?;
    let keys = Some(vec![key]);
    assert!(
        database
            .repository
            .get_schedule(keys.as_deref(), schedule)
            .await?
            .is_none()
    );
    assert!(matches!(
        database
            .repository
            .list_executions(keys.as_deref(), schedule, None, 100)
            .await,
        Err(RepositoryError::NotFound)
    ));
    assert!(
        database
            .repository
            .claim_deliveries(
                "expiry-worker",
                now,
                std::time::Duration::from_secs(300),
                100
            )
            .await?
            .is_empty()
    );
    assert!(
        !database
            .repository
            .delivery_lease_is_live(leased, "expired-lease-worker", now)
            .await?
    );
    assert!(matches!(
        database
            .repository
            .mark_delivery_failed(leased, "expired-lease-worker", now, "must remain expired")
            .await,
        Err(RepositoryError::LeaseLost)
    ));
    Ok(())
}
