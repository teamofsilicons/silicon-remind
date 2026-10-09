//! Migration 0010 on an empty database and on a populated pre-0010 database,
//! and the API contract registry it updates.

use uuid::Uuid;

use super::{due_ids, fixture_now, test_database};
use crate::infrastructure::postgres::health_check;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Fingerprint of every pre-existing column of the rows the fixture creates.
const FINGERPRINT: &str = "SELECT md5(string_agg(t, '|' ORDER BY t)) FROM (\
     SELECT (s.id, s.org_id, s.owner_principal_id, s.silicon_id, s.reminder_text, s.timezone, \
             s.schedule_kind, s.cron_expression, s.status, s.next_run_at, s.version, \
             s.completed_at, s.deleted_at, s.purge_after, s.created_at, s.updated_at)::text AS t \
     FROM schedules s \
     UNION ALL SELECT e::text FROM executions e \
     UNION ALL SELECT (d.id, d.org_id, d.owner_principal_id, d.silicon_id, \
             d.endpoint_url_ciphertext, d.endpoint_url_nonce, d.signing_secret_ciphertext, \
             d.signing_secret_nonce, d.encryption_key_version, d.version, d.disabled_at, \
             d.purge_after, d.created_at, d.updated_at)::text FROM hook_destinations d \
     UNION ALL SELECT r::text FROM deleted_reminders r \
     UNION ALL SELECT b::text FROM bug_reports b \
     UNION ALL SELECT a::text FROM audit_records a \
     UNION ALL SELECT i::text FROM idempotency_records i \
     UNION ALL SELECT x::text FROM iam_identity_bindings x \
     UNION ALL SELECT si::text FROM silicon_identities si) rows";

#[tokio::test]
async fn migrations_apply_to_an_empty_database() -> anyhow::Result<()> {
    let database = test_database().await?;
    health_check(&database.pool).await?;
    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' \
         AND table_name IN ('accounts','account_keys','identity_links','reminder_viewers','silicon_allowances')",
    )
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(tables, 5);
    let contracts: Vec<(i32, String)> =
        sqlx::query_as("SELECT version, status FROM api_contract_versions ORDER BY version")
            .fetch_all(&database.pool)
            .await?;
    assert_eq!(
        contracts,
        vec![(1, "sunset".to_owned()), (2, "active".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn migration_0010_keeps_every_existing_row_on_the_upgrade_path() -> anyhow::Result<()> {
    let database = crate::test_support::TestPostgres::start().await?;
    let pool = database.pool(2).await?;
    // origin/main's schema: migrations 0001-0009, applied the way sqlx does.
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 10) {
        let mut transaction = pool.begin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.sql.as_ref()))
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
    }
    let owner = Uuid::now_v7();
    let schedule = Uuid::now_v7();
    let now = fixture_now();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO organization_lifecycle(org_id,state) VALUES('tos','active');\
         INSERT INTO silicon_identities(org_id,principal_id,silicon_id,state) VALUES('tos','{owner}','si:scout','active');\
         INSERT INTO iam_identity_bindings(identity_kind,public_id,local_id) VALUES('silicon','si:scout','{owner}');\
         INSERT INTO schedules(id,org_id,owner_principal_id,silicon_id,reminder_text,timezone,schedule_kind,cron_expression,status,next_run_at) \
             VALUES('{schedule}','tos','{owner}','si:scout','stand up','UTC','recurring','0 9 * * *','active','{due}');\
         INSERT INTO executions(id,schedule_id,org_id,silicon_id,schedule_version,schedule_kind,scheduled_for,reminder_text,timezone,status,next_attempt_at) \
             VALUES('{execution}','{schedule}','tos','si:scout',1,'recurring','{due}','stand up','UTC','pending','{due}');\
         INSERT INTO hook_destinations(id,org_id,owner_principal_id,silicon_id,endpoint_url_ciphertext,endpoint_url_nonce,signing_secret_ciphertext,signing_secret_nonce,encryption_key_version) \
             VALUES('{destination}','tos','{owner}','si:scout','\\x01','\\x000000000000000000000000','\\x02','\\x000000000000000000000000',1);\
         INSERT INTO bug_reports(id,org_id,actor_id,idempotency_key,request_hash,message,status) VALUES('{report}','tos','{owner}','key-12345','h','bug','queued');\
         INSERT INTO idempotency_records(id,org_id,actor_type,actor_id,operation,idempotency_key,request_hash,state,expires_at) \
             VALUES('{idempotency}','tos','silicon','{owner}','schedule.create','key-12345',decode(repeat('ab',32),'hex'),'reserved',now()+interval '1 day');\
         INSERT INTO audit_records(id,org_id,actor_type,actor_id,action,resource_type) VALUES('{audit}','tos','silicon','{owner}','schedule.created','schedule');",
        due = (now - chrono::Duration::minutes(1)).to_rfc3339(),
        execution = Uuid::now_v7(),
        destination = Uuid::now_v7(),
        report = Uuid::now_v7(),
        idempotency = Uuid::now_v7(),
        audit = Uuid::now_v7(),
    )))
    .execute(&pool)
    .await?;
    let before: String = sqlx::query_scalar(FINGERPRINT).fetch_one(&pool).await?;

    // remind-migrate then records 0001-0009 as applied and runs 0010 only.
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 10) {
        sqlx::query("CREATE TABLE IF NOT EXISTS _sqlx_migrations (version bigint PRIMARY KEY, description text NOT NULL, installed_on timestamptz NOT NULL DEFAULT now(), success boolean NOT NULL, checksum bytea NOT NULL, execution_time bigint NOT NULL)")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES($1,$2,true,$3,0)")
            .bind(migration.version)
            .bind(migration.description.as_ref())
            .bind(migration.checksum.as_ref())
            .execute(&pool)
            .await?;
    }
    crate::infrastructure::postgres::migrate(&pool).await?;
    let after: String = sqlx::query_scalar(FINGERPRINT).fetch_one(&pool).await?;
    assert_eq!(before, after, "0010 changed no existing value");
    health_check(&pool).await?;

    let aad: i16 = sqlx::query_scalar("SELECT aad_version FROM hook_destinations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(
        aad, 1,
        "existing ciphertext keeps its original associated data"
    );
    let repository = crate::infrastructure::postgres::PostgresRepository::new(pool.clone());
    assert!(
        due_ids(&repository, now).await?.contains(&schedule),
        "an unlinked legacy reminder still fires"
    );
    Ok(())
}

#[tokio::test]
async fn contract_sunset_requires_seven_idle_days_and_never_retires_current() -> anyhow::Result<()>
{
    let database = test_database().await?;
    sqlx::query("INSERT INTO api_contract_versions(version,status,deprecated_at,last_requested_at) VALUES(3,'deprecated',clock_timestamp()-interval '8 days',NULL),(4,'deprecated',clock_timestamp()-interval '8 days',clock_timestamp()-interval '6 days'),(5,'deprecated',clock_timestamp()-interval '6 days',NULL)")
        .execute(&database.pool)
        .await?;
    crate::api::contracts::sweep(&database.pool).await?;
    let states: Vec<(i32, String)> =
        sqlx::query_as("SELECT version,status FROM api_contract_versions ORDER BY version")
            .fetch_all(&database.pool)
            .await?;
    assert_eq!(
        states,
        vec![
            (1, "sunset".into()),
            (2, "active".into()),
            (3, "sunset".into()),
            (4, "deprecated".into()),
            (5, "deprecated".into())
        ]
    );
    // The current contract is never retired by the sweep.
    sqlx::query("UPDATE api_contract_versions SET status='deprecated',deprecated_at=clock_timestamp()-interval '30 days' WHERE version=2")
        .execute(&database.pool)
        .await?;
    crate::api::contracts::sweep(&database.pool).await?;
    let current: String =
        sqlx::query_scalar("SELECT status FROM api_contract_versions WHERE version=2")
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(current, "deprecated");
    Ok(())
}
