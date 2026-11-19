use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde::Serialize;
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
    pub downloads: u64,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(skip)]
    pub icon_rgba: Option<(usize, usize, Vec<u8>)>,
}

#[derive(Deserialize)]
struct ProjectVersion {
    #[serde(default)]
    files: Vec<VersionFile>,
    #[serde(default)]
    dependencies: Vec<VersionDependency>,
}

#[derive(Deserialize)]
struct VersionDependency {
    project_id: Option<String>,
    version_id: Option<String>,
    dependency_type: Option<String>,
}

#[derive(Deserialize)]
struct VersionFile {
    url: String,
    filename: String,
    #[serde(default)]
    primary: bool,
    hashes: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
struct PackManifest {
    name: String,
    #[serde(rename = "versionId", alias = "version_id")]
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
    if !matches!(kind, "modpack" | "resourcepack" | "mod" | "shader") { bail!("Unsupported Modrinth project type"); }
    let facets = format!("[[\"project_type:{kind}\"]]");
    let response = client()?.get(format!("{API}/search"))
        .query(&[("query", query), ("facets", facets.as_str()), ("limit", "30")])
        .send()?.error_for_status()?.json::<serde_json::Value>()?;
    let mut projects: Vec<Project> = serde_json::from_value(response.get("hits").cloned().unwrap_or_default())?;
    let http = client()?;
    for project in projects.iter_mut().take(12) {
        let Some(url) = project.icon_url.as_deref() else { continue };
        let Ok(bytes) = http.get(url).timeout(std::time::Duration::from_secs(5)).send().and_then(reqwest::blocking::Response::error_for_status).and_then(|response| response.bytes()) else { continue };
        let Ok(image) = image::load_from_memory(&bytes) else { continue };
        let image = image.resize(88, 88, image::imageops::FilterType::Triangle).to_rgba8();
        project.icon_rgba = Some((image.width() as usize, image.height() as usize, image.into_raw()));
    }
    Ok(projects)
}

fn project_versions(project_id: &str, game_versions: Option<&str>, loaders: Option<&str>) -> Result<Vec<ProjectVersion>> {
    let http = client()?;
    let mut request = http.get(format!("{API}/project/{project_id}/version"));
    let game_versions_json = game_versions.map(|version| serde_json::to_string(&vec![version])).transpose()?;
    let loaders_json = loaders.filter(|loader| !loader.is_empty()).map(|loader| serde_json::to_string(&vec![loader])).transpose()?;
    if let Some(value) = game_versions_json.as_deref() {
        request = request.query(&[("game_versions", value)]);
    }
    if let Some(value) = loaders_json.as_deref() {
        request = request.query(&[("loaders", value)]);
    }
    Ok(request.send()?.error_for_status()?.json::<Vec<ProjectVersion>>()?)
}

fn safe_filename(filename: &str) -> Result<&str> {
    let path = Path::new(filename);
    if filename.trim().is_empty()
        || filename.contains('/') || filename.contains('\\')
        || path.file_name().and_then(|name| name.to_str()) != Some(filename)
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        bail!("Modrinth returned an unsafe download filename");
    }
    Ok(filename)
}

fn save_project_file(project_id: &str, root: &Path, folder: &str, kind: &str, game_version: Option<&str>, loader: Option<&str>) -> Result<String> {
    let versions = project_versions(project_id, game_version, loader)
        .with_context(|| format!("Could not retrieve {kind} versions from Modrinth"))?;
    let version = versions.iter().find(|version| !version.files.is_empty())
        .context(if game_version.is_some() { "No compatible downloadable version was found. Check the selected Minecraft version and loader." } else { "This project has no downloadable versions." })?;
    let file = version.files.iter().find(|file| file.primary).or_else(|| version.files.first())
        .context("The selected version contains no downloadable files")?;
    let filename = safe_filename(&file.filename)?;
    if kind != "mod" && !filename.to_ascii_lowercase().ends_with(".zip") {
        bail!("The selected {kind} is not a ZIP archive");
    }
    let http = client()?;
    let bytes = http.get(&file.url).send()
        .with_context(|| format!("Could not connect to the {kind} download host"))?
        .error_for_status()
        .with_context(|| format!("The {kind} download host rejected the request"))?
        .bytes()
        .with_context(|| format!("Could not read the {kind} download"))?;
    if bytes.is_empty() { bail!("The downloaded {kind} file was empty"); }
    if let Some(expected) = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1"))
        && !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected)
    {
        bail!("{kind} checksum verification failed");
    }
    let directory = root.join(folder);
    fs::create_dir_all(&directory)?;
    let mut destination = directory.join(filename);
    if destination.exists() {
        let stem = Path::new(filename).file_stem().and_then(|value| value.to_str()).unwrap_or("pack");
        let extension = Path::new(filename).extension().and_then(|value| value.to_str()).unwrap_or("zip");
        let mut suffix = 2usize;
        loop {
            destination = directory.join(format!("{stem}-{suffix}.{extension}"));
            if !destination.exists() { break; }
            suffix += 1;
        }
    }
    let temporary = destination.with_extension(format!("{}.lunar-download", destination.extension().and_then(|value| value.to_str()).unwrap_or("zip")));
    fs::write(&temporary, &bytes)?;
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(destination.display().to_string())
}

pub fn install_resource_pack(project_id: &str, root: &Path, _minecraft: &str) -> Result<String> {
    save_project_file(project_id, root, "resourcepacks", "resource pack", None, None)
}

pub fn install_shader_pack(project_id: &str, root: &Path, _minecraft: &str) -> Result<String> {
    save_project_file(project_id, root, "shaderpacks", "shader pack", None, None)
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
        if let Some(relative) = entry_name.strip_prefix("overrides/").filter(|relative| !relative.is_empty()) {
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
            if let Ok(response) = http.get(url).send().and_then(reqwest::blocking::Response::error_for_status)
                && let Ok(bytes) = response.bytes()
            {
                let valid = item.hashes.get("sha1").is_none_or(|expected| {
                    format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected)
                });
                if valid {
                    downloaded = Some(bytes);
                    break;
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
    if let Some(expected) = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1"))
        && !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected)
    {
        bail!("Modpack checksum verification failed");
    }
    install_modpack_bytes(&bytes, root)
}


#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledPack {
    pub name: String,
    pub version: String,
    pub minecraft: String,
    pub loaders: std::collections::BTreeMap<String, String>,
    pub instance: PathBuf,
}

pub fn installed_modpacks(root: &Path) -> Result<Vec<InstalledPack>> {
    let mut packs = Vec::new();
    let directory = root.join("modpacks");
    if !directory.exists() { return Ok(packs); }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let instance = entry.path().join("instance");
        let profile_path = entry.path().join("lunar-modpack.json");
        if !profile_path.exists() { continue; }
        let value: ValuePack = serde_json::from_slice(&fs::read(profile_path)?)?;
        packs.push(InstalledPack {
            name: value.name,
            version: value.version,
            minecraft: value.minecraft,
            loaders: value.loaders,
            instance,
        });
    }
    packs.sort_by_key(|pack| pack.name.to_lowercase());
    Ok(packs)
}

#[derive(Deserialize)]
struct ValuePack {
    name: String,
    version: String,
    minecraft: String,
    #[serde(default)]
    loaders: std::collections::BTreeMap<String, String>,
}

fn download_mod_version(version: &ProjectVersion, directory: &Path, seen: &mut std::collections::HashSet<String>, depth: usize, minecraft: &str, loader: &str) -> Result<Vec<String>> {
    if depth > 8 { bail!("Mod dependency chain is unexpectedly deep"); }
    fs::create_dir_all(directory)?;
    let file = version.files.iter().find(|file| file.primary).or_else(|| version.files.first())
        .context("Mod version contains no downloadable file")?;
    let filename = safe_filename(&file.filename)?;
    if !filename.to_ascii_lowercase().ends_with(".jar") { bail!("Modrinth returned a non-JAR mod file"); }
    let mut paths = Vec::new();
    let mut destination = directory.join(filename);
    if !destination.exists() {
        let bytes = client()?.get(&file.url).send()?.error_for_status()?.bytes()?;
        if bytes.is_empty() { bail!("The downloaded mod file was empty"); }
        if let Some(expected) = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1"))
            && !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected)
        {
            bail!("Mod checksum verification failed");
        }
        let temporary = destination.with_extension("jar.lunar-download");
        fs::write(&temporary, &bytes)?;
        fs::rename(&temporary, &destination)?;
    }
    paths.push(destination.display().to_string());
    for dependency in version.dependencies.iter().filter(|dependency| dependency.dependency_type.as_deref() == Some("required")) {
        let key = dependency.project_id.as_deref().or(dependency.version_id.as_deref()).unwrap_or_default();
        if key.is_empty() || !seen.insert(key.to_owned()) { continue; }
        let dependency_version = if let Some(version_id) = dependency.version_id.as_deref() {
            client()?.get(format!("{API}/version/{version_id}")).send()?.error_for_status()?.json::<ProjectVersion>()?
        } else if let Some(project_id) = dependency.project_id.as_deref() {
            project_versions(project_id, Some(minecraft), Some(loader))?
                .into_iter().find(|candidate| !candidate.files.is_empty())
                .with_context(|| format!("A required dependency ({project_id}) has no compatible release for Minecraft {minecraft} and {loader}"))?
        } else {
            continue;
        };
        paths.extend(download_mod_version(&dependency_version, directory, seen, depth + 1, minecraft, loader)?);
    }
    Ok(paths)
}

pub fn install_mod(project_id: &str, root: &Path, minecraft: &str, loader: &str) -> Result<String> {
    let versions = project_versions(project_id, Some(minecraft), Some(loader))
        .with_context(|| format!("Could not retrieve mod versions for Minecraft {minecraft}"))?;
    let version = versions.iter().find(|version| !version.files.is_empty())
        .context("No mod version matches the selected Minecraft version and loader")?;
    let mut seen = std::collections::HashSet::new();
    seen.insert(project_id.to_owned());
    let paths = download_mod_version(version, &root.join("mods"), &mut seen, 0, minecraft, loader)?;
    Ok(paths.join("\n"))
}

pub fn list_files(root: &Path, kind: &str) -> Result<Vec<PathBuf>> {
    let directory = root.join(kind);
    if !directory.exists() { return Ok(Vec::new()); }
    let mut files = fs::read_dir(directory)?.filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path()).collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

pub fn set_file_enabled(path: &Path, enabled: bool) -> Result<PathBuf> {
    let parent = path.parent().context("Content path is invalid")?.canonicalize()?;
    let target = path.canonicalize()?;
    if !target.starts_with(&parent) || !target.is_file() {
        bail!("Selected content is not a regular file in its managed folder");
    }
    let name = target.file_name().and_then(|value| value.to_str()).context("Invalid content filename")?;
    let disabled = name.ends_with(".disabled");
    if enabled == !disabled {
        return Ok(target);
    }
    let new_name = if enabled { name.strip_suffix(".disabled").unwrap_or(name).to_owned() } else { format!("{name}.disabled") };
    let destination = parent.join(new_name);
    if destination.exists() { bail!("A file with the target name already exists"); }
    fs::rename(&target, &destination)?;
    Ok(destination)
}

pub fn remove_file(path: &Path, root: &Path, kind: &str) -> Result<()> {
    let directory = root.join(kind).canonicalize()?;
    let target = path.canonicalize()?;
    if !target.starts_with(&directory) || !target.is_file() { bail!("Selected content file is outside its managed directory"); }
    fs::remove_file(target)?;
    Ok(())
}

pub fn remove_modpack(pack: &InstalledPack, root: &Path) -> Result<()> {
    let directory = root.join("modpacks").canonicalize()?;
    let target = pack.instance.parent().context("Modpack instance path is invalid")?.canonicalize()?;
    if !target.starts_with(&directory) || !target.join("lunar-modpack.json").exists() { bail!("Selected folder is not a managed LUNAR modpack"); }
    fs::remove_dir_all(target)?;
    Ok(())
}

fn option_list(root: &Path, key: &str) -> Vec<String> {
    let text = fs::read_to_string(root.join("options.txt")).unwrap_or_default();
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name != key { return None; }
        serde_json::from_str::<Vec<String>>(value).ok()
    }).unwrap_or_default()
}

pub fn resource_pack_enabled(root: &Path, filename: &str) -> bool {
    option_list(root, "resourcePacks").contains(&format!("file/{filename}"))
}

pub fn set_resource_pack_enabled(root: &Path, filename: &str, enabled: bool) -> Result<()> {
    fs::create_dir_all(root)?;
    let path = root.join("options.txt");
    let source = fs::read_to_string(&path).unwrap_or_default();
    let token = format!("file/{filename}");
    let mut active = option_list(root, "resourcePacks");
    let inactive = option_list(root, "incompatibleResourcePacks");
    active.retain(|value| value != &token);
    if enabled { active.push(token); }
    let mut output = Vec::new();
    let mut active_written = false;
    let mut inactive_written = false;
    for line in source.lines() {
        if let Some((key, _)) = line.split_once(':') {
            if key == "resourcePacks" {
                output.push(format!("resourcePacks:{}", serde_json::to_string(&active)?));
                active_written = true;
                continue;
            }
            if key == "incompatibleResourcePacks" {
                output.push(format!("incompatibleResourcePacks:{}", serde_json::to_string(&inactive)?));
                inactive_written = true;
                continue;
            }
        }
        output.push(line.to_owned());
    }
    if !active_written { output.push(format!("resourcePacks:{}", serde_json::to_string(&active)?)); }
    if !inactive_written { output.push(format!("incompatibleResourcePacks:{}", serde_json::to_string(&inactive)?)); }
    fs::write(path, output.join("\n"))?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filename_rejects_paths() {
        assert!(safe_filename("valid-pack.zip").is_ok());
        assert!(safe_filename("../outside.zip").is_err());
        assert!(safe_filename("folder\\\\outside.zip").is_err());
    }

    #[test]
    fn resource_pack_toggle_does_not_mark_disabled_packs_incompatible() {
        let root = std::env::temp_dir().join(format!("lunar-resource-toggle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        set_resource_pack_enabled(&root, "sample.zip", true).unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert!(options.contains("resourcePacks:[\\"file/sample.zip\\"]"));
        assert!(options.contains("incompatibleResourcePacks:[]"));
        set_resource_pack_enabled(&root, "sample.zip", false).unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert!(options.contains("resourcePacks:[]"));
        assert!(options.contains("incompatibleResourcePacks:[]"));
        let _ = fs::remove_dir_all(root);
    }
}
