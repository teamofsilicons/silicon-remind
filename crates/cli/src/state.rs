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
    pub last_update_check: u64,
    pub sessions: BTreeMap<String, StoredSession>,
    pub test_keys: BTreeMap<String, Secret>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            url: "https://backend.remind.teamofsilicons.com".into(),
            auto_update: true,
            last_update_check: 0,
            sessions: BTreeMap::new(),
            test_keys: BTreeMap::new(),
        }
    }
}
pub struct Store {
    pub state: State,
    dir: PathBuf,
    _lock: File,
}
impl Store {
    pub fn open() -> anyhow::Result<Self> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        let dir = PathBuf::from(home).join(".remind");
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
        let state = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .context("invalid ~/.remind/state.json; repair the file before continuing")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            state,
            dir,
            _lock: lock,
        })
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
            File::open(&self.dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }
}
pub fn slot(url: &str, test: Option<uuid::Uuid>) -> String {
    format!(
        "{}#{}",
        url.trim_end_matches('/'),
        test.map_or_else(|| "production".into(), |id| id.to_string())
    )
}
