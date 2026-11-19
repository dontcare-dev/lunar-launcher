use std::collections::BTreeSet;

pub fn analyze(log: &str) -> String {
    let lower = log.to_ascii_lowercase();
    let patterns: [(&str, &str, &str); 12] = [
        ("outofmemoryerror", "Critical", "Java ran out of memory. Lower render distance, close other apps, or adjust the instance RAM allocation."),
        ("java.lang.noclassdeffounderror", "Error", "A required class is missing. Check mod dependencies and confirm every mod matches this Minecraft version and loader."),
        ("java.lang.nosuchmethoderror", "Error", "A mod is calling an incompatible method. Update or remove the mod that introduced the mismatch."),
        ("mixin apply failed", "Error", "A mixin could not be applied. Look for outdated or conflicting mods in the lines immediately above this message."),
        ("mod resolution failed", "Error", "The mod loader could not resolve the installed mod set. Check loader versions and required dependencies."),
        ("incompatible mod set", "Error", "At least one installed mod is incompatible with this game or loader version."),
        ("failed to load mod", "Error", "A mod failed during initialization. Review its name and the first exception in the report."),
        ("unsupportedclassversionerror", "Error", "A mod or library requires a newer Java runtime. Select the Java version required by this Minecraft release."),
        ("could not reserve enough space", "Error", "Java could not reserve the requested memory. Reduce the allocated RAM and close other applications."),
        ("glfw error", "Warning", "The graphics/windowing layer reported an error. Update the graphics driver and check display compatibility."),
        ("opengl error", "Warning", "The graphics driver reported an OpenGL problem. Update the graphics driver and disable shaders for testing."),
        ("exit code -1073741819", "Warning", "Windows reported an application access violation. Check graphics drivers, overlays and recently added mods."),
    ];
    let mut findings = Vec::new();
    for (needle, severity, detail) in patterns {
        if lower.contains(needle) {
            findings.push(format!("[{severity}] {needle}\n{detail}"));
        }
    }
    let mut unique = BTreeSet::new();
    let mut evidence = Vec::new();
    for line in log.lines().rev() {
        let trimmed = line.trim();
        let line_lower = trimmed.to_ascii_lowercase();
        if trimmed.is_empty() { continue; }
        if (line_lower.contains("caused by:") || line_lower.contains("exception") || line_lower.contains("error") || line_lower.contains("failed") || line_lower.contains(" at "))
            && unique.insert(trimmed.to_owned())
        {
            evidence.push(trimmed.to_owned());
            if evidence.len() == 8 { break; }
        }
    }
    let mut output = String::from("LUNAR CRASH ANALYSIS\n\n");
    if findings.is_empty() {
        output.push_str("No known crash signature was detected. This is a heuristic scan, not a definitive diagnosis. Check the first exception and the final 100 lines of the log.\n");
    } else {
        output.push_str(&findings.join("\n\n"));
        output.push_str("\n\n");
    }
    if !evidence.is_empty() {
        output.push_str("LOG EVIDENCE\n");
        for line in evidence { output.push_str(&format!("• {line}\n")); }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn identifies_java_out_of_memory() { assert!(analyze("java.lang.OutOfMemoryError: Java heap space").contains("ran out of memory")); }
    #[test] fn identifies_missing_class() { assert!(analyze("java.lang.NoClassDefFoundError: net/example/Mod").contains("required class is missing")); }
    #[test] fn identifies_missing_method() { assert!(analyze("java.lang.NoSuchMethodError: example").contains("incompatible method")); }
    #[test] fn identifies_mixin_failure() { assert!(analyze("Mixin apply failed for mod example").contains("mixin could not be applied")); }
    #[test] fn identifies_mod_resolution_failure() { assert!(analyze("Mod resolution failed").contains("could not resolve")); }
    #[test] fn identifies_failed_mod_loading() { assert!(analyze("Failed to load mod example").contains("failed during initialization")); }
    #[test] fn identifies_java_version_mismatch() { assert!(analyze("UnsupportedClassVersionError: class version 65").contains("newer Java runtime")); }
    #[test] fn identifies_memory_reservation_failure() { assert!(analyze("Could not reserve enough space for object heap").contains("could not reserve")); }
    #[test] fn identifies_graphics_errors() { assert!(analyze("GLFW error 65542").contains("graphics/windowing")); assert!(analyze("OpenGL error 1282").contains("OpenGL problem")); }
    #[test] fn identifies_windows_access_violation() { assert!(analyze("Process exited with code -1073741819").contains("access violation")); }
    #[test] fn handles_unknown_logs_without_claiming_a_diagnosis() { assert!(analyze("Minecraft stopped unexpectedly").contains("No known crash signature")); }
    #[test] fn includes_recent_exception_evidence() { assert!(analyze("first line\nCaused by: java.lang.IllegalStateException: broken").contains("Caused by: java.lang.IllegalStateException: broken")); }
    #[test] fn limits_evidence_to_eight_lines() { let log = (0..20).map(|i| format!("ERROR failure {i}")).collect::<Vec<_>>().join("\n"); assert_eq!(analyze(&log).matches("ERROR failure").count(), 8); }
    #[test] fn ignores_blank_lines() { assert!(analyze("\n\n").contains("No known crash signature")); }
    #[test] fn detects_signatures_case_insensitively() { assert!(analyze("JAVA.LANG.OUTOFMEMORYERROR").contains("ran out of memory")); }
    #[test] fn deduplicates_repeated_evidence_lines() { assert_eq!(analyze("ERROR repeated\nERROR repeated\n").matches("ERROR repeated").count(), 1); }
}
