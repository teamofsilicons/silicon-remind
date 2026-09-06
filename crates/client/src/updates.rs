//! Best-effort registry maintenance. Compiled libraries change only after a rebuild.
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
static LAST_CLIENT_CHECK: AtomicU64 = AtomicU64::new(0);
static CHECK_RUNNING: AtomicBool = AtomicBool::new(false);

/// Observable updater result. Update failures never change an API response.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UpdateStatus {
    Disabled,
    Throttled,
    Current,
    Updated { version: String },
    Available { version: String },
    NoCargoProject,
    Unavailable,
}

pub(crate) async fn client_maintenance() {
    if std::env::var("SILICON_REMIND_CLIENT_AUTO_UPDATE")
        .is_ok_and(|v| matches!(v.as_str(), "false" | "0" | "off"))
    {
        return;
    }
    let now = unix_time();
    let last = LAST_CLIENT_CHECK.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < 3600 {
        return;
    }
    if CHECK_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    LAST_CLIENT_CHECK.store(now, Ordering::Relaxed);
    // The task owns the guard even if its caller is cancelled midway through Cargo.
    let _ = tokio::spawn(async {
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                CHECK_RUNNING.store(false, Ordering::Release);
            }
        }
        let _guard = Guard;
        let _ = maintain(
            "silicon-remind-client",
            env!("CARGO_PKG_VERSION"),
            false,
            true,
        )
        .await;
    })
    .await;
}

/// Registry check and optional update. CLI callers persist their own hourly throttle.
pub async fn maintain(
    crate_name: &str,
    current: &str,
    executable: bool,
    apply: bool,
) -> UpdateStatus {
    if !matches!(crate_name, "silicon-remind-client" | "silicon-remind-cli") {
        return UpdateStatus::Unavailable;
    }
    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent("silicon-remind-updater")
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(_) => return UpdateStatus::Unavailable,
    };
    let mut response = match http
        .get(format!("https://crates.io/api/v1/crates/{crate_name}"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return UpdateStatus::Unavailable,
    };
    const MAX_REGISTRY_BYTES: usize = 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|bytes| bytes > MAX_REGISTRY_BYTES as u64)
    {
        return UpdateStatus::Unavailable;
    }
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len().saturating_add(chunk.len()) <= MAX_REGISTRY_BYTES => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(None) => break,
            _ => return UpdateStatus::Unavailable,
        }
    }
    let body: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return UpdateStatus::Unavailable,
    };
    let Some(latest) = body["crate"]["max_stable_version"].as_str() else {
        return UpdateStatus::Unavailable;
    };
    let (Ok(latest_version), Ok(current_version)) = (
        semver::Version::parse(latest),
        semver::Version::parse(current),
    ) else {
        return UpdateStatus::Unavailable;
    };
    if latest_version <= current_version {
        return UpdateStatus::Current;
    }
    let version = latest_version.to_string();
    if !apply {
        return UpdateStatus::Available { version };
    }
    let install_root = if executable {
        match cli_install_root() {
            Some(root) => Some(root),
            None => return UpdateStatus::Available { version },
        }
    } else {
        None
    };
    let manifest = if executable {
        None
    } else {
        match manifest() {
            Some(p) => Some(p),
            None => return UpdateStatus::NoCargoProject,
        }
    };
    let crate_name = crate_name.to_owned();
    let command_version = version.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut command = Command::new("cargo");
        if let Some(manifest) = manifest {
            command
                .arg("update")
                .arg("--manifest-path")
                .arg(manifest)
                .arg("-p")
                .arg(crate_name)
                .arg("--precise")
                .arg(command_version);
        } else {
            command.args([
                "install",
                &crate_name,
                "--version",
                &command_version,
                "--locked",
                "--force",
            ]);
            if let Some(root) = install_root {
                command.arg("--root").arg(root);
            }
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
    .await;
    if matches!(result, Ok(true)) {
        UpdateStatus::Updated { version }
    } else {
        UpdateStatus::Unavailable
    }
}
fn cli_install_root() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    if executable.file_stem()? != "remind" {
        return None;
    }
    let bin = executable.parent()?;
    let root = bin.parent()?;
    (bin.file_name()? == "bin" && root.join(".crates.toml").is_file()).then(|| root.to_path_buf())
}
fn manifest() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SILICON_REMIND_CLIENT_MANIFEST") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .map(|p| p.join("Cargo.toml"))
        .find(|p| Path::new(p).is_file())
}
/// Current Unix seconds used to persist the CLI's update throttle.
pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
