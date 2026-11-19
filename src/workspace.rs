use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub minecraft: String,
    pub memory_mb: u32,
    pub java_major: Option<u32>,
    pub created_at: u64,
    pub last_played: Option<u64>,
    pub play_count: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn slug(value: &str) -> String {
    value.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' })
        .collect::<String>().split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-")
}

fn metadata_path(path: &Path) -> PathBuf {
    path.join(".lunar-instance.json")
}

pub fn instance_path(root: &Path, instance: &Instance) -> PathBuf {
    root.join("instances").join(&instance.id)
}

pub fn list_instances(root: &Path) -> Result<Vec<Instance>> {
    let directory = root.join("instances");
    if !directory.exists() { return Ok(Vec::new()); }
    let mut items = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let path = metadata_path(&entry.path());
        if let Ok(bytes) = fs::read(path)
            && let Ok(item) = serde_json::from_slice::<Instance>(&bytes)
        {
            items.push(item);
        }
    }
    items.sort_by(|a, b| b.last_played.cmp(&a.last_played).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(items)
}

pub fn create_instance(root: &Path, name: &str, minecraft: &str) -> Result<Instance> {
    let clean = name.trim();
    if clean.chars().count() < 3 || clean.chars().count() > 32 { bail!("Instance names must contain 3–32 characters"); }
    if !clean.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == ' ' || ch == '-' || ch == '_') { bail!("Use letters, numbers, spaces, hyphens or underscores in instance names"); }
    if !crate::launcher::installed_versions(root)?.contains(minecraft) { bail!("Install this Minecraft version before creating an instance"); }
    let base = slug(clean);
    if base.is_empty() { bail!("Invalid instance name"); }
    let mut id = base.clone();
    let mut suffix = 2;
    while root.join("instances").join(&id).exists() { id = format!("{base}-{suffix}"); suffix += 1; }
    let path = root.join("instances").join(&id);
    for folder in ["mods", "resourcepacks", "shaderpacks", "screenshots", "saves", "logs", "crash-reports", "config"] { fs::create_dir_all(path.join(folder))?; }
    let item = Instance { id, name: clean.to_owned(), minecraft: minecraft.to_owned(), memory_mb: 4096, java_major: None, created_at: now(), last_played: None, play_count: 0 };
    save_instance(root, &item)?;
    Ok(item)
}

pub fn save_instance(root: &Path, item: &Instance) -> Result<()> {
    if item.id.is_empty() || !item.id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-') { bail!("Invalid instance identifier"); }
    let path = root.join("instances").join(&item.id);
    fs::create_dir_all(&path)?;
    let temporary = metadata_path(&path).with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(item)?)?;
    fs::rename(temporary, metadata_path(&path))?;
    Ok(())
}

pub fn delete_instance(root: &Path, item: &Instance) -> Result<()> {
    let base = root.join("instances").canonicalize()?;
    let target = instance_path(root, item).canonicalize()?;
    if !target.starts_with(&base) || !metadata_path(&target).is_file() { bail!("Selected folder is not a Lunar instance"); }
    fs::remove_dir_all(target)?;
    Ok(())
}

pub fn record_launch(root: &Path, item: &mut Instance) -> Result<()> {
    item.last_played = Some(now());
    item.play_count = item.play_count.saturating_add(1);
    save_instance(root, item)
}

pub fn list_files(directory: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    if !directory.exists() { return Ok(Vec::new()); }
    let mut output = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() { continue; }
        let path = entry.path();
        let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
        if extensions.is_empty() || extensions.iter().any(|item| *item == extension) { output.push(path); }
    }
    output.sort();
    Ok(output)
}

pub fn toggle_mod(path: &Path, enabled: bool) -> Result<PathBuf> {
    let parent = path.parent().context("Mod path is invalid")?.canonicalize()?;
    let target = path.canonicalize()?;
    if !target.starts_with(&parent) || !target.is_file() { bail!("Selected mod is not a regular file"); }
    let name = target.file_name().and_then(|value| value.to_str()).context("Invalid mod filename")?;
    let disabled = name.ends_with(".jar.disabled");
    if enabled == !disabled { return Ok(target); }
    let new_name = if enabled { name.strip_suffix(".disabled").unwrap_or(name).to_owned() } else { format!("{name}.disabled") };
    let destination = parent.join(new_name);
    if destination.exists() { bail!("A file with the target name already exists"); }
    fs::rename(&target, &destination)?;
    Ok(destination)
}

fn add_directory(writer: &mut ZipWriter<fs::File>, root: &Path, current: &Path) -> Result<()> {
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
        if entry.file_type()?.is_dir() {
            writer.add_directory(format!("{relative}/"), options)?;
            add_directory(writer, root, &path)?;
        } else if entry.file_type()?.is_file() {
            writer.start_file(relative, options)?;
            let mut file = fs::File::open(path)?;
            std::io::copy(&mut file, writer)?;
        }
    }
    Ok(())
}

pub fn backup_world(instance: &Path, world: &str) -> Result<PathBuf> {
    if world.is_empty() || world.contains(['/', '\\']) || world == "." || world == ".." { bail!("Invalid world name"); }
    let source = instance.join("saves").join(world);
    if !source.is_dir() { bail!("World folder was not found"); }
    let output_dir = instance.join("backups");
    fs::create_dir_all(&output_dir)?;
    let output = output_dir.join(format!("{}-{}.zip", slug(world), now()));
    let file = fs::File::create(&output)?;
    let mut writer = ZipWriter::new(file);
    add_directory(&mut writer, source.parent().unwrap_or(&source), &source)?;
    writer.finish()?;
    Ok(output)
}

pub fn import_content_file(source: &Path, destination: &Path, extensions: &[&str]) -> Result<PathBuf> {
    if !source.is_file() { bail!("Selected content file was not found"); }
    let extension = source.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
    if !extensions.iter().any(|allowed| *allowed == extension) { bail!("This file type is not supported here"); }
    let name = source.file_name().context("Selected file has no filename")?;
    fs::create_dir_all(destination)?;
    let mut target = destination.join(name);
    if target.exists() {
        let stem = source.file_stem().and_then(|value| value.to_str()).unwrap_or("content");
        let extension = source.extension().and_then(|value| value.to_str()).unwrap_or("");
        let mut suffix = 2usize;
        loop {
            let filename = if extension.is_empty() { format!("{stem}-{suffix}") } else { format!("{stem}-{suffix}.{extension}") };
            target = destination.join(filename);
            if !target.exists() { break; }
            suffix += 1;
        }
    }
    let temporary = target.with_extension(format!("{}.part", target.extension().and_then(|value| value.to_str()).unwrap_or("file")));
    fs::copy(source, &temporary)?;
    fs::rename(&temporary, &target)?;
    Ok(target)
}

pub fn restore_world(archive_path: &Path, instance: &Path) -> Result<String> {
    let file = fs::File::open(archive_path)?;
    let mut archive = ZipArchive::new(file).context("Backup is not a valid ZIP archive")?;
    let saves = instance.join("saves");
    fs::create_dir_all(&saves)?;
    let staging = instance.join("backups").join(format!(".restore-{}", now()));
    if staging.exists() { fs::remove_dir_all(&staging)?; }
    fs::create_dir_all(&staging)?;
    let result = (|| -> Result<String> {
        let mut extracted = 0usize;
        let mut world_name: Option<String> = None;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let Some(relative) = entry.enclosed_name() else { bail!("Backup contains an unsafe path"); };
            let mut components = relative.components();
            let Some(std::path::Component::Normal(root)) = components.next() else { bail!("Backup has an invalid world folder"); };
            if components.clone().any(|part| !matches!(part, std::path::Component::Normal(_))) { bail!("Backup contains an unsafe path"); }
            let root_name = root.to_string_lossy().to_string();
            if world_name.as_ref().is_some_and(|name| name != &root_name) { bail!("Backup contains more than one world folder"); }
            world_name = Some(root_name);
            let destination = staging.join(&relative);
            if entry.is_dir() { fs::create_dir_all(destination)?; }
            else {
                if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
                let mut output = fs::File::create(destination)?;
                std::io::copy(&mut entry, &mut output)?;
                extracted += 1;
            }
        }
        if extracted == 0 { bail!("Backup archive contains no world files"); }
        let name = world_name.context("Backup does not contain a world folder")?;
        let staged_world = staging.join(&name);
        if !staged_world.is_dir() { bail!("Backup world folder is missing"); }
        let destination = saves.join(&name);
        let previous = staging.join(".previous-world");
        let had_previous = destination.exists();
        if had_previous { fs::rename(&destination, &previous)?; }
        if let Err(error) = fs::rename(&staged_world, &destination) {
            if had_previous { let _ = fs::rename(&previous, &destination); }
            return Err(error.into());
        }
        if had_previous { fs::remove_dir_all(previous)?; }
        Ok(format!("Restored {name} successfully."))
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

pub fn export_instance(instance: &Path, destination: &Path) -> Result<()> {
    if !metadata_path(instance).is_file() { bail!("Selected folder is not a Lunar instance"); }
    if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
    let file = fs::File::create(destination)?;
    let mut writer = ZipWriter::new(file);
    add_directory(&mut writer, instance, instance)?;
    writer.finish()?;
    Ok(())
}

pub fn import_instance(archive_path: &Path, root: &Path, name: &str) -> Result<Instance> {
    let mut archive = ZipArchive::new(fs::File::open(archive_path).context("Could not open instance archive")?).context("Invalid Lunar instance archive")?;
    let mut manifest = None;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.name() == ".lunar-instance.json" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            manifest = Some(serde_json::from_slice::<Instance>(&bytes)?);
            break;
        }
    }
    let mut item = manifest.context("Archive does not contain a Lunar instance manifest")?;
    let base = slug(name);
    if base.is_empty() { bail!("Invalid instance name"); }
    let mut id = base.clone();
    let mut suffix = 2;
    while root.join("instances").join(&id).exists() { id = format!("{base}-{suffix}"); suffix += 1; }
    item.id = id;
    item.name = name.to_owned();
    let destination_root = root.join("instances").join(&item.id);
    fs::create_dir_all(&destination_root)?;
    let mut count = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(relative) = entry.enclosed_name() else { bail!("Archive contains an unsafe path"); };
        if relative.as_os_str().is_empty() { continue; }
        let destination = destination_root.join(relative);
        if entry.is_dir() { fs::create_dir_all(destination)?; }
        else {
            if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
            let mut output = fs::File::create(destination)?;
            std::io::copy(&mut entry, &mut output)?;
            count += 1;
        }
    }
    if count == 0 { let _ = fs::remove_dir_all(&destination_root); bail!("Archive contains no instance files"); }
    save_instance(root, &item)?;
    Ok(item)
}

pub fn search(root: &Path, query: &str) -> Result<Vec<String>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() { return Ok(Vec::new()); }
    let mut results = Vec::new();
    for name in ["instances", "mods", "resourcepacks", "shaderpacks", "screenshots", "crash-reports", "logs"] {
        let directory = root.join(name);
        if !directory.exists() { continue; }
        let mut pending = vec![directory];
        while let Some(current) = pending.pop() {
            let Ok(entries) = fs::read_dir(current) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) { pending.push(path); }
                else if path.file_name().and_then(|value| value.to_str()).is_some_and(|value| value.to_lowercase().contains(&needle)) {
                    results.push(path.display().to_string());
                    if results.len() >= 100 { return Ok(results); }
                }
            }
        }
    }
    Ok(results)
}

pub fn read_tail(path: &Path, max_lines: usize) -> Result<String> {
    let text = fs::read_to_string(path)?;
    let lines = text.lines().collect::<Vec<_>>();
    Ok(lines.iter().rev().take(max_lines).copied().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))
}

pub fn system_memory_mb() -> Option<(u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        let text = fs::read_to_string("/proc/meminfo").ok()?;
        let total = text.lines().find(|line| line.starts_with("MemTotal:"))?.split_whitespace().nth(1)?.parse::<u64>().ok()? / 1024;
        let available = text.lines().find(|line| line.starts_with("MemAvailable:"))?.split_whitespace().nth(1)?.parse::<u64>().ok()? / 1024;
        return Some((total, available));
    }
    #[cfg(target_os = "windows")]
    {
        let output = std::process::Command::new("powershell").args(["-NoProfile", "-Command", "$m=Get-CimInstance Win32_OperatingSystem; Write-Output ([math]::Round($m.TotalVisibleMemorySize/1024)); Write-Output ([math]::Round($m.FreePhysicalMemory/1024))"]).output().ok()?;
        let values = String::from_utf8_lossy(&output.stdout).lines().filter_map(|line| line.trim().parse::<u64>().ok()).collect::<Vec<_>>();
        if values.len() >= 2 { return Some((values[0], values[1])); }
    }
    #[cfg(target_os = "macos")]
    {
        let total = std::process::Command::new("sysctl").args(["-n", "hw.memsize"]).output().ok()?;
        let total = String::from_utf8_lossy(&total.stdout).trim().parse::<u64>().ok()? / 1024 / 1024;
        let page = std::process::Command::new("vm_stat").output().ok()?;
        let text = String::from_utf8_lossy(&page.stdout);
        let free = text.lines().filter_map(|line| line.split_whitespace().last()?.trim_end_matches('.').parse::<u64>().ok()).sum::<u64>() * 4096 / 1024 / 1024;
        return Some((total, free));
    }
    #[allow(unreachable_code)]
    None
}

pub fn format_time(timestamp: Option<u64>) -> String {
    timestamp.map(|value| format!("{} days ago", now().saturating_sub(value) / 86400)).unwrap_or_else(|| "Never".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_import_rejects_wrong_extensions_and_avoids_overwrites() {
        let root = std::env::temp_dir().join(format!("lunar-content-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("sample.jar");
        fs::write(&source, b"mod").unwrap();
        let destination = root.join("mods");
        let first = import_content_file(&source, &destination, &["jar"]).unwrap();
        let second = import_content_file(&source, &destination, &["jar"]).unwrap();
        assert_ne!(first, second);
        assert!(import_content_file(&source, &destination, &["zip"]).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn instance_names_are_safely_slugged() {
        assert_eq!(slug("My Survival World"), "my-survival-world");
        assert_eq!(slug("../../unsafe"), "unsafe");
    }

    #[test]
    fn instance_manifest_round_trips() {
        let root = std::env::temp_dir().join(format!("lunar-workspace-{}", std::process::id()));
        let item = Instance { id: "test-instance".into(), name: "Test Instance".into(), minecraft: "1.21.1".into(), memory_mb: 4096, java_major: None, created_at: 1, last_played: None, play_count: 0 };
        save_instance(&root, &item).unwrap();
        assert_eq!(list_instances(&root).unwrap()[0].name, item.name);
        let _ = fs::remove_dir_all(root);
    }
}