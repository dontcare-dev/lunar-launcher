use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{fs, path::{Path, PathBuf}, process::Command};
use crate::{content::InstalledPack, launcher::{self, InstalledVersion}};

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder().user_agent(concat!("LUNAR/", env!("CARGO_PKG_VERSION"))).timeout(std::time::Duration::from_secs(120)).build()?)
}

fn coordinate_path(name: &str) -> Result<String> {
    let parts = name.split(':').collect::<Vec<_>>();
    if parts.len() < 3 { bail!("Invalid loader library coordinate: {name}"); }
    let filename = if parts.len() > 3 { format!("{}-{}-{}.jar", parts[1], parts[2], parts[3]) } else { format!("{}-{}.jar", parts[1], parts[2]) };
    Ok(format!("{}/{}/{}/{}", parts[0].replace('.', "/"), parts[1], parts[2], filename))
}

fn download_libraries(profile: &Value, root: &Path) -> Result<Vec<PathBuf>> {
    let client = client()?;
    let mut result = Vec::new();
    if let Some(items) = profile.get("libraries").and_then(Value::as_array) {
        for item in items {
            let Some(name) = item.get("name").and_then(Value::as_str) else { continue };
            let artifact = item.pointer("/downloads/artifact");
            let relative = artifact.and_then(|a| a.get("path")).and_then(Value::as_str).map(str::to_owned).unwrap_or(coordinate_path(name)?);
            let base = item.get("url").and_then(Value::as_str).unwrap_or("https://libraries.minecraft.net/");
            let url = artifact.and_then(|a| a.get("url")).and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| format!("{}/{}", base.trim_end_matches('/'), relative));
            let destination = root.join("libraries").join(&relative);
            if !destination.exists() {
                if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
                let bytes = client.get(url).send()?.error_for_status()?.bytes()?;
                if let Some(expected) = artifact.and_then(|a| a.get("sha1")).and_then(Value::as_str) {
                    use sha1::{Digest, Sha1};
                    if !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected) { bail!("Loader library checksum failed: {name}"); }
                }
                let temp = destination.with_extension("lunar-download");
                fs::write(&temp, &bytes)?;
                fs::rename(temp, &destination)?;
            }
            result.push(destination);
        }
    }
    Ok(result)
}

fn merge(mut base: InstalledVersion, profile: &Value, instance: &Path, root: &Path) -> Result<InstalledVersion> {
    for library in download_libraries(profile, root)? {
        if !base.libraries.contains(&library) { base.libraries.push(library); }
    }
    if let Some(items) = profile.get("libraries").and_then(Value::as_array) {
        let target = base.metadata["libraries"].as_array_mut().context("Minecraft profile has no library list")?;
        for item in items {
            if !target.contains(item) { target.push(item.clone()); }
        }
    }
    if let Some(main) = profile.get("mainClass").and_then(Value::as_str) { base.main_class = main.to_owned(); base.metadata["mainClass"] = json!(main); }
    if !base.metadata["arguments"].is_object() { base.metadata["arguments"] = json!({}); }
    if let Some(args) = profile.get("arguments").and_then(Value::as_object) {
        let target = base.metadata["arguments"].as_object_mut().context("Invalid game arguments")?;
        for key in ["game", "jvm"] {
            if let Some(items) = args.get(key).and_then(Value::as_array) {
                let target_items = target.entry(key.to_owned()).or_insert_with(|| json!([]));
                if let Some(existing) = target_items.as_array_mut() { for item in items { if !existing.contains(item) { existing.push(item.clone()); } } }
            }
        }
    }
    if let Some(legacy) = profile.get("minecraftArguments").and_then(Value::as_str) { base.metadata["minecraftArguments"] = json!(legacy); }
    if let Some(java) = profile.get("javaVersion") { base.metadata["javaVersion"] = java.clone(); }
    let original_directory = base.directory.clone();
    let original_jar = original_directory.join(format!("{}.jar", base.id));
    base.id = profile.get("id").and_then(Value::as_str).unwrap_or(&base.id).to_owned();
    let profile_directory = root.join("versions").join(&base.id);
    fs::create_dir_all(&profile_directory)?;
    let profile_jar = profile_directory.join(format!("{}.jar", base.id));
    if !profile_jar.exists() {
        fs::copy(&original_jar, &profile_jar).context("Could not prepare the Fabric game profile")?;
    }
    base.directory = profile_directory.clone();
    base.game_directory = Some(instance.to_path_buf());
    base.natives = profile_directory.join("natives");
    fs::create_dir_all(&base.natives)?;
    base.metadata["id"] = json!(base.id);
    base.metadata["jar"] = json!(base.id);
    fs::write(profile_directory.join(format!("{}.json", base.id)), serde_json::to_vec_pretty(&base.metadata)?)?;
    fs::write(profile_directory.join(".lunar-installed"), base.id.as_bytes())?;
    Ok(base)
}

fn fetch_profile(url: &str) -> Result<Value> { Ok(client()?.get(url).send()?.error_for_status()?.json()?) }

fn fabric_or_quilt(mc: &str, version: &str, instance: &Path, root: &Path, quilt: bool) -> Result<InstalledVersion> {
    let base = if launcher::installed_versions(root)?.contains(mc) { launcher::load_installed(mc, root)? } else { launcher::install(mc, root, |_| {})? };
    let url = if quilt { format!("https://meta.quiltmc.org/v3/versions/loader/{mc}/{version}/profile/json") } else { format!("https://meta.fabricmc.net/v2/versions/loader/{mc}/{version}/profile/json") };
    merge(base, &fetch_profile(&url)?, instance, root)
}

fn run_installer(url: &str, root: &Path) -> Result<()> {
    let bytes = client()?.get(url).send()?.error_for_status()?.bytes()?;
    let directory = root.join("installers");
    fs::create_dir_all(&directory)?;
    let installer = directory.join(format!("loader-{}.jar", std::process::id()));
    fs::write(&installer, bytes)?;
    let mut java = crate::runtime::find_java(17, root).unwrap_or_else(|| PathBuf::from(if cfg!(target_os = "windows") { "java.exe" } else { "java" }));
    if crate::runtime::version(&java).is_err() { java = crate::runtime::install_java(17, root)?; }
    let output = Command::new(java).arg("-jar").arg(&installer).arg("--installClient").arg(root).output().context("Java is required to run the loader installer")?;
    let _ = fs::remove_file(installer);
    if !output.status.success() { bail!("Loader installer failed: {}", String::from_utf8_lossy(&output.stderr)); }
    Ok(())
}

fn generated_profile(mc: &str, root: &Path, instance: &Path, loader: &str, requested: Option<&str>) -> Result<InstalledVersion> {
    let mut profiles = Vec::new();
    for entry in fs::read_dir(root.join("versions"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let id = entry.file_name().to_string_lossy().into_owned();
        if !id.to_lowercase().contains(loader) || requested.is_some_and(|version| !id.contains(version)) { continue; }
        let path = entry.path().join(format!("{id}.json"));
        if let Ok(bytes) = fs::read(path)
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            && value.get("inheritsFrom").and_then(Value::as_str) == Some(mc)
        {
            profiles.push(value);
        }
    }
    let profile = profiles.into_iter().next().context("Loader installer did not create a compatible profile")?;
    let base = launcher::load_installed(mc, root)?;
    merge(base, &profile, instance, root)
}

fn forge(mc: &str, requested: &str, root: &Path, instance: &Path) -> Result<InstalledVersion> {
    if !launcher::installed_versions(root)?.contains(mc) { launcher::install(mc, root, |_| {})?; }
    let promos = fetch_profile("https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json")?;
    let recommended = format!("{mc}-recommended");
    let latest = format!("{mc}-latest");
    let selected = promos.pointer(&format!("/promos/{recommended}")).or_else(|| promos.pointer(&format!("/promos/{latest}"))).and_then(Value::as_str).context("No Forge release exists for this Minecraft version")?;
    let selected = if requested.is_empty() || requested == "recommended" || requested == "latest" { selected } else { requested };
    let id = format!("{mc}-{selected}");
    if let Ok(profile) = generated_profile(mc, root, instance, "forge", Some(selected)) {
        return Ok(profile);
    }
    run_installer(&format!("https://maven.minecraftforge.net/net/minecraftforge/forge/{id}/forge-{id}-installer.jar"), root)?;
    generated_profile(mc, root, instance, "forge", Some(selected))
}

fn neoforge(mc: &str, requested: &str, root: &Path, instance: &Path) -> Result<InstalledVersion> {
    if !launcher::installed_versions(root)?.contains(mc) { launcher::install(mc, root, |_| {})?; }
    let versions = fetch_profile("https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge")?;
    let prefix = mc.strip_prefix("1.").unwrap_or(mc).split('.').take(2).collect::<Vec<_>>().join(".");
    let all = versions.as_array().or_else(|| versions.get("versions").and_then(Value::as_array)).context("NeoForge release list is invalid")?;
    let selected = if requested.is_empty() { all.iter().filter_map(Value::as_str).find(|v| v.starts_with(&format!("{prefix}."))).context("No NeoForge release exists for this Minecraft version")? } else { requested };
    if let Ok(profile) = generated_profile(mc, root, instance, "neoforge", Some(selected)) {
        return Ok(profile);
    }
    run_installer(&format!("https://maven.neoforged.net/releases/net/neoforged/neoforge/{selected}/neoforge-{selected}-installer.jar"), root)?;
    generated_profile(mc, root, instance, "neoforge", Some(selected))
}


#[derive(serde::Deserialize)]
struct FabricLoaderEntry {
    loader: FabricLoaderInfo,
}

#[derive(serde::Deserialize)]
struct FabricLoaderInfo {
    version: String,
    #[serde(default)]
    stable: bool,
}

pub fn install_fabric_profile(mc: &str, root: &Path, instance: &Path) -> Result<InstalledVersion> {
    let entries: Vec<FabricLoaderEntry> = serde_json::from_value(fetch_profile(&format!("https://meta.fabricmc.net/v2/versions/loader/{mc}"))?)?;
    let selected = entries.iter().find(|entry| entry.loader.stable).or_else(|| entries.first())
        .context("No Fabric loader release is available for this Minecraft version")?;
    fabric_or_quilt(mc, &selected.loader.version, instance, root, false)
}

pub fn prepare_instance(mc: &str, loader: Option<&str>, root: &Path, instance: &Path) -> Result<InstalledVersion> {
    match loader.unwrap_or("vanilla") {
        "fabric" => install_fabric_profile(mc, root, instance),
        "quilt" => {
            let entries = fetch_profile(&format!("https://meta.quiltmc.org/v3/versions/loader/{mc}"))?;
            let selected = entries.as_array().and_then(|items| items.first())
                .and_then(|item| item.pointer("/loader/version")).and_then(Value::as_str)
                .context("No Quilt loader release is available for this Minecraft version")?;
            fabric_or_quilt(mc, selected, instance, root, true)
        }
        "forge" => forge(mc, "", root, instance),
        "neoforge" => neoforge(mc, "", root, instance),
        _ => {
            let mut base = if launcher::installed_versions(root)?.contains(mc) {
                launcher::load_installed(mc, root)?
            } else {
                launcher::install(mc, root, |_| {})?
            };
            base.game_directory = Some(instance.to_path_buf());
            Ok(base)
        }
    }
}

pub fn prepare_modpack(pack: &InstalledPack, root: &Path) -> Result<InstalledVersion> {
    match pack.loaders.iter().next() {
        Some((kind, version)) if kind == "fabric-loader" => {
            if let Ok(profile) = generated_profile(&pack.minecraft, root, &pack.instance, "fabric", Some(version)) { return Ok(profile); }
            fabric_or_quilt(&pack.minecraft, version, &pack.instance, root, false)
        }
        Some((kind, version)) if kind == "quilt-loader" => {
            if let Ok(profile) = generated_profile(&pack.minecraft, root, &pack.instance, "quilt", Some(version)) { return Ok(profile); }
            fabric_or_quilt(&pack.minecraft, version, &pack.instance, root, true)
        }
        Some((kind, version)) if kind == "forge" => forge(&pack.minecraft, version, root, &pack.instance),
        Some((kind, version)) if kind == "neoforge" => neoforge(&pack.minecraft, version, root, &pack.instance),
        _ => { let mut base = if launcher::installed_versions(root)?.contains(&pack.minecraft) { launcher::load_installed(&pack.minecraft, root)? } else { launcher::install(&pack.minecraft, root, |_| {})? }; base.game_directory = Some(pack.instance.clone()); Ok(base) }
    }
}
