use anyhow::{bail, Context, Result};
use std::{fs, io::Cursor, path::{Path, PathBuf}, process::Command};
use zip::ZipArchive;

fn platform() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "windows") { "windows" } else if cfg!(target_os = "macos") { "mac" } else { "linux" };
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x64" };
    (os, arch)
}

pub fn find_java(major: u32, root: &Path) -> Option<PathBuf> {
    let runtime = root.join("runtimes").join(major.to_string());
    let executable = if cfg!(target_os = "windows") { "java.exe" } else { "java" };
    let mut pending = vec![runtime];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(directory).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() { pending.push(path); }
            else if path.file_name().is_some_and(|name| name == executable) && path.to_string_lossy().contains("bin") { return Some(path); }
        }
    }
    None
}

pub fn install_java(major: u32, root: &Path) -> Result<PathBuf> {
    if let Some(existing) = find_java(major, root) { return Ok(existing); }
    let (os, arch) = platform();
    let url = format!("https://api.adoptium.net/v3/binary/latest/{major}/ga/{os}/{arch}/jre/hotspot/normal/eclipse");
    let response = reqwest::blocking::Client::builder().user_agent("LUNAR").timeout(std::time::Duration::from_secs(240)).build()?.get(url).send()?.error_for_status()?;
    let bytes = response.bytes()?;
    let directory = root.join("runtimes").join(major.to_string());
    fs::create_dir_all(&directory)?;
    if os == "windows" {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).context("Invalid Java runtime archive")?;
        for index in 0..archive.len() {
            let mut file = archive.by_index(index)?;
            let Some(relative) = file.enclosed_name().map(Path::to_path_buf) else { continue };
            let destination = directory.join(relative);
            if file.is_dir() { fs::create_dir_all(destination)?; }
            else {
                if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
                let mut output = fs::File::create(destination)?;
                std::io::copy(&mut file, &mut output)?;
            }
        }
    } else {
        let archive = directory.join("runtime.tar.gz");
        fs::write(&archive, &bytes)?;
        let output = Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&directory).output()
            .context("The tar utility is required to extract Java")?;
        let _ = fs::remove_file(archive);
        if !output.status.success() { bail!("Could not extract Java runtime: {}", String::from_utf8_lossy(&output.stderr)); }
    }
    find_java(major, root).context("Java archive extracted but its executable was not found")
}

pub fn version(path: &Path) -> Result<String> {
    let output = Command::new(path).arg("-version").output().context("Could not execute Java")?;
    let text = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() { bail!("Java runtime failed: {text}"); }
    Ok(text)
}
