use super::*;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
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
        app_id: "tos>remind".into(),
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
    let mut body = json!({"environment_id":id,"application":{"app_id":"tos>remind","base_url":"https://remind.teamofsilicons.com","app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":15},"environment":{"environment_id":id,"org_id":"tos","name":"Discovery test","version":1,"key_generation":1,"created_at":"2026-09-13T00:00:00Z","creator_type":"carbon","creator_id":"tester"},"webhook_key_digest":hex::encode(hash("12345678901234567890123456789012"))});
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .and(wiremock::matchers::basic_auth(
            "tos>remind",
            secret.expose_secret(),
        ))
        .and(wiremock::matchers::header(
            "x-testing-application",
            format!(
                "Basic {}",
                STANDARD.encode(format!("tos>remind:{}", secret.expose_secret()))
            ),
        ))
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
    sqlx::query("INSERT INTO internal_event_receipts(id,source,event_id,event_type,payload,payload_hash,next_attempt_at) VALUES(gen_random_uuid(),'test','receipt-one','test.event','{}',decode(repeat('a',64),'hex'),clock_timestamp())")
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
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM internal_event_receipts")
            .fetch_one(&worker.pool)
            .await?,
        1
    );
    body["environment"]["version"] = json!(2);
    body["environment"]["cleaned_at"] = json!("2026-09-13T01:00:00Z");
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .and(wiremock::matchers::basic_auth(
            "tos>remind",
            secret.expose_secret(),
        ))
        .and(wiremock::matchers::header(
            "x-testing-application",
            format!(
                "Basic {}",
                STANDARD.encode(format!("tos>remind:{}", secret.expose_secret()))
            ),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;
    let mut cleaning = Box::pin(tests.enter_worker(id));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut cleaning)
            .await
            .is_err(),
        "cleaning waits for the admitted worker's shared lifecycle lease"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM internal_event_receipts")
            .fetch_one(&worker.pool)
            .await?,
        1
    );
    worker.finish(false).await?;
    let cleaned = cleaning
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing cleaned lease"))?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM internal_event_receipts")
            .fetch_one(&cleaned.pool)
            .await?,
        0
    );
    cleaned.finish(false).await?;
    body["environment"]["version"] = json!(1);
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .and(wiremock::matchers::basic_auth(
            "tos>remind",
            secret.expose_secret(),
        ))
        .and(wiremock::matchers::header(
            "x-testing-application",
            format!(
                "Basic {}",
                STANDARD.encode(format!("tos>remind:{}", secret.expose_secret()))
            ),
        ))
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

    // IAM may reuse a deleted world's name while Remind retains its old record.
    // Independent imported worlds must not collide with that local metadata.
    let replacement = Uuid::now_v7();
    body["environment_id"] = json!(replacement);
    body["environment"]["environment_id"] = json!(replacement);
    let replacement_secret = SecretString::from(format!("ask_{}", "r".repeat(43)));
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .and(wiremock::matchers::basic_auth(
            "tos>remind",
            replacement_secret.expose_secret(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;
    let replacement_lease = tests.enter(&replacement_secret, false).await?;
    assert_eq!(replacement_lease.environment.id, replacement);
    assert_eq!(replacement_lease.environment.name, "Discovery test");
    replacement_lease.finish(false).await?;
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/application/testing-context"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .mount(&server)
        .await;
    assert!(
        tests.enter_worker(id).await.is_err(),
        "a stored worker must not follow its credential into a different world"
    );
    let names: i64 = sqlx::query_scalar("SELECT count(*) FROM public.testing_environments WHERE org_id='tos' AND name='Discovery test'")
        .fetch_one(&pool).await?;
    assert_eq!(names, 2);

    // The legacy root namespace keeps its original active-name uniqueness.
    let legacy = Uuid::now_v7();
    sqlx::query("INSERT INTO public.testing_environments(id,org_id,creator_id,name,description,iam_environment_id,key_hash,secrets) SELECT $1,org_id,creator_id,name,description,iam_environment_id,$2,secrets FROM public.testing_environments WHERE id=$3")
        .bind(legacy).bind(hash("legacy-name-one")).bind(replacement).execute(&pool).await?;
    let duplicate = sqlx::query("INSERT INTO public.testing_environments(id,org_id,creator_id,name,description,iam_environment_id,key_hash,secrets) SELECT $1,org_id,creator_id,name,description,iam_environment_id,$2,secrets FROM public.testing_environments WHERE id=$3")
        .bind(Uuid::now_v7()).bind(hash("legacy-name-two")).bind(replacement).execute(&pool).await;
    assert!(
        matches!(duplicate, Err(sqlx::Error::Database(ref error)) if error.constraint() == Some("testing_environments_active_name"))
    );
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
