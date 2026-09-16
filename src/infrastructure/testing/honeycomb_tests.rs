use super::honeycomb::Operation;
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use testcontainers::{ContainerAsync, ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres as PostgresImage;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

struct Fixture {
    _container: ContainerAsync<PostgresImage>,
    tests: TestEnvironments,
    server: MockServer,
    secret: SecretString,
    context: Value,
    operation: Operation,
}
impl Fixture {
    async fn new() -> anyhow::Result<Self> {
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
        let iam = IamSettings {
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
        let tests = TestEnvironments::connect(&database, &iam, cipher).await?;
        let operation = Operation {
            operation_id: Uuid::now_v7(),
            environment_id: Uuid::now_v7(),
            org_id: "tos".into(),
            app_id: "tos>remind".into(),
            environment_revision: 1,
            generation: 1,
            key_version: 1,
            action: "prepare".into(),
            testing_key: "K".repeat(32),
            snapshot: json!({}),
            reason: "requested".into(),
            retired_apps: vec![],
        };
        let context = json!({"environment_id":operation.environment_id,"application":{"app_id":"tos>remind","base_url":"https://remind.teamofsilicons.com","app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":15},"environment":{"environment_id":operation.environment_id,"org_id":"tos","name":"Managed sandbox","version":1,"key_generation":1,"created_at":"2026-09-13T00:00:00Z","creator_type":"carbon","creator_id":"tester"},"webhook_key_digest":hex::encode(hash(&operation.testing_key))});
        Ok(Self {
            _container: container,
            tests,
            server,
            secret: SecretString::from(format!("ask_{}", "t".repeat(43))),
            context,
            operation,
        })
    }
    async fn publish(&self) {
        self.server.reset().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/application/testing-context"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&self.context))
            .mount(&self.server)
            .await;
    }
    fn next(&mut self, action: &str) {
        self.operation.operation_id = Uuid::now_v7();
        self.operation.environment_revision += 1;
        self.operation.action = action.into();
    }
    async fn marker(&self) -> anyhow::Result<()> {
        let lease = self.tests.enter(&self.secret, false).await?;
        sqlx::query("INSERT INTO bug_reports(id,org_id,actor_id,idempotency_key,request_hash,message,status) VALUES($1,'tos','tester','test','hash','isolated marker','simulated')").bind(Uuid::now_v7()).execute(&lease.pool).await?;
        lease.finish(true).await?;
        Ok(())
    }
    async fn count(&self) -> anyhow::Result<i64> {
        let lease = self.tests.enter(&self.secret, false).await?;
        let count = sqlx::query_scalar("SELECT count(*) FROM bug_reports")
            .fetch_one(&lease.pool)
            .await?;
        lease.finish(false).await?;
        Ok(count)
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "Ordered lifecycle scenario verifies replay safety across clean, disabled sessions, rotation and purge"
)]
async fn coordinator_lifecycle_fences_cleanup_and_replays() -> anyhow::Result<()> {
    let mut f = Fixture::new().await?;
    f.publish().await;
    let prepared = f.tests.apply_honeycomb(&f.operation).await?;
    assert_eq!(prepared["state"], "completed");
    assert!(!prepared.to_string().contains(&f.operation.testing_key));
    assert_eq!(prepared, f.tests.apply_honeycomb(&f.operation).await?);
    let prepare = f.operation.clone();
    let mut altered = prepare.clone();
    altered.snapshot = json!({"changed":true});
    assert!(f.tests.apply_honeycomb(&altered).await.is_err());
    f.marker().await?;
    let id = f.operation.environment_id;
    let admitted = f
        .tests
        .enter_worker(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("worker was not admitted"))?;
    f.next("clean");
    f.operation.generation += 1;
    let tests = f.tests.clone();
    let clean = f.operation.clone();
    let mut cleaning = tokio::spawn(async move { tests.apply_honeycomb(&clean).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(75), &mut cleaning)
            .await
            .is_err(),
        "clean must wait for already admitted work"
    );
    admitted.finish(false).await?;
    assert_eq!(cleaning.await??["state"], "completed");
    assert!(
        f.tests.enter(&f.secret, false).await.is_err(),
        "old IAM state must not admit work after clean"
    );
    f.context["environment"]["version"] = json!(2);
    f.context["environment"]["cleaned_at"] = json!("2026-09-16T01:00:00Z");
    f.publish().await;
    assert!(
        !f.tests
            .webhook_is_current(id, Utc::now() - chrono::Duration::days(1))
            .await?
    );
    assert_eq!(f.count().await?, 0);
    f.marker().await?;
    assert_eq!(
        f.tests.apply_honeycomb(&f.operation).await?["state"],
        "completed"
    );
    assert_eq!(
        f.count().await?,
        1,
        "replayed cleanup must preserve newly created records"
    );
    f.next("disable");
    f.tests.apply_honeycomb(&f.operation).await?;
    assert!(f.tests.enter(&f.secret, false).await.is_err());
    assert!(f.tests.enter_worker(id).await?.is_none());
    f.next("restore");
    f.tests.apply_honeycomb(&f.operation).await?;
    assert_eq!(f.count().await?, 1);
    f.next("rotate-key");
    f.operation.key_version += 1;
    f.operation.testing_key = "R".repeat(32);
    f.tests.apply_honeycomb(&f.operation).await?;
    assert!(
        f.tests.enter(&f.secret, false).await.is_err(),
        "old IAM key version must fail closed"
    );
    f.context["environment"]["version"] = json!(3);
    f.context["environment"]["key_generation"] = json!(2);
    f.context["webhook_key_digest"] = json!(hex::encode(hash(&f.operation.testing_key)));
    f.publish().await;
    let worker = f
        .tests
        .enter_worker(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing restored worker"))?;
    f.tests.validate_dispatch(&worker.environment).await?;
    f.context["environment"]["version"] = json!(4);
    f.publish().await;
    assert!(
        f.tests
            .validate_dispatch(&worker.environment)
            .await
            .is_err(),
        "IAM changes between admission and dispatch must block a retry"
    );
    worker.finish(false).await?;
    f.next("clean");
    f.operation.generation += 1;
    sqlx::raw_sql(AssertSqlSafe(format!(
        "ALTER TABLE {}.telemetry_events RENAME TO unavailable_events",
        schema(id)
    )))
    .execute(&f.tests.control)
    .await?;
    assert!(f.tests.apply_honeycomb(&f.operation).await.is_err());
    assert_eq!(
        f.tests
            .honeycomb_receipt("tos", id, f.operation.operation_id)
            .await?["state"],
        "failed"
    );
    assert!(
        f.tests.enter(&f.secret, false).await.is_err(),
        "failed cleanup remains fenced"
    );
    sqlx::raw_sql(AssertSqlSafe(format!(
        "ALTER TABLE {}.unavailable_events RENAME TO telemetry_events",
        schema(id)
    )))
    .execute(&f.tests.control)
    .await?;
    assert_eq!(
        f.tests.apply_honeycomb(&f.operation).await?["state"],
        "completed"
    );
    f.context["environment"]["cleaned_at"] = json!("2026-09-16T02:00:00Z");
    f.publish().await;
    assert_eq!(f.count().await?, 0);
    let mut stale = f.operation.clone();
    stale.operation_id = Uuid::now_v7();
    assert!(f.tests.apply_honeycomb(&stale).await.is_err());
    f.next("purge");
    f.tests.apply_honeycomb(&f.operation).await?;
    assert!(f.tests.enter(&f.secret, false).await.is_err());
    assert_eq!(f.tests.apply_honeycomb(&prepare).await?, prepared);
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)")
            .bind(schema(id))
            .fetch_one(&f.tests.control)
            .await?;
    assert!(
        !exists,
        "historical prepare receipt must not recreate purged data"
    );
    assert!(
        f.tests
            .honeycomb_receipt("other", id, prepare.operation_id)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn activity_is_retryable_generation_scoped_and_retirement_is_selected() -> anyhow::Result<()>
{
    let mut f = Fixture::new().await?;
    f.publish().await;
    f.tests.apply_honeycomb(&f.operation).await?;
    f.marker().await?;
    let receiver = MockServer::start().await;
    assert!(
        f.tests
            .report_honeycomb_activity(&receiver.uri().parse()?)
            .await
            .is_err()
    );
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/environments/{}/apps/tos%3Eremind/activity",
            f.operation.environment_id
        )))
        .and(header(
            "X-Testing-Environment-Key",
            f.operation.testing_key.as_str(),
        ))
        .and(body_json(json!({"generation":1,"key_version":1})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
        .expect(1)
        .mount(&receiver)
        .await;
    f.tests
        .report_honeycomb_activity(&receiver.uri().parse()?)
        .await?;
    f.tests
        .report_honeycomb_activity(&receiver.uri().parse()?)
        .await?;
    f.next("retire-applications");
    f.operation.retired_apps = vec!["tos>other".into()];
    f.tests.apply_honeycomb(&f.operation).await?;
    assert_eq!(
        f.count().await?,
        1,
        "unselected participant must retain records"
    );
    f.next("retire-applications");
    f.operation.retired_apps = vec!["tos>remind".into()];
    f.tests.apply_honeycomb(&f.operation).await?;
    assert!(
        f.tests
            .enter_worker(f.operation.environment_id)
            .await?
            .is_none()
    );
    f.next("import");
    f.operation.retired_apps.clear();
    f.tests.apply_honeycomb(&f.operation).await?;
    assert_eq!(f.count().await?, 0);
    Ok(())
}
