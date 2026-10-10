//! `remind login` against a stub Silicon Accounts and a stub Remind: the device sign-in
//! (pending, slow_down, approved; denied; expired) and short-lived token exchange (ok, and
//! every refusal), always as Remind's public client.
mod common;

use anyhow::{Context as _, Result};
use common::{Home, form, identity, json_error, oauth_error, ok_json, tokens};
use serde_json::{Value, json};
use std::io::Write as _;
use std::process::Stdio;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

async fn servers() -> (MockServer, MockServer) {
    (MockServer::start().await, MockServer::start().await)
}

async fn remind_accepts(remind: &MockServer, access: &str, kind: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v2/auth/me"))
        .and(header("authorization", format!("Bearer {access}").as_str()))
        .and(header("x-remind-api-version", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(identity(kind)))
        .mount(remind)
        .await;
}

fn device_start(accounts: &MockServer, expires_in: u64) -> Value {
    json!({"device_code": "sad_secret_device_code", "user_code": "WDJB-MJHT",
        "verification_uri": format!("{}/device", accounts.uri()),
        "verification_uri_complete": format!("{}/device?code=WDJB-MJHT", accounts.uri()),
        "expires_in": expires_in, "interval": 1})
}

fn token_requests(
    requests: &[Request],
    grant: &str,
) -> Vec<std::collections::HashMap<String, String>> {
    requests
        .iter()
        .filter(|r| r.url.path() == "/v1/oauth/token")
        .map(|r| form(&r.body))
        .filter(|f| f.get("grant_type").map(String::as_str) == Some(grant))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_carbon_device_sign_in_waits_through_pending_and_slow_down() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    Mock::given(method("POST"))
        .and(path("/v1/device/authorize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(device_start(&accounts, 600)))
        .expect(1)
        .mount(&accounts)
        .await;
    let device_grant = "urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code";
    for answer in ["authorization_pending", "slow_down"] {
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_string_contains(device_grant))
            .respond_with(ResponseTemplate::new(400).set_body_json(oauth_error(answer, answer)))
            .up_to_n_times(1)
            .mount(&accounts)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains(device_grant))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens(
            "at-carbon",
            "sar_carbon",
            "carbon",
        )))
        .mount(&accounts)
        .await;
    remind_accepts(&remind, "at-carbon", "carbon").await;
    let started = std::time::Instant::now();
    let output = home
        .remind(Some(&remind.uri()), Some(&accounts.uri()))
        .args(["login", "--json", "--label", "test laptop"])
        .output()?;
    let stderr = String::from_utf8(output.stderr.clone())?;
    let status = ok_json(output)?;
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["kind"], "carbon");
    assert_eq!(status["id"], "c:ada");
    assert_eq!(status["uuid"], "aB3");
    assert_eq!(status["verified"], true);
    assert_eq!(status["method"], "device");
    let events: Vec<Value> = stderr
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let code = events
        .iter()
        .find(|e| e["event"] == "device_code")
        .context("device_code event")?;
    assert_eq!(code["user_code"], "WDJB-MJHT");
    assert_eq!(
        code["verification_uri"],
        format!("{}/device", accounts.uri())
    );
    assert_eq!(code["browser_opened"], false);
    let slow = events
        .iter()
        .find(|e| e["event"] == "slow_down")
        .context("slow_down event")?;
    assert_eq!(slow["interval"], 6, "slow_down adds 5 seconds");
    assert!(
        started.elapsed().as_secs() >= 7,
        "the poll honoured interval and slow_down"
    );
    assert!(
        !stderr.contains("sad_secret_device_code"),
        "the device code is never shown"
    );
    let requests = accounts.received_requests().await.context("requests")?;
    let authorize = requests
        .iter()
        .find(|r| r.url.path() == "/v1/device/authorize")
        .context("authorize")?;
    let body: Value = serde_json::from_slice(&authorize.body)?;
    assert_eq!(body["client_id"], "remind");
    assert_eq!(body["client_label"], "test laptop");
    let polls = token_requests(&requests, "urn:ietf:params:oauth:grant-type:device_code");
    assert_eq!(polls.len(), 3);
    assert!(
        polls
            .iter()
            .all(|f| f["client_id"] == "remind" && !f.contains_key("client_secret"))
    );
    let state = home.state()?;
    let saved = &state["sign_ins"][format!("{}#production", remind.uri())];
    assert_eq!(saved["refresh_token"], "sar_carbon");
    assert_eq!(saved["accounts_url"], accounts.uri());
    let again = ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["login", "--json"])
            .output()?,
    )?;
    assert_eq!(
        again["already_signed_in"], true,
        "a second login without --force changes nothing"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_denied_or_expired_device_sign_in_fails_with_its_reason() -> Result<()> {
    for (answer, code) in [
        ("access_denied", "device_denied"),
        ("expired_token", "device_expired"),
    ] {
        let (accounts, remind) = servers().await;
        let home = Home::new()?;
        Mock::given(method("POST"))
            .and(path("/v1/device/authorize"))
            .respond_with(ResponseTemplate::new(200).set_body_json(device_start(&accounts, 600)))
            .mount(&accounts)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(oauth_error(answer, answer)))
            .mount(&accounts)
            .await;
        let output = home
            .remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["login", "--json"])
            .output()?;
        assert_eq!(output.status.code(), Some(3), "{answer}");
        let error = json_error(&output)?;
        assert_eq!(error["error"]["code"], code);
        assert!(
            error["error"]["hint"]
                .as_str()
                .unwrap_or("")
                .contains("remind login")
        );
        assert!(!home.state_path().exists(), "nothing saved after {answer}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_code_that_runs_out_locally_stops_waiting() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    Mock::given(method("POST"))
        .and(path("/v1/device/authorize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(device_start(&accounts, 2)))
        .mount(&accounts)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(oauth_error("authorization_pending", "pending")),
        )
        .mount(&accounts)
        .await;
    let output = home
        .remind(Some(&remind.uri()), Some(&accounts.uri()))
        .args(["login", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(json_error(&output)?["error"]["code"], "device_expired");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_sign_in_turned_off_says_how_silicons_sign_in() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    Mock::given(method("POST"))
        .and(path("/v1/device/authorize"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "code": "unauthorized_client",
            "message": "The app 'remind' hasn't turned on device sign-ins, so its tools can't start one."}})))
        .mount(&accounts)
        .await;
    let output = home
        .remind(Some(&remind.uri()), Some(&accounts.uri()))
        .args(["login", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(3));
    let error = json_error(&output)?;
    assert_eq!(error["error"]["code"], "sign_in_not_allowed");
    assert!(
        error["error"]["hint"]
            .as_str()
            .unwrap_or("")
            .contains("silicon-accounts login --app remind -q")
    );
    Ok(())
}

async fn slt_server(accounts: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_string_contains("grant-type%3Aslt"))
        .respond_with(response)
        .mount(accounts)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silicon_signs_in_with_a_short_lived_token_in_all_three_forms() -> Result<()> {
    for form_name in ["stdin", "flag", "positional"] {
        let (accounts, remind) = servers().await;
        let home = Home::new()?;
        slt_server(
            &accounts,
            ResponseTemplate::new(200).set_body_json(tokens("at-1", "sar_1", "silicon")),
        )
        .await;
        remind_accepts(&remind, "at-1", "silicon").await;
        let mut command = home.remind(Some(&remind.uri()), Some(&accounts.uri()));
        command.arg("login");
        match form_name {
            "stdin" => command
                .args(["--slt-stdin", "--json"])
                .stdin(Stdio::piped()),
            "flag" => command.args(["--slt", "slt_secret_token_value", "--json"]),
            _ => command.args(["slt_secret_token_value", "--json"]),
        };
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if form_name == "stdin" {
            child
                .stdin
                .take()
                .context("stdin")?
                .write_all(b"slt_secret_token_value\n")?;
        }
        let output = child.wait_with_output()?;
        let stdout = String::from_utf8(output.stdout.clone())?;
        let stderr = String::from_utf8(output.stderr.clone())?;
        let status = ok_json(output)?;
        assert_eq!(status["authenticated"], true, "{form_name}");
        assert_eq!(status["kind"], "silicon");
        assert_eq!(status["id"], "si:scout");
        assert_eq!(status["custodian"]["id"], "c:ada");
        assert_eq!(status["can_manage_reminders"], true);
        assert_eq!(status["method"], "slt");
        let requests = accounts.received_requests().await.context("requests")?;
        let exchange = token_requests(&requests, "urn:silicon:params:oauth:grant-type:slt");
        assert_eq!(exchange.len(), 1);
        assert_eq!(exchange[0]["slt"], "slt_secret_token_value");
        assert_eq!(exchange[0]["client_id"], "remind");
        assert!(!exchange[0].contains_key("client_secret"));
        let saved = std::fs::read_to_string(home.state_path())?;
        for text in [&stdout, &stderr, &saved] {
            assert!(
                !text.contains("slt_secret_token_value"),
                "the short-lived token is never echoed or stored"
            );
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_refused_short_lived_token_is_explained() -> Result<()> {
    let cases = [
        (
            "The short-lived token was already used; each one works once. Get a new one.",
            "slt_already_used",
            "works once",
        ),
        (
            "The short-lived token expired at 2026-10-10T00:00:00.000Z (they last 120 seconds); get a new one with `silicon-accounts login --app remind`.",
            "slt_expired",
            "2 minutes",
        ),
        (
            "The short-lived token was issued for the app 'briefcase', not for 'remind'; get one for 'remind' with `silicon-accounts login --app remind`.",
            "slt_wrong_app",
            "'briefcase'",
        ),
        (
            "The short-lived token is not known: it is mistyped or was never issued.",
            "slt_unknown",
            "passed whole",
        ),
    ];
    for (description, code, hint_part) in cases {
        let (accounts, remind) = servers().await;
        let home = Home::new()?;
        slt_server(
            &accounts,
            ResponseTemplate::new(400).set_body_json(oauth_error("invalid_grant", description)),
        )
        .await;
        let output = home
            .remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["login", "slt_refused", "--json"])
            .output()?;
        assert_eq!(output.status.code(), Some(3), "{code}");
        let error = json_error(&output)?;
        assert_eq!(error["error"]["code"], code);
        assert_eq!(error["error"]["message"], description);
        let hint = error["error"]["hint"].as_str().unwrap_or("");
        assert!(
            hint.contains("silicon-accounts login --app remind -q"),
            "{hint}"
        );
        assert!(hint.contains(hint_part), "{hint}");
        assert!(!home.state_path().exists());
        assert!(
            remind
                .received_requests()
                .await
                .context("requests")?
                .is_empty()
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn something_that_is_not_a_short_lived_token_is_never_sent() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    let output = home
        .remind(Some(&remind.uri()), Some(&accounts.uri()))
        .args(["login", "oac_previous_identity_token", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(3));
    let error = json_error(&output)?;
    assert_eq!(error["error"]["code"], "not_a_short_lived_token");
    assert!(
        accounts
            .received_requests()
            .await
            .context("requests")?
            .is_empty()
    );
    let bad_url = home
        .remind(Some("http://remind.example"), Some(&accounts.uri()))
        .args(["login", "slt_kept_unused", "--json"])
        .output()?;
    assert_eq!(
        bad_url.status.code(),
        Some(2),
        "a bad origin is refused before the token is used"
    );
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
async fn a_token_remind_refuses_is_not_kept_and_its_sign_in_is_ended() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    slt_server(
        &accounts,
        ResponseTemplate::new(200).set_body_json(tokens("at-1", "sar_1", "silicon")),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked": true})))
        .expect(1)
        .mount(&accounts)
        .await;
    Mock::given(path("/api/v2/auth/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": {
            "code": "unauthenticated", "message": "The token was issued by another Silicon Accounts."}})))
        .mount(&remind)
        .await;
    let output = home
        .remind(Some(&remind.uri()), Some(&accounts.uri()))
        .args(["login", "slt_ok", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        json_error(&output)?["error"]["code"],
        "token_refused_by_remind"
    );
    assert!(!home.state_path().exists());
    let revoke = accounts
        .received_requests()
        .await
        .context("requests")?
        .into_iter()
        .find(|r| r.url.path() == "/v1/oauth/revoke")
        .context("revoke")?;
    assert_eq!(form(&revoke.body)["token"], "sar_1");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_token_sign_in_replaces_and_ends_the_previous_one() -> Result<()> {
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
    slt_server(
        &accounts,
        ResponseTemplate::new(200).set_body_json(tokens("at-new", "sar_new", "silicon")),
    )
    .await;
    remind_accepts(&remind, "at-new", "silicon").await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"revoked": true})))
        .expect(1)
        .mount(&accounts)
        .await;
    ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["login", "slt_new", "--json"])
            .output()?,
    )?;
    let saved = &home.state()?["sign_ins"][format!("{}#production", remind.uri())];
    assert_eq!(saved["refresh_token"], "sar_new");
    let revoke = accounts
        .received_requests()
        .await
        .context("requests")?
        .into_iter()
        .find(|r| r.url.path() == "/v1/oauth/revoke")
        .context("revoke")?;
    let fields = form(&revoke.body);
    assert_eq!(fields["token"], "sar_old");
    assert_eq!(fields["client_id"], "remind");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signing_in_while_remind_is_down_keeps_the_sign_in_unverified() -> Result<()> {
    let (accounts, remind) = servers().await;
    let home = Home::new()?;
    slt_server(
        &accounts,
        ResponseTemplate::new(200).set_body_json(tokens("at-1", "sar_1", "silicon")),
    )
    .await;
    Mock::given(path("/api/v2/auth/me"))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(
                json!({"error": {"code": "dependency_unavailable", "message": "down"}}),
            ),
        )
        .mount(&remind)
        .await;
    let status = ok_json(
        home.remind(Some(&remind.uri()), Some(&accounts.uri()))
            .args(["login", "slt_ok", "--json"])
            .output()?,
    )?;
    assert_eq!(status["authenticated"], true);
    assert_eq!(status["verified"], false);
    assert!(status["warning"].is_string());
    assert!(home.state_path().exists());
    Ok(())
}
