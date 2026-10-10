//! Local state under `{home}/.remind/`: saved settings, Silicon Accounts sign-ins and test
//! environment keys.
//!
//! - `{home}` is `$SILICON_HOME` when set, otherwise `$HOME`; `remind config home <dir>`
//!   writes a pointer (`{default home}/.remind/home`) that moves it elsewhere.
//! - The directory is mode 0700 and every file in it 0600 (Unix).
//! - `state.json` is replaced atomically (temporary file, fsync, rename, directory fsync).
//! - `state.lock` is locked exclusively only while state is read, changed and written, or
//!   while a sign-in is refreshed, never while waiting for a person (device sign-in).
//! - Reading never creates anything, so discovery commands leave a clean home untouched.
//! - A `state.json` from Remind 0.5 or earlier (sign-ins tied to the previous identity
//!   service) is read without its sign-ins and archived as `state.iam-<time>.json` on the
//!   next write; an unreadable `state.json` is moved aside as `state.corrupt-<time>.json`.
use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use silicon_remind_client::{Secret, accounts::SignedInAccount};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// The schema written by this version.
pub const STATE_VERSION: u32 = 2;

/// One saved sign-in: Remind's Silicon Accounts tokens for one account.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSignIn {
    /// The Silicon Accounts origin that issued the tokens (refresh and sign-out go there).
    pub accounts_url: String,
    /// The app id the tokens were issued to (`remind`).
    pub app_id: String,
    /// Access token (EdDSA JWT, 30 minutes).
    pub access_token: Secret,
    /// Rotating refresh token (`sar_…`).
    pub refresh_token: Secret,
    /// Access token expiry, Unix seconds.
    pub expires_at: i64,
    /// When the sign-in itself ends, Unix seconds.
    #[serde(default)]
    pub refresh_expires_at: Option<i64>,
    /// Granted scopes.
    #[serde(default)]
    pub scope: String,
    /// Who signed in.
    pub account: SignedInAccount,
    /// `device` (a Carbon approved a code) or `slt` (a short-lived token).
    pub method: String,
    /// When this sign-in was made, Unix seconds.
    pub signed_in_at: i64,
    /// Last refresh, Unix seconds.
    #[serde(default)]
    pub refreshed_at: Option<i64>,
}

impl StoredSignIn {
    /// True while the access token has at least `margin` seconds left.
    pub fn fresh(&self, now: i64, margin: i64) -> bool {
        self.expires_at.saturating_sub(now) >= margin
    }
    /// True once the sign-in itself has ended.
    pub fn ended(&self, now: i64) -> bool {
        self.refresh_expires_at.is_some_and(|at| at <= now)
    }
}

/// Everything in `state.json`.
#[derive(Clone, Serialize, Deserialize)]
pub struct State {
    /// Schema version (2 since Silicon Accounts).
    #[serde(default)]
    pub version: u32,
    /// Saved Remind API origin.
    #[serde(default = "default_url")]
    pub url: String,
    /// Saved Silicon Accounts origin; the production one when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts_url: Option<String>,
    /// Operational telemetry (on by default).
    #[serde(default = "enabled")]
    pub telemetry: bool,
    /// Sign-ins by slot (`<api origin>#production` or `<api origin>#<test environment id>`).
    #[serde(default)]
    pub sign_ins: BTreeMap<String, StoredSignIn>,
    /// Test environment keys by slot.
    #[serde(default)]
    pub test_keys: BTreeMap<String, Secret>,
    /// The test environment selected with `remind env use`, by API origin.
    #[serde(default)]
    pub selected_tests: BTreeMap<String, Uuid>,
    /// Test environment names by slot, for the footer.
    #[serde(default)]
    pub test_names: BTreeMap<String, String>,
    /// Sign-ins saved by Remind 0.5 and earlier; read only to notice and drop them.
    #[serde(default, rename = "sessions", skip_serializing)]
    legacy_sessions: Option<serde_json::Value>,
}

fn default_url() -> String {
    silicon_remind_client::DEFAULT_URL.to_owned()
}
fn enabled() -> bool {
    true
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            url: default_url(),
            accounts_url: None,
            telemetry: true,
            sign_ins: BTreeMap::new(),
            test_keys: BTreeMap::new(),
            selected_tests: BTreeMap::new(),
            test_names: BTreeMap::new(),
            legacy_sessions: None,
        }
    }
}

impl State {
    /// Forgets a test environment on this machine: key, name, selection and its own sign-in
    /// (returned so the caller can end it at Silicon Accounts).
    pub fn forget_test(&mut self, url: &str, id: Uuid) -> Option<StoredSignIn> {
        let key = slot(url, Some(id));
        if self.selected_tests.get(url) == Some(&id) {
            self.selected_tests.remove(url);
        }
        self.test_names.remove(&key);
        self.test_keys.remove(&key);
        self.sign_ins.remove(&key)
    }
}

/// What reading `state.json` found.
pub struct Snapshot {
    /// The state (defaults when the file is missing or unreadable).
    pub state: State,
    /// The file exists.
    pub exists: bool,
    /// It holds sign-ins from Remind 0.5 or earlier, which no longer work.
    pub legacy_sign_ins: bool,
    /// It is from before schema 2.
    pub legacy: bool,
    /// Why it could not be read, when it could not.
    pub unreadable: Option<String>,
}

/// The `.remind` directory of the selected home.
#[derive(Clone, Debug)]
pub struct Home {
    dir: PathBuf,
}

impl Home {
    /// Resolves `{home}/.remind`: the `config home` pointer, else `$SILICON_HOME`, else `$HOME`.
    pub fn resolve() -> anyhow::Result<Self> {
        Ok(Self {
            dir: configured_home()?.join(".remind"),
        })
    }

    /// For tests: a `.remind` directory inside `parent`.
    #[cfg(test)]
    pub fn at(parent: &Path) -> Self {
        Self {
            dir: parent.join(".remind"),
        }
    }

    /// `{home}/.remind`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `{home}`, the parent of `.remind`.
    pub fn parent(&self) -> &Path {
        self.dir.parent().unwrap_or(self.dir.as_path())
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    /// Reads the state without creating anything and without failing: a missing file gives
    /// defaults, an unreadable one gives defaults plus the reason.
    pub fn read(&self) -> Snapshot {
        read_state(&self.state_path())
    }

    /// Takes the exclusive lock, creating the directory (0700) and lock file (0600) if needed.
    /// Blocks until another remind process on this home releases it.
    pub fn lock(&self) -> anyhow::Result<Locked> {
        std::fs::create_dir_all(&self.dir).with_context(|| {
            format!(
                "could not create the state directory {}",
                self.dir.display()
            )
        })?;
        restrict_dir(&self.dir)?;
        let file = private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join("state.lock"))
            .with_context(|| format!("could not open {}", self.dir.join("state.lock").display()))?;
        file.lock()
            .with_context(|| format!("could not lock {}", self.dir.join("state.lock").display()))?;
        Ok(Locked {
            home: self.clone(),
            _file: file,
        })
    }

    /// Reads, changes and writes the state under the lock. Returns `f`'s result and notices
    /// about files that were archived or moved aside on the way.
    pub fn update<T>(
        &self,
        f: impl FnOnce(&mut State) -> anyhow::Result<T>,
    ) -> anyhow::Result<(T, Vec<String>)> {
        let locked = self.lock()?;
        let mut snapshot = locked.read();
        let value = f(&mut snapshot.state)?;
        let notices = locked.write(&snapshot)?;
        Ok((value, notices))
    }
}

/// The state while this process holds the lock.
pub struct Locked {
    home: Home,
    _file: File,
}

impl Locked {
    /// Reads the current state (another process may have changed it before the lock).
    pub fn read(&self) -> Snapshot {
        self.home.read()
    }

    /// Writes `snapshot.state` atomically as schema 2. A legacy file is first archived and an
    /// unreadable one moved aside; the returned notices say so.
    pub fn write(&self, snapshot: &Snapshot) -> anyhow::Result<Vec<String>> {
        let path = self.home.state_path();
        let mut notices = Vec::new();
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        if snapshot.exists && snapshot.unreadable.is_some() {
            let aside = self.home.dir.join(format!("state.corrupt-{stamp}.json"));
            std::fs::rename(&path, &aside)
                .with_context(|| format!("could not move {} aside", path.display()))?;
            notices.push(format!(
                "{} could not be read, so it was moved to {} and Remind started from empty settings. Saved test environment keys can be fetched again with `remind env key <id>`.",
                path.display(),
                aside.display()
            ));
        } else if snapshot.exists && snapshot.legacy {
            let archive = self.home.dir.join(format!("state.iam-{stamp}.json"));
            copy_private(&path, &archive)?;
            notices.push(if snapshot.legacy_sign_ins {
                format!(
                    "Sign-ins saved by an earlier Remind no longer work; they were archived in {}. Sign in again: `remind login` (Carbons) or `silicon-accounts login --app remind -q | remind login --slt-stdin` (Silicons).",
                    archive.display()
                )
            } else {
                format!(
                    "Settings from an earlier Remind were upgraded; the old file is kept as {}.",
                    archive.display()
                )
            });
        }
        let mut state = snapshot.state.clone();
        state.version = STATE_VERSION;
        write_atomically(&self.home.dir, &path, &serde_json::to_vec_pretty(&state)?)?;
        Ok(notices)
    }
}

fn read_state(path: &Path) -> Snapshot {
    let missing = |unreadable: Option<String>, exists: bool| Snapshot {
        state: State::default(),
        exists,
        legacy_sign_ins: false,
        legacy: false,
        unreadable,
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return missing(None, false),
        Err(error) => return missing(Some(format!("{}: {error}", path.display())), true),
    };
    match serde_json::from_slice::<State>(&bytes) {
        Ok(mut state) => {
            let legacy = state.version < STATE_VERSION;
            let legacy_sign_ins = state
                .legacy_sessions
                .take()
                .is_some_and(|sessions| sessions.as_object().is_some_and(|m| !m.is_empty()));
            if legacy {
                upgrade_legacy(&mut state);
            }
            Snapshot {
                state,
                exists: true,
                legacy_sign_ins,
                legacy,
                unreadable: None,
            }
        }
        Err(error) => missing(
            Some(format!("{} is not valid: {error}", path.display())),
            true,
        ),
    }
}

/// Keeps what still works from a Remind 0.5 state file: the API origin, telemetry and
/// 32-character test environment keys (keys of the previous identity service's sandboxes
/// cannot be used any more).
fn upgrade_legacy(state: &mut State) {
    state.sign_ins.clear();
    state
        .test_keys
        .retain(|_, key| silicon_remind_client::is_test_environment_key(key.expose()));
    let keys = state.test_keys.clone();
    state
        .selected_tests
        .retain(|url, id| keys.contains_key(&slot(url, Some(*id))));
    state.test_names.retain(|key, _| keys.contains_key(key));
}

/// `<api origin>#production` or `<api origin>#<test environment id>`.
pub fn slot(url: &str, test: Option<Uuid>) -> String {
    format!(
        "{}#{}",
        url.trim_end_matches('/'),
        test.map_or_else(|| "production".into(), |id| id.to_string())
    )
}

/// Saves `location` (an existing directory) as the home for later invocations.
pub fn configure_home(location: &Path) -> anyhow::Result<PathBuf> {
    if !location.is_dir() {
        bail!("not a directory: {}", location.display());
    }
    let location = std::fs::canonicalize(location)?;
    let pointer_dir = default_home()?.join(".remind");
    std::fs::create_dir_all(&pointer_dir)?;
    restrict_dir(&pointer_dir)?;
    write_atomically(
        &pointer_dir,
        &pointer_dir.join("home"),
        location.to_string_lossy().as_bytes(),
    )?;
    Ok(location)
}

fn configured_home() -> anyhow::Result<PathBuf> {
    let default_home = default_home()?;
    match std::fs::read_to_string(default_home.join(".remind/home")) {
        Ok(value) => {
            let path = PathBuf::from(value.trim());
            if !path.is_dir() {
                bail!(
                    "the home set with `remind config home` is not a directory any more: {} (set another, or delete {})",
                    path.display(),
                    default_home.join(".remind/home").display()
                );
            }
            Ok(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(default_home),
        Err(error) => Err(error.into()),
    }
}

fn default_home() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("SILICON_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .context("neither SILICON_HOME nor HOME is set; set one to a directory")?;
    if home.is_empty() {
        bail!("SILICON_HOME (or HOME) is empty; set it to a directory");
    }
    Ok(PathBuf::from(home))
}

fn private_options() -> OpenOptions {
    #[allow(unused_mut, reason = "mode is only set on Unix")]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

fn restrict_dir(dir: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("could not restrict {} to its owner", dir.display()))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Writes `bytes` to `path` through a temporary file in `dir`: fsync, rename, then fsync of
/// the directory so the rename itself survives a crash (Unix; on Windows the rename is
/// already durable and a directory cannot be opened as a file).
fn write_atomically(dir: &Path, path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let temporary = dir.join(format!(".state-{}.tmp", Uuid::now_v7()));
    let mut file = private_options()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("could not create {}", temporary.display()))?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.with_context(|| format!("could not save {}", path.display()))
}

fn copy_private(from: &Path, to: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(from)?;
    let mut file = private_options()
        .write(true)
        .create_new(true)
        .open(to)
        .with_context(|| format!("could not create {}", to.display()))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use silicon_remind_client::models::AccountKind;

    fn sign_in(access: &str) -> StoredSignIn {
        StoredSignIn {
            accounts_url: "https://accounts.example".into(),
            app_id: "remind".into(),
            access_token: Secret::new(access),
            refresh_token: Secret::new("sar_refresh"),
            expires_at: 2_000_000_000,
            refresh_expires_at: None,
            scope: "profile timezone".into(),
            account: SignedInAccount {
                uuid: "zQo".into(),
                id: "si:scout".into(),
                kind: AccountKind::Silicon,
                display_name: "Scout".into(),
                pfp_url: None,
                custodian: None,
            },
            method: "slt".into(),
            signed_in_at: 1_900_000_000,
            refreshed_at: None,
        }
    }

    #[test]
    fn reading_a_missing_home_creates_nothing() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let home = Home::at(temp.path());
        let snapshot = home.read();
        assert!(!snapshot.exists && snapshot.unreadable.is_none());
        assert_eq!(snapshot.state.url, silicon_remind_client::DEFAULT_URL);
        assert!(!temp.path().join(".remind").exists());
    }

    #[cfg(unix)]
    #[test]
    fn writes_are_private_and_atomic() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::TempDir::new()?;
        let home = Home::at(temp.path());
        home.update(|state| {
            state
                .sign_ins
                .insert(slot("https://r.example", None), sign_in("a1"));
            Ok(())
        })?;
        let mode = |path: &Path| std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777);
        assert_eq!(mode(home.dir())?, 0o700);
        assert_eq!(mode(&home.dir().join("state.json"))?, 0o600);
        assert_eq!(mode(&home.dir().join("state.lock"))?, 0o600);
        let names: Vec<_> = std::fs::read_dir(home.dir())?
            .filter_map(|entry| {
                entry
                    .ok()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
            })
            .collect();
        assert!(
            names.iter().all(|name| !name.ends_with(".tmp")),
            "{names:?}"
        );
        let saved = home.read();
        assert_eq!(saved.state.version, STATE_VERSION);
        let stored = &saved.state.sign_ins[&slot("https://r.example/", None)];
        assert_eq!(stored.access_token.expose(), "a1");
        Ok(())
    }

    #[test]
    fn the_lock_serializes_read_modify_write() -> anyhow::Result<()> {
        let temp = tempfile::TempDir::new()?;
        let home = Home::at(temp.path());
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let home = home.clone();
                std::thread::spawn(move || {
                    home.update(|state| {
                        let count = state.test_names.len();
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        state
                            .test_names
                            .insert(format!("slot-{n}"), count.to_string());
                        Ok(())
                    })
                })
            })
            .collect();
        for thread in threads {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("thread panicked"))??;
        }
        assert_eq!(home.read().state.test_names.len(), 8, "no update was lost");
        Ok(())
    }

    #[test]
    fn an_unreadable_file_is_reported_then_moved_aside_on_write() -> anyhow::Result<()> {
        let temp = tempfile::TempDir::new()?;
        let home = Home::at(temp.path());
        std::fs::create_dir_all(home.dir())?;
        std::fs::write(home.dir().join("state.json"), b"{not json")?;
        let snapshot = home.read();
        assert!(snapshot.exists && snapshot.unreadable.is_some());
        let ((), notices) = home.update(|state| {
            state.telemetry = false;
            Ok(())
        })?;
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("moved to"), "{}", notices[0]);
        let aside: Vec<_> = std::fs::read_dir(home.dir())?
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("state.corrupt-")
            })
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read(aside[0].path())?, b"{not json");
        assert!(!home.read().state.telemetry);
        Ok(())
    }

    #[test]
    fn legacy_state_drops_old_sign_ins_and_keeps_what_still_works() -> anyhow::Result<()> {
        let temp = tempfile::TempDir::new()?;
        let home = Home::at(temp.path());
        std::fs::create_dir_all(home.dir())?;
        let id = "01992000-0000-7000-8000-000000000004";
        let other = "01992000-0000-7000-8000-000000000005";
        let legacy = serde_json::json!({
            "url": "https://r.example", "auto_update": false, "telemetry": false, "last_update_check": 0,
            "sessions": {"https://r.example#production#silicon:si:a@tos": {"session": {
                "access_token": "oat_x", "refresh_token": "ort_x", "expires_in": 3600, "token_type": "Bearer",
                "scope": "", "actor": {"type": "silicon", "public_id": "si:a"}, "org_id": "tos"},
                "org": "tos", "expires_at": 1}},
            "selected_sessions": {"https://r.example#production": "x"},
            "test_keys": {
                format!("https://r.example#{id}"): "12345678901234567890123456789012",
                format!("https://r.example#{other}"): format!("ask_{}", "t".repeat(43))
            },
            "selected_tests": {"https://r.example": other},
            "test_names": {format!("https://r.example#{id}"): "kept", format!("https://r.example#{other}"): "dropped"}
        });
        std::fs::write(home.dir().join("state.json"), serde_json::to_vec(&legacy)?)?;
        let snapshot = home.read();
        assert!(snapshot.legacy && snapshot.legacy_sign_ins && snapshot.unreadable.is_none());
        assert!(snapshot.state.sign_ins.is_empty());
        assert_eq!(snapshot.state.url, "https://r.example");
        assert!(!snapshot.state.telemetry);
        assert_eq!(snapshot.state.test_keys.len(), 1);
        assert!(snapshot.state.selected_tests.is_empty());
        assert_eq!(snapshot.state.test_names.len(), 1);
        let ((), notices) = home.update(|_| Ok(()))?;
        assert!(notices[0].contains("Sign in again"), "{}", notices[0]);
        let archived: Vec<_> = std::fs::read_dir(home.dir())?
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("state.iam-"))
            .collect();
        assert_eq!(archived.len(), 1);
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.dir().join("state.json"))?)?;
        assert_eq!(written["version"], STATE_VERSION);
        assert!(written.get("sessions").is_none() && written.get("auto_update").is_none());
        let ((), notices) = home.update(|_| Ok(()))?;
        assert!(notices.is_empty(), "an upgraded file is not archived twice");
        Ok(())
    }

    #[test]
    fn forgetting_a_test_environment_returns_its_own_sign_in() {
        let mut state = State::default();
        let id = Uuid::now_v7();
        let key = slot("https://r.example", Some(id));
        state
            .test_keys
            .insert(key.clone(), Secret::new("k".repeat(32)));
        state.test_names.insert(key.clone(), "qa".into());
        state.selected_tests.insert("https://r.example".into(), id);
        state.sign_ins.insert(key, sign_in("env"));
        let ended = state.forget_test("https://r.example", id);
        assert_eq!(
            ended.map(|s| s.access_token.expose().to_owned()),
            Some("env".into())
        );
        assert!(state.test_keys.is_empty() && state.test_names.is_empty());
        assert!(state.selected_tests.is_empty() && state.sign_ins.is_empty());
    }

    #[test]
    fn freshness_uses_a_margin() {
        let stored = sign_in("a");
        assert!(stored.fresh(stored.expires_at - 61, 60));
        assert!(!stored.fresh(stored.expires_at - 59, 60));
        assert!(!stored.ended(0));
    }
}
