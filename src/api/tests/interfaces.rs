//! A standalone frontend uses the public API, a real Postgres worker, and two independent receivers.
use anyhow::{Context as _, ensure};
use axum::body::Body;
use chrono::Utc;
use http::{Request, StatusCode};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt as _;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{body_partial_json, header, method, path},
};

use super::Harness;
use crate::{
    application::ports::SystemClock,
    config::Settings,
    domain::ActorKind,
    infrastructure::webhook::WebhookClient,
    metrics::Metrics,
    worker::{
        delivery::{DeliveryProcessor, DeliveryProcessorConfig},
        scheduler::materialize_due,
    },
};

#[tokio::test]
async fn browser_can_preflight_and_read_auth_errors() -> anyhow::Result<()> {
    let h = Harness::new().await?;
    for (verb, route) in [
        ("POST", "schedules"),
        ("DELETE", "webhooks/a"),
        ("PUT", "ting"),
    ] {
        let response = h
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri(format!("/api/v2/{route}"))
                    .header("origin", "https://frontend.example")
                    .header("access-control-request-method", verb)
                    .header(
                        "access-control-request-headers",
                        "authorization, idempotency-key, content-type",
                    )
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        ensure!(
            response.headers()["access-control-allow-methods"]
                .to_str()?
                .contains(verb)
        );
        ensure!(
            !response
                .headers()
                .contains_key("access-control-allow-credentials")
        );
    }
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v2/schedules")
                .header("origin", "https://frontend.example")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["access-control-allow-origin"], "*");
    Ok(())
}

async fn create_due(h: &Harness, token: &str, key: &str) -> anyhow::Result<String> {
    let (status, created) = h.send("POST", "/api/v2/schedules", Some(token),
        Some(json!({"text":"Check the build","kind":"one_time","timezone":"UTC","cron":"* * * * *"})),
        &[("idempotency-key",key)]).await?;
    ensure!(status == StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().context("missing schedule id")?;
    sqlx::query(
        "UPDATE schedules SET next_run_at=clock_timestamp()-interval '1 second' WHERE id=$1",
    )
    .bind(uuid::Uuid::parse_str(id)?)
    .execute(&h.pool)
    .await?;
    Ok(format!("/api/v2/schedules/{id}/executions"))
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One integration scenario exercises enrollment, independent receivers and replay."
)]
async fn a_due_reminder_reaches_ting_without_a_hook_and_retries_only_the_failed_channel()
-> anyhow::Result<()> {
    let h = Harness::with_ting(true).await?;
    h.seed("One", ActorKind::Silicon, "si:one", None).await?;
    let token = h.bearer("One", ActorKind::Silicon, "si:one")?;
    Mock::given(method("POST")).and(path("/v1/proofs/user-verification"))
        .and(body_partial_json(json!({"receiving_app":"ting","scopes":["subscriptions.register"],"subject_token":token.trim_start_matches("Bearer ")})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"proof_token":"sap_recipient"}))).expect(1).mount(&h.accounts).await;
    Mock::given(method("POST"))
        .and(path("/v1/subscriptions"))
        .and(header("authorization", "Bearer sap_recipient"))
        .and(body_partial_json(json!({"app_id":"remind","for":"One"})))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"app_id":"remind","for":"One","active":true,"id":"sub_1"})),
        )
        .expect(1)
        .mount(&h.accounts)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/proofs/app-verification"))
        .and(body_partial_json(
            json!({"receiving_app":"ting","scopes":["tings.send"]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"proof_token":"sap_sender"})))
        .mount(&h.accounts)
        .await;
    Mock::given(method("POST")).and(path("/v1/tings")).and(header("authorization","Bearer sap_sender"))
        .and(body_partial_json(json!({"type":"remind.reminderDue","for":"One","data":{"text":"Check the build","timezone":"UTC"}})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"status":"accepted","id":"ting_id"}))).expect(2).mount(&h.accounts).await;

    let mut settings = Settings::for_tests(&h.accounts.uri(), &h.database.url)?;
    settings.ting_url = Some(h.accounts.uri().parse()?);
    let state = crate::api::ApiState::new(&settings, h.pool.clone())?;
    let worker = DeliveryProcessor::new(
        state.repository.clone(),
        state.identity.clone(),
        WebhookClient::new(Duration::from_secs(1), Duration::from_secs(2), 1024)?,
        state.encryption.clone(),
        DeliveryProcessorConfig {
            production: false,
            worker_id: "e2e".into(),
            lease_duration: Duration::from_secs(60),
            max_concurrency: 1,
            retry: settings.retry,
        },
        Arc::new(SystemClock),
        Metrics::new(),
    )
    .with_ting(state.ting.clone());
    let tick = async || -> anyhow::Result<()> {
        materialize_due(&state.repository, "e2e", Utc::now(), 4, &state.metrics).await?;
        assert_eq!(worker.run_once().await?, 1);
        Ok(())
    };
    let history = create_due(&h, &token, "no-webhook-required-key").await?;
    tick().await?;
    let page = h.send("GET", &history, Some(&token), None, &[]).await?.1;
    let item = &page["items"][0];
    assert_eq!(item["status"], "delivered");
    assert_eq!(item["ting_status"], "accepted");
    assert_eq!(item["webhook_status"], "none");
    assert!(item["hook_event_id"].is_null());

    let record = h
        .send("GET", "/api/v2/ting", Some(&token), None, &[])
        .await?
        .1;
    assert!(record["registered_at"].is_string());
    let endpoint = format!("{}/hook", h.accounts.uri());
    let (status, _) = h
        .send(
            "POST",
            "/api/v2/webhooks",
            Some(&token),
            Some(json!({"endpoint_url":endpoint})),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::CREATED);
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.accounts)
        .await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.accounts)
        .await;
    let history = create_due(&h, &token, "optional-webhook-key").await?;
    tick().await?;
    let page = h.send("GET", &history, Some(&token), None, &[]).await?.1;
    assert_eq!(page["items"][0]["status"], "retrying");
    assert_eq!(page["items"][0]["ting_status"], "accepted");
    sqlx::query("UPDATE executions SET next_attempt_at=clock_timestamp() WHERE status='retrying'")
        .execute(&h.pool)
        .await?;
    tick().await?;
    let page = h.send("GET", &history, Some(&token), None, &[]).await?.1;
    let item = &page["items"][0];
    assert_eq!(item["status"], "delivered");
    assert_eq!(item["webhook_status"], "delivered");
    assert_eq!(item["hook_event_id"], item["id"]);
    let requests = h
        .accounts
        .received_requests()
        .await
        .context("missing captured deliveries")?;
    let sends: Vec<Value> = requests
        .iter()
        .filter(|r| r.url.path() == "/v1/tings")
        .map(wiremock::Request::body_json)
        .collect::<Result<_, _>>()?;
    assert!(
        sends
            .iter()
            .all(|body| body["key"] == body["data"]["execution_id"])
    );
    Ok(())
}
