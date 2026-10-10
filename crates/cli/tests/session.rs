//! Keeping a sign-in fresh and ending it, against stub Silicon Accounts and Remind servers:
//! refresh rotation, single-flight refresh across processes, retry after a 401, ended
//! sign-ins, `login status` and `logout`.
mod common;

use anyhow::{Context as _, Result};
use common::{Home, form, identity, json_error, oauth_error, ok_json, tokens};
use serde_json::json;
use std::time::Duration;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

async fn servers() -> (MockServer, MockServer) {
    (MockServer::start().await, MockServer::start().await)
}

async fn refresh_answers(
    accounts: &MockServer,
    old: &str,
    new_access: &str,
    new_refresh: &str,
    delay: Duration,
) {
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains(
            format!("refresh_token={old}").as_str(),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(tokens(new_access, new_refresh, "silicon"))
                .set_delay(delay),
        )
        .expect(1)
        .mount(accounts)
        .await;
}

async fn empty_list_for(remind: &MockServer, access: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v2/schedules"))
        .and(header("authorization", format!("Bearer {access}").as_str()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items": [], "next_cursor": null})),
        )
        .mount(remind)
        .await;
}

fn saved(home: &Home, remind: &MockServer) -> Result<serde_json::Value> {
    Ok(home.state()?["sign_ins"][format!("{}#production", remind.uri())].clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_close_to_expiry_is_rotated_once_and_the_new_pair_is_kept() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    home.sign_in(
        &remind.uri(),
        &accounts.uri(),
        None,
        "at-old",
        "sar_old",
        30,
    )?;
    refresh_answers(&accounts, "sar_old", "at-new", "sar_new", Duration::ZERO).await;
    empty_list_for(&remind, "at-new").await;
    for _ in 0..2 {
        let page = ok_json(
            home.remind(Some(&remind.uri()), Some(&accounts.uri()))
                .args(["list", "--json"])
                .output()?,
        )?;
        assert_eq!(page["items"], json!([]));
    }
    let stored = saved(&home, &remind)?;
    assert_eq!(stored["access_token"], "at-new");
    assert_eq!(stored["refresh_token"], "sar_new");
    let refresh = accounts
        .received_requests()
        .await
        .context("requests")?
        .into_iter()
        .find(|r| r.url.path() == "/v1/oauth/token")
        .context("refresh")?;
    let fields = form(&refresh.body);
    assert_eq!(fields["client_id"], "remind");
    assert!(!fields.contains_key("client_secret"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_processes_never_present_the_same_refresh_token() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    home.sign_in(&remind.uri(), &accounts.uri(), None, "at-old", "sar_old", 5)?;
    refresh_answers(
        &accounts,
        "sar_old",
        "at-new",
        "sar_new",
        Duration::from_millis(700),
    )
    .await;
    empty_list_for(&remind, "at-new").await;
    let children: Vec<_> = (0..3)
        .map(|_| {
            home.remind(Some(&remind.uri()), Some(&accounts.uri()))
                .args(["list", "--json"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
        })
        .collect::<Result<_, _>>()?;
    for child in children {
        ok_json(child.wait_with_output()?)?;
    }
    let refreshes = accounts
        .received_requests()
        .await
        .context("requests")?
        .iter()
        .filter(|r| r.url.path() == "/v1/oauth/token")
        .count();
    assert_eq!(
        refreshes, 1,
        "one process refreshed; the others used its result"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_token_is_refreshed_once_and_the_request_retried() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    home.sign_in(
        &remind.uri(),
        &accounts.uri(),
        None,
        "at-old",
        "sar_old",
        1500,
    )?;
    Mock::given(method("GET"))
        .and(path("/api/v2/schedules"))
        .and(header("authorization", "Bearer at-old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": {
            "code": "token_revoked", "message": "This access token was issued before a sign-out."}})))
        .expect(1)
        .mount(&remind)
        .await;
    refresh_answers(&accounts, "sar_old", "at-new", "sar_new", Duration::ZERO).await;
    empty_list_for(&remind, "at-new").await;
    ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["list", "--json"])
            .output()?,
    )?;
    assert_eq!(saved(&home, &remind)?["refresh_token"], "sar_new");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_401_a_refresh_cannot_cure_is_not_retried() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    home.sign_in(
        &remind.uri(),
        &accounts.uri(),
        None,
        "at-old",
        "sar_old",
        1500,
    )?;
    Mock::given(path("/api/v2/schedules"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": {
            "code": "account_deleted", "message": "This account was deleted."}})))
        .expect(1)
        .mount(&remind)
        .await;
    let output = home
        .remind(Some(&remind.uri()), Some(&accounts.uri()))
        .args(["list", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(json_error(&output)?["error"]["code"], "account_deleted");
    assert!(
        accounts
            .received_requests()
            .await
            .context("requests")?
            .is_empty()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ended_sign_in_is_removed_and_reported() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    home.sign_in(
        &remind.uri(),
        &accounts.uri(),
        None,
        "at-old",
        "sar_old",
        10,
    )?;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(oauth_error(
            "invalid_grant",
            "This refresh token was already used once. Presenting a used refresh token revokes the whole sign-in to protect the account, so this sign-in is now revoked; sign in again.",
        )))
        .expect(1)
        .mount(&accounts)
        .await;
    let output = home
        .remind(Some(&remind.uri()), Some(&accounts.uri()))
        .args(["list", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(3));
    let error = json_error(&output)?;
    assert_eq!(error["error"]["code"], "sign_in_ended");
    assert!(
        error["error"]["hint"]
            .as_str()
            .unwrap_or("")
            .contains("remind login")
    );
    assert!(saved(&home, &remind)?.is_null(), "the dead sign-in is gone");
    let status = ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(status, json!({"authenticated": false}));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_is_verified_by_remind_or_reported_unverified_offline_or_when_down() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    home.sign_in(&remind.uri(), &accounts.uri(), None, "at-1", "sar_1", 1500)?;
    Mock::given(path("/api/v2/auth/me"))
        .and(header("authorization", "Bearer at-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(identity("silicon")))
        .up_to_n_times(1)
        .mount(&remind)
        .await;
    let run = |args: &[&str]| {
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(args)
            .output()
    };
    let verified = ok_json(run(&["login", "status", "--json"])?)?;
    for key in [
        "uuid",
        "id",
        "kind",
        "display_name",
        "expires_at",
        "refresh_expires_at",
    ] {
        assert!(
            verified.get(key).is_some(),
            "status lacks {key}: {verified}"
        );
    }
    assert_eq!(verified["authenticated"], true);
    assert_eq!(verified["verified"], true);
    assert_eq!(verified["uuid"], "zQo");
    assert!(verified.get("access_token").is_none() && verified.get("refresh_token").is_none());
    let offline = ok_json(run(&["login", "status", "--offline", "--json"])?)?;
    assert_eq!(offline["verified"], false);
    assert_eq!(offline["id"], "si:scout");
    Mock::given(path("/api/v2/auth/me"))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(
                json!({"error": {"code": "dependency_unavailable", "message": "down"}}),
            ),
        )
        .mount(&remind)
        .await;
    let down = ok_json(run(&["login", "status", "--json"])?)?;
    assert_eq!(down["authenticated"], true);
    assert_eq!(down["verified"], false);
    assert!(down["warning"].is_string());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_a_sign_in_remind_no_longer_accepts() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    home.sign_in(&remind.uri(), &accounts.uri(), None, "at-1", "sar_1", 1500)?;
    Mock::given(path("/api/v2/auth/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": {
            "code": "account_deleted", "message": "This account was deleted."}})))
        .mount(&remind)
        .await;
    let status = ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "account_deleted");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logout_ends_the_sign_in_at_silicon_accounts_and_forgets_it() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    home.sign_in(&remind.uri(), &accounts.uri(), None, "at-1", "sar_1", 1500)?;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked": true})))
        .expect(1)
        .mount(&accounts)
        .await;
    let out = ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["logout", "--json"])
            .output()?,
    )?;
    assert_eq!(
        out,
        json!({"signed_out": true, "uuid": "zQo", "id": "si:scout", "kind": "silicon", "revoked": true})
    );
    let revoke = accounts
        .received_requests()
        .await
        .context("requests")?
        .into_iter()
        .find(|r| r.url.path() == "/v1/oauth/revoke")
        .context("revoke")?;
    let fields = form(&revoke.body);
    assert_eq!(fields["token"], "sar_1");
    assert_eq!(fields["token_type_hint"], "refresh_token");
    assert_eq!(fields["client_id"], "remind");
    assert!(!fields.contains_key("client_secret"));
    assert!(saved(&home, &remind)?.is_null());
    let again = ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["logout", "--json"])
            .output()?,
    )?;
    assert_eq!(
        again,
        json!({"signed_out": false, "reason": "not_signed_in"})
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logout_forgets_locally_even_when_silicon_accounts_is_unreachable() -> Result<()> {
    let remind = MockServer::start().await;
    let home = Home::new()?;
    home.sign_in(
        &remind.uri(),
        "http://127.0.0.1:9",
        None,
        "at-1",
        "sar_1",
        1500,
    )?;
    let output = home
        .remind(Some(&remind.uri()), None)
        .args(["logout", "--json"])
        .output()?;
    let out = ok_json(output.clone())?;
    assert_eq!(out["signed_out"], true);
    assert_eq!(out["revoked"], false);
    assert!(String::from_utf8_lossy(&output.stderr).contains("warning"));
    assert!(saved(&home, &remind)?.is_null());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_environment_uses_the_production_sign_in_unless_it_has_its_own() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    let id = "01992000-0000-7000-8000-000000000004";
    let key = "12345678901234567890123456789012";
    home.sign_in(
        &remind.uri(),
        &accounts.uri(),
        None,
        "at-prod",
        "sar_prod",
        1500,
    )?;
    let mut state = home.state()?;
    state["test_keys"] = json!({format!("{}#{id}", remind.uri()): key});
    home.write_state(&state)?;
    Mock::given(path("/api/v2/schedules"))
        .and(header("authorization", "Bearer at-prod"))
        .and(header("x-remind-test-key", key))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items": [], "next_cursor": null})),
        )
        .expect(1)
        .mount(&remind)
        .await;
    ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["--test", id, "list", "--json"])
            .output()?,
    )?;
    let status = ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["--test", id, "login", "status", "--offline", "--json"])
            .output()?,
    )?;
    assert_eq!(status["uses_production_sign_in"], true);
    assert_eq!(status["test_environment"], id);
    let logout = ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["--test", id, "logout", "--json"])
            .output()?,
    )?;
    assert_eq!(logout["reason"], "no_test_environment_sign_in");
    assert!(
        saved(&home, &remind)?.is_object(),
        "the production sign-in stays"
    );
    let missing_key = home
        .remind(Some(&remind.uri()), Some(&accounts.uri()))
        .args([
            "--test",
            "01992000-0000-7000-8000-000000000009",
            "list",
            "--json",
        ])
        .output()?;
    assert_eq!(missing_key.status.code(), Some(2));
    assert!(
        json_error(&missing_key)?["error"]["hint"]
            .as_str()
            .unwrap_or("")
            .contains("remind env key")
    );
    Ok(())
}
