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

#[tokio::test]
async fn profiles_come_from_the_user_base_not_from_lookups() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    harness
        .lookup("Ada", ActorKind::Carbon, "c:ada", None)
        .await;
    harness
        .member("Ada", ActorKind::Carbon, "c:ada", "Ada Lovelace")
        .await;
    // First sight of the account reads the lookup and the user base.
    let token = harness.bearer("Ada", ActorKind::Carbon, "c:ada")?;
    let (status, me) = harness
        .send("GET", "/api/v2/auth/me", Some(&token), None, &[])
        .await?;
    assert_eq!(
        (status, me["display_name"].as_str(), me["pfp_url"].as_str()),
        (
            StatusCode::OK,
            Some("Ada Lovelace"),
            Some("https://example.test/pfp.png")
        )
    );

    // A late account.updated describing an older moment does not win over
    // what the user base shows now.
    let stale = body(
        "evt-stale-name",
        "account.updated",
        &json!({"uuid": "Ada", "membership_id": "remind:Ada", "changed": ["display_name"],
                "account": {"uuid": "Ada", "membership_id": "remind:Ada", "kind": "carbon", "id": "c:ada",
                            "display_name": "Ada (old)", "pfp_url": "", "version": 7,
                            "updated_at": "2026-10-01T00:00:00.000Z"}}),
    );
    let (status, response) = deliver(
        &harness,
        &stale,
        TEST_WEBHOOK_SECRET,
        chrono::Utc::now().timestamp(),
    )
    .await?;
    assert_eq!(
        (status, response["status"].as_str()),
        (StatusCode::OK, Some("processed"))
    );
    let (_, me) = harness
        .send("GET", "/api/v2/auth/me", Some(&token), None, &[])
        .await?;
    assert_eq!(me["display_name"], "Ada Lovelace");
    Ok(())
}

/// Found by the end-to-end run: an account Remind first met through a lookup
/// (shared with by id) kept an empty name and photo after it signed in,
/// because the lookup counted as a fresh read for the whole lookup TTL.
#[tokio::test]
async fn an_account_first_looked_up_gets_its_profile_when_it_signs_in() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    harness
        .seed("Ada", ActorKind::Carbon, "c:ada", None)
        .await?;
    harness
        .seed(
            "Sco",
            ActorKind::Silicon,
            "si:scout",
            Some(("Ada", "c:ada")),
        )
        .await?;
    // Zed is known to Remind only through a lookup by id, which never carries
    // a display name or photo.
    harness
        .lookup("Zed", ActorKind::Carbon, "c:zed", None)
        .await;
    let scout = harness.bearer("Sco", ActorKind::Silicon, "si:scout")?;
    let (status, grant) = harness
        .send(
            "POST",
            "/api/v2/viewers",
            Some(&scout),
            Some(json!({"id": "c:zed"})),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::CREATED, "{grant}");

    // Zed signs in to Remind, so the user base has its profile from now on.
    harness
        .member("Zed", ActorKind::Carbon, "c:zed", "Zed Shaw")
        .await;
    let zed = harness.bearer("Zed", ActorKind::Carbon, "c:zed")?;
    let (status, me) = harness
        .send("GET", "/api/v2/auth/me", Some(&zed), None, &[])
        .await?;
    assert_eq!(
        (status, me["display_name"].as_str(), me["pfp_url"].as_str()),
        (
            StatusCode::OK,
            Some("Zed Shaw"),
            Some("https://example.test/pfp.png")
        )
    );
    Ok(())
}
