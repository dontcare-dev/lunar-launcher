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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActivePlaySession {
    pub started_at: u64,
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
    pub active_session: Option<ActivePlaySession>,
}

impl PlayStats {
    fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "SilverDevStudios", "LUNAR")
            .map(|dirs| dirs.data_dir().join("play-stats.json"))
    }

    pub fn load() -> Self {
        let mut stats = Self::path().and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        if let Some(active) = stats.active_session.take() {
            let ended_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let duration = ended_at.saturating_sub(active.started_at).min(12 * 60 * 60);
            stats.apply_record(active.version, active.instance, active.started_at, duration, ended_at);
            let _ = stats.save();
        }
        stats
    }

    pub fn begin_session(&mut self, version: String, instance: Option<String>) -> Result<()> {
        let started_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        self.active_session = Some(ActivePlaySession { started_at, version, instance });
        self.save()
    }

    pub fn save(&self) -> Result<()> {
        if let Some(path) = Self::path() {
            if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
            let temporary = path.with_extension("tmp");
            fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
            let backup = path.with_extension("bak");
            let had_previous = path.exists();
            if had_previous {
                let _ = fs::remove_file(&backup);
                fs::rename(&path, &backup)?;
            }
            if let Err(error) = fs::rename(&temporary, &path) {
                if had_previous { let _ = fs::rename(&backup, &path); }
                let _ = fs::remove_file(&temporary);
                return Err(error.into());
            }
            if had_previous { let _ = fs::remove_file(backup); }
        }
        Ok(())
    }

    fn apply_record(&mut self, version: String, instance: Option<String>, started_at: u64, duration_seconds: u64, ended_at: u64) {
        self.total_launches = self.total_launches.saturating_add(1);
        self.total_playtime_seconds = self.total_playtime_seconds.saturating_add(duration_seconds);
        self.last_played = Some(ended_at);
        let version_total = self.version_playtime.entry(version.clone()).or_default();
        *version_total = version_total.saturating_add(duration_seconds);
        self.recent_sessions.insert(0, PlaySession { started_at, ended_at, duration_seconds, version, instance });
        self.recent_sessions.truncate(100);
        self.active_session = None;
    }

    pub fn record(&mut self, version: String, instance: Option<String>, started_at: u64, duration_seconds: u64) -> Result<()> {
        let ended_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        self.apply_record(version, instance, started_at, duration_seconds, ended_at);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_formats_minutes() { assert_eq!(format_duration(59), "0m"); }
    #[test]
    fn duration_formats_exact_hours() { assert_eq!(format_duration(3600), "1h 0m"); }
    #[test]
    fn duration_formats_hours_and_minutes() { assert_eq!(format_duration(5_580), "1h 33m"); }
    #[test]
    fn duration_ignores_remaining_seconds() { assert_eq!(format_duration(125), "2m"); }
    #[test]
    fn timestamp_is_today_for_recent_epoch() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(format_timestamp(now), "Today");
    }
    #[test]
    fn timestamp_is_yesterday_for_one_day_old_epoch() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(format_timestamp(now.saturating_sub(86_400)), "Yesterday");
    }
    #[test]
    fn timestamp_handles_future_values_without_underflow() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(format_timestamp(now.saturating_add(86_400)), "Today");
    }
    #[test]
    fn stats_deserialize_legacy_empty_document() {
        let stats: PlayStats = serde_json::from_str("{}").unwrap();
        assert_eq!(stats.total_launches, 0);
        assert!(stats.recent_sessions.is_empty());
    }
    #[test]
    fn stats_round_trip_preserves_session_history() {
        let mut stats = PlayStats::default();
        stats.total_launches = 2;
        stats.total_playtime_seconds = 7200;
        stats.version_playtime.insert("1.21.4".into(), 7200);
        stats.recent_sessions.push(PlaySession {
            started_at: 10, ended_at: 3610, duration_seconds: 3600,
            version: "1.21.4".into(), instance: Some("Survival".into()),
        });
        let encoded = serde_json::to_string(&stats).unwrap();
        let decoded: PlayStats = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.total_launches, 2);
        assert_eq!(decoded.recent_sessions[0].instance.as_deref(), Some("Survival"));
    }
    #[test]
    fn missing_optional_session_instance_deserializes() {
        let session: PlaySession = serde_json::from_str(r#"{"started_at":1,"ended_at":2,"duration_seconds":1,"version":"1.20.4"}"#).unwrap();
        assert!(session.instance.is_none());
    }
}
