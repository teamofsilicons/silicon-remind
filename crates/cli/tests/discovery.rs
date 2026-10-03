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
        .env_remove("REMIND_URL")
        .env_remove("REMIND_ORG")
        .env_remove("REMIND_ACCOUNT")
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
        output.stderr.is_empty()
            || String::from_utf8_lossy(&output.stderr).starts_with("Test environment:"),
        "JSON commands may only print the selected environment on stderr"
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}
fn session() -> Value {
    json!({"access_token":"access-fixture", "refresh_token":"refresh-fixture",
        "expires_in":3600, "token_type":"Bearer", "scope":"", "actor":{"type":"silicon","public_id":"si:fixture"}, "org_id":"tos"})
}
fn identity(actor: &str) -> Value {
    json!({"principal_id":"01992000-0000-7000-8000-000000000001", "actor_type":actor,
        "public_id":if actor == "silicon" { "si:fixture" } else { "c:fixture" }, "org_id":"tos", "membership_id":"01992000-0000-7000-8000-000000000002",
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
    let info = json!({"app_id":"custom-remind", "iam_url":"https://iam.example.test", "iam_environment_id":null});
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
        let mut tokens = session();
        tokens["actor"] = json!({"type":actor,"public_id":identity(actor)["public_id"]});
        let home = TempDir::new()?;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .and(wiremock::matchers::body_json(
                json!({"slt":"short-lived-fixture"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(tokens))
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
        assert_eq!(status["public_id"], identity(actor)["public_id"]);
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
        .expect(1)
        .mount(&server)
        .await;
    let status = success(
        cli(home.path())
            .args(["--test", id, "login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(status["authenticated"], true);
    let state: Value = serde_json::from_slice(&fs::read(home.path().join(".remind/state.json"))?)?;
    let stored = &state["sessions"][format!("{}#{id}", server.uri())];
    assert_eq!(stored["session"]["refresh_token"], "successor-refresh");
    assert!(stored["pending_refresh_key"].is_null());
    let production = success(
        cli(home.path())
            .args(["login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(production, json!({"authenticated":false}));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_and_legacy_refresh_replays_are_renewed_before_status() -> Result<()> {
    for started in [Value::Null, json!(1)] {
        let home = TempDir::new()?;
        let server = MockServer::start().await;
        save(home.path(), &server.uri(), true, None)?;
        let state_path = home.path().join(".remind/state.json");
        let key = format!("{}#production", server.uri());
        let mut state: Value = serde_json::from_slice(&fs::read(&state_path)?)?;
        state["sessions"][&key]["pending_refresh_key"] = json!("original-refresh-attempt");
        state["sessions"][&key]["refresh_started_at"] = started;
        fs::write(&state_path, state.to_string())?;
        for (old, new) in [("refresh-fixture", "replayed"), ("replayed", "fresh")] {
            let mut tokens = session();
            tokens["access_token"] = json!(format!("access-{new}"));
            tokens["refresh_token"] = json!(new);
            Mock::given(method("POST"))
                .and(path("/api/v1/auth/refresh"))
                .and(wiremock::matchers::body_json(json!({"refresh_token":old})))
                .respond_with(ResponseTemplate::new(200).set_body_json(tokens))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/api/v1/auth/me"))
            .and(header("authorization", "Bearer access-fresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(identity("silicon")))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            success(
                cli(home.path())
                    .args(["login", "status", "--json"])
                    .output()?
            )?["authenticated"],
            true
        );
        let state: Value = serde_json::from_slice(&fs::read(&state_path)?)?;
        assert_eq!(state["sessions"][&key]["session"]["refresh_token"], "fresh");
        assert!(state["sessions"][&key]["refresh_started_at"].is_null());
    }
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
            if !expired && code == 401 {
                Mock::given(path("/api/v1/auth/refresh"))
                    .respond_with(ResponseTemplate::new(401).set_body_json(
                        json!({"error":{"code":"invalid_token", "message":"family revoked"}}),
                    ))
                    .expect(1)
                    .mount(&server)
                    .await;
            }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_rejection_refreshes_once_and_persists_for_the_next_process() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    save(home.path(), &server.uri(), false, None)?;
    Mock::given(path("/api/v1/auth/me"))
        .and(header("authorization", "Bearer access-fixture"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(
                json!({"error":{"code":"invalid_token", "message":"access inactive"}}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut refreshed = session();
    refreshed["access_token"] = json!("successor-access");
    refreshed["refresh_token"] = json!("successor-refresh");
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/refresh"))
        .and(wiremock::matchers::body_json(
            json!({"refresh_token":"refresh-fixture"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(refreshed))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/auth/me"))
        .and(header("authorization", "Bearer successor-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(identity("silicon")))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/schedules"))
        .and(header("authorization", "Bearer successor-access"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[], "next_cursor":null})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = success(cli(home.path()).args(["list", "--json"]).output()?)?;
    assert_eq!(result["items"], json!([]));
    let status = success(
        cli(home.path())
            .args(["login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(status["authenticated"], true);
    let state: Value = serde_json::from_slice(&fs::read(home.path().join(".remind/state.json"))?)?;
    let stored = &state["sessions"][format!("{}#production", server.uri())];
    assert_eq!(stored["session"]["refresh_token"], "successor-refresh");
    assert!(stored["pending_refresh_key"].is_null());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_sandbox_footer_survives_errors_and_exit_preserves_production() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    let id = "01992000-0000-7000-8000-000000000004";
    save(home.path(), &server.uri(), false, None)?;
    let file = home.path().join(".remind/state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&file)?)?;
    state["selected_tests"] = json!({server.uri():id});
    state["test_names"] = json!({format!("{}#{id}",server.uri()):"Isolated test"});
    state["test_keys"][format!("{}#{id}", server.uri())] = json!(format!("ask_{}", "t".repeat(43)));
    fs::write(&file, serde_json::to_vec(&state)?)?;
    let failed = cli(home.path()).args(["list", "--json"]).output()?;
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(
        String::from_utf8(failed.stderr)?
            .lines()
            .last()
            .is_some_and(|line| line.contains("Test environment: Isolated test"))
    );
    let help = cli(home.path()).args(["create", "--help"]).output()?;
    assert!(help.status.success());
    assert!(String::from_utf8(help.stderr)?.contains("Test environment: Isolated test"));
    let exit = cli(home.path()).args(["env", "exit", "--json"]).output()?;
    assert_eq!(success(exit)?["environment"], "production");
    let after: Value = serde_json::from_slice(&fs::read(file)?)?;
    let production_slot = format!("{}#production", server.uri());
    assert_eq!(
        after["sessions"][&production_slot]["session"],
        state["sessions"][&production_slot]["session"]
    );
    assert_eq!(
        after["sessions"][&production_slot]["org"],
        state["sessions"][&production_slot]["org"]
    );
    assert!(
        after["selected_tests"]
            .as_object()
            .is_some_and(|m| m.is_empty())
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .context("request log")?
            .len(),
        0
    );
    Ok(())
}

#[test]
fn updates_are_honeycomb_managed_and_parse_errors_keep_the_test_footer() -> Result<()> {
    let home = TempDir::new()?;
    let result = success(cli(home.path()).args(["update", "--json"]).output()?)?;
    assert_eq!(result["status"], "managed");
    assert_eq!(result["command"], "honeycomb update 'remind'");
    let result = cli(home.path()).args(["daemon", "install"]).output()?;
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Honeycomb manages"));
    let id = "01992000-0000-7000-8000-000000000011";
    let result = cli(home.path())
        .args(["--test", id, "not-a-command"])
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .lines()
            .last()
            .is_some_and(|line| line.contains(id))
    );
    Ok(())
}

/// An organization header cannot turn retired unscoped credentials into an IAM5 session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unscoped_login_requires_reauthentication_even_with_org_override() -> Result<()> {
    for explicit_org in [false, true] {
        let home = TempDir::new()?;
        let server = MockServer::start().await;
        let mut tokens = session();
        tokens["org_id"] = Value::Null;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tokens))
            .expect(1)
            .mount(&server)
            .await;
        let mut command = cli(home.path());
        command.args(["--url", &server.uri(), "login", "slt-fixture", "--json"]);
        if explicit_org {
            command.args(["--org", "tos"]);
        }
        let output = command.output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Sign in again"));
        assert_eq!(
            server.received_requests().await.context("requests")?.len(),
            1
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn separate_account_org_sessions_survive_later_logins() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    for (name, kind, org) in [("first", "silicon", "tos"), ("second", "carbon", "bricks")] {
        let mut tokens = session();
        let mut actor = identity(kind);
        actor["org_id"] = json!(org);
        tokens["actor"] = json!({"type":kind,"public_id":actor["public_id"]});
        tokens["org_id"] = json!(org);
        tokens["access_token"] = json!(format!("access-{name}"));
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .and(wiremock::matchers::body_json(json!({"slt":name})))
            .respond_with(ResponseTemplate::new(200).set_body_json(tokens))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/auth/me"))
            .and(header("authorization", format!("Bearer access-{name}")))
            .and(header("x-org-id", org))
            .respond_with(ResponseTemplate::new(200).set_body_json(actor))
            .mount(&server)
            .await;
        success(
            cli(home.path())
                .args(["--url", &server.uri(), "login", name, "--json"])
                .output()?,
        )?;
    }
    let contexts = success(
        cli(home.path())
            .args(["--url", &server.uri(), "auth", "contexts", "--json"])
            .output()?,
    )?;
    assert_eq!(contexts["items"].as_array().context("items")?.len(), 2);
    assert!(!contexts.to_string().contains("access-first"));
    let first = success(
        cli(home.path())
            .args([
                "--url",
                &server.uri(),
                "--account",
                "si:fixture",
                "--org",
                "tos",
                "login",
                "status",
                "--json",
            ])
            .output()?,
    )?;
    assert_eq!(first["public_id"], "si:fixture");
    assert_eq!(first["org_id"], "tos");
    let wrong = success(
        cli(home.path())
            .args([
                "--url",
                &server.uri(),
                "--account",
                "si:fixture",
                "--org",
                "bricks",
                "login",
                "status",
                "--json",
            ])
            .output()?,
    )?;
    assert_eq!(wrong, json!({"authenticated":false}));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_cannot_move_a_saved_context_and_retains_retry_identity() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    save(home.path(), &server.uri(), true, None)?;
    let mut tokens = session();
    tokens["org_id"] = json!("bricks");
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens))
        .expect(1)
        .mount(&server)
        .await;
    let output = cli(home.path())
        .args(["login", "status", "--json"])
        .output()?;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("changed the selected account or organization")
    );
    let state: Value = serde_json::from_slice(&fs::read(home.path().join(".remind/state.json"))?)?;
    let stored = &state["sessions"][format!("{}#production", server.uri())];
    assert_eq!(stored["org"], "tos");
    assert_eq!(stored["session"]["org_id"], "tos");
    assert!(stored["pending_refresh_key"].is_string());
    let requests = server.received_requests().await.context("requests")?;
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/api/v1/auth/refresh")
            .count(),
        1
    );
    assert!(!requests.iter().any(|r| r.url.path() == "/api/v1/auth/me"));
    assert!(
        requests
            .iter()
            .all(|r| r.headers.get("x-org-id").is_none_or(|org| org != "bricks"))
    );
    Ok(())
}
