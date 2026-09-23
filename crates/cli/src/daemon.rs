//! Removal and inspection of the retired standalone update service.
use crate::{args::Daemon, state::Store};
use anyhow::{Context as _, bail};
use std::{path::PathBuf, process::Command};

pub async fn execute(command: &Daemon) -> anyhow::Result<()> {
    if matches!(command, Daemon::Run | Daemon::Install) {
        bail!(
            "Honeycomb manages Remind updates. Run `honeycomb update 'remind'`; remove the old updater with `remind daemon uninstall`."
        );
    }
    let os_home = PathBuf::from(
        std::env::var_os("HOME").context("HOME is required to install the user service")?,
    );
    let store = Store::open()?;
    let silicon_home = store.home_dir().to_owned();
    drop(store);
    let executable = std::env::current_exe()?;
    let (path, content, install, uninstall, status) = if cfg!(target_os = "macos") {
        let path = os_home.join("Library/LaunchAgents/com.teamofsilicons.remind.plist");
        let uid = Command::new("id").arg("-u").output()?;
        let domain = format!("gui/{}", String::from_utf8(uid.stdout)?.trim());
        let service = format!("{domain}/com.teamofsilicons.remind");
        let xml = |v: &str| {
            v.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        };
        let content = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>com.teamofsilicons.remind</string><key>ProgramArguments</key><array><string>{}</string><string>daemon</string><string>run</string></array><key>EnvironmentVariables</key><dict><key>SILICON_HOME</key><string>{}</string><key>PATH</key><string>{}</string></dict><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>60</integer></dict></plist>",
            xml(&executable.to_string_lossy()),
            xml(&silicon_home.to_string_lossy()),
            xml(&std::env::var("PATH").unwrap_or_default())
        );
        let install = vec![
            "launchctl".into(),
            "bootstrap".into(),
            domain,
            path.to_string_lossy().into_owned(),
        ];
        (
            path,
            content,
            install,
            vec!["launchctl".into(), "bootout".into(), service.clone()],
            vec!["launchctl".into(), "print".into(), service],
        )
    } else if cfg!(target_os = "linux") {
        let path = os_home.join(".config/systemd/user/silicon-remind.service");
        let quote = |v: &str| {
            v.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('%', "%%")
                .replace('$', "$$")
        };
        let content = format!(
            "[Unit]\nDescription=Silicon Remind hourly updater\n[Service]\nType=simple\nExecStart=\"{}\" daemon run\nEnvironment=\"SILICON_HOME={}\"\nEnvironment=\"PATH={}\"\nRestart=always\nRestartSec=60\n[Install]\nWantedBy=default.target\n",
            quote(&executable.to_string_lossy()),
            quote(&silicon_home.to_string_lossy()),
            quote(&std::env::var("PATH").unwrap_or_default())
        );
        (
            path,
            content,
            vec![
                "systemctl".into(),
                "--user".into(),
                "enable".into(),
                "--now".into(),
                "silicon-remind.service".into(),
            ],
            vec![
                "systemctl".into(),
                "--user".into(),
                "disable".into(),
                "--now".into(),
                "silicon-remind.service".into(),
            ],
            vec![
                "systemctl".into(),
                "--user".into(),
                "status".into(),
                "silicon-remind.service".into(),
            ],
        )
    } else {
        bail!(
            "Automatic service installation supports macOS and Linux. On this platform, supervise `remind daemon run` with your operating system's service manager."
        );
    };
    match command {
        Daemon::Install => {
            if path.exists() {
                let _ = invoke(&uninstall);
            }
            std::fs::create_dir_all(path.parent().context("service path has no parent")?)?;
            std::fs::write(&path, content)?;
            if cfg!(target_os = "linux") {
                invoke(&["systemctl".into(), "--user".into(), "daemon-reload".into()])?;
            }
            invoke(&install)?;
            println!("Hourly updater installed. Configure with remind config auto-update on|off.");
        }
        Daemon::Uninstall => {
            invoke(&uninstall)?;
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        Daemon::Status => invoke(&status)?,
        Daemon::Run => unreachable!(),
    }
    Ok(())
}
fn invoke(args: &[String]) -> anyhow::Result<()> {
    if !Command::new(&args[0]).args(&args[1..]).status()?.success() {
        bail!(
            "Service manager rejected the action; check your user service session and run remind daemon status"
        );
    }
    Ok(())
}
