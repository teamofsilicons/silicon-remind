use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use testcontainers::{ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres as PostgresImage;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One ordered integration scenario verifies cleanup and revocation of the same sandbox"
)]
async fn discovery_isolated_cleanup_revocation_and_worker_admission() -> anyhow::Result<()> {
    let container = PostgresImage::default()
        .with_tag("17-alpine")
        .start()
        .await?;
    let url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        container.get_host().await?,
        container.get_host_port_ipv4(5432).await?
    );
    let database = DatabaseSettings {
        url: SecretString::from(url.clone()),
        max_connections: 10.try_into()?,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(5),
        statement_timeout: Some(Duration::from_secs(5)),
    };
    let pool = PgPool::connect(&url).await?;
    TestEnvironments::migrate(&pool).await?;
    let server = MockServer::start().await;
    let settings = IamSettings {
        base_url: server.uri().parse()?,
        app_id: "remind".into(),
        app_secret: SecretString::from(format!("ask_{}", "p".repeat(43))),
        request_timeout: Duration::from_secs(2),
        webhook_keys: std::collections::BTreeMap::default(),
    };
    let cipher = SecretCipherKeyring::from_base64url(
        1,
        &std::collections::BTreeMap::from([(
            1,
            SecretString::from(URL_SAFE_NO_PAD.encode([7; 32])),
        )]),
    )?;
    let tests = TestEnvironments::connect(&database, &settings, cipher).await?;
    let secret = SecretString::from(format!("ask_{}", "t".repeat(43)));
    let id = Uuid::now_v7();
    let mut body = json!({"environment_id":id,"application":{"app_id":"remind","base_url":"https://remind.teamofsilicons.com","app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":15},"environment":{"environment_id":id,"org_id":"tos","name":"Discovery test","version":1,"key_generation":1,"created_at":"2026-09-13T00:00:00Z","creator_type":"carbon","creator_id":"tester"},"webhook_key_digest":hex::encode(hash("12345678901234567890123456789012"))});
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;
    let lease = tests.enter(&secret, false).await?;
    assert_eq!(lease.environment.id, id);
    assert_eq!(lease.environment.creator_id, "tester");
    assert!(lease.iam_key.is_none());
    assert_eq!(
        lease
            .iam
            .as_ref()
            .map(IamClient::public_info)
            .and_then(|v| v.get("iam_environment_id").cloned()),
        Some(json!(id))
    );
    sqlx::query("UPDATE api_contract_versions SET request_count=42")
        .execute(&lease.pool)
        .await?;
    lease.finish(true).await?;
    assert!(
        tests.enter(&secret, true).await.is_err(),
        "application selection grants no administrative cleaning"
    );
    let worker = tests
        .enter_worker(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing worker lease"))?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT request_count FROM api_contract_versions WHERE version=1"
        )
        .fetch_one(&worker.pool)
        .await?,
        42
    );
    worker.finish(false).await?;
    body["environment"]["version"] = json!(2);
    body["environment"]["cleaned_at"] = json!("2026-09-13T01:00:00Z");
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;
    let cleaned = tests
        .enter_worker(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing cleaned lease"))?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT request_count FROM api_contract_versions WHERE version=1"
        )
        .fetch_one(&cleaned.pool)
        .await?,
        0
    );
    cleaned.finish(false).await?;
    body["environment"]["version"] = json!(1);
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;
    assert!(
        tests.enter(&secret, false).await.is_err(),
        "stale control revision must fail closed"
    );
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error":{"code":"unauthorized","message":"revoked"}})),
        )
        .mount(&server)
        .await;
    assert!(
        tests.enter_worker(id).await.is_err(),
        "revoked app secrets must stop background delivery"
    );
    assert!(tests.enter(&secret, false).await.is_err());
    let public_schedule: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.schedules')::text")
            .fetch_one(&pool)
            .await?;
    assert!(
        public_schedule.is_none(),
        "discovery never materializes production reminder tables"
    );
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One real database scenario proves idle suppression and fresh admission for due work and maintenance"
)]
async fn idle_worker_skips_iam_but_due_and_retention_keep_fresh_admission() -> anyhow::Result<()> {
    let container = if std::env::var_os("REMIND_WORKER_TEST_DATABASE_URL").is_none() {
        Some(
            PostgresImage::default()
                .with_tag("17-alpine")
                .start()
                .await?,
        )
    } else {
        None
    };
    let url = match &container {
        Some(container) => format!(
            "postgres://postgres:postgres@{}:{}/postgres",
            container.get_host().await?,
            container.get_host_port_ipv4(5432).await?
        ),
        None => std::env::var("REMIND_WORKER_TEST_DATABASE_URL")?,
    };
    let database = DatabaseSettings {
        url: SecretString::from(url.clone()),
        max_connections: 10.try_into()?,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(5),
        statement_timeout: Some(Duration::from_secs(5)),
    };
    let pool = PgPool::connect(&url).await?;
    TestEnvironments::migrate(&pool).await?;
    let server = MockServer::start().await;
    let settings = IamSettings {
        base_url: server.uri().parse()?,
        app_id: "remind".into(),
        app_secret: SecretString::from(format!("ask_{}", "p".repeat(43))),
        request_timeout: Duration::from_secs(2),
        webhook_keys: std::collections::BTreeMap::default(),
    };
    let cipher = SecretCipherKeyring::from_base64url(
        1,
        &std::collections::BTreeMap::from([(
            1,
            SecretString::from(URL_SAFE_NO_PAD.encode([7; 32])),
        )]),
    )?;
    let tests = TestEnvironments::connect(&database, &settings, cipher).await?;
    let secret = SecretString::from(format!("ask_{}", "t".repeat(43)));
    let id = Uuid::now_v7();
    let body = json!({"environment_id":id,"application":{"app_id":"remind","base_url":"https://remind.teamofsilicons.com","app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":15},"environment":{"environment_id":id,"org_id":"tos","name":"Discovery test","version":1,"key_generation":1,"created_at":"2026-09-13T00:00:00Z","creator_type":"carbon","creator_id":"tester"},"webhook_key_digest":hex::encode(hash("12345678901234567890123456789012"))});
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;

    let lease = tests.enter(&secret, false).await?;
    let data = lease.pool.clone();
    lease.finish(false).await?;
    let now = Utc::now();
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error":{"code":"invalid_client","message":"revoked"}})),
        )
        .expect(0)
        .mount(&server)
        .await;
    for _ in 0..125 {
        assert!(tests.enter_worker_if_pending(id, now, 1).await?.is_none());
    }
    let owner = Uuid::now_v7();
    let schedule = Uuid::now_v7();
    sqlx::query("INSERT INTO organization_lifecycle(org_id,state) VALUES('tos','active')")
        .execute(&data)
        .await?;
    sqlx::query("INSERT INTO silicon_identities(org_id,principal_id,silicon_id,state) VALUES('tos',$1,'si:worker','active')").bind(owner).execute(&data).await?;
    sqlx::query("INSERT INTO schedules(id,org_id,owner_principal_id,silicon_id,reminder_text,schedule_kind,cron_expression,next_run_at) VALUES($1,'tos',$2,'si:worker','synthetic','one_time','0 0 1 1 *',$3)").bind(schedule).bind(owner).bind(now+chrono::Duration::days(1)).execute(&data).await?;
    assert!(tests.enter_worker_if_pending(id, now, 1).await?.is_none());
    sqlx::query("UPDATE schedules SET status='paused', next_run_at=NULL WHERE id=$1")
        .bind(schedule)
        .execute(&data)
        .await?;
    assert!(tests.enter_worker_if_pending(id, now, 1).await?.is_none());
    server.verify().await;
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error":{"code":"invalid_client","message":"revoked"}})),
        )
        .mount(&server)
        .await;
    // Due work cannot be admitted from the local hint when IAM refuses it.
    sqlx::query("UPDATE schedules SET status='active',next_run_at=$2 WHERE id=$1")
        .bind(schedule)
        .bind(now)
        .execute(&data)
        .await?;
    assert!(tests.enter_worker_if_pending(id, now, 1).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM executions")
            .fetch_one(&data)
            .await?,
        0
    );
    sqlx::query("UPDATE schedules SET status='paused',next_run_at=NULL WHERE id=$1")
        .bind(schedule)
        .execute(&data)
        .await?;
    let execution = Uuid::now_v7();
    sqlx::query("INSERT INTO executions(id,schedule_id,org_id,silicon_id,schedule_version,schedule_kind,scheduled_for,reminder_text,timezone,next_attempt_at,lease_owner,lease_expires_at) VALUES($1,$2,'tos','si:worker',1,'one_time',$3,'synthetic','UTC',$3,'other-worker',$4)").bind(execution).bind(schedule).bind(now).bind(now+chrono::Duration::minutes(1)).execute(&data).await?;
    assert!(
        tests.enter_worker_if_pending(id, now, 1).await?.is_none(),
        "unexpired lease must remain idle"
    );
    sqlx::query("UPDATE executions SET lease_expires_at=$2 WHERE id=$1")
        .bind(execution)
        .bind(now)
        .execute(&data)
        .await?;
    assert!(
        tests.enter_worker_if_pending(id, now, 1).await.is_err(),
        "expired lease must rediscover authority"
    );
    sqlx::query("UPDATE executions SET status='retrying',attempt_count=1,attempted_at=$2,lease_owner=NULL,lease_expires_at=NULL WHERE id=$1").bind(execution).bind(now).execute(&data).await?;
    assert!(
        tests.enter_worker_if_pending(id, now, 1).await.is_err(),
        "retry on a paused schedule still needs admission"
    );
    sqlx::query("DELETE FROM executions WHERE id=$1")
        .bind(execution)
        .execute(&data)
        .await?;
    // A due retention record also requires fresh authority, without a due job.
    sqlx::query("INSERT INTO idempotency_records(id,org_id,actor_type,actor_id,operation,idempotency_key,request_hash,created_at,expires_at) VALUES($1,'tos','silicon','si:worker','fixture','fixture-key',decode(repeat('00',32),'hex'),$2,$3)").bind(Uuid::now_v7()).bind(now-chrono::Duration::days(2)).bind(now-chrono::Duration::days(1)).execute(&data).await?;
    assert!(
        tests.enter_worker_if_pending(id, now, 1).await.is_err(),
        "retention must not starve"
    );
    sqlx::query("UPDATE organization_lifecycle SET state='revoked',revoked_at=$1")
        .bind(now)
        .execute(&data)
        .await?;
    assert!(
        tests.enter_worker_if_pending(id, now, 1).await.is_err(),
        "revocation cleanup must not starve"
    );
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .expect(1)
        .mount(&server)
        .await;
    let admitted = tests
        .enter_worker_if_pending(id, now, 1)
        .await?
        .ok_or_else(|| anyhow::anyhow!("expected live lease"))?;
    assert_eq!(admitted.environment.id, id);
    admitted.finish(false).await?;
    server.verify().await;
    assert!(
        sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass('public.schedules')::text")
            .fetch_one(&pool)
            .await?
            .is_none()
    );
    Ok(())
}
