//! `POST /webhook/`: Silicon Accounts deliveries, verified over the raw body.

use http::StatusCode;
use serde_json::{Value, json};
use silicon_accounts_client::sign_webhook;

use super::{Harness, code};
use crate::{domain::ActorKind, test_support::TEST_WEBHOOK_SECRET};

fn body(event_id: &str, event_type: &str, data: &Value) -> String {
    json!({
        "app_id": "remind",
        "data": data,
        "event_id": event_id,
        "occurred_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "silicon": null,
        "type": event_type,
    })
    .to_string()
}

async fn deliver(
    harness: &Harness,
    raw: &str,
    secret: &str,
    timestamp: i64,
) -> anyhow::Result<(StatusCode, Value)> {
    let signature = sign_webhook(secret, timestamp, raw.as_bytes());
    let timestamp = timestamp.to_string();
    let request = http::Request::builder()
        .method("POST")
        .uri("/webhook/")
        .header("content-type", "application/json")
        .header("x-accounts-timestamp", timestamp.as_str())
        .header("x-accounts-signature", signature.as_str())
        .body(axum::body::Body::from(raw.to_owned()))?;
    let response = tower::ServiceExt::oneshot(harness.app.clone(), request).await?;
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await?
        .to_bytes();
    Ok((
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    ))
}

#[tokio::test]
async fn deliveries_need_a_fresh_valid_signature_and_apply_once() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    harness
        .seed("Ada", ActorKind::Carbon, "c:ada", None)
        .await?;
    let now = chrono::Utc::now().timestamp();
    let ping = body("evt-ping", "ping", &json!({}));

    let (status, response) = harness
        .send(
            "POST",
            "/webhook/",
            None,
            Some(json!({"type": "ping"})),
            &[],
        )
        .await?;
    assert_eq!(
        (status, code(&response)),
        (StatusCode::UNAUTHORIZED, "webhook_signature_invalid")
    );
    let (status, response) = deliver(&harness, &ping, "whsec_someone_else_entirely", now).await?;
    assert_eq!(
        (status, code(&response)),
        (StatusCode::UNAUTHORIZED, "webhook_signature_invalid")
    );
    let (status, _) = deliver(&harness, &ping, TEST_WEBHOOK_SECRET, now - 600).await?;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a delivery signed ten minutes ago is a possible replay"
    );

    let (status, response) = deliver(&harness, &ping, TEST_WEBHOOK_SECRET, now).await?;
    assert_eq!(
        (status, response["status"].as_str()),
        (StatusCode::OK, Some("ignored"))
    );
    let (status, response) =
        deliver(&harness, "{\"not\":\"an event\"}", TEST_WEBHOOK_SECRET, now).await?;
    assert_eq!(
        (status, code(&response)),
        (StatusCode::BAD_REQUEST, "webhook_body_invalid")
    );
    let future = body(
        "evt-future",
        "account.something_new",
        &json!({"uuid": "Ada"}),
    );
    let (status, response) = deliver(&harness, &future, TEST_WEBHOOK_SECRET, now).await?;
    assert_eq!(
        (status, response["status"].as_str()),
        (StatusCode::OK, Some("ignored"))
    );

    // A sign-out ends every older token; the same event twice applies once.
    let token = harness.bearer("Ada", ActorKind::Carbon, "c:ada")?;
    let (status, _) = harness
        .send("GET", "/api/v2/auth/me", Some(&token), None, &[])
        .await?;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let signed_out = body(
        "evt-out",
        "membership.signed_out",
        &json!({"uuid": "Ada", "membership_id": "remind:Ada", "reason": "session_revoked"}),
    );
    let (status, response) = deliver(&harness, &signed_out, TEST_WEBHOOK_SECRET, now).await?;
    assert_eq!(
        (status, response["status"].as_str()),
        (StatusCode::OK, Some("processed"))
    );
    let (status, response) = deliver(&harness, &signed_out, TEST_WEBHOOK_SECRET, now).await?;
    assert_eq!(
        (status, response["status"].as_str()),
        (StatusCode::OK, Some("duplicate"))
    );
    let (status, response) = harness
        .send("GET", "/api/v2/auth/me", Some(&token), None, &[])
        .await?;
    assert_eq!(
        (status, code(&response)),
        (StatusCode::UNAUTHORIZED, "token_revoked")
    );
    Ok(())
}
