//! The Remind client against a stub server: contract 2 paths and headers, credentials
//! (access token, verification proof, test environment key), and typed errors that keep
//! Remind's `{error:{code,message,hint}}` and Silicon Accounts' refusals.
use serde_json::json;
use silicon_remind_client::{Client, Error, Refusal, Secret, accounts::SignIn, models};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
};

fn schedule() -> serde_json::Value {
    json!({"id": "01992000-0000-7000-8000-000000000001",
        "owner": {"uuid": "zQo", "id": "si:scout", "kind": "silicon"}, "silicon_id": "si:scout",
        "text": "Standup", "timezone": "UTC", "kind": "recurring", "cron": "0 9 * * *",
        "status": "active", "section": "current", "next_run_at": null, "archived_at": null,
        "purge_after": null, "created_at": "2026-10-10T00:00:00Z", "updated_at": "2026-10-10T00:00:00Z"})
}

#[tokio::test]
async fn requests_use_contract_two_with_a_bearer_and_an_optional_test_key()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/schedules"))
        .and(query_param("silicon_id", "si:scout"))
        .and(query_param("section", "archived"))
        .and(header("authorization", "Bearer at-1"))
        .and(header("x-remind-api-version", "2"))
        .and(header(
            "x-remind-test-key",
            "12345678901234567890123456789012",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"items": [schedule()], "next_cursor": null})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/health/live"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"service": "silicon-remind", "status": "ok", "version": "0.6.0"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri())?
        .with_session(Secret::new("at-1"))?
        .with_test_environment(Secret::new("12345678901234567890123456789012"))?;
    let page = client
        .reminders(&models::ListSchedules {
            silicon_id: Some("si:scout".into()),
            section: models::ScheduleSection::Archived,
            ..Default::default()
        })
        .await?;
    assert_eq!(
        page.items[0].owner.as_ref().map(|o| o.uuid.as_str()),
        Some("zQo")
    );
    client.health(false).await?;
    let requests = server.received_requests().await.unwrap_or_default();
    let health = requests
        .iter()
        .find(|r| r.url.path() == "/health/live")
        .ok_or("health")?;
    assert!(
        !health.headers.contains_key("x-remind-test-key"),
        "health never carries the key"
    );
    Ok(())
}

#[tokio::test]
async fn another_app_reads_with_a_verification_proof() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/auth/me"))
        .and(header("authorization", "Proof sap_proof"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uuid": "aB3", "kind": "carbon", "id": "c:ada", "display_name": "Ada", "pfp_url": "",
            "custodian": null, "can_manage_reminders": false, "credential": "proof",
            "issuing_app": "interface", "visible_silicons": 2})))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri())?.with_proof(Secret::new("sap_proof"))?;
    let me = client.me().await?;
    assert_eq!(me.credential, "proof");
    assert_eq!(me.issuing_app.as_deref(), Some("interface"));
    assert!(
        Client::new(&server.uri())?
            .with_proof(Secret::new("at-1"))
            .is_err()
    );
    assert!(
        Client::new(&server.uri())?
            .with_session(Secret::new("sap_x"))
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn remind_errors_keep_code_message_hint_and_request_id()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(
            "/api/v2/schedules/01992000-0000-7000-8000-000000000001",
        ))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("retry-after", "7")
                .set_body_json(json!({"error": {"code": "not_reminder_owner",
                    "message": "Only si:scout changes this reminder.", "hint": "Ask si:scout.",
                    "request_id": "req_123"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/silicons"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>bad gateway</html>"))
        .mount(&server)
        .await;
    let client = Client::new(&server.uri())?.with_session(Secret::new("at-1"))?;
    let error = client
        .archive_reminder("01992000-0000-7000-8000-000000000001".parse()?)
        .await
        .err()
        .ok_or("archive must fail")?;
    assert_eq!(error.code(), "not_reminder_owner");
    assert_eq!(error.status(), Some(403));
    assert_eq!(error.hint(), Some("Ask si:scout."));
    assert_eq!(error.request_id(), Some("req_123"));
    assert!(matches!(
        error,
        Error::Api {
            retry_after: Some(7),
            ..
        }
    ));
    assert!(error.to_string().contains("Hint: Ask si:scout."));
    let error = client
        .silicons(None, 10)
        .await
        .err()
        .ok_or("silicons must fail")?;
    assert_eq!(error.status(), Some(502));
    assert_eq!(error.code(), "unexpected_response");
    assert!(error.is_transient());
    Ok(())
}

#[tokio::test]
async fn origins_are_https_or_this_machine_only() {
    assert!(Client::new("https://backend.remind.teamofsilicons.com").is_ok());
    assert!(Client::new("http://127.0.0.1:4181").is_ok());
    assert!(Client::new("http://localhost:4181").is_ok());
    assert!(Client::new("http://remind.example").is_err());
    assert!(Client::new("https://remind.example/api").is_err());
    assert!(SignIn::new("http://accounts.example", "remind").is_err());
    assert!(SignIn::new("https://accounts.example", " ").is_err());
}

#[tokio::test]
async fn sign_in_failures_are_typed() -> Result<(), Box<dyn std::error::Error>> {
    let accounts = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant",
            "error_description": "This refresh token was already used once."})),
        )
        .mount(&accounts)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("grant-type%3Aslt"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant",
            "error_description": "The short-lived token was issued at 2026-10-10T00:00:00Z by a sign-in of si:rusty that ended when its custodian rotated its STK."})))
        .mount(&accounts)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"revoked": false, "message": "unknown"})),
        )
        .mount(&accounts)
        .await;
    let sign_in = SignIn::new(&accounts.uri(), "remind")?;
    let ended = sign_in
        .refresh(&Secret::new("sar_used"))
        .await
        .err()
        .ok_or("refresh must fail")?;
    assert!(matches!(
        ended,
        Error::SignInRefused {
            refusal: Refusal::SignInEnded,
            ..
        }
    ));
    assert_eq!(ended.code(), "sign_in_ended");
    assert!(ended.hint().is_some_and(|h| h.contains("remind login")));
    assert!(!sign_in.revoke(&Secret::new("sar_unknown")).await?);
    let refused = sign_in
        .exchange_slt(&Secret::new("slt_x"))
        .await
        .err()
        .ok_or("exchange must fail")?;
    assert_eq!(
        refused.code(),
        "sign_in_refused",
        "an unclassified invalid_grant is still a refusal"
    );
    let unreachable = SignIn::new("http://127.0.0.1:9", "remind")?
        .exchange_slt(&Secret::new("slt_x"))
        .await
        .err()
        .ok_or("must fail")?;
    assert_eq!(unreachable.code(), "connection_failed");
    assert!(
        unreachable
            .hint()
            .is_some_and(|h| h.contains("still unused"))
    );
    Ok(())
}
