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
    project_id: Option<String>,
    #[serde(default)]
    files: Vec<VersionFile>,
    #[serde(default)]
    dependencies: Vec<VersionDependency>,
}

#[derive(Clone, Deserialize)]
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
        || value.contains(['\\', ':'])
        || path.components().any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("The downloaded package contains an unsafe file path");
    }
    Ok(path.to_path_buf())
}


pub fn mark_project_installed(root: &Path, kind: &str, project_id: &str, installed_paths: &str) -> Result<()> {
    let path = root.join(".lunar-installed-content.json");
    let mut registry: serde_json::Value = if path.exists() {
        serde_json::from_slice(&fs::read(&path)?)?
    } else {
        serde_json::json!({})
    };
    let entries = registry.as_object_mut().context("Installed content registry is invalid")?;
    let projects = entries.entry(kind.to_owned()).or_insert_with(|| serde_json::json!({}));
    let projects = projects.as_object_mut().context("Installed content registry entry is invalid")?;
    let paths = installed_paths.lines().filter(|value| !value.trim().is_empty()).collect::<Vec<_>>();
    projects.insert(project_id.to_owned(), serde_json::json!(paths));
    fs::write(path, serde_json::to_vec_pretty(&registry)?)?;
    Ok(())
}

pub fn project_is_installed(root: &Path, kind: &str, project_id: &str) -> bool {
    let path = root.join(".lunar-installed-content.json");
    fs::read(path).ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|registry| registry.get(kind).and_then(serde_json::Value::as_object).cloned())
        .and_then(|projects| projects.get(project_id).and_then(serde_json::Value::as_array).cloned())
        .is_some_and(|paths| !paths.is_empty() && paths.iter().all(|value| {
            value.as_str().is_some_and(|path| Path::new(path).exists())
        }))
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
    if kind != "mod" {
        let archive = ZipArchive::new(Cursor::new(bytes.as_ref()))
            .with_context(|| format!("The downloaded {kind} is not a valid ZIP archive"))?;
        if archive.is_empty() { bail!("The downloaded {kind} archive contains no files"); }
        drop(archive);
    }
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

pub fn install_nyx_client(instance: &Path, minecraft: &str, loader: &str) -> Result<String> {
    if minecraft != "1.21.1" || loader != "fabric" {
        bail!("Nyx Client currently supports Minecraft 1.21.1 with Fabric only");
    }
    let http = client()?;
    let releases = http.get("https://api.github.com/repos/dontcare-dev/lunar-launcher/releases?per_page=20")
        .send()?.error_for_status()?.json::<Vec<serde_json::Value>>()?;
    let asset_url = releases.iter()
        .filter(|release| release.get("tag_name").and_then(serde_json::Value::as_str).is_some_and(|tag| tag.starts_with("nyx-client-build-")))
        .find_map(|release| release.get("assets").and_then(serde_json::Value::as_array).and_then(|assets| {
            assets.iter().find(|asset| asset.get("name").and_then(serde_json::Value::as_str) == Some("nyx-client.jar"))
                .and_then(|asset| asset.get("browser_download_url"))
                .and_then(serde_json::Value::as_str)
        }))
        .context("No published Nyx Client build is available yet")?;
    let bytes = http.get(asset_url).send()?.error_for_status()?.bytes()?;
    if bytes.is_empty() { bail!("The Nyx Client download was empty"); }
    let mut archive = ZipArchive::new(Cursor::new(bytes.as_ref())).context("The Nyx Client download is not a valid JAR")?;
    if archive.by_name("fabric.mod.json").is_err() {
        bail!("The downloaded file is not a valid Fabric client mod");
    }
    drop(archive);
    let directory = instance.join("mods");
    fs::create_dir_all(&directory)?;
    let destination = directory.join("nyx-client.jar");
    let temporary = directory.join("nyx-client.jar.lunar-download");
    fs::write(&temporary, &bytes)?;
    if destination.exists() {
        fs::remove_file(&destination)?;
    }
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
    let mut recovered_dependencies = Vec::<VersionDependency>::new();
    for item in &manifest.files {
        if item.env.as_ref().and_then(|env| env.get("client")).is_some_and(|value| value == "unsupported") { continue; }
        let destination = instance.join(safe_relative(&item.path)?);
        if destination.exists() { continue; }
        let mut downloaded = None;
        for url in &item.downloads {
            for attempt in 0..3 {
                if let Ok(response) = http.get(url).send().and_then(reqwest::blocking::Response::error_for_status)
                    && let Ok(bytes) = response.bytes()
                {
                    let valid = item.hashes.get("sha1").is_none_or(|expected| {
                        format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected)
                    });
                    if valid && !bytes.is_empty() {
                        downloaded = Some(bytes);
                        break;
                    }
                }
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(350 * (attempt + 1) as u64));
                }
            }
            if downloaded.is_some() { break; }
        }
        if downloaded.is_none()
            && let Some(hash) = item.hashes.get("sha1")
            && let Ok(response) = http.get(format!("{API}/version_file/{hash}?algorithm=sha1")).send()
            && let Ok(response) = response.error_for_status()
            && let Ok(version) = response.json::<serde_json::Value>()
        {
            if let Some(dependencies) = version.get("dependencies").cloned()
                && let Ok(dependencies) = serde_json::from_value::<Vec<VersionDependency>>(dependencies)
            {
                recovered_dependencies.extend(dependencies);
            }
            if let Some(files) = version.get("files").and_then(serde_json::Value::as_array) {
                for file in files {
                    let Some(url) = file.get("url").and_then(serde_json::Value::as_str) else { continue };
                    if let Ok(response) = http.get(url).send().and_then(reqwest::blocking::Response::error_for_status)
                        && let Ok(bytes) = response.bytes()
                        && !bytes.is_empty()
                        && format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(hash)
                    {
                        downloaded = Some(bytes);
                        break;
                    }
                }
            }
        }
        let bytes = downloaded.ok_or_else(|| anyhow!("Could not download or verify modpack file {} after retries and Modrinth hash recovery", item.path))?;
        if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
        fs::write(destination, bytes)?;
    }
    if !recovered_dependencies.is_empty() {
        let minecraft = manifest.dependencies.get("minecraft").context("Modpack Minecraft version is missing")?;
        let loader = manifest.dependencies.iter()
            .find(|(key, _)| key.as_str() != "minecraft")
            .map(|(key, _)| key.trim_end_matches("-loader").to_owned())
            .unwrap_or_else(|| "fabric".to_owned());
        let mods_directory = instance.join("mods");
        let mut seen = std::collections::HashSet::new();
        for dependency in recovered_dependencies.into_iter().filter(|dependency| dependency.dependency_type.as_deref() == Some("required")) {
            let key = dependency.project_id.as_deref().or(dependency.version_id.as_deref()).unwrap_or_default();
            if key.is_empty() || !seen.insert(key.to_owned()) { continue; }
            let version = if let Some(version_id) = dependency.version_id.as_deref() {
                http.get(format!("{API}/version/{version_id}")).send()?.error_for_status()?.json::<ProjectVersion>()?
            } else if let Some(project_id) = dependency.project_id.as_deref() {
                project_versions(project_id, Some(minecraft), Some(&loader))?
                    .into_iter().find(|version| !version.files.is_empty())
                    .with_context(|| format!("Required mod dependency {project_id} has no compatible release"))?
            } else {
                continue;
            };
            download_mod_version(&version, &mods_directory, &mut seen, 0, minecraft, &loader)
                .with_context(|| format!("Could not install required mod dependency {key}"))?;
        }
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

fn install_required_mod_dependencies(dependencies: &[VersionDependency], instance: &Path, minecraft: &str, loader: &str) -> Result<Vec<String>> {
    let http = client()?;
    let mods_directory = instance.join("mods");
    let mut seen = std::collections::HashSet::new();
    let mut installed = Vec::new();
    for dependency in dependencies.iter().filter(|dependency| dependency.dependency_type.as_deref() == Some("required")) {
        let key = dependency.project_id.as_deref().or(dependency.version_id.as_deref()).unwrap_or_default();
        if key.is_empty() || !seen.insert(key.to_owned()) { continue; }
        let version = if let Some(version_id) = dependency.version_id.as_deref() {
            http.get(format!("{API}/version/{version_id}")).send()?.error_for_status()?.json::<ProjectVersion>()?
        } else if let Some(project_id) = dependency.project_id.as_deref() {
            let metadata = http.get(format!("{API}/project/{project_id}")).send()?.error_for_status()?.json::<serde_json::Value>()?;
            match metadata.get("project_type").and_then(serde_json::Value::as_str) {
                Some("mod") => {
                    let version = project_versions(project_id, Some(minecraft), Some(loader))?
                        .into_iter().find(|version| !version.files.is_empty())
                        .with_context(|| format!("Required mod dependency {project_id} has no compatible release for Minecraft {minecraft} and {loader}"))?;
                    let paths = download_mod_version(&version, &mods_directory, &mut seen, 0, minecraft, loader)
                        .with_context(|| format!("Could not install required mod dependency {project_id}"))?;
                    installed.extend(paths);
                    continue;
                }
                Some("resourcepack") => {
                    installed.push(save_project_file(project_id, instance, "resourcepacks", "required resource pack", None, None)?);
                    continue;
                }
                Some("shader") => {
                    installed.push(save_project_file(project_id, instance, "shaderpacks", "required shader pack", None, None)?);
                    continue;
                }
                Some(kind) => bail!("Required dependency {project_id} is a {kind}; Lunar cannot merge that project type into this modpack yet"),
                None => bail!("Modrinth did not identify the type of required dependency {project_id}"),
            }
        } else {
            continue;
        };
        if let Some(project_id) = version.project_id.as_deref() {
            let metadata = http.get(format!("{API}/project/{project_id}")).send()?.error_for_status()?.json::<serde_json::Value>()?;
            if metadata.get("project_type").and_then(serde_json::Value::as_str) != Some("mod") {
                bail!("Required version dependency {key} is not a Minecraft mod");
            }
        }
        installed.extend(download_mod_version(&version, &mods_directory, &mut seen, 0, minecraft, loader)
            .with_context(|| format!("Could not install required mod dependency {key}"))?);
    }
    Ok(installed)
}

pub fn install_modpack(project_id: &str, root: &Path) -> Result<String> {
    let http = client()?;
    let versions = http.get(format!("{API}/project/{project_id}/version")).send()?.error_for_status()?.json::<Vec<ProjectVersion>>()?;
    let version = versions.first().context("No modpack version was found")?;
    let dependencies = version.dependencies.clone();
    let file = version.files.iter().find(|file| file.primary).or_else(|| version.files.first()).context("The modpack version contains no file")?;
    let expected = file.hashes.as_ref().and_then(|hashes| hashes.get("sha1"));
    let mut downloaded = None;
    for attempt in 0..3 {
        if let Ok(response) = http.get(&file.url).send().and_then(reqwest::blocking::Response::error_for_status)
            && let Ok(bytes) = response.bytes()
            && !bytes.is_empty()
            && expected.is_none_or(|hash| format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(hash))
        {
            downloaded = Some(bytes);
            break;
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(500 * (attempt + 1) as u64));
        }
    }
    let bytes = downloaded.context("Could not download or verify the modpack archive after three attempts")?;
    let instance_path = install_modpack_bytes(&bytes, root)?;
    if !dependencies.is_empty() {
        let instance = PathBuf::from(&instance_path);
        let profile_path = instance.parent().context("Modpack directory is invalid")?.join("lunar-modpack.json");
        let profile: serde_json::Value = serde_json::from_slice(&fs::read(profile_path)?)?;
        let minecraft = profile.get("minecraft").and_then(serde_json::Value::as_str).context("Modpack Minecraft version is missing")?;
        let loader = profile.get("loaders").and_then(serde_json::Value::as_object)
            .and_then(|loaders| loaders.iter().find(|(key, _)| key.as_str() != "minecraft"))
            .map(|(key, _)| key.trim_end_matches("-loader").to_owned())
            .unwrap_or_else(|| "fabric".to_owned());
        install_required_mod_dependencies(&dependencies, &instance, minecraft, &loader)?;
    }
    Ok(instance_path)
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
    let destination = directory.join(filename);
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
        assert!(options.contains("resourcePacks:[\"file/sample.zip\"]"));
        assert!(options.contains("incompatibleResourcePacks:[]"));
        set_resource_pack_enabled(&root, "sample.zip", false).unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert!(options.contains("resourcePacks:[]"));
        assert!(options.contains("incompatibleResourcePacks:[]"));
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn safe_relative_accepts_nested_normal_paths() {
        assert_eq!(safe_relative("overrides/config/example.json").unwrap(), PathBuf::from("overrides/config/example.json"));
    }

    #[test]
    fn safe_relative_rejects_parent_traversal() {
        assert!(safe_relative("../outside").is_err());
        assert!(safe_relative("a/../../outside").is_err());
    }

    #[test]
    fn safe_relative_rejects_absolute_and_empty_paths() {
        assert!(safe_relative("").is_err());
        assert!(safe_relative("/tmp/outside").is_err());
        assert!(safe_relative("C:\\outside").is_err());
    }

    #[test]
    fn safe_filename_rejects_empty_and_dot_names() {
        assert!(safe_filename("").is_err());
        assert!(safe_filename(".").is_err());
        assert!(safe_filename("..").is_err());
    }

    #[test]
    fn resource_pack_enable_is_idempotent() {
        let root = std::env::temp_dir().join(format!("lunar-resource-idempotent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        set_resource_pack_enabled(&root, "pack.zip", true).unwrap();
        set_resource_pack_enabled(&root, "pack.zip", true).unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert_eq!(options.matches("file/pack.zip").count(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resource_pack_disable_preserves_other_enabled_packs() {
        let root = std::env::temp_dir().join(format!("lunar-resource-preserve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        set_resource_pack_enabled(&root, "one.zip", true).unwrap();
        set_resource_pack_enabled(&root, "two.zip", true).unwrap();
        set_resource_pack_enabled(&root, "one.zip", false).unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert!(!options.contains("file/one.zip"));
        assert!(options.contains("file/two.zip"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unknown_project_types_before_network_access() {
        assert!(search("", "unknown").is_err());
    }

    #[test]
    fn installed_project_status_tracks_installed_files() {
        let root = std::env::temp_dir().join(format!("lunar-installed-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let installed = root.join("mods").join("sample.jar");
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, b"test").unwrap();
        mark_project_installed(&root, "mod", "project-123", installed.to_str().unwrap()).unwrap();
        assert!(project_is_installed(&root, "mod", "project-123"));
        fs::remove_file(&installed).unwrap();
        assert!(!project_is_installed(&root, "mod", "project-123"));
        let _ = fs::remove_dir_all(root);
    }

}
