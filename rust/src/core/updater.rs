use std::io::Read;

const GITHUB_API_RELEASES: &str = "https://api.github.com/repos/yvgude/lean-ctx/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(args: &[String]) {
    let check_only = args.iter().any(|a| a == "--check");

    println!();
    println!("  \x1b[1m◆ lean-ctx updater\x1b[0m  \x1b[2mv{CURRENT_VERSION}\x1b[0m");
    println!("  \x1b[2mChecking github.com/yvgude/lean-ctx …\x1b[0m");

    let release = match fetch_latest_release() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error fetching release info: {e}");
            std::process::exit(1);
        }
    };

    let latest_tag = match release["tag_name"].as_str() {
        Some(t) => t.trim_start_matches('v').to_string(),
        None => {
            eprintln!("Could not parse release tag from GitHub API.");
            std::process::exit(1);
        }
    };

    if latest_tag == CURRENT_VERSION {
        println!("  \x1b[32m✓\x1b[0m Already up to date (v{CURRENT_VERSION}).");
        println!("  \x1b[2mIf your IDE still uses an older version, restart it to reconnect the MCP server.\x1b[0m");
        println!();
        return;
    }

    println!("  Update available: v{CURRENT_VERSION} → \x1b[1;32mv{latest_tag}\x1b[0m");

    if check_only {
        println!("Run 'lean-ctx update' to install.");
        return;
    }

    let asset_name = platform_asset_name();
    println!("  \x1b[2mDownloading {asset_name} …\x1b[0m");

    let download_url = match find_asset_url(&release, &asset_name) {
        Some(u) => u,
        None => {
            eprintln!("No binary found for this platform ({asset_name}).");
            eprintln!("Download manually: https://github.com/yvgude/lean-ctx/releases/latest");
            std::process::exit(1);
        }
    };

    let bytes = match download_bytes(&download_url) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Download failed: {e}");
            std::process::exit(1);
        }
    };

    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Cannot locate current executable: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = replace_binary(&bytes, &asset_name, &current_exe) {
        eprintln!("Failed to replace binary: {e}");
        std::process::exit(1);
    }

    println!();
    println!("  \x1b[1;32m✓ Updated to lean-ctx v{latest_tag}\x1b[0m");
    println!("  \x1b[2mBinary: {}\x1b[0m", current_exe.display());

    println!();
    println!("  \x1b[36m\x1b[1mUpdating agent rules & hooks…\x1b[0m");
    post_update_refresh();

    println!();
    crate::terminal_ui::print_logo_animated();
    println!();
    println!("  \x1b[33m\x1b[1m⟳ Restart your IDE / AI tool to activate the new version.\x1b[0m");
    println!("    \x1b[2mClose and re-open Cursor, VS Code, Claude Code, etc. completely.\x1b[0m");
    println!("    \x1b[2mThe MCP server must reconnect to use the updated binary.\x1b[0m");
    println!();
}

fn post_update_refresh() {
    if let Some(home) = dirs::home_dir() {
        let rules_result = crate::rules_inject::inject_all_rules(&home);
        let rules_count = rules_result.injected.len() + rules_result.updated.len();
        if rules_count > 0 {
            let names: Vec<String> = rules_result
                .injected
                .iter()
                .chain(rules_result.updated.iter())
                .cloned()
                .collect();
            println!("    \x1b[32m✓\x1b[0m Rules updated: {}", names.join(", "));
        }
        if !rules_result.already.is_empty() {
            println!(
                "    \x1b[32m✓\x1b[0m Rules up-to-date: {}",
                rules_result.already.join(", ")
            );
        }

        crate::hooks::refresh_installed_hooks();
        println!("    \x1b[32m✓\x1b[0m Hook scripts refreshed");
    }
}

fn fetch_latest_release() -> Result<serde_json::Value, String> {
    let response = ureq::get(GITHUB_API_RELEASES)
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| e.to_string())?;

    response
        .into_body()
        .read_to_string()
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
}

fn find_asset_url(release: &serde_json::Value, asset_name: &str) -> Option<String> {
    release["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(asset_name))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(|s| s.to_string())
}

fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::get(url)
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .call()
        .map_err(|e| e.to_string())?;

    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn replace_binary(
    archive_bytes: &[u8],
    asset_name: &str,
    current_exe: &std::path::Path,
) -> Result<(), String> {
    let binary_bytes = if asset_name.ends_with(".zip") {
        extract_from_zip(archive_bytes)?
    } else {
        extract_from_tar_gz(archive_bytes)?
    };

    let tmp_path = current_exe.with_extension("tmp");
    std::fs::write(&tmp_path, &binary_bytes).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755));
    }

    // On Windows, a running executable can be renamed but not overwritten.
    // Move the current binary out of the way first, then move the new one in.
    // If the file is locked (MCP server running), schedule a deferred update.
    #[cfg(windows)]
    {
        let old_path = current_exe.with_extension("old.exe");
        let _ = std::fs::remove_file(&old_path);

        match std::fs::rename(current_exe, &old_path) {
            Ok(()) => {
                if let Err(e) = std::fs::rename(&tmp_path, current_exe) {
                    let _ = std::fs::rename(&old_path, current_exe);
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(format!("Cannot place new binary: {e}"));
                }
                let _ = std::fs::remove_file(&old_path);
                return Ok(());
            }
            Err(_) => {
                return deferred_windows_update(&tmp_path, current_exe);
            }
        }
    }

    #[cfg(not(windows))]
    {
        std::fs::rename(&tmp_path, current_exe).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            format!("Cannot replace binary (permission denied?): {e}")
        })?;

        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("codesign")
                .args(["--force", "-s", "-", &current_exe.display().to_string()])
                .output();
        }

        Ok(())
    }
}

/// Generate the deferred update .bat script content.
/// Extracted for testability — the script must:
/// 1. NOT use `timeout /t` (conflicts with GNU timeout from Git Bash/Cygwin)
/// 2. Use `ping 127.0.0.1 -n 2` for delay (works everywhere on Windows)
/// 3. Have a retry limit to prevent infinite loops
pub fn generate_update_script(target: &str, pending: &str, old: &str) -> String {
    format!(
        r#"@echo off
echo Waiting for lean-ctx to be released...
set RETRIES=0
:retry
if %RETRIES% GEQ 60 (
    echo Update timed out after 60 attempts. Please close all editors and run: lean-ctx update
    del /f "{pending}" >nul 2>&1
    pause
    exit /b 1
)
set /a RETRIES+=1
ping 127.0.0.1 -n 2 >nul
move /Y "{target}" "{old}" >nul 2>&1
if errorlevel 1 goto retry
move /Y "{pending}" "{target}" >nul 2>&1
if errorlevel 1 (
    move /Y "{old}" "{target}" >nul 2>&1
    echo Update failed. Please close all editors and run: lean-ctx update
    pause
    exit /b 1
)
del /f "{old}" >nul 2>&1
echo Updated successfully!
del "%~f0" >nul 2>&1
"#,
        target = target,
        pending = pending,
        old = old,
    )
}

/// On Windows, when the binary is locked by an MCP server, we can't rename it.
/// Instead, stage the new binary and spawn a background cmd process that waits
/// for the lock to be released, then performs the swap.
#[cfg(windows)]
fn deferred_windows_update(
    staged_path: &std::path::Path,
    target_exe: &std::path::Path,
) -> Result<(), String> {
    let pending_path = target_exe.with_file_name("lean-ctx-pending.exe");
    std::fs::rename(staged_path, &pending_path).map_err(|e| {
        let _ = std::fs::remove_file(staged_path);
        format!("Cannot stage update: {e}")
    })?;

    let target_str = target_exe.display().to_string();
    let pending_str = pending_path.display().to_string();
    let old_str = target_exe.with_extension("old.exe").display().to_string();

    let script = generate_update_script(&target_str, &pending_str, &old_str);

    let script_path = target_exe.with_file_name("lean-ctx-update.bat");
    std::fs::write(&script_path, &script)
        .map_err(|e| format!("Cannot write update script: {e}"))?;

    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "/MIN", &script_path.display().to_string()])
        .spawn();

    println!("\nThe binary is currently in use by your AI editor's MCP server.");
    println!("A background update has been scheduled.");
    println!(
        "Close your editor (Cursor, VS Code, etc.) and the update will complete automatically."
    );
    println!("Or run the script manually: {}", script_path.display());

    Ok(())
}

fn extract_from_tar_gz(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::read::GzDecoder;

    let gz = GzDecoder::new(data);
    let mut archive = tar::Archive::new(gz);

    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if name == "lean-ctx" || name == "lean-ctx.exe" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            return Ok(bytes);
        }
    }
    Err("lean-ctx binary not found inside archive".to_string())
}

fn extract_from_zip(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Cursor;

    let cursor = Cursor::new(data);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| e.to_string())?;

    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().to_string();
        if name == "lean-ctx.exe" || name == "lean-ctx" {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            return Ok(bytes);
        }
    }
    Err("lean-ctx binary not found inside zip archive".to_string())
}

fn detect_linux_libc() -> &'static str {
    let output = std::process::Command::new("ldd").arg("--version").output();
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let combined = format!("{text}{stderr}");
        for line in combined.lines() {
            if let Some(ver) = line.split_whitespace().last() {
                let parts: Vec<&str> = ver.split('.').collect();
                if parts.len() == 2 {
                    if let (Ok(major), Ok(minor)) =
                        (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                    {
                        if major > 2 || (major == 2 && minor >= 35) {
                            return "gnu";
                        }
                        return "musl";
                    }
                }
            }
        }
    }
    "musl"
}

fn platform_asset_name() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let target = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_string(),
        ("linux", "x86_64") => format!("x86_64-unknown-linux-{}", detect_linux_libc()),
        ("linux", "aarch64") => format!("aarch64-unknown-linux-{}", detect_linux_libc()),
        ("windows", "x86_64") => "x86_64-pc-windows-msvc".to_string(),
        _ => {
            eprintln!(
                "Unsupported platform: {os}/{arch}. Download manually from \
                https://github.com/yvgude/lean-ctx/releases/latest"
            );
            std::process::exit(1);
        }
    };

    if os == "windows" {
        format!("lean-ctx-{target}.zip")
    } else {
        format!("lean-ctx-{target}.tar.gz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_script_does_not_use_timeout_command() {
        let script = generate_update_script(
            r"C:\bin\lean-ctx.exe",
            r"C:\bin\lean-ctx-pending.exe",
            r"C:\bin\lean-ctx.old.exe",
        );
        assert!(
            !script.contains("timeout"),
            "script must not use `timeout` (conflicts with GNU timeout): {script}"
        );
    }

    #[test]
    fn update_script_uses_ping_for_delay() {
        let script = generate_update_script("target.exe", "pending.exe", "old.exe");
        assert!(
            script.contains("ping 127.0.0.1 -n 2"),
            "script must use ping for delay: {script}"
        );
    }

    #[test]
    fn update_script_has_retry_limit() {
        let script = generate_update_script("target.exe", "pending.exe", "old.exe");
        assert!(
            script.contains("RETRIES") && script.contains("GEQ 60"),
            "script must have a retry counter with limit: {script}"
        );
    }

    #[test]
    fn update_script_increments_retry_counter() {
        let script = generate_update_script("target.exe", "pending.exe", "old.exe");
        assert!(
            script.contains("set /a RETRIES+=1"),
            "script must increment retry counter: {script}"
        );
    }

    #[test]
    fn update_script_has_timeout_exit() {
        let script = generate_update_script("target.exe", "pending.exe", "old.exe");
        assert!(
            script.contains("timed out") && script.contains("exit /b 1"),
            "script must exit with error after timeout: {script}"
        );
    }

    #[test]
    fn update_script_cleans_pending_on_timeout() {
        let script = generate_update_script("t.exe", "pending.exe", "old.exe");
        let timeout_section = script
            .find("GEQ 60")
            .map(|i| &script[i..i + 200])
            .unwrap_or("");
        assert!(
            timeout_section.contains("del") && timeout_section.contains("pending.exe"),
            "script must clean up pending file on timeout: {timeout_section}"
        );
    }

    #[test]
    fn update_script_substitutes_paths_correctly() {
        let script = generate_update_script(
            r"C:\Users\Jaina\bin\lean-ctx.exe",
            r"C:\Users\Jaina\bin\lean-ctx-pending.exe",
            r"C:\Users\Jaina\bin\lean-ctx.old.exe",
        );
        assert!(
            script.contains(r"C:\Users\Jaina\bin\lean-ctx.exe"),
            "target path: {script}"
        );
        assert!(
            script.contains(r"C:\Users\Jaina\bin\lean-ctx-pending.exe"),
            "pending path: {script}"
        );
        assert!(
            script.contains(r"C:\Users\Jaina\bin\lean-ctx.old.exe"),
            "old path: {script}"
        );
    }

    #[test]
    fn update_script_paths_with_spaces() {
        let script = generate_update_script(
            r"C:\Program Files\lean-ctx\lean-ctx.exe",
            r"C:\Program Files\lean-ctx\lean-ctx-pending.exe",
            r"C:\Program Files\lean-ctx\lean-ctx.old.exe",
        );
        assert!(
            script.contains(r"C:\Program Files\lean-ctx\lean-ctx.exe"),
            "paths with spaces must be preserved: {script}"
        );
    }

    #[test]
    fn update_script_is_valid_batch() {
        let script = generate_update_script("t.exe", "p.exe", "o.exe");
        assert!(script.starts_with("@echo off"), "must start with @echo off");
        assert!(script.contains(":retry"), "must have :retry label");
        assert!(script.contains("goto retry"), "must have goto retry");
        assert!(
            script.contains(r#"del "%~f0""#),
            "must self-delete at the end"
        );
    }

    #[test]
    fn update_script_rollback_on_failure() {
        let script = generate_update_script("target.exe", "pending.exe", "old.exe");
        let move_back = script.contains(r#"move /Y "old.exe" "target.exe""#);
        assert!(move_back, "must roll back on failure: {script}");
    }
}
