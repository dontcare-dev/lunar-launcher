use anyhow::{Context, Result};
use serde::Deserialize;

const MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Clone, Debug, Deserialize)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "releaseTime")]
    pub release_time: String,
}

#[derive(Debug, Deserialize)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Deserialize)]
pub struct LatestVersions {
    pub release: String,
    pub snapshot: String,
}

pub fn fetch_manifest() -> Result<VersionManifest> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("LUNAR/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20))
        .build()?
        .get(MANIFEST_URL)
        .send()
        .context("Could not reach Mojang's version service")?
        .error_for_status()
        .context("Mojang's version service returned an error")?
        .json()
        .context("Mojang returned an invalid version manifest")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_entries_deserialize() {
        let raw = r#"{
            "latest":{"release":"1.21.4","snapshot":"25w01a"},
            "versions":[{
                "id":"1.21.4",
                "type":"release",
                "url":"https://example.invalid/version.json",
                "releaseTime":"2025-01-01T00:00:00+00:00"
            }]
        }"#;
        let manifest: VersionManifest = serde_json::from_str(raw).unwrap();
        assert_eq!(manifest.latest.release, "1.21.4");
        assert_eq!(manifest.versions[0].kind, "release");
    }
}
