use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::{fs, io::{Cursor, Read}, path::{Component, Path, PathBuf}};
use zip::ZipArchive;

const API: &str = "https://api.modrinth.com/v2";

#[derive(Clone, Debug, Deserialize)]
pub struct Project {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub project_type: String,
    pub downloads: u64,
    pub icon_url: Option<String>,
}

#[derive(Deserialize)]
struct ProjectVersion { files: Vec<VersionFile> }

#[derive(Deserialize)]
struct VersionFile {
    url: String,
    filename: String,
    primary: bool,
    hashes: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
struct PackManifest {
    name: String,
    version_id: String,
    dependencies: std::collections::HashMap<String, String>,
    files: Vec<PackFile>,
}

#[derive(Deserialize)]
struct PackFile {
    path: String,
    hashes: std::collections::HashMap<String, String>,
    downloads: Vec<String>,
    env: Option<std::collections::HashMap<String, String>>,
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("LUNAR/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(90))
        .build()?)
}

fn safe_relative(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.as_os_str().is_empty() || path.is_absolute()
        || path.components().any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("The downloaded package contains an unsafe file path");
    }
    Ok(path.to_path_buf())
}

pub fn search(query: &str, kind: &str) -> Result<Vec<Project>> {
    if !matches!(kind, "modpack" | "resourcepack") { bail!("Unsupported Modrinth project type"); }
    let facets = format!("[[\"project_type:{kind}\"]]");
    let response = client()?.get(format!("{API}/search"))
        .query(&[("query", query), ("facets", facets.as_str()), ("limit", "30")])
        .send()?.error_for_status()?.json::<serde_json::Value>()?;
    Ok(serde_json::from_value(response.get("hits").cloned().unwrap_or_default())?)
}

pub fn install_resource_pack(project_id: &str, root: &Path) -> Result<String> {
    let http = client()?;
    let versions = http.get(format!("{API}/project/{project_id}/version")).send()?.error_for_status()?.json::<Vec<ProjectVersion>>()?;
    let version = versions.first().context("No downloadable resource-pack version was found")?;
    let file = version.files.iter().find(|file| file.primary).or_else(|| version.files.first()).context("The resource-pack version contains no files")?;
    let bytes = http.get(&file.url).send()?.error_for_status()?.bytes()?;
    if let Some(expected) = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1")) {
        if !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected) { bail!("Resource-pack checksum verification failed"); }
    }
    let directory = root.join("resourcepacks");
    fs::create_dir_all(&directory)?;
    let destination = directory.join(&file.filename);
    fs::write(&destination, bytes)?;
    Ok(destination.display().to_string())
}

pub fn install_modpack_bytes(bytes: &[u8], root: &Path) -> Result<String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).context("This is not a valid Modrinth .mrpack file")?;
    let mut manifest_file = archive.by_name("modrinth.index.json").context("Modpack manifest is missing")?;
    let mut manifest_bytes = Vec::new();
    manifest_file.read_to_end(&mut manifest_bytes)?;
    drop(manifest_file);
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes).context("Modpack manifest is invalid")?;
    if !manifest.dependencies.contains_key("minecraft") { bail!("The modpack does not specify a Minecraft version"); }
    let slug: String = manifest.name.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' }).collect();
    let slug = slug.split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-");
    if slug.is_empty() { bail!("The modpack name is invalid"); }
    let instance = root.join("modpacks").join(format!("{slug}-{}", manifest.version_id)).join("instance");
    fs::create_dir_all(&instance)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let entry_name = entry.name().to_owned();
        if let Some(relative) = entry_name.strip_prefix("overrides/") {
            let destination = instance.join(safe_relative(relative)?);
            if entry.is_dir() { fs::create_dir_all(destination)?; }
            else {
                if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
                let mut output = fs::File::create(destination)?;
                std::io::copy(&mut entry, &mut output)?;
            }
        }
    }
    let http = client()?;
    for item in &manifest.files {
        if item.env.as_ref().and_then(|env| env.get("client")).is_some_and(|value| value == "unsupported") { continue; }
        let destination = instance.join(safe_relative(&item.path)?);
        if destination.exists() { continue; }
        let mut downloaded = None;
        for url in &item.downloads {
            if let Ok(response) = http.get(url).send().and_then(reqwest::blocking::Response::error_for_status) {
                if let Ok(bytes) = response.bytes() {
                    let valid = item.hashes.get("sha1").is_none_or(|expected| format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected));
                    if valid { downloaded = Some(bytes); break; }
                }
            }
        }
        let bytes = downloaded.ok_or_else(|| anyhow!("Could not download or verify modpack file {}", item.path))?;
        if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
        fs::write(destination, bytes)?;
    }
    let profile = serde_json::json!({
        "name": manifest.name,
        "version": manifest.version_id,
        "minecraft": manifest.dependencies.get("minecraft"),
        "loaders": manifest.dependencies.iter().filter(|(key, _)| key.as_str() != "minecraft").collect::<std::collections::BTreeMap<_, _>>(),
        "instance": instance
    });
    let profile_path = instance.parent().context("Modpack directory is invalid")?.join("lunar-modpack.json");
    fs::write(profile_path, serde_json::to_vec_pretty(&profile)?)?;
    Ok(instance.display().to_string())
}

pub fn install_modpack(project_id: &str, root: &Path) -> Result<String> {
    let http = client()?;
    let versions = http.get(format!("{API}/project/{project_id}/version")).send()?.error_for_status()?.json::<Vec<ProjectVersion>>()?;
    let version = versions.first().context("No modpack version was found")?;
    let file = version.files.iter().find(|file| file.primary).or_else(|| version.files.first()).context("The modpack version contains no file")?;
    let bytes = http.get(&file.url).send()?.error_for_status()?.bytes()?;
    if let Some(expected) = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1")) {
        if !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected) { bail!("Modpack checksum verification failed"); }
    }
    install_modpack_bytes(&bytes, root)
}

pub fn import_resource_pack(source: &Path, root: &Path) -> Result<String> {
    let filename = source.file_name().context("Resource-pack filename is invalid")?;
    let destination = root.join("resourcepacks").join(filename);
    fs::create_dir_all(destination.parent().context("Resource-pack directory is invalid")?)?;
    fs::copy(source, &destination)?;
    Ok(destination.display().to_string())
}
