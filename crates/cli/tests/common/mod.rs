//! Shared fixtures for the `remind` integration tests: an isolated home, stub Silicon
//! Accounts and Remind servers (wiremock), and saved sign-ins.
#![allow(dead_code, reason = "each test file uses a different subset")]

use anyhow::Result;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};
use tempfile::TempDir;

/// A fresh, empty home for one test.
pub struct Home {
    _dir: TempDir,
    pub path: PathBuf,
}

impl Home {
    pub fn new() -> Result<Self> {
        let dir = TempDir::new()?;
        let path = dir.path().to_path_buf();
        Ok(Self { _dir: dir, path })
    }

    /// `remind` with only this home and the given origins in its environment.
    pub fn remind(&self, remind_url: Option<&str>, accounts_url: Option<&str>) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_remind"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.path)
            .env("REMIND_TELEMETRY_ENABLED", "false")
            .stdin(Stdio::null());
        if let Some(url) = remind_url {
            command.env("REMIND_URL", url);
        }
        if let Some(url) = accounts_url {
            command.env("ACCOUNTS_URL", url);
        }
        command
    }

    pub fn state_path(&self) -> PathBuf {
        self.path.join(".remind/state.json")
    }

    pub fn state(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&std::fs::read(self.state_path())?)?)
    }

    pub fn write_state(&self, state: &Value) -> Result<()> {
        std::fs::create_dir_all(self.path.join(".remind"))?;
        std::fs::write(self.state_path(), serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }

    /// Saves a sign-in for `remind_url` (production slot unless `test` names one).
    pub fn sign_in(
        &self,
        remind_url: &str,
        accounts_url: &str,
        test: Option<&str>,
        access: &str,
        refresh: &str,
        expires_in: i64,
    ) -> Result<()> {
        let mut state = if self.state_path().exists() {
            self.state()?
        } else {
            json!({"version": 2, "url": remind_url})
        };
        let slot = format!("{remind_url}#{}", test.unwrap_or("production"));
        state["sign_ins"][slot] = json!({
            "accounts_url": accounts_url,
            "app_id": "remind",
            "access_token": access,
            "refresh_token": refresh,
            "expires_at": now() + expires_in,
            "refresh_expires_at": now() + 86_400 * 900,
            "scope": "profile timezone",
            "account": silicon_account(),
            "method": "slt",
            "signed_in_at": now() - 60,
        });
        self.write_state(&state)
    }

    /// Paths under `.remind`, sorted.
    pub fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.path.join(".remind"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

pub fn silicon_account() -> Value {
    json!({"uuid": "zQo", "id": "si:scout", "kind": "silicon", "display_name": "Scout",
        "custodian": {"uuid": "aB3", "id": "c:ada", "kind": "carbon"}})
}

/// A Silicon Accounts token response for the Silicon si:scout (or the Carbon c:ada).
pub fn tokens(access: &str, refresh: &str, kind: &str) -> Value {
    let account = if kind == "carbon" {
        json!({"uuid": "aB3", "membership_id": "remind:aB3", "kind": "carbon", "id": "c:ada",
            "display_name": "Ada", "pfp_url": "", "version": 1})
    } else {
        json!({"uuid": "zQo", "membership_id": "remind:zQo", "kind": "silicon", "id": "si:scout",
            "display_name": "Scout", "pfp_url": "", "custodian": {"uuid": "aB3", "id": "c:ada"}, "version": 2})
    };
    json!({"access_token": access, "token_type": "Bearer", "expires_in": 1800, "refresh_token": refresh,
        "refresh_token_expires_at": "2029-03-25T02:31:52.745Z", "scope": "profile timezone",
        "membership_id": account["membership_id"], "account": account})
}

/// Remind's `GET /api/v2/auth/me` answer.
pub fn identity(kind: &str) -> Value {
    if kind == "carbon" {
        json!({"uuid": "aB3", "kind": "carbon", "id": "c:ada", "display_name": "Ada", "pfp_url": "",
            "custodian": null, "can_manage_reminders": false, "credential": "access_token",
            "issuing_app": null, "visible_silicons": 1})
    } else {
        json!({"uuid": "zQo", "kind": "silicon", "id": "si:scout", "display_name": "Scout", "pfp_url": "",
            "custodian": {"uuid": "aB3", "id": "c:ada", "kind": "carbon"}, "can_manage_reminders": true,
            "credential": "access_token", "issuing_app": null, "visible_silicons": 1})
    }
}

/// `{"error":"invalid_grant","error_description":…}` as Silicon Accounts answers.
pub fn oauth_error(error: &str, description: &str) -> Value {
    json!({"error": error, "error_description": description})
}

/// Runs and expects exit 0 with one JSON object on stdout.
pub fn ok_json(output: Output) -> Result<Value> {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// The JSON error object a failed `--json` command printed on stderr (its last JSON line).
pub fn json_error(output: &Output) -> Result<Value> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr
        .lines()
        .rev()
        .find(|line| line.starts_with("{\"error\""))
        .ok_or_else(|| anyhow::anyhow!("no JSON error on stderr: {stderr}"))?;
    Ok(serde_json::from_str(line)?)
}

/// Form fields of a request body.
pub fn form(body: &[u8]) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}
