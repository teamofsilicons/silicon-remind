//! Where output goes, and how failures are described and mapped to exit codes.
//!
//! - Results go to stdout: one compact JSON object with `--json`, pretty JSON or text
//!   otherwise.
//! - Progress, warnings and suggestions go to stderr: JSON lines with `--json`, text
//!   otherwise (suggestions only in text mode).
//! - Failures go to stderr as `{"error":{"code","message","hint"?,"status"?,"request_id"?,
//!   "retry_after"?}}` with `--json`, and as text otherwise.
use serde::Serialize;
use serde_json::{Value, json};
use silicon_remind_client::Error as ClientError;

/// Success.
pub const EXIT_OK: u8 = 0;
/// Any failure not listed below.
pub const EXIT_FAILURE: u8 = 1;
/// Invalid arguments or input, refused before anything was sent.
pub const EXIT_USAGE: u8 = 2;
/// Not signed in, sign-in refused or ended, or HTTP 401.
pub const EXIT_AUTH: u8 = 3;
/// HTTP 403: signed in, but not allowed.
pub const EXIT_FORBIDDEN: u8 = 4;
/// Stopped with Ctrl-C.
pub const EXIT_INTERRUPTED: u8 = 130;

/// Output mode for one invocation.
#[derive(Clone, Copy, Debug)]
pub struct Output {
    /// `--json` was given.
    pub json: bool,
}

impl Output {
    /// Prints a result on stdout.
    pub fn result(&self, value: &impl Serialize) -> anyhow::Result<()> {
        let value = serde_json::to_value(value)?;
        if self.json {
            println!("{}", serde_json::to_string(&value)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Ok(())
    }

    /// Prints `json` with `--json`, else `text`, on stdout.
    pub fn either(&self, json: &Value, text: &str) -> anyhow::Result<()> {
        if self.json {
            println!("{}", serde_json::to_string(json)?);
        } else {
            println!("{}", text.trim_end());
        }
        Ok(())
    }

    /// Progress on stderr: the JSON line with `--json`, else the text.
    pub fn progress(&self, json: &Value, text: &str) {
        if self.json {
            eprintln!("{json}");
        } else {
            eprintln!("{}", text.trim_end());
        }
    }

    /// A warning on stderr.
    pub fn warn(&self, text: &str) {
        if self.json {
            eprintln!("{}", json!({ "warning": text }));
        } else {
            eprintln!("warning: {text}");
        }
    }

    /// A suggested next step on stderr (text mode only).
    pub fn suggest(&self, text: &str) {
        if !self.json {
            eprintln!("{}", text.trim_end());
        }
    }
}

/// A failure the CLI itself describes (not an answer from Remind or Silicon Accounts).
#[derive(Debug)]
pub struct CliError {
    /// Stable code.
    pub code: &'static str,
    /// What went wrong and why.
    pub message: String,
    /// What to do next.
    pub hint: Option<String>,
    /// Exit status.
    pub exit: u8,
}

impl CliError {
    /// A failure with a code, message, hint and exit status.
    pub fn new(
        code: &'static str,
        exit: u8,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            hint: Some(hint.into()).filter(|h: &String| !h.is_empty()),
            exit,
        }
    }

    /// Invalid input (exit 2).
    pub fn usage(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new("invalid_input", EXIT_USAGE, message, hint)
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " Hint: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CliError {}

/// The exit status for a failure.
pub fn exit_code(error: &anyhow::Error) -> u8 {
    if let Some(error) = error.downcast_ref::<CliError>() {
        return error.exit;
    }
    match error.downcast_ref::<ClientError>() {
        Some(ClientError::Api { status: 401, .. } | ClientError::SignInRefused { .. }) => EXIT_AUTH,
        Some(ClientError::Accounts {
            status: Some(401), ..
        }) => EXIT_AUTH,
        Some(ClientError::Api { status: 403, .. }) => EXIT_FORBIDDEN,
        Some(ClientError::Invalid(_)) => EXIT_USAGE,
        _ => EXIT_FAILURE,
    }
}

/// The machine-readable form of a failure.
pub fn machine_error(error: &anyhow::Error) -> Value {
    if let Some(error) = error.downcast_ref::<CliError>() {
        let mut body = json!({ "code": error.code, "message": error.message });
        if let Some(hint) = &error.hint {
            body["hint"] = json!(hint);
        }
        return json!({ "error": body });
    }
    if let Some(error) = error.downcast_ref::<ClientError>() {
        let mut body = json!({ "code": error.code(), "message": error.message() });
        if let Some(hint) = error.hint() {
            body["hint"] = json!(hint);
        }
        if let Some(status) = error.status() {
            body["status"] = json!(status);
        }
        if let Some(request_id) = error.request_id() {
            body["request_id"] = json!(request_id);
        }
        if let ClientError::Api {
            retry_after: Some(seconds),
            ..
        } = error
        {
            body["retry_after"] = json!(seconds);
        }
        return json!({ "error": body });
    }
    json!({ "error": { "code": "cli_error", "message": format!("{error:#}") } })
}

/// The text form of a failure.
pub fn human_error(error: &anyhow::Error) -> String {
    if let Some(error) = error.downcast_ref::<ClientError>() {
        let mut text = format!("Error: {}", error.message().trim_end());
        if let Some(hint) = error.hint() {
            text.push_str(&format!("\nHint: {hint}"));
        }
        let mut details = vec![format!("code {}", error.code())];
        if let Some(status) = error.status() {
            details.push(format!("HTTP {status}"));
        }
        if let Some(request_id) = error.request_id() {
            details.push(format!("request {request_id}"));
        }
        text.push_str(&format!("\n({})", details.join(", ")));
        return text;
    }
    if let Some(error) = error.downcast_ref::<CliError>() {
        let mut text = format!("Error: {}", error.message);
        if let Some(hint) = &error.hint {
            text.push_str(&format!("\nHint: {hint}"));
        }
        return text;
    }
    format!("Error: {error:#}\nRun `remind <command> --help` for usage and examples.")
}

/// An instant as RFC 3339 UTC, to the second.
pub fn rfc3339(unix: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(unix, 0).map_or_else(
        || unix.to_string(),
        |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    )
}

/// `2026-10-10T08:30:00Z (in 29m)`.
pub fn when(unix: i64, now: i64) -> String {
    let delta = unix - now;
    let span = |seconds: i64| {
        let seconds = seconds.abs();
        if seconds >= 2 * 86_400 {
            format!("{}d", seconds / 86_400)
        } else if seconds >= 3_600 {
            format!("{}h", seconds / 3_600)
        } else if seconds >= 60 {
            format!("{}m", seconds / 60)
        } else {
            format!("{seconds}s")
        }
    };
    if delta >= 0 {
        format!("{} (in {})", rfc3339(unix), span(delta))
    } else {
        format!("{} ({} ago)", rfc3339(unix), span(delta))
    }
}

/// Aligned `key  value` lines.
pub fn key_values(rows: &[(&str, String)]) -> String {
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    rows.iter()
        .map(|(key, value)| format!("{key:<width$}  {value}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_map_to_exit_codes_and_keep_the_service_error_body() {
        let api = anyhow::Error::new(ClientError::Api {
            status: 403,
            code: "not_reminder_owner".into(),
            message: "Only si:scout changes this reminder.".into(),
            hint: Some("Ask si:scout.".into()),
            request_id: Some("req_1".into()),
            retry_after: None,
        });
        assert_eq!(exit_code(&api), EXIT_FORBIDDEN);
        assert_eq!(
            machine_error(&api),
            json!({"error":{"code":"not_reminder_owner","message":"Only si:scout changes this reminder.",
                "hint":"Ask si:scout.","status":403,"request_id":"req_1"}})
        );
        let unauthenticated = anyhow::Error::new(ClientError::Api {
            status: 401,
            code: "token_revoked".into(),
            message: "m".into(),
            hint: None,
            request_id: None,
            retry_after: Some(3),
        });
        assert_eq!(exit_code(&unauthenticated), EXIT_AUTH);
        assert_eq!(machine_error(&unauthenticated)["error"]["retry_after"], 3);
        let local = anyhow::Error::new(CliError::usage("bad", "do this"));
        assert_eq!(exit_code(&local), EXIT_USAGE);
        assert_eq!(
            machine_error(&local),
            json!({"error":{"code":"invalid_input","message":"bad","hint":"do this"}})
        );
        assert_eq!(exit_code(&anyhow::anyhow!("other")), EXIT_FAILURE);
    }

    #[test]
    fn times_are_rfc3339_with_a_relative_hint() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(when(1_800, 0), "1970-01-01T00:30:00Z (in 30m)");
        assert_eq!(when(0, 7_200), "1970-01-01T00:00:00Z (2h ago)");
        assert_eq!(
            key_values(&[("a", "1".into()), ("long", "2".into())]),
            "a     1\nlong  2\n"
        );
    }
}
