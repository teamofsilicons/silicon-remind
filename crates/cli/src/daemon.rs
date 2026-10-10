//! Hidden `remind daemon uninstall|status`: removes the hourly updater service that Remind
//! 0.1 installed (a macOS LaunchAgent or a systemd user unit). Remind runs no updater of its
//! own any more; Silicon Apps keeps it up to date. Kept for one release.
use crate::{
    args::Daemon,
    output::{CliError, EXIT_OK, Output},
};
use anyhow::Context as _;
use serde_json::json;
use std::{path::PathBuf, process::Command};

struct Unit {
    path: PathBuf,
    stop: Vec<String>,
    inspect: Vec<String>,
}

fn unit() -> anyhow::Result<Option<Unit>> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    if cfg!(target_os = "macos") {
        let uid = Command::new("id").arg("-u").output()?;
        let service = format!(
            "gui/{}/com.teamofsilicons.remind",
            String::from_utf8(uid.stdout)?.trim()
        );
        Ok(Some(Unit {
            path: home.join("Library/LaunchAgents/com.teamofsilicons.remind.plist"),
            stop: vec!["launchctl".into(), "bootout".into(), service.clone()],
            inspect: vec!["launchctl".into(), "print".into(), service],
        }))
    } else if cfg!(target_os = "linux") {
        let name = "silicon-remind.service".to_owned();
        Ok(Some(Unit {
            path: home.join(".config/systemd/user").join(&name),
            stop: vec![
                "systemctl".into(),
                "--user".into(),
                "disable".into(),
                "--now".into(),
                name.clone(),
            ],
            inspect: vec!["systemctl".into(), "--user".into(), "status".into(), name],
        }))
    } else {
        Ok(None)
    }
}

/// Runs `remind daemon …`.
pub fn execute(out: Output, command: &Daemon) -> anyhow::Result<u8> {
    if matches!(command, Daemon::Install | Daemon::Run) {
        return Err(CliError::usage(
            "Remind no longer runs an updater of its own: Silicon Apps keeps it up to date.",
            "Remove the old updater with `remind daemon uninstall`.",
        )
        .into());
    }
    let Some(unit) = unit()? else {
        out.either(
            &json!({"installed": false}),
            "Remind never installed an updater service on this platform.",
        )?;
        return Ok(EXIT_OK);
    };
    let installed = unit.path.exists();
    match command {
        Daemon::Status => {
            out.either(
                &json!({"installed": installed, "path": unit.path}),
                &if installed {
                    format!(
                        "The retired updater is still installed at {}; remove it with `remind daemon uninstall`.",
                        unit.path.display()
                    )
                } else {
                    "The retired updater is not installed.".to_owned()
                },
            )?;
            if installed && !out.json {
                let _ = Command::new(&unit.inspect[0])
                    .args(&unit.inspect[1..])
                    .status();
            }
        }
        Daemon::Uninstall => {
            if installed {
                // The service may already be stopped; removing its file is what matters.
                let _ = Command::new(&unit.stop[0]).args(&unit.stop[1..]).status();
                std::fs::remove_file(&unit.path)
                    .with_context(|| format!("could not delete {}", unit.path.display()))?;
            }
            out.either(
                &json!({"removed": installed, "path": unit.path}),
                &if installed {
                    format!("Removed the retired updater ({}).", unit.path.display())
                } else {
                    "The retired updater is not installed; nothing to remove.".to_owned()
                },
            )?;
        }
        Daemon::Install | Daemon::Run => unreachable!("refused above"),
    }
    Ok(EXIT_OK)
}
