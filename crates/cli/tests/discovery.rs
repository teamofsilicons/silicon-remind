//! The Silicon Apps contract (help, `accounts --json`, `login status --json` signed out, in an
//! empty home), home selection, old and broken state files, and retired commands.
mod common;

use anyhow::Result;
use common::{Home, json_error, ok_json};
use serde_json::{Value, json};

fn golden_accounts(api_url: &str, accounts_url: &str) -> Value {
    json!({
        "app_id": "remind",
        "client_id": "remind",
        "accounts_url": accounts_url,
        "api_url": api_url,
        "version": env!("CARGO_PKG_VERSION"),
        "api_version": 2,
        "sign_in": {
            "carbon": "remind login",
            "silicon": "silicon-accounts login --app remind -q | remind login --slt-stdin",
            "status": "remind login status --json"
        },
        "docs_url": "https://docs.remind.teamofsilicons.com"
    })
}

#[test]
fn the_three_discovery_commands_work_in_an_empty_home_and_write_nothing() -> Result<()> {
    let home = Home::new()?;
    let silicon_home = Home::new()?;
    let run = |args: &[&str]| {
        home.remind(None, None)
            .env("SILICON_HOME", &silicon_home.path)
            .args(args)
            .output()
    };
    let help = run(&["--help"])?;
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout)?;
    assert!(help.contains("remind login") && help.contains("accounts --json"));
    for retired in [
        "iam",
        "IAM",
        "Honeycomb",
        "honeycomb",
        "organization",
        "--org",
    ] {
        assert!(!help.contains(retired), "help mentions {retired}");
    }
    assert_eq!(
        ok_json(run(&["accounts", "--json"])?)?,
        golden_accounts(
            "https://backend.remind.teamofsilicons.com",
            "https://accounts.teamofsilicons.com"
        )
    );
    let status = run(&["login", "status", "--json"])?;
    assert!(
        status.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(ok_json(status)?, json!({"authenticated": false}));
    let text = run(&["login", "status"])?;
    assert_eq!(
        text.status.code(),
        Some(1),
        "signed out without --json exits 1"
    );
    assert!(String::from_utf8(text.stdout)?.contains("Not signed in"));
    assert!(!home.path.join(".remind").exists());
    assert!(!silicon_home.path.join(".remind").exists());
    Ok(())
}

#[test]
fn accounts_follows_flags_environment_and_saved_settings() -> Result<()> {
    let home = Home::new()?;
    let from_env = ok_json(
        home.remind(Some("http://127.0.0.1:4181"), Some("http://localhost:9590"))
            .args(["accounts", "--json"])
            .output()?,
    )?;
    assert_eq!(
        from_env,
        golden_accounts("http://127.0.0.1:4181", "http://localhost:9590")
    );
    ok_json(
        home.remind(None, None)
            .args(["config", "set-url", "http://localhost:4181/", "--json"])
            .output()?,
    )?;
    ok_json(
        home.remind(None, None)
            .args([
                "config",
                "set-accounts-url",
                "http://127.0.0.1:9590",
                "--json",
            ])
            .output()?,
    )?;
    let saved = ok_json(
        home.remind(None, None)
            .args(["accounts", "--json"])
            .output()?,
    )?;
    assert_eq!(saved["api_url"], "http://localhost:4181");
    assert_eq!(saved["accounts_url"], "http://127.0.0.1:9590");
    let flag = ok_json(
        home.remind(None, None)
            .args(["--url", "https://remind.example", "accounts", "--json"])
            .output()?,
    )?;
    assert_eq!(flag["api_url"], "https://remind.example");
    let custom = ok_json(
        home.remind(None, None)
            .env("REMIND_APP_ID", "remind-dev")
            .args(["accounts", "--json"])
            .output()?,
    )?;
    assert_eq!(custom["app_id"], "remind-dev");
    assert_eq!(
        custom["sign_in"]["silicon"],
        "silicon-accounts login --app remind-dev -q | remind login --slt-stdin"
    );
    Ok(())
}

#[test]
fn hidden_iam_prints_exactly_the_accounts_object() -> Result<()> {
    let home = Home::new()?;
    let accounts = ok_json(
        home.remind(None, None)
            .args(["accounts", "--json"])
            .output()?,
    )?;
    let iam = ok_json(home.remind(None, None).args(["iam", "--json"]).output()?)?;
    assert_eq!(iam, accounts);
    Ok(())
}

#[test]
fn discovery_still_exits_zero_without_a_usable_home() -> Result<()> {
    let home = Home::new()?;
    let accounts = home
        .remind(None, None)
        .env("SILICON_HOME", "")
        .args(["accounts", "--json"])
        .output()?;
    assert_eq!(ok_json(accounts)?["app_id"], "remind");
    let status = ok_json(
        home.remind(None, None)
            .env("SILICON_HOME", "")
            .args(["login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "home_unavailable");
    let list = home
        .remind(None, None)
        .env("SILICON_HOME", "")
        .args(["list", "--json"])
        .output()?;
    assert_eq!(list.status.code(), Some(2));
    assert_eq!(json_error(&list)?["error"]["code"], "home_unavailable");
    Ok(())
}

#[test]
fn home_selection_follows_silicon_home_and_the_saved_pointer() -> Result<()> {
    let home = Home::new()?;
    let silicon = Home::new()?;
    let configured = Home::new()?;
    let show = |silicon_home: Option<&std::path::Path>| -> Result<Value> {
        let mut command = home.remind(None, None);
        if let Some(path) = silicon_home {
            command.env("SILICON_HOME", path);
        }
        ok_json(command.args(["config", "show", "--json"]).output()?)
    };
    assert_eq!(show(None)?["home"], home.path.to_string_lossy().as_ref());
    assert_eq!(
        show(Some(&silicon.path))?["home"],
        silicon.path.to_string_lossy().as_ref()
    );
    ok_json(
        home.remind(None, None)
            .env("SILICON_HOME", &silicon.path)
            .args(["config", "home"])
            .arg(&configured.path)
            .arg("--json")
            .output()?,
    )?;
    let canonical = std::fs::canonicalize(&configured.path)?;
    assert_eq!(
        show(Some(&silicon.path))?["home"],
        canonical.to_string_lossy().as_ref()
    );
    assert_eq!(show(None)?["home"], home.path.to_string_lossy().as_ref());
    let missing = home
        .remind(None, None)
        .args(["config", "home"])
        .arg(home.path.join("missing"))
        .output()?;
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("not a directory"));
    Ok(())
}

#[test]
fn a_state_file_from_the_previous_release_asks_to_sign_in_again() -> Result<()> {
    let home = Home::new()?;
    home.write_state(&json!({
        "url": "https://backend.remind.teamofsilicons.com", "auto_update": false, "telemetry": true,
        "last_update_check": 0,
        "sessions": {"https://backend.remind.teamofsilicons.com#production#silicon:si:scout@tos": {
            "session": {"access_token": "oat_old", "refresh_token": "ort_old", "expires_in": 3600,
                "token_type": "Bearer", "scope": "", "actor": {"type": "silicon", "public_id": "si:scout"},
                "org_id": "tos"}, "org": "tos", "expires_at": 1}},
        "selected_sessions": {}, "test_keys": {}
    }))?;
    let status = ok_json(
        home.remind(None, None)
            .args(["login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "sign_in_again");
    let list = home.remind(None, None).args(["list", "--json"]).output()?;
    assert_eq!(list.status.code(), Some(3));
    let error = json_error(&list)?;
    assert_eq!(error["error"]["code"], "not_signed_in");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("earlier Remind")
    );
    let changed = home
        .remind(None, None)
        .args(["config", "telemetry", "off", "--json"])
        .output()?;
    assert!(changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("archived"));
    assert!(
        home.files()
            .iter()
            .any(|name| name.starts_with("state.iam-")),
        "{:?}",
        home.files()
    );
    let state = home.state()?;
    assert_eq!(state["version"], 2);
    assert!(state.get("sessions").is_none());
    Ok(())
}

#[test]
fn an_unreadable_state_file_never_breaks_discovery() -> Result<()> {
    let home = Home::new()?;
    std::fs::create_dir_all(home.path.join(".remind"))?;
    std::fs::write(home.state_path(), b"{ broken")?;
    let status = ok_json(
        home.remind(None, None)
            .args(["login", "status", "--json"])
            .output()?,
    )?;
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "state_unreadable");
    assert_eq!(
        ok_json(
            home.remind(None, None)
                .args(["accounts", "--json"])
                .output()?
        )?["app_id"],
        "remind"
    );
    Ok(())
}

#[test]
fn retired_commands_explain_what_replaced_them() -> Result<()> {
    let home = Home::new()?;
    let auth = home
        .remind(None, None)
        .args(["--json", "auth", "login", "--slt-stdin"])
        .output()?;
    assert_eq!(auth.status.code(), Some(2));
    assert!(
        json_error(&auth)?["error"]["hint"]
            .as_str()
            .unwrap_or("")
            .contains("remind login")
    );
    let update = ok_json(
        home.remind(None, None)
            .args(["--json", "update", "--check"])
            .output()?,
    )?;
    assert_eq!(update["manager"], "silicon-apps");
    let daemon = home
        .remind(None, None)
        .args(["daemon", "install", "--json"])
        .output()?;
    assert_eq!(daemon.status.code(), Some(2));
    let removed = home
        .remind(None, None)
        .args(["--org", "tos", "list"])
        .output()?;
    assert_eq!(removed.status.code(), Some(2));
    assert!(
        !home.path.join(".remind").exists(),
        "none of these wrote state"
    );
    Ok(())
}

#[test]
fn a_selected_test_environment_is_named_after_every_command() -> Result<()> {
    let home = Home::new()?;
    let id = "01992000-0000-7000-8000-000000000004";
    let url = "http://127.0.0.1:9";
    home.write_state(&json!({
        "version": 2, "url": url,
        "test_keys": {format!("{url}#{id}"): "12345678901234567890123456789012"},
        "selected_tests": {url: id},
        "test_names": {format!("{url}#{id}"): "release-qa"}
    }))?;
    let failed = home.remind(None, None).args(["list", "--json"]).output()?;
    assert_eq!(failed.status.code(), Some(3));
    let stderr = String::from_utf8(failed.stderr)?;
    assert!(
        stderr
            .lines()
            .last()
            .is_some_and(|l| l.contains("release-qa")),
        "{stderr}"
    );
    let parse_error = home.remind(None, None).args(["not-a-command"]).output()?;
    assert!(String::from_utf8(parse_error.stderr)?.contains("release-qa"));
    let exit = ok_json(
        home.remind(None, None)
            .args(["env", "exit", "--json"])
            .output()?,
    )?;
    assert_eq!(exit["environment"], "production");
    assert!(
        home.state()?["selected_tests"]
            .as_object()
            .is_some_and(|m| m.is_empty())
    );
    let production = home.remind(None, None).args(["list", "--json"]).output()?;
    assert!(!String::from_utf8(production.stderr)?.contains("release-qa"));
    Ok(())
}
