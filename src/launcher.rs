use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{atomic::{AtomicUsize, Ordering}, Arc, Mutex},
};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Clone, Debug)]
pub struct InstallProgress {
    pub phase: String,
    pub completed: usize,
    pub total: usize,
}

#[derive(Clone, Debug)]
pub struct InstalledVersion {
    pub id: String,
    pub directory: PathBuf,
    pub main_class: String,
    pub assets: String,
    pub game_assets: PathBuf,
    pub libraries: Vec<PathBuf>,
    pub natives: PathBuf,
    pub metadata: Value,
    pub game_directory: Option<PathBuf>,
}

pub fn game_directory() -> Result<PathBuf> {
    let base = directories::ProjectDirs::from("dev", "SilverDevStudios", "LUNAR")
        .context("Could not locate the application data directory")?;
    let path = base.data_dir().join("minecraft");
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("LUNAR/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(90))
        .build()?)
}

fn fetch_json(client: &reqwest::blocking::Client, url: &str) -> Result<Value> {
    Ok(client.get(url).send()?.error_for_status()?.json()?)
}

fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn download_once(client: &reqwest::blocking::Client, url: &str, destination: &Path, expected: Option<&str>) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if destination.exists() {
        if let Some(hash) = expected {
            if digest(destination).is_ok_and(|actual| actual.eq_ignore_ascii_case(hash)) {
                return Ok(());
            }
        } else if fs::metadata(destination).is_ok_and(|metadata| metadata.len() > 0) {
            return Ok(());
        }
    }
    let mut response = client.get(url).send()?.error_for_status()?;
    let temporary = destination.with_extension("lunar-download");
    let mut output = File::create(&temporary)?;
    let mut hasher = Sha1::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    output.flush()?;
    drop(output);
    if let Some(expected) = expected {
        let actual = format!("{:x}", hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(&temporary);
            bail!("Checksum mismatch while downloading {url}");
        }
    }
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    fs::rename(temporary, destination)?;
    Ok(())
}


fn maven_artifact_path(coordinate: &str, classifier: Option<&str>) -> Option<String> {
    let (coordinate, extension) = coordinate.split_once('@').unwrap_or((coordinate, "jar"));
    let mut parts = coordinate.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    let embedded_classifier = parts.next();
    let classifier = classifier.or(embedded_classifier);
    let suffix = classifier.map(|value| format!("-{value}")).unwrap_or_default();
    Some(format!(
        "{}/{}/{}/{}-{}{}.{}",
        group.replace('.', "/"),
        artifact,
        version,
        artifact,
        version,
        suffix,
        extension
    ))
}

fn download(client: &reqwest::blocking::Client, url: &str, destination: &Path, expected: Option<&str>) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..3 {
        match download_once(client, url, destination, expected) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_secs(attempt + 1));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("Download failed for {url}")))
}

fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

fn rules_allow(value: &Value) -> bool {
    let Some(rules) = value.get("rules").and_then(Value::as_array) else {
        return true;
    };
    let mut allowed = false;
    for rule in rules {
        let os_matches = rule.get("os")
            .and_then(|os| os.get("name"))
            .and_then(Value::as_str)
            .is_none_or(|name| name == current_os());
        let arch_matches = rule.get("os")
            .and_then(|os| os.get("arch"))
            .and_then(Value::as_str)
            .is_none_or(|arch| {
                if cfg!(target_arch = "x86_64") { arch == "x86_64" || arch == "amd64" }
                else if cfg!(target_arch = "aarch64") { arch == "aarch64" || arch == "arm64" }
                else { true }
            });
        let feature_matches = rule.get("features").and_then(Value::as_object).is_none_or(|features| features.values().all(|value| value.as_bool() == Some(false)));
        if os_matches && arch_matches && feature_matches {
            allowed = rule.get("action").and_then(Value::as_str) == Some("allow");
        }
    }
    allowed
}

fn safe_extract_natives(archive: &Path, destination: &Path, excludes: &[String]) -> Result<()> {
    fs::create_dir_all(destination)?;
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let name = entry.name().to_owned();
        if entry.is_dir() || excludes.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        let relative = Path::new(&name);
        if relative.components().any(|component| !matches!(component, std::path::Component::Normal(_))) {
            continue;
        }
        let output = destination.join(relative);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut target = File::create(output)?;
        std::io::copy(&mut entry, &mut target)?;
    }
    Ok(())
}


fn materialize_legacy_assets(root: &Path, asset_index: &Value, objects: &serde_json::Map<String, Value>, index_id: &str) -> Result<()> {
    let virtual_assets = asset_index.get("virtual").and_then(Value::as_bool).unwrap_or(false);
    let map_resources = asset_index.get("map_to_resources").and_then(Value::as_bool).unwrap_or(false);
    if !virtual_assets && !map_resources {
        return Ok(());
    }
    for (name, object) in objects {
        let hash = object.get("hash").and_then(Value::as_str).context("Asset hash missing")?;
        let relative = Path::new(name);
        if relative.components().any(|component| !matches!(component, std::path::Component::Normal(_))) {
            continue;
        }
        let source = root.join("assets").join("objects").join(&hash[..2]).join(hash);
        let destination = if virtual_assets {
            root.join("assets").join("virtual").join(index_id).join(relative)
        } else {
            root.join("resources").join(relative)
        };
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        if destination.exists() {
            continue;
        }
        if fs::hard_link(&source, &destination).is_err() {
            fs::copy(&source, &destination)?;
        }
    }
    Ok(())
}

fn download_assets(
    http: &reqwest::blocking::Client,
    root: &Path,
    objects: &serde_json::Map<String, Value>,
    mut progress: impl FnMut(InstallProgress),
) -> Result<()> {
    let tasks = objects.values().map(|object| {
        let hash = object.get("hash").and_then(Value::as_str).unwrap_or_default().to_owned();
        let prefix = hash.get(..2).unwrap_or_default();
        let path = root.join("assets").join("objects").join(prefix).join(&hash);
        let url = format!("https://resources.download.minecraft.net/{prefix}/{hash}");
        (url, path, hash)
    }).collect::<Vec<_>>();
    if tasks.iter().any(|(_, _, hash)| hash.len() < 2 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())) {
        bail!("Asset index contains an invalid SHA-1 hash");
    }
    let total = tasks.len();
    let queue = Arc::new(Mutex::new(VecDeque::from(tasks)));
    let completed = Arc::new(AtomicUsize::new(0));
    let failure = Arc::new(Mutex::new(None::<String>));
    let workers = total.clamp(1, 8);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let completed = Arc::clone(&completed);
            let failure = Arc::clone(&failure);
            let http = http.clone();
            scope.spawn(move || loop {
                if failure.lock().is_ok_and(|error| error.is_some()) {
                    break;
                }
                let task = queue.lock().ok().and_then(|mut queue| queue.pop_front());
                let Some((url, path, hash)) = task else { break };
                if let Err(error) = download(&http, &url, &path, Some(&hash)) {
                    if let Ok(mut slot) = failure.lock()
                        && slot.is_none()
                    {
                        *slot = Some(format!("{error:#}"));
                    }
                    break;
                }
                completed.fetch_add(1, Ordering::Relaxed);
            });
        }
        let mut last_reported = 0;
        loop {
            let count = completed.load(Ordering::Relaxed);
            if count >= total || failure.lock().is_ok_and(|error| error.is_some()) {
                break;
            }
            if count >= last_reported + 25 {
                progress(InstallProgress { phase: "Assets".into(), completed: count, total });
                last_reported = count;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    if let Some(error) = failure.lock().map_err(|_| anyhow!("Asset worker failed"))?.take() {
        bail!("Asset download failed: {error}");
    }
    progress(InstallProgress { phase: "Assets".into(), completed: total, total });
    Ok(())
}

pub fn install(version_id: &str, root: &Path, mut progress: impl FnMut(InstallProgress)) -> Result<InstalledVersion> {
    let http = client()?;
    let manifest = fetch_json(&http, MANIFEST_URL)?;
    let entry = manifest.get("versions").and_then(Value::as_array)
        .and_then(|versions| versions.iter().find(|entry| entry.get("id").and_then(Value::as_str) == Some(version_id)))
        .ok_or_else(|| anyhow!("Minecraft version {version_id} was not found in the official manifest"))?;
    let metadata_url = entry.get("url").and_then(Value::as_str).context("Version manifest entry has no metadata URL")?;
    let directory = root.join("versions").join(version_id);
    fs::create_dir_all(&directory)?;
    let install_marker = directory.join(".lunar-installed");
    if install_marker.exists() {
        fs::remove_file(&install_marker)?;
    }
    let metadata_path = directory.join(format!("{version_id}.json"));
    download(&http, metadata_url, &metadata_path, entry.get("sha1").and_then(Value::as_str))?;
    let metadata: Value = serde_json::from_slice(&fs::read(&metadata_path)?)?;
    let client_jar = directory.join(format!("{version_id}.jar"));
    let client_download = metadata.pointer("/downloads/client").context("This version does not provide a client download")?;
    download(
        &http,
        client_download.get("url").and_then(Value::as_str).context("Client JAR URL missing")?,
        &client_jar,
        client_download.get("sha1").and_then(Value::as_str),
    )?;
    progress(InstallProgress { phase: "Minecraft client".into(), completed: 1, total: 1 });

    let libraries_root = root.join("libraries");
    let natives = root.join("versions").join(version_id).join("natives");
    if natives.exists() {
        fs::remove_dir_all(&natives)?;
    }
    fs::create_dir_all(&natives)?;
    let mut libraries = Vec::new();
    let entries = metadata.get("libraries").and_then(Value::as_array).cloned().unwrap_or_default();
    let total = entries.len();
    for (index, library) in entries.into_iter().enumerate() {
        let completed = index + 1;
        if !rules_allow(&library) {
            progress(InstallProgress { phase: "Libraries".into(), completed, total });
            continue;
        }
        let downloads = library.get("downloads").cloned().unwrap_or_else(|| json!({}));
        let base_url = library.get("url").and_then(Value::as_str).unwrap_or("https://libraries.minecraft.net/");
        if let Some(artifact) = downloads.get("artifact") {
            if let (Some(url), Some(relative)) = (
                artifact.get("url").and_then(Value::as_str),
                artifact.get("path").and_then(Value::as_str),
            ) {
                let path = libraries_root.join(relative);
                download(&http, url, &path, artifact.get("sha1").and_then(Value::as_str))?;
                libraries.push(path);
            }
        } else if let Some(coordinate) = library.get("name").and_then(Value::as_str)
            && let Some(relative) = maven_artifact_path(coordinate, None)
        {
            let path = libraries_root.join(&relative);
            let url = format!("{}/{}", base_url.trim_end_matches('/'), relative);
            download(&http, &url, &path, None)?;
            libraries.push(path);
        }
        let native_key = library.get("natives")
            .and_then(|natives| natives.get(current_os()))
            .and_then(Value::as_str)
            .map(|value| value.replace("${arch}", if cfg!(target_arch = "x86_64") { "64" } else { "32" }));
        if let Some(native_key) = native_key {
            let modern_native = downloads.pointer(&format!("/classifiers/{}", native_key));
            let native_file = if let Some(native) = modern_native {
                if let (Some(url), Some(relative)) = (
                    native.get("url").and_then(Value::as_str),
                    native.get("path").and_then(Value::as_str),
                ) {
                    let path = libraries_root.join(relative);
                    download(&http, url, &path, native.get("sha1").and_then(Value::as_str))?;
                    Some(path)
                } else {
                    None
                }
            } else if let Some(coordinate) = library.get("name").and_then(Value::as_str) {
                if let Some(relative) = maven_artifact_path(coordinate, Some(&native_key)) {
                    let path = libraries_root.join(&relative);
                    let url = format!("{}/{}", base_url.trim_end_matches('/'), relative);
                    download(&http, &url, &path, None)?;
                    Some(path)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(path) = native_file {
                let excludes = library.pointer("/extract/exclude")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>())
                    .unwrap_or_default();
                safe_extract_natives(&path, &natives, &excludes)?;
            }
        }
        progress(InstallProgress { phase: "Libraries".into(), completed, total });
    }

    let asset_index_info = metadata.get("assetIndex").context("Asset index metadata missing")?;
    let asset_index_id = asset_index_info.get("id").and_then(Value::as_str).context("Asset index ID missing")?;
    let asset_index_path = root.join("assets").join("indexes").join(format!("{asset_index_id}.json"));
    download(
        &http,
        asset_index_info.get("url").and_then(Value::as_str).context("Asset index URL missing")?,
        &asset_index_path,
        asset_index_info.get("sha1").and_then(Value::as_str),
    )?;
    let asset_index: Value = serde_json::from_slice(&fs::read(&asset_index_path)?)?;
    let objects = asset_index.get("objects").and_then(Value::as_object).context("Asset index objects missing")?;
    download_assets(&http, root, objects, &mut progress)?;
    materialize_legacy_assets(root, &asset_index, objects, asset_index_id)?;
    if let Some(logging_file) = metadata.pointer("/logging/client/file")
        && let (Some(id), Some(url)) = (
            logging_file.get("id").and_then(Value::as_str),
            logging_file.get("url").and_then(Value::as_str),
        )
    {
        let path = root.join("assets").join("log_configs").join(id);
        download(&http, url, &path, logging_file.get("sha1").and_then(Value::as_str))?;
    }

    libraries.push(client_jar.clone());
    let main_class = metadata.get("mainClass").and_then(Value::as_str).context("Minecraft main class missing")?.to_owned();
    let game_assets = if asset_index.get("virtual").and_then(Value::as_bool).unwrap_or(false) {
        root.join("assets").join("virtual").join(asset_index_id)
    } else if asset_index.get("map_to_resources").and_then(Value::as_bool).unwrap_or(false) {
        root.join("resources")
    } else {
        root.join("assets")
    };
    fs::write(directory.join(".lunar-installed"), version_id.as_bytes())?;
    Ok(InstalledVersion {
        id: version_id.to_owned(),
        directory,
        main_class,
        assets: metadata.get("assets").and_then(Value::as_str).unwrap_or(asset_index_id).to_owned(),
        game_assets,
        libraries,
        natives,
        metadata,
        game_directory: None,
    })
}

fn allowed_arguments(value: &Value) -> Vec<String> {
    let mut output = Vec::new();
    let Some(items) = value.as_array() else {
        return output;
    };
    for item in items {
        if let Some(text) = item.as_str() {
            output.push(text.to_owned());
        } else if item.is_object() && rules_allow(item)
            && let Some(values) = item.get("value")
        {
            if let Some(text) = values.as_str() {
                output.push(text.to_owned());
            } else if let Some(values) = values.as_array() {
                output.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
    }
    output
}

fn replace_arguments(arguments: &mut [String], values: &[(&str, String)]) {
    for argument in arguments {
        for (key, value) in values {
            *argument = argument.replace(key, value);
        }
    }
}

fn parse_java_major(version_line: &str) -> Option<u32> {
    let version = version_line.split('"').nth(1).unwrap_or(version_line);
    let mut parts = version.split('.');
    let first = parts.next()?.parse::<u32>().ok()?;
    if first == 1 {
        parts.next()?.parse::<u32>().ok()
    } else {
        Some(first)
    }
}

pub fn detect_java() -> Result<String> {
    let output = Command::new(if cfg!(target_os = "windows") { "java.exe" } else { "java" })
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("Java was not found on PATH")?;
    let version = String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("Java detected").to_owned();
    if !output.status.success() {
        bail!("Java could not run: {version}");
    }
    Ok(version)
}

fn apply_performance_options(game_dir: &Path, mode: &str) -> Result<()> {
    let path = game_dir.join("options.txt");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut options = std::collections::BTreeMap::<String, String>::new();
    for line in existing.lines() {
        if let Some((key, value)) = line.split_once(':') {
            options.insert(key.to_owned(), value.to_owned());
        }
    }
    let settings: &[(&str, &str)] = match mode {
        "Custom" => &[],
        "Maximum FPS" => &[
            ("graphics", "fast"),
            ("renderDistance", "2"),
            ("simulationDistance", "2"),
            ("maxFps", "260"),
            ("enableVsync", "false"),
            ("particles", "2"),
            ("clouds", "0"),
            ("biomeBlendRadius", "0"),
            ("mipmapLevels", "0"),
            ("entityShadows", "false"),
            ("ao", "false"),
            ("ambientOcclusion", "false"),
            ("entityDistanceScaling", "0.5"),
            ("renderClouds", "false"),
            ("bobView", "false"),
            ("fovEffectScale", "0.0"),
            ("screenEffectScale", "0.0"),
            ("distortionEffectScale", "0.0"),
            ("useVbo", "true"),
        ],
        "Performance" => &[
            ("graphics", "fast"),
            ("renderDistance", "3"),
            ("simulationDistance", "3"),
            ("maxFps", "260"),
            ("enableVsync", "false"),
            ("particles", "2"),
            ("clouds", "0"),
            ("biomeBlendRadius", "0"),
            ("mipmapLevels", "0"),
            ("entityShadows", "false"),
            ("ao", "false"),
            ("ambientOcclusion", "false"),
            ("entityDistanceScaling", "0.5"),
            ("renderClouds", "false"),
            ("bobView", "false"),
            ("fovEffectScale", "0.0"),
            ("screenEffectScale", "0.0"),
            ("distortionEffectScale", "0.0"),
            ("useVbo", "true"),
        ],
        _ => &[
            ("graphics", "fast"),
            ("renderDistance", "8"),
            ("simulationDistance", "6"),
            ("maxFps", "120"),
            ("enableVsync", "false"),
            ("particles", "1"),
            ("clouds", "1"),
            ("biomeBlendRadius", "2"),
            ("mipmapLevels", "2"),
            ("entityShadows", "false"),
            ("ao", "false"),
            ("entityDistanceScaling", "0.75"),
            ("useVbo", "true"),
        ],
    };
    for (key, value) in settings {
        options.insert((*key).to_owned(), (*value).to_owned());
    }
    let content = options.into_iter()
        .map(|(key, value)| format!("{key}:{value}"))
        .collect::<Vec<_>>()
        .join("\n");
    let temporary = path.with_extension("lunar-tmp");
    fs::write(&temporary, format!("{content}\n"))?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

pub fn launch_offline(installed: &InstalledVersion, profile: &crate::profiles::OfflineProfile, memory_mb: u32, performance_mode: &str) -> Result<Child> {
    launch_offline_settings(installed, profile, memory_mb, performance_mode, None, "")
}

pub fn launch_offline_settings(installed: &InstalledVersion, profile: &crate::profiles::OfflineProfile, memory_mb: u32, performance_mode: &str, preferred_java: Option<u32>, custom_jvm_arguments: &str) -> Result<Child> {
    launch_with_identity(installed, &profile.name, &profile.uuid, memory_mb, performance_mode, None, preferred_java, custom_jvm_arguments)
}

#[allow(dead_code)]
pub fn launch_with_account(installed: &InstalledVersion, account: &crust_core::authenticator::Account, memory_mb: u32, performance_mode: &str) -> Result<Child> {
    launch_with_account_settings(installed, account, memory_mb, performance_mode, None, "")
}

#[allow(dead_code)]
pub fn launch_with_account_settings(installed: &InstalledVersion, account: &crust_core::authenticator::Account, memory_mb: u32, performance_mode: &str, preferred_java: Option<u32>, custom_jvm_arguments: &str) -> Result<Child> {
    launch_with_identity(installed, &account.name, &account.uuid, memory_mb, performance_mode, Some(account), preferred_java, custom_jvm_arguments)
}

#[allow(clippy::too_many_arguments)]
fn launch_with_identity(installed: &InstalledVersion, username: &str, uuid: &str, memory_mb: u32, performance_mode: &str, account: Option<&crust_core::authenticator::Account>, preferred_java: Option<u32>, custom_jvm_arguments: &str) -> Result<Child> {
    let default_game_dir = installed.directory.parent().and_then(Path::parent).unwrap_or(&installed.directory);
    let game_dir = installed.game_directory.as_deref().unwrap_or(default_game_dir);
    let minimum_major = installed.metadata.pointer("/javaVersion/majorVersion").and_then(Value::as_u64).unwrap_or(8) as u32;
    let required_major = preferred_java.filter(|major| *major >= minimum_major).unwrap_or(minimum_major);
    let mut java = crate::runtime::find_java(required_major, game_dir).or_else(|| crate::runtime::find_java(required_major, default_game_dir)).unwrap_or_else(|| PathBuf::from(if cfg!(target_os = "windows") { "java.exe" } else { "java" }));
    let runtime = match crate::runtime::version(&java) {
        Ok(version) if parse_java_major(&version).is_some_and(|major| major >= required_major) => version,
        _ => {
            java = crate::runtime::install_java(required_major, default_game_dir)?;
            crate::runtime::version(&java)?
        }
    };
    let runtime_major = parse_java_major(&runtime).ok_or_else(|| anyhow!("Could not identify the installed Java version"))?;
    if runtime_major < required_major {
        bail!("Minecraft {} requires Java {}, but the detected runtime is Java {}", installed.id, required_major, runtime_major);
    }
    apply_performance_options(game_dir, performance_mode)?;
    let assets_dir = installed.directory.parent().and_then(Path::parent).unwrap_or(game_dir).join("assets");
    let separator = if cfg!(target_os = "windows") { ";" } else { ":" };
    let classpath = installed.libraries.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>().join(separator);
    let classpath_arg = installed.metadata.pointer("/arguments/jvm")
        .map(allowed_arguments)
        .filter(|items| !items.is_empty());
    let mut jvm = classpath_arg.unwrap_or_default();
    if jvm.is_empty() {
        jvm.extend(["-Djava.library.path=${natives_directory}".into(), "-cp".into(), "${classpath}".into()]);
    }
    let mut game = installed.metadata.pointer("/arguments/game")
        .map(allowed_arguments)
        .unwrap_or_default();
    if game.is_empty()
        && let Some(legacy) = installed.metadata.get("minecraftArguments").and_then(Value::as_str)
    {
        game = shlex::split(legacy).unwrap_or_else(|| legacy.split_whitespace().map(str::to_owned).collect());
    }
    let substitutions = [
        ("${natives_directory}", installed.natives.to_string_lossy().into_owned()),
        ("${launcher_name}", "LUNAR".into()),
        ("${launcher_version}", env!("CARGO_PKG_VERSION").into()),
        ("${classpath}", classpath),
        ("${auth_player_name}", username.to_owned()),
        ("${version_name}", installed.id.clone()),
        ("${game_directory}", game_dir.to_string_lossy().into_owned()),
        ("${assets_root}", assets_dir.to_string_lossy().into_owned()),
        ("${assets_index_name}", installed.assets.clone()),
        ("${game_assets}", installed.game_assets.to_string_lossy().into_owned()),
        ("${path}", installed.metadata.pointer("/logging/client/file/id").and_then(Value::as_str).map(|id| assets_dir.join("log_configs").join(id).to_string_lossy().into_owned()).unwrap_or_default()),
        ("${auth_uuid}", uuid.to_owned()),
        ("${auth_access_token}", account.map(|value| value.access_token.clone()).unwrap_or_else(|| "0".into())),
        ("${user_type}", if account.is_some() { "msa" } else { "legacy" }.into()),
        ("${version_type}", installed.metadata.get("type").and_then(Value::as_str).unwrap_or("release").to_owned()),
        ("${clientid}", account.and_then(|value| value.client_id.clone()).unwrap_or_default()),
        ("${auth_xuid}", account.and_then(|value| value.xuid().map(str::to_owned)).unwrap_or_default()),
        ("${user_properties}", account.map(|value| value.user_properties.clone()).unwrap_or_else(|| "{}".into())),
    ];
    replace_arguments(&mut jvm, &substitutions);
    replace_arguments(&mut game, &substitutions);
    let mut command = Command::new(java);
    let logs = game_dir.join("logs");
    fs::create_dir_all(&logs)?;
    let log = OpenOptions::new().create(true).append(true).open(logs.join("lunar-game.log"))?;
    let log = Arc::new(Mutex::new(log));
    command.current_dir(game_dir)
        .arg(format!("-Xms{}M", (memory_mb / 2).max(512)))
        .arg(format!("-Xmx{}M", memory_mb.clamp(1024, 8192)))
        .args(jvm)
        .args(shlex::split(custom_jvm_arguments).unwrap_or_default())
        .args([
            "-XX:+UseG1GC",
            "-XX:+ParallelRefProcEnabled",
            "-XX:MaxGCPauseMillis=50",
            "-XX:+DisableExplicitGC",
            "-XX:G1ReservePercent=20",
            "-XX:InitiatingHeapOccupancyPercent=15",
        ])
        .arg(&installed.main_class)
        .args(game)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().context("Could not start Minecraft. Check that a compatible Java runtime is installed.")?;
    if let Some(stdout) = child.stdout.take() {
        let log = Arc::clone(&log);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if let Ok(mut file) = log.lock() {
                    let _ = file.write_all(line.as_bytes());
                }
                line.clear();
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if let Ok(mut file) = log.lock() {
                    let _ = file.write_all(line.as_bytes());
                }
                line.clear();
            }
        });
    }
    Ok(child)
}

pub fn load_installed(version_id: &str, root: &Path) -> Result<InstalledVersion> {
    let directory = root.join("versions").join(version_id);
    let marker = directory.join(".lunar-installed");
    if !marker.exists() {
        bail!("Minecraft {version_id} has not completed installation");
    }
    let metadata_path = directory.join(format!("{version_id}.json"));
    let metadata: Value = serde_json::from_slice(&fs::read(&metadata_path)
        .with_context(|| format!("Version {version_id} is not installed"))?)?;
    let client_jar = directory.join(format!("{version_id}.jar"));
    if !client_jar.exists() {
        bail!("Minecraft client JAR is missing for {version_id}");
    }
    let mut libraries = Vec::new();
    let libraries_root = root.join("libraries");
    if let Some(entries) = metadata.get("libraries").and_then(Value::as_array) {
        for library in entries {
            if !rules_allow(library) {
                continue;
            }
            let path = library.pointer("/downloads/artifact/path")
                .and_then(Value::as_str)
                .map(|relative| libraries_root.join(relative))
                .or_else(|| library.get("name").and_then(Value::as_str).and_then(|name| maven_artifact_path(name, None)).map(|relative| libraries_root.join(relative)));
            if let Some(path) = path.filter(|path| path.exists()) {
                libraries.push(path);
            }
        }
    }
    libraries.push(client_jar.clone());
    let asset_index_id = metadata.pointer("/assetIndex/id").and_then(Value::as_str).context("Asset index ID missing")?;
    let asset_index_path = root.join("assets").join("indexes").join(format!("{asset_index_id}.json"));
    let asset_index: Value = serde_json::from_slice(&fs::read(&asset_index_path)
        .with_context(|| format!("Asset index {asset_index_id} is missing"))?)?;
    let game_assets = if asset_index.get("virtual").and_then(Value::as_bool).unwrap_or(false) {
        root.join("assets").join("virtual").join(asset_index_id)
    } else if asset_index.get("map_to_resources").and_then(Value::as_bool).unwrap_or(false) {
        root.join("resources")
    } else {
        root.join("assets")
    };
    let main_class = metadata.get("mainClass").and_then(Value::as_str).context("Minecraft main class missing")?.to_owned();
    Ok(InstalledVersion {
        id: version_id.to_owned(),
        directory: directory.clone(),
        main_class,
        assets: metadata.get("assets").and_then(Value::as_str).unwrap_or(asset_index_id).to_owned(),
        libraries,
        game_assets,
        natives: directory.join("natives"),
        metadata,
        game_directory: None,
    })
}

pub fn remove_version(version_id: &str, root: &Path) -> Result<()> {
    if version_id.is_empty()
        || !version_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || version_id == "."
        || version_id == ".."
    {
        bail!("Invalid Minecraft version identifier");
    }
    let directory = root.join("versions").join(version_id);
    if !directory.join(".lunar-installed").exists() {
        bail!("This version is not a completed LUNAR installation");
    }
    fs::remove_dir_all(directory)?;
    Ok(())
}

pub fn installed_versions(root: &Path) -> Result<BTreeSet<String>> {
    let mut versions = BTreeSet::new();
    let path = root.join("versions");
    if !path.exists() {
        return Ok(versions);
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if entry.path().join(".lunar-installed").exists() && entry.path().join(format!("{id}.jar")).exists() {
                versions.insert(id);
            }
        }
    }
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_profile_preserves_unrelated_options() {
        let root = std::env::temp_dir().join(format!("lunar-performance-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("options.txt"), "key_key.attack:key.mouse.left\ngamma:1.0\n").unwrap();
        apply_performance_options(&root, "Performance").unwrap();
        let options = fs::read_to_string(root.join("options.txt")).unwrap();
        assert!(options.contains("key_key.attack:key.mouse.left"));
        assert!(options.contains("gamma:1.0"));
        assert!(options.contains("renderDistance:3"));
        assert!(options.contains("simulationDistance:3"));
        assert!(options.contains("maxFps:260"));
        assert!(options.contains("mipmapLevels:0"));
        assert!(options.contains("enableVsync:false"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn custom_performance_mode_preserves_video_settings_and_other_options() {
        let root = std::env::temp_dir().join(format!("lunar-custom-performance-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("options.txt");
        fs::write(&path, "renderDistance:12\nsimulationDistance:8\ngamma:1.0\n").unwrap();
        apply_performance_options(&root, "Custom").unwrap();
        let options = fs::read_to_string(&path).unwrap();
        assert!(options.contains("renderDistance:12"));
        assert!(options.contains("simulationDistance:8"));
        assert!(options.contains("gamma:1.0"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn removes_only_completed_versions() {
        let root = std::env::temp_dir().join(format!("lunar-version-test-{}", std::process::id()));
        let version = root.join("versions").join("1.21.1");
        fs::create_dir_all(&version).unwrap();
        fs::write(version.join(".lunar-installed"), b"1.21.1").unwrap();
        remove_version("1.21.1", &root).unwrap();
        assert!(!version.exists());
        assert!(remove_version("../outside", &root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_java_runtime_versions() {
        assert_eq!(parse_java_major("openjdk version \"21.0.4\" 2024-07-16"), Some(21));
        assert_eq!(parse_java_major("java version \"1.8.0_412\""), Some(8));
        assert_eq!(parse_java_major("not a version"), None);
    }

    #[test]
    fn resolves_maven_coordinates() {
        assert_eq!(maven_artifact_path("com.mojang:netty:1.8.8", None).as_deref(), Some("com/mojang/netty/1.8.8/netty-1.8.8.jar"));
        assert_eq!(maven_artifact_path("org.lwjgl.lwjgl:lwjgl:2.9.1", Some("natives-linux")).as_deref(), Some("org/lwjgl/lwjgl/lwjgl/2.9.1/lwjgl-2.9.1-natives-linux.jar"));
    }

    #[test]
    fn evaluates_library_rules() {
        assert!(rules_allow(&json!({})));
        assert!(rules_allow(&json!({
            "rules": [{"action": "allow", "os": {"name": current_os()}}]
        })));
        assert!(!rules_allow(&json!({
            "rules": [{"action": "disallow", "os": {"name": current_os()}}]
        })));
        assert!(!rules_allow(&json!({
            "rules": [{"action": "allow", "features": {"is_demo_user": true}}]
        })));
    }
}
