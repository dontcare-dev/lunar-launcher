use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf, time::{SystemTime, UNIX_EPOCH}};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaySession {
    pub started_at: u64,
    pub ended_at: u64,
    pub duration_seconds: u64,
    pub version: String,
    pub instance: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayStats {
    pub total_playtime_seconds: u64,
    pub total_launches: u64,
    pub last_played: Option<u64>,
    pub version_playtime: BTreeMap<String, u64>,
    pub recent_sessions: Vec<PlaySession>,
}

impl PlayStats {
    fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "SilverDevStudios", "LUNAR")
            .map(|dirs| dirs.data_dir().join("play-stats.json"))
    }

    pub fn load() -> Self {
        Self::path().and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        if let Some(path) = Self::path() {
            if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
            let temporary = path.with_extension("tmp");
            fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
            fs::rename(temporary, path)?;
        }
        Ok(())
    }

    pub fn record(&mut self, version: String, instance: Option<String>, started_at: u64, duration_seconds: u64) -> Result<()> {
        let ended_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        self.total_launches = self.total_launches.saturating_add(1);
        self.total_playtime_seconds = self.total_playtime_seconds.saturating_add(duration_seconds);
        self.last_played = Some(ended_at);
        *self.version_playtime.entry(version.clone()).or_default() += duration_seconds;
        self.recent_sessions.insert(0, PlaySession { started_at, ended_at, duration_seconds, version, instance });
        self.recent_sessions.truncate(100);
        self.save()
    }
}

pub fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    if hours > 0 { format!("{hours}h {minutes}m") } else { format!("{minutes}m") }
}

pub fn format_timestamp(timestamp: u64) -> String {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs().saturating_sub(timestamp) / 86400;
    if days == 0 { "Today".into() } else if days == 1 { "Yesterday".into() } else { format!("{days} days ago") }
}
