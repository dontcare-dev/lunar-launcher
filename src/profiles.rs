use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OfflineProfile {
    pub name: String,
    pub uuid: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProfileStore {
    pub profiles: Vec<OfflineProfile>,
    pub selected: Option<String>,
}

impl OfflineProfile {
    pub fn create(name: &str) -> Result<Self> {
        let name = name.trim();
        if !(3..=16).contains(&name.len())
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            bail!("Names must be 3–16 characters and contain only letters, numbers, or underscores.");
        }

        let digest = md5::compute(format!("OfflinePlayer:{name}").as_bytes());
        let mut bytes = digest.0;
        bytes[6] = (bytes[6] & 0x0f) | 0x30;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let uuid = format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            u32::from_be_bytes(bytes[0..4].try_into().unwrap()),
            u16::from_be_bytes(bytes[4..6].try_into().unwrap()),
            u16::from_be_bytes(bytes[6..8].try_into().unwrap()),
            u16::from_be_bytes(bytes[8..10].try_into().unwrap()),
            u64::from_be_bytes([0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]])
        );

        Ok(Self { name: name.to_owned(), uuid })
    }
}

impl ProfileStore {
    fn path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("dev", "SilverDevStudios", "LUNAR")
            .context("Could not locate the application data directory")?;
        fs::create_dir_all(dirs.data_dir())?;
        Ok(dirs.data_dir().join("profiles.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .ok()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        let bytes = serde_json::to_vec_pretty(self)?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    pub fn add(&mut self, name: &str) -> Result<()> {
        let profile = OfflineProfile::create(name)?;
        if self.profiles.iter().any(|item| item.name.eq_ignore_ascii_case(&profile.name)) {
            bail!("That profile already exists.");
        }
        self.selected = Some(profile.name.clone());
        self.profiles.push(profile);
        self.save()
    }

    pub fn rename(&mut self, old_name: &str, new_name: &str) -> Result<()> {
        let replacement = OfflineProfile::create(new_name)?;
        if self.profiles.iter().any(|item| {
            !item.name.eq_ignore_ascii_case(old_name)
                && item.name.eq_ignore_ascii_case(&replacement.name)
        }) {
            bail!("That profile name is already in use.");
        }
        let profile = self.profiles.iter_mut()
            .find(|item| item.name == old_name)
            .context("Profile not found")?;
        profile.name = replacement.name.clone();
        profile.uuid = replacement.uuid;
        if self.selected.as_deref() == Some(old_name) {
            self.selected = Some(replacement.name);
        }
        self.save()
    }

    pub fn select(&mut self, name: &str) -> Result<()> {
        if !self.profiles.iter().any(|profile| profile.name == name) {
            bail!("Profile not found");
        }
        self.selected = Some(name.to_owned());
        self.save()
    }

    pub fn delete(&mut self, name: &str) -> Result<()> {
        self.profiles.retain(|profile| profile.name != name);
        if self.selected.as_deref() == Some(name) {
            self.selected = self.profiles.first().map(|profile| profile.name.clone());
        }
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuid_is_stable() {
        let first = OfflineProfile::create("LunarPlayer").unwrap();
        let second = OfflineProfile::create("LunarPlayer").unwrap();
        assert_eq!(first.uuid, second.uuid);
        assert_eq!(first.uuid.len(), 36);
        assert_eq!(OfflineProfile::create("Notch").unwrap().uuid, "b50ad385-829d-3141-a216-7e7d7539ba7f");
    }

    #[test]
    fn rejects_invalid_names() {
        assert!(OfflineProfile::create("ab").is_err());
        assert!(OfflineProfile::create("bad name").is_err());
        assert!(OfflineProfile::create("valid_name").is_ok());
    }
}
