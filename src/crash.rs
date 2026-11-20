use anyhow::{Context, Result};
use std::{fs, path::{Path, PathBuf}, time::SystemTime};

#[derive(Clone, Debug)]
pub struct CrashAnalysis {
    pub source: PathBuf,
    pub summary: String,
    pub findings: Vec<String>,
    pub excerpt: String,
}

fn latest_file(directories: &[PathBuf]) -> Option<PathBuf> {
    let mut files = Vec::new();
    for directory in directories {
        let Ok(entries) = fs::read_dir(directory) else { continue };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) { continue; }
            let path = entry.path();
            let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
            if ["log", "txt"].contains(&extension.as_str()) {
                let modified = entry.metadata().and_then(|metadata| metadata.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                files.push((modified, path));
            }
        }
    }
    files.sort_by_key(|item| std::cmp::Reverse(item.0));
    files.into_iter().next().map(|(_, path)| path)
}

fn classify(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut findings = Vec::new();
    let rules = [
        ("java.lang.OutOfMemoryError", "Memory exhaustion: Minecraft or a mod ran out of available Java memory. Review the instance RAM allocation and memory-heavy mods."),
        ("mixinapplyerror", "Mixin application failed: a mod patch could not be applied. Check mod versions and loader compatibility."),
        ("modloadingexception", "Mod loading failed: inspect the first mod-loading exception and update or remove the named mod."),
        ("nosuchmethoderror", "API mismatch: a mod expects a method that is missing from the installed game, loader, or another mod."),
        ("classnotfoundexception", "Missing class: a required library or mod dependency may be absent."),
        ("unsupportedclassversionerror", "Java version mismatch: a mod or game component was compiled for a newer Java runtime."),
        ("duplicate mods", "Duplicate mod IDs were reported. Remove duplicate copies from the instance mods folder."),
        ("requires fabric", "Loader mismatch: at least one installed mod requires Fabric."),
        ("requires forge", "Loader mismatch: at least one installed mod requires Forge."),
        ("opengl", "Graphics initialization issue: check GPU drivers and the selected graphics backend."),
        ("failed to create", "A component failed during startup. The surrounding log lines should identify the failing component."),
        ("exit code: -1", "Minecraft exited with a generic crash code. Use the exception and stack trace above to identify the cause."),
    ];
    for (needle, message) in rules {
        if lower.contains(&needle.to_lowercase()) { findings.push(message.to_owned()); }
    }
    if findings.is_empty() {
        findings.push("No known crash signature matched. Review the final exception and the first 'Caused by' section in the report.".into());
    }
    findings
}

pub fn analyze(root: &Path, instance: Option<&Path>) -> Result<CrashAnalysis> {
    let base = instance.unwrap_or(root);
    let crash_dir = base.join("crash-reports");
    let logs_dir = base.join("logs");
    let crash = latest_file(&[crash_dir]);
    let log = latest_file(&[logs_dir]);
    let source = crash.or(log).context("No crash report or log file was found for this instance")?;
    let text = fs::read_to_string(&source).context("Could not read the selected crash report")?;
    let findings = classify(&text);
    let summary = if findings.len() == 1 && findings[0].starts_with("No known") {
        "Crash signature not identified".to_owned()
    } else {
        format!("{} possible issue(s) detected", findings.len())
    };
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(45);
    let excerpt = lines[start..].join("\n");
    Ok(CrashAnalysis { source, summary, findings, excerpt })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_common_minecraft_crash_signatures() {
        let findings = classify("java.lang.OutOfMemoryError: Java heap space\norg.spongepowered.asm.mixin.throwables.MixinApplyError");
        assert_eq!(findings.len(), 2);
        assert!(findings[0].contains("Memory exhaustion"));
    }

    #[test]
    fn unknown_crash_still_returns_guidance() {
        let findings = classify("Minecraft stopped unexpectedly");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("No known"));
    }
}
