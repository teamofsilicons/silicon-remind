//! Permission-restricted atomic local state, serialized across CLI invocations.
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use silicon_remind_client::{Secret, models::Session};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write as _,
    path::PathBuf,
};

#[derive(Serialize, Deserialize)]
pub struct StoredSession {
    #[serde(default)]
    pub refresh_started_at: Option<i64>,
    pub session: Session,
    pub expires_at: i64,
    pub org: String,
    #[serde(default)]
    pub pending_refresh_key: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct State {
    pub url: String,
    pub auto_update: bool,
    #[serde(default = "default_telemetry")]
    pub telemetry: bool,
    pub last_update_check: u64,
    pub sessions: BTreeMap<String, StoredSession>,
    #[serde(default)]
    pub selected_sessions: BTreeMap<String, String>,
    pub test_keys: BTreeMap<String, Secret>,
    #[serde(default)]
    pub selected_tests: BTreeMap<String, uuid::Uuid>,
    #[serde(default)]
    pub test_names: BTreeMap<String, String>,
}
fn default_telemetry() -> bool {
    true
}
impl Default for State {
    fn default() -> Self {
        Self {
            url: "https://backend.remind.teamofsilicons.com".into(),
            auto_update: false,
            telemetry: true,
            last_update_check: 0,
            sessions: BTreeMap::new(),
            selected_sessions: BTreeMap::new(),
            test_keys: BTreeMap::new(),
            selected_tests: BTreeMap::new(),
            test_names: BTreeMap::new(),
        }
    }
}
pub struct Store {
    pub state: State,
    dir: PathBuf,
    _lock: File,
}
impl Store {
    pub fn forget_sessions(&mut self, base: &str) {
        self.state
            .sessions
            .retain(|key, _| key != base && !key.starts_with(&format!("{base}#")));
        self.state.selected_sessions.remove(base);
    }
    pub fn home_dir(&self) -> &std::path::Path {
        self.dir.parent().unwrap_or(self.dir.as_path())
    }
    pub fn exists() -> bool {
        configured_home().is_ok_and(|p| p.join(".remind/state.json").is_file())
    }
    pub fn open() -> anyhow::Result<Self> {
        let home = configured_home()?;
        let dir = home.join(".remind");
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(dir.join("state.lock"))?;
        lock.lock()?;
        let path = dir.join("state.json");
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
                format!(
                    "invalid {}; repair the file before continuing",
                    path.display()
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            state,
            dir,
            _lock: lock,
        })
    }
    pub fn configure_home(location: &std::path::Path) -> anyhow::Result<()> {
        if !location.is_dir() {
            anyhow::bail!("home location is not a directory: {}", location.display());
        }
        let location = std::fs::canonicalize(location)?;
        let pointer_dir = default_home()?.join(".remind");
        std::fs::create_dir_all(&pointer_dir)?;
        std::fs::write(
            pointer_dir.join("home"),
            location.to_string_lossy().as_bytes(),
        )?;
        Ok(())
    }
    pub fn save(&self) -> anyhow::Result<()> {
        let path = self.dir.join(format!("state-{}.tmp", uuid::Uuid::now_v7()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let result = (|| -> anyhow::Result<()> {
            file.write_all(&serde_json::to_vec_pretty(&self.state)?)?;
            file.sync_all()?;
            std::fs::rename(&path, self.dir.join("state.json"))?;
            // Sync the directory so the rename itself is durable. Windows cannot open a
            // directory as a file - CreateFileW without backup semantics fails with
            // ERROR_ACCESS_DENIED - and MoveFileEx is already durable there, so Unix only.
            #[cfg(unix)]
            File::open(&self.dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }
}

fn configured_home() -> anyhow::Result<PathBuf> {
    let default_home = default_home()?;
    let pointer = default_home.join(".remind/home");
    match std::fs::read_to_string(pointer) {
        Ok(value) => {
            let path = PathBuf::from(value.trim());
            if !path.is_dir() {
                anyhow::bail!(
                    "configured home location is not a directory: {}",
                    path.display()
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
        .context("neither SILICON_HOME nor HOME is set")?;
    if home.is_empty() {
        anyhow::bail!("home directory is empty; set SILICON_HOME or HOME to a directory");
    }
    Ok(PathBuf::from(home))
}
pub fn slot(url: &str, test: Option<uuid::Uuid>) -> String {
    format!(
        "{}#{}",
        url.trim_end_matches('/'),
        test.map_or_else(|| "production".into(), |id| id.to_string())
    )
}

pub fn selected_slot(
    state: &State,
    base: &str,
    org: Option<&str>,
    account: Option<&str>,
) -> anyhow::Result<String> {
    let matches = |key: &str, saved: &StoredSession| {
        (key == base || key.starts_with(&format!("{base}#")))
            && saved.session.org_id.as_deref() == Some(saved.org.as_str())
            && org.is_none_or(|org| org == saved.org)
            && account
                .is_none_or(|account| saved.session.actor["public_id"].as_str() == Some(account))
    };
    if let Some(key) = state.selected_sessions.get(base)
        && let Some(saved) = state.sessions.get(key)
        && matches(key, saved)
    {
        return Ok(key.clone());
    }
    let mut found = state
        .sessions
        .iter()
        .filter(|(key, saved)| matches(key, saved));
    match (found.next(), found.next()) {
        (Some((key, _)), None) => Ok(key.clone()),
        (None, _) => Ok(base.to_owned()),
        _ => anyhow::bail!(
            "several saved contexts match; select both --account <public-id> and --org <organization>"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(account: &str, org: &str) -> StoredSession {
        StoredSession {
            session: Session {
                access_token: Secret::new("access"),
                refresh_token: Secret::new("refresh"),
                expires_in: 1800,
                token_type: "Bearer".into(),
                scope: String::new(),
                actor: serde_json::json!({"type":"carbon", "public_id":account}),
                org_id: Some(org.into()),
            },
            org: org.into(),
            expires_at: 9999999999,
            pending_refresh_key: None,
            refresh_started_at: None,
        }
    }

    #[test]
    fn selection_never_reuses_another_account_org_or_environment() -> anyhow::Result<()> {
        let mut state = State::default();
        let base = slot("https://remind.example", None);
        let (a, b) = (format!("{base}#a"), format!("{base}#b"));
        state
            .sessions
            .insert(a.clone(), session("c:alice", "alpha"));
        state.sessions.insert(b.clone(), session("c:bob", "beta"));
        state.selected_sessions.insert(base.clone(), a.clone());
        assert_eq!(selected_slot(&state, &base, None, None)?, a);
        assert_eq!(
            selected_slot(&state, &base, Some("beta"), Some("c:bob"))?,
            b
        );
        assert_eq!(
            selected_slot(&state, &base, Some("beta"), Some("c:alice"))?,
            base
        );
        let test = slot("https://remind.example", Some(uuid::Uuid::nil()));
        assert_eq!(selected_slot(&state, &test, None, None)?, test);
        state
            .sessions
            .get_mut(&a)
            .ok_or_else(|| anyhow::anyhow!("missing fixture"))?
            .session
            .org_id = None;
        assert_eq!(selected_slot(&state, &base, Some("alpha"), None)?, base);
        Ok(())
    }
}
