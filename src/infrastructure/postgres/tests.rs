use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ContainerAsync, ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

use crate::domain::ReminderReadScope;

use super::{
    ActorType, AuditContext, CreateSchedule, ExecutionRow, IamLifecycleOutcome, IdempotencyContext,
    IdempotentMutation, ListSchedules, NewHookDestination, NewInternalEvent, PostgresRepository,
    RepositoryError, ScheduleCursor, health_check, migrate,
};

struct TestDatabase {
    _container: ContainerAsync<Postgres>,
    pool: PgPool,
    repository: PostgresRepository,
}

struct LifecycleFixture {
    now: DateTime<Utc>,
    target_principal_id: Uuid,
    target_schedule_id: Uuid,
    other_schedule_id: Uuid,
    execution_id: Uuid,
}

struct VisibilityFixture {
    carbon_scope: ReminderReadScope,
    denied_schedule: Uuid,
    first_allowed_schedule: Uuid,
    second_allowed_schedule: Uuid,
    allowed_execution: Uuid,
    denied_execution: Uuid,
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
    let now = Utc::now();
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

fn visibility_filters(
    read_scope: ReminderReadScope,
    cursor: Option<ScheduleCursor>,
    limit: u32,
) -> ListSchedules {
    ListSchedules {
        org_id: "tos".to_owned(),
        read_scope,
        silicon_id: None,
        status: None,
        cursor,
        limit,
    }
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
    assert_eq!(outcome.executions_failed, 1);
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
    let now = Utc::now();
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
async fn terminal_execution_only_completes_its_materialized_one_time_generation()
-> anyhow::Result<()> {
    let database = test_database().await?;
    let now = Utc::now();

    let current_id = seed_due_schedule(&database.pool, "current:tos", now, "one_time").await?;
    let current = materialize_schedule(&database.repository, current_id, now, None).await?;
    assert_eq!(current.schedule_version, 2);
    assert_eq!(current.schedule_kind, "one_time");
    claim_execution(&database.repository, current.id, now).await?;
    database
        .repository
        .mark_delivery_succeeded(
            current.id,
            "execution-test-worker",
            Uuid::now_v7(),
            now + Duration::seconds(1),
        )
        .await?;
    assert_schedule_state(&database.pool, current_id, "completed", 3, true).await?;

    let edited_id = seed_due_schedule(&database.pool, "edited:tos", now, "one_time").await?;
    let edited = materialize_schedule(&database.repository, edited_id, now, None).await?;
    let replacement_next_run_at = now + Duration::hours(1);
    replace_with_one_time(&database.pool, edited_id, replacement_next_run_at).await?;
    claim_execution(&database.repository, edited.id, now).await?;
    database
        .repository
        .mark_delivery_succeeded(
            edited.id,
            "execution-test-worker",
            Uuid::now_v7(),
            now + Duration::seconds(1),
        )
        .await?;
    assert_schedule_state(&database.pool, edited_id, "active", 3, false).await?;

    let changed_kind_id =
        seed_due_schedule(&database.pool, "changed-kind:tos", now, "recurring").await?;
    let recurring = materialize_schedule(
        &database.repository,
        changed_kind_id,
        now,
        Some(now + Duration::minutes(1)),
    )
    .await?;
    assert_eq!(recurring.schedule_version, 2);
    assert_eq!(recurring.schedule_kind, "recurring");
    replace_with_one_time(&database.pool, changed_kind_id, replacement_next_run_at).await?;
    claim_execution(&database.repository, recurring.id, now).await?;
    database
        .repository
        .mark_delivery_failed(
            recurring.id,
            "execution-test-worker",
            now + Duration::seconds(1),
            "terminal test failure",
        )
        .await?;
    assert_schedule_state(&database.pool, changed_kind_id, "active", 3, false).await?;
    Ok(())
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
        Err(RepositoryError::WebhookNotConfigured)
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

    let now = Utc::now();
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
    assert_disabled_destination_blocks_new_creation_but_not_replay(
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

async fn assert_disabled_destination_blocks_new_creation_but_not_replay(
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
    assert!(matches!(
        database
            .repository
            .get_schedulable_silicon_identity("tos", schedule.owner_principal_id)
            .await,
        Err(RepositoryError::WebhookNotConfigured)
    ));

    let mut disabled_schedule = schedule.clone();
    disabled_schedule.id = Uuid::now_v7();
    let disabled_idempotency = IdempotencyContext {
        key: "binding-disabled-1".to_owned(),
        request_hash: [9; 32],
        ..idempotency.clone()
    };
    assert!(matches!(
        database
            .repository
            .create_schedule_idempotent(&disabled_schedule, &disabled_idempotency, audit)
            .await,
        Err(RepositoryError::WebhookNotConfigured)
    ));

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

async fn replace_with_one_time(
    pool: &PgPool,
    schedule_id: Uuid,
    next_run_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let result = sqlx::query(
        "UPDATE schedules SET \
             schedule_kind = 'one_time', cron_expression = '0 0 * * *', \
             next_run_at = $1, \
             version = version + 1 \
         WHERE id = $2",
    )
    .bind(next_run_at)
    .bind(schedule_id)
    .execute(pool)
    .await?;
    anyhow::ensure!(
        result.rows_affected() == 1,
        "test schedule was not replaced"
    );
    Ok(())
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
    let now = Utc::now();
    let target_schedule_id = Uuid::now_v7();
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
    Ok(LifecycleFixture {
        now,
        target_principal_id,
        target_schedule_id,
        other_schedule_id,
        execution_id,
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
