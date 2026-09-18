//! Compatibility API for Honeycomb-managed CLI updates. Never mutates executables or projects.
use std::time::{SystemTime, UNIX_EPOCH};
/// Observable updater result. Update failures never change an API response.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UpdateStatus {
    Disabled,
    Managed { manager: String, command: String },
    Throttled,
    Current,
    Updated { version: String },
    Available { version: String },
    NoCargoProject,
    Unavailable,
}

/// Returns update ownership without invoking Cargo or accessing the registry.
pub async fn maintain(
    crate_name: &str,
    _current: &str,
    _executable: bool,
    _apply: bool,
) -> UpdateStatus {
    match crate_name {
        "silicon-remind-cli" => UpdateStatus::Managed {
            manager: "honeycomb".into(),
            command: "honeycomb update 'tos>remind'".into(),
        },
        "silicon-remind-client" => UpdateStatus::Disabled,
        _ => UpdateStatus::Unavailable,
    }
}
/// Current Unix seconds retained for older CLI state formats.
pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn update_requests_never_mutate_the_installation_or_project() {
        for apply in [false, true] {
            assert!(matches!(
                maintain("silicon-remind-cli", "0.0.0", true, apply).await,
                UpdateStatus::Managed { .. }
            ));
            assert!(matches!(
                maintain("silicon-remind-client", "0.0.0", false, apply).await,
                UpdateStatus::Disabled
            ));
        }
    }
}
