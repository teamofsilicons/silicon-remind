use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

struct Fixture {
    _database: crate::test_support::TestPostgres,
    control: PgPool,
    tests: TestEnvironments,
}

async fn fixture() -> anyhow::Result<Fixture> {
    let database = crate::test_support::TestPostgres::start().await?;
    let settings = DatabaseSettings {
        url: SecretString::from(database.url.clone()),
        max_connections: 10.try_into()?,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(5),
        statement_timeout: Some(Duration::from_secs(5)),
    };
    let control = PgPool::connect(&database.url).await?;
    TestEnvironments::migrate(&control).await?;
    let cipher = SecretCipherKeyring::from_base64url(
        1,
        &std::collections::BTreeMap::from([(
            1,
            SecretString::from(URL_SAFE_NO_PAD.encode([7; 32])),
        )]),
    )?;
    Ok(Fixture {
        tests: TestEnvironments::connect(&settings, cipher).await?,
        control,
        _database: database,
    })
}

fn input(name: &str) -> CreateTestEnvironment {
    CreateTestEnvironment {
        name: name.to_owned(),
        description: Some("fixture".to_owned()),
        iam_test_key: None,
        iam_app_secret: None,
    }
}

fn access(readers: &[&str], managers: &[&str]) -> EnvironmentAccess {
    EnvironmentAccess {
        readers: readers.iter().map(|uuid| (*uuid).to_owned()).collect(),
        managers: managers.iter().map(|uuid| (*uuid).to_owned()).collect(),
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One environment's whole lifecycle is clearest as one ordered scenario"
)]
async fn an_owned_environment_lives_through_key_clean_retire_and_restore() -> anyhow::Result<()> {
    let fixture = fixture().await?;
    let tests = &fixture.tests;
    let owner = access(&["Own"], &["Own"]);
    let custodian = access(&["Cust", "Own"], &["Cust", "Own"]);
    let sibling = access(&["Sib", "Own"], &["Sib"]);
    let outsider = access(&["Out"], &["Out"]);

    let retired_iam = tests
        .create(
            "Own",
            CreateTestEnvironment {
                iam_test_key: serde_json::from_str("\"legacy\"")?,
                ..input("legacy")
            },
        )
        .await
        .err()
        .map(|error| error.code());
    assert_eq!(retired_iam, Some("iam_test_key_retired".into()));

    let (environment, key) = tests.create("Own", input("staging")).await?;
    assert_eq!(environment.owner_uuid.as_deref(), Some("Own"));
    assert!(matches!(
        tests.create("Own", input("staging")).await,
        Err(AppError::Conflict { code }) if code == "test_environment_name_taken"
    ));
    assert!(
        tests.create("Other", input("staging")).await.is_ok(),
        "names are per owner"
    );

    let listed = |access| async move { tests.list(&access, false, None, 50).await };
    assert_eq!(listed(owner.clone()).await?.len(), 1);
    assert_eq!(
        listed(sibling.clone()).await?.len(),
        1,
        "the owner's circle sees it"
    );
    assert!(listed(outsider.clone()).await?.is_empty());
    assert_eq!(
        tests.key(&sibling, environment.id).await?.expose_secret(),
        key.expose_secret()
    );
    assert!(matches!(
        tests.key(&outsider, environment.id).await,
        Err(AppError::NotFound)
    ));

    // The key opens the environment's own schema, never production tables.
    let lease = tests.enter(&key, false).await?;
    sqlx::query("INSERT INTO accounts (uuid, kind, public_id) VALUES ('Own', 'silicon', 'si:own')")
        .execute(&lease.pool)
        .await?;
    lease.finish(true).await?;
    let schema_rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {}.accounts",
        schema(environment.id)
    )))
    .fetch_one(&fixture.control)
    .await?;
    assert_eq!(schema_rows, 1);
    assert!(
        sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass('public.schedules')::text")
            .fetch_one(&fixture.control)
            .await?
            .is_none()
    );

    // Only managers rotate; the old key stops working at once.
    assert!(matches!(
        tests.rotate(&sibling, environment.id, false).await,
        Err(error) if error.code() == "not_environment_manager"
    ));
    let rotated = tests.rotate(&custodian, environment.id, false).await?;
    assert!(
        matches!(tests.enter(&key, false).await, Err(error) if error.code() == "test_key_invalid")
    );

    // Anyone with the key cleans everything inside.
    let mut cleaning = tests.enter(&rotated, true).await?;
    tests.clean(&mut cleaning).await?;
    cleaning.finish(true).await?;
    let cleaned: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {}.accounts",
        schema(environment.id)
    )))
    .fetch_one(&fixture.control)
    .await?;
    assert_eq!(cleaned, 0);

    // Retire, then restore within 30 days with a new key.
    tests.delete(&owner, environment.id).await?;
    assert!(tests.enter(&rotated, false).await.is_err());
    assert!(listed(owner.clone()).await?.is_empty());
    assert_eq!(tests.list(&owner, true, None, 50).await?.len(), 1);
    let restored = tests.rotate(&owner, environment.id, true).await?;
    tests.enter(&restored, false).await?.finish(false).await?;
    assert_eq!(tests.retire_owned_by("Own").await?, 1);
    assert!(tests.enter(&restored, false).await.is_err());

    // Environments IAM or Honeycomb controlled stay dormant.
    let dormant = Uuid::now_v7();
    let dormant_key = "D".repeat(32);
    sqlx::query("INSERT INTO public.testing_environments (id, org_id, creator_id, owner_uuid, name, iam_environment_id, key_hash, secrets, iam_control_version) VALUES ($1, 'tos', 'honeycomb', 'Own', 'imported', $2, $3, '{}'::jsonb, 1)")
        .bind(dormant)
        .bind(Uuid::now_v7())
        .bind(hash(&dormant_key))
        .execute(&fixture.control)
        .await?;
    assert!(
        tests
            .enter(&SecretString::from(dormant_key), false)
            .await
            .is_err()
    );
    assert!(
        tests
            .list(&owner, true, None, 50)
            .await?
            .iter()
            .all(|e| e.id != dormant)
    );
    assert!(!tests.active_ids(None).await?.contains(&dormant));
    Ok(())
}

#[tokio::test]
async fn the_worker_skips_idle_environments_and_admits_due_and_maintenance_work()
-> anyhow::Result<()> {
    let fixture = fixture().await?;
    let tests = &fixture.tests;
    let (environment, key) = tests.create("Own", input("worker")).await?;
    let id = environment.id;
    let lease = tests.enter(&key, false).await?;
    let data = lease.pool.clone();
    lease.finish(false).await?;
    let now = Utc::now();
    for _ in 0..125 {
        assert!(tests.enter_worker_if_pending(id, now, 1).await?.is_none());
    }
    let owner = Uuid::now_v7();
    let schedule = Uuid::now_v7();
    sqlx::query("INSERT INTO schedules(id,org_id,owner_principal_id,silicon_id,reminder_text,schedule_kind,cron_expression,next_run_at) VALUES($1,NULL,$2,'si:worker','synthetic','one_time','0 0 1 1 *',$3)")
        .bind(schedule).bind(owner).bind(now + chrono::Duration::days(1)).execute(&data).await?;
    assert!(
        tests.enter_worker_if_pending(id, now, 1).await?.is_none(),
        "future work stays idle"
    );
    sqlx::query("UPDATE schedules SET next_run_at=$2 WHERE id=$1")
        .bind(schedule)
        .bind(now)
        .execute(&data)
        .await?;
    let admitted = tests
        .enter_worker_if_pending(id, now, 1)
        .await?
        .ok_or_else(|| anyhow::anyhow!("due work must be admitted"))?;
    admitted.finish(false).await?;
    sqlx::query("UPDATE schedules SET status='paused', next_run_at=NULL WHERE id=$1")
        .bind(schedule)
        .execute(&data)
        .await?;
    let execution = Uuid::now_v7();
    sqlx::query("INSERT INTO executions(id,schedule_id,org_id,silicon_id,schedule_version,schedule_kind,scheduled_for,reminder_text,timezone,next_attempt_at,lease_owner,lease_expires_at) VALUES($1,$2,NULL,'si:worker',1,'one_time',$3,'synthetic','UTC',$3,'other-worker',$4)")
        .bind(execution).bind(schedule).bind(now).bind(now + chrono::Duration::minutes(1)).execute(&data).await?;
    assert!(
        tests.enter_worker_if_pending(id, now, 1).await?.is_none(),
        "an unexpired lease stays idle"
    );
    sqlx::query("UPDATE executions SET lease_expires_at=$2 WHERE id=$1")
        .bind(execution)
        .bind(now)
        .execute(&data)
        .await?;
    assert!(
        tests.enter_worker_if_pending(id, now, 1).await?.is_some(),
        "an expired lease is work"
    );
    sqlx::query("DELETE FROM executions WHERE id=$1")
        .bind(execution)
        .execute(&data)
        .await?;
    sqlx::query("INSERT INTO idempotency_records(id,org_id,actor_type,actor_id,operation,idempotency_key,request_hash,created_at,expires_at) VALUES($1,NULL,'silicon','Own','fixture','fixture-key',decode(repeat('00',32),'hex'),$2,$3)")
        .bind(Uuid::now_v7()).bind(now - chrono::Duration::days(2)).bind(now - chrono::Duration::days(1)).execute(&data).await?;
    assert!(
        tests.enter_worker_if_pending(id, now, 1).await?.is_some(),
        "retention must not starve"
    );
    Ok(())
}
