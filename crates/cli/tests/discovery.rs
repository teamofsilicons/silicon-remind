//! Exercise the installed command grammar, isolated state, and live auth checks.
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

fn cli(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_remind"));
    command
        .env("HOME", home)
        .env_remove("SILICON_HOME")
        .env_remove("SILICON_REMIND_TEST")
        .env_remove("REMIND_URL")
        .env_remove("REMIND_ORG")
        .arg("--no-update");
    command
}
fn success(output: Output) -> Result<Value> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "JSON commands must not print suggestions"
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}
fn session() -> Value {
    json!({"access_token":"access-fixture", "refresh_token":"refresh-fixture",
        "expires_in":3600, "token_type":"Bearer", "scope":"", "actor":{}, "org_id":"tos"})
}
fn identity(actor: &str) -> Value {
    json!({"principal_id":"01992000-0000-7000-8000-000000000001", "actor_type":actor,
        "public_id":"fixture:tos", "org_id":"tos", "membership_id":"01992000-0000-7000-8000-000000000002",
        "org_role":"member", "authorization_epoch":1, "can_manage_reminders":actor == "silicon"})
}
fn save(home: &Path, url: &str, expired: bool, test: Option<&str>) -> Result<()> {
    fs::create_dir_all(home.join(".remind"))?;
    let key = format!("{url}#{}", test.unwrap_or("production"));
    let mut sessions = serde_json::Map::new();
    sessions.insert(
        key.clone(),
        json!({"session":session(), "org":"tos",
        "expires_at": if expired { 0 } else { chrono::Utc::now().timestamp() + 3600 }}),
    );
    let mut test_keys = serde_json::Map::new();
    if test.is_some() {
        test_keys.insert(key, json!("12345678901234567890123456789012"));
    }
    fs::write(
        home.join(".remind/state.json"),
        serde_json::to_vec(&json!({
            "url":url, "auto_update":false, "last_update_check":0, "sessions":sessions, "test_keys":test_keys
        }))?,
    )?;
    Ok(())
}

#[test]
fn help_and_home_selection() -> Result<()> {
    let home = TempDir::new()?;
    let silicon = TempDir::new()?;
    let configured = TempDir::new()?;
    let help = cli(home.path()).arg("--help").output()?;
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout)?;
    assert!(help.contains("iam") && help.contains("login"));
    assert!(!home.path().join(".remind").exists());
    assert!(!cli(home.path()).arg("login").output()?.status.success());
    let normal = success(
        cli(home.path())
            .args(["config", "show", "--json"])
            .output()?,
    )?;
    assert_eq!(normal["home"], home.path().to_string_lossy().as_ref());
    let alternate = success(
        cli(home.path())
            .env("SILICON_HOME", silicon.path())
            .args(["config", "show", "--json"])
            .output()?,
    )?;
    assert_eq!(alternate["home"], silicon.path().to_string_lossy().as_ref());
    success(
        cli(home.path())
            .env("SILICON_HOME", silicon.path())
            .args(["config", "home"])
            .arg(configured.path())
            .arg("--json")
            .output()?,
    )?;
    let selected = success(
        cli(home.path())
            .env("SILICON_HOME", silicon.path())
            .args(["config", "show", "--json"])
            .output()?,
    )?;
    assert_eq!(
        selected["home"],
        fs::canonicalize(configured.path())?
            .to_string_lossy()
            .as_ref()
    );
    let normal = success(
        cli(home.path())
            .args(["config", "show", "--json"])
            .output()?,
    )?;
    assert_eq!(normal["home"], home.path().to_string_lossy().as_ref());
    assert!(
        !cli(home.path())
            .env("SILICON_HOME", "")
            .args(["config", "show"])
            .output()?
            .status
            .success()
    );
    assert!(
        !cli(home.path())
            .args(["config", "home"])
            .arg(home.path().join("missing"))
            .output()?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn missing_session_is_machine_readable_without_contacting_server() -> Result<()> {
    let home = TempDir::new()?;
    let result = success(
        cli(home.path())
            .args(["--url", "http://127.0.0.1:1", "login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(result, json!({"authenticated":false}));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn iam_discovers_server_configuration_without_credentials() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    let info = json!({"app_id":"custom>remind", "iam_url":"https://iam.example.test", "iam_environment_id":null});
    Mock::given(method("GET"))
        .and(path("/api/v1/auth/iam"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&info))
        .expect(1)
        .mount(&server)
        .await;
    let result = success(
        cli(home.path())
            .args(["--url", &server.uri(), "iam", "--json"])
            .output()?,
    )?;
    assert_eq!(result, info);
    let requests = server.received_requests().await.context("requests")?;
    assert!(!requests[0].headers.contains_key("authorization"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_status_verifies_both_actor_types_and_preserves_direct_login() -> Result<()> {
    for actor in ["carbon", "silicon"] {
        let home = TempDir::new()?;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .and(wiremock::matchers::body_json(
                json!({"slt":"short-lived-fixture"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(session()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/auth/me"))
            .and(header("authorization", "Bearer access-fixture"))
            .and(header("x-org-id", "tos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(identity(actor)))
            .expect(2)
            .mount(&server)
            .await;
        success(
            cli(home.path())
                .args([
                    "--url",
                    &server.uri(),
                    "login",
                    "short-lived-fixture",
                    "--json",
                ])
                .output()?,
        )?;
        let status = success(
            cli(home.path())
                .args(["--url", &server.uri(), "login", "status", "--json"])
                .output()?,
        )?;
        assert_eq!(status["authenticated"], true);
        assert_eq!(status["actor_type"], actor);
        assert_eq!(status["public_id"], "fixture:tos");
        assert!(status.get("access_token").is_none() && status.get("refresh_token").is_none());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_refreshes_expired_sandbox_session_in_the_same_context() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    let id = "01992000-0000-7000-8000-000000000003";
    save(home.path(), &server.uri(), true, Some(id))?;
    let mut refreshed = session();
    refreshed["access_token"] = json!("successor-access");
    refreshed["refresh_token"] = json!("successor-refresh");
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/refresh"))
        .and(header(
            "x-remind-test-key",
            "12345678901234567890123456789012",
        ))
        .and(wiremock::matchers::body_json(
            json!({"refresh_token":"refresh-fixture"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(refreshed))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/auth/me"))
        .and(header(
            "x-remind-test-key",
            "12345678901234567890123456789012",
        ))
        .and(header("authorization", "Bearer successor-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(identity("silicon")))
        .expect(2)
        .mount(&server)
        .await;
    let status = success(
        cli(home.path())
            .env("SILICON_REMIND_TEST", id)
            .args(["login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(status["authenticated"], true);
    let state: Value = serde_json::from_slice(&fs::read(home.path().join(".remind/state.json"))?)?;
    let stored = &state["sessions"][format!("{}#{id}", server.uri())];
    assert_eq!(stored["session"]["refresh_token"], "successor-refresh");
    assert!(stored["pending_refresh_key"].is_null());
    let explicit = success(
        cli(home.path())
            .env(
                "SILICON_REMIND_TEST",
                "01992000-0000-7000-8000-000000000004",
            )
            .args(["--test", id, "login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(explicit["authenticated"], true);
    let production = success(
        cli(home.path())
            .args(["login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(production, json!({"authenticated":false}));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_distinguishes_rejected_authority_from_service_failures() -> Result<()> {
    for expired in [false, true] {
        for code in [401, 403, 503] {
            let home = TempDir::new()?;
            let server = MockServer::start().await;
            save(home.path(), &server.uri(), expired, None)?;
            Mock::given(path(if expired {
                "/api/v1/auth/refresh"
            } else {
                "/api/v1/auth/me"
            }))
            .respond_with(
                ResponseTemplate::new(code)
                    .set_body_json(json!({"error":{"code":"fixture_error", "message":"rejected"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
            let result = cli(home.path())
                .args(["login", "status", "--json"])
                .output()?;
            if code == 401 {
                assert_eq!(success(result)?, json!({"authenticated":false}));
            } else {
                assert!(!result.status.success());
                assert!(result.stdout.is_empty());
                let error: Value = serde_json::from_slice(&result.stderr)?;
                assert_eq!(error["error"]["status"], code);
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn bundled_native_binary_keeps_the_public_command_name() -> Result<()> {
    let dir = TempDir::new()?;
    let executable = dir.path().join("remind-native");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_remind"), &executable)?;
    let output = Command::new(executable).arg("--help").output()?;
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout)?;
    assert!(help.contains("Usage: remind [OPTIONS]"));
    assert!(!help.contains("remind-native"));
    Ok(())
}
