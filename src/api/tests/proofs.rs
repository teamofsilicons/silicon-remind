//! User verification proofs from other apps (the Silicon Interface reads
//! reminders for an account).

use http::StatusCode;
use serde_json::{Value, json};
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{body_partial_json, method, path},
};

use super::{Harness, code};
use crate::domain::ActorKind;

fn verification(issuer: &str, scopes: &[&str], receiving: &str) -> Value {
    json!({
        "valid": true,
        "proof_id": "prf_1",
        "kind": "user_verification",
        "expires_at": (chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "issuing_app": {"app_id": issuer, "name": issuer},
        "receiving_app": {"app_id": receiving, "name": receiving},
        "user": {"uuid": "Ada", "id": "c:ada", "kind": "carbon", "membership_id": format!("{issuer}:Ada")},
        "scopes": scopes,
    })
}

async fn proof(harness: &Harness, token: &str, answer: Value) {
    Mock::given(method("POST"))
        .and(path("/v1/proofs/verify"))
        .and(body_partial_json(json!({"proof_token": token})))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(&harness.accounts)
        .await;
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "Every proof outcome is checked against one seeded reminder"
)]
async fn proofs_read_for_allowed_issuers_and_never_write() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    harness
        .seed("Ada", ActorKind::Carbon, "c:ada", None)
        .await?;
    harness
        .seed("One", ActorKind::Silicon, "si:one", Some(("Ada", "c:ada")))
        .await?;
    let one = harness.bearer("One", ActorKind::Silicon, "si:one")?;
    let (status, _) = harness
        .send("POST", "/api/v2/schedules", Some(&one), Some(json!({"text": "Stand up", "kind": "recurring", "cron": "0 9 * * *", "timezone": "UTC"})), &[("idempotency-key", "proof-reminder-1")])
        .await?;
    assert_eq!(status, StatusCode::CREATED);

    proof(
        &harness,
        "sap_valid",
        verification("interface", &["remind.schedules.read"], "remind"),
    )
    .await;
    proof(
        &harness,
        "sap_no_scope",
        verification("interface", &["remind.other"], "remind"),
    )
    .await;
    proof(
        &harness,
        "sap_glass",
        verification("glass", &["remind.schedules.read"], "remind"),
    )
    .await;
    proof(
        &harness,
        "sap_elsewhere",
        verification("interface", &["remind.schedules.read"], "briefcase"),
    )
    .await;
    proof(
        &harness,
        "sap_invalid",
        json!({"valid": false, "expires_at": null}),
    )
    .await;

    // Interface reads what Ada (the account it acts for) can read.
    let (status, body) = harness
        .send(
            "GET",
            "/api/v2/schedules",
            Some("Proof sap_valid"),
            None,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["items"][0]["silicon_id"], "si:one");
    let (status, me) = harness
        .send("GET", "/api/v2/auth/me", Some("Proof sap_valid"), None, &[])
        .await?;
    assert_eq!(
        (
            status,
            me["credential"].as_str(),
            me["issuing_app"].as_str()
        ),
        (StatusCode::OK, Some("proof"), Some("interface"))
    );

    for (authorization, verb, uri, expected_status, expected_code) in [
        (
            "Proof sap_valid",
            "POST",
            "/api/v2/schedules",
            StatusCode::UNAUTHORIZED,
            "proof_not_accepted",
        ),
        (
            "Proof sap_valid",
            "GET",
            "/api/v2/webhooks",
            StatusCode::UNAUTHORIZED,
            "proof_not_accepted",
        ),
        (
            "Proof sap_no_scope",
            "GET",
            "/api/v2/schedules",
            StatusCode::FORBIDDEN,
            "proof_scope_missing",
        ),
        (
            "Proof sap_glass",
            "GET",
            "/api/v2/schedules",
            StatusCode::FORBIDDEN,
            "proof_issuer_not_allowed",
        ),
        (
            "Proof sap_elsewhere",
            "GET",
            "/api/v2/schedules",
            StatusCode::UNAUTHORIZED,
            "proof_invalid",
        ),
        (
            "Proof sap_invalid",
            "GET",
            "/api/v2/schedules",
            StatusCode::UNAUTHORIZED,
            "proof_invalid",
        ),
        (
            "Bearer sap_valid",
            "GET",
            "/api/v2/schedules",
            StatusCode::UNAUTHORIZED,
            "proof_as_bearer",
        ),
    ] {
        let body = (verb == "POST").then(
            || json!({"text": "x", "kind": "one_time", "cron": "0 9 * * *", "timezone": "UTC"}),
        );
        let (status, response) = harness
            .send(
                verb,
                uri,
                Some(authorization),
                body,
                &[("idempotency-key", "proof-write-1")],
            )
            .await?;
        assert_eq!(
            (status, code(&response)),
            (expected_status, expected_code),
            "{authorization} {verb} {uri}: {response}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn removed_access_and_malformed_proofs_are_refused() -> anyhow::Result<()> {
    let h = Harness::new().await?;
    h.seed("Ada", ActorKind::Carbon, "c:ada", None).await?;
    sqlx::query("UPDATE accounts SET status = 'access_removed' WHERE uuid = 'Ada'")
        .execute(&h.pool)
        .await?;
    proof(
        &h,
        "sap_suspended",
        verification("interface", &["remind.schedules.read"], "remind"),
    )
    .await;
    let (status, response) = h
        .send(
            "GET",
            "/api/v2/schedules",
            Some("Proof sap_suspended"),
            None,
            &[],
        )
        .await?;
    assert_eq!(
        (status, code(&response)),
        (StatusCode::UNAUTHORIZED, "access_removed")
    );
    let before = h
        .accounts
        .received_requests()
        .await
        .unwrap_or_default()
        .len();
    for token in [
        "Proof malformed".to_owned(),
        format!("Proof sap_{}", "x".repeat(4096)),
    ] {
        let (status, response) = h
            .send("GET", "/api/v2/schedules", Some(&token), None, &[])
            .await?;
        assert_eq!(
            (status, code(&response)),
            (StatusCode::UNAUTHORIZED, "proof_invalid")
        );
    }
    assert_eq!(
        before,
        h.accounts
            .received_requests()
            .await
            .unwrap_or_default()
            .len()
    );
    Ok(())
}
