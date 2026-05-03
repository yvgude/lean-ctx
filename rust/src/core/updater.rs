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
            tracing::error!("Error fetching release info: {e}");
            std::process::exit(1);
        }
    };

    let latest_tag = if let Some(t) = release["tag_name"].as_str() {
        t.trim_start_matches('v').to_string()
    } else {
        tracing::error!("Could not parse release tag from GitHub API.");
        std::process::exit(1);
    };

    if latest_tag == CURRENT_VERSION {
        println!("  \x1b[32m✓\x1b[0m Already up to date (v{CURRENT_VERSION}).");
        println!("  \x1b[2mIf your IDE still uses an older version, restart it to reconnect the MCP server.\x1b[0m");
        println!();
        if !check_only {
            println!("  \x1b[36m\x1b[1mRefreshing setup (shell hook, MCP configs, rules)…\x1b[0m");
            post_update_rewire();
            println!();
        }
        return;
    }

    println!("  Update available: v{CURRENT_VERSION} → \x1b[1;32mv{latest_tag}\x1b[0m");

    if check_only {
        println!("Run 'lean-ctx update' to install.");
        return;
    }

    let asset_name = platform_asset_name();
    println!("  \x1b[2mDownloading {asset_name} …\x1b[0m");

    let Some(download_url) = find_asset_url(&release, &asset_name) else {
        tracing::error!("No binary found for this platform ({asset_name}).");
        eprintln!("Download manually: https://github.com/yvgude/lean-ctx/releases/latest");
        std::process::exit(1);
    };

    let bytes = match download_bytes(&download_url) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("Download failed: {e}");
            std::process::exit(1);
        }
    };

    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Cannot locate current executable: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = replace_binary(&bytes, &asset_name, &current_exe) {
        tracing::error!("Failed to replace binary: {e}");
        eprintln!();
        eprintln!("Continuing with a setup refresh so your wiring stays correct.");
        post_update_rewire();
        std::process::exit(1);
    }

    println!();
    println!("  \x1b[1;32m✓ Updated to lean-ctx v{latest_tag}\x1b[0m");
    println!("  \x1b[2mBinary: {}\x1b[0m", current_exe.display());

    println!();
    println!("  \x1b[36m\x1b[1mRefreshing setup (shell hook, MCP configs, rules)…\x1b[0m");
    post_update_rewire();

    println!();
    crate::terminal_ui::print_logo_animated();
    println!();
    println!("  \x1b[33m\x1b[1m⟳ Restart your IDE and shell to activate the new version.\x1b[0m");
    println!("    \x1b[2mClose and re-open Cursor, VS Code, Claude Code, etc. completely.\x1b[0m");
    println!("    \x1b[2mThe MCP server must reconnect to use the updated binary.\x1b[0m");
    println!(
        "    \x1b[2mRun 'source ~/.zshrc' (or restart terminal) for updated shell aliases.\x1b[0m"
    );
    println!();
}

fn post_update_rewire() {
    let opts = crate::setup::SetupOptions {
        non_interactive: true,
        yes: true,
        // After a binary update, rewire existing configs to the current binary.
        fix: true,
        json: false,
    };
    if let Err(e) = crate::setup::run_setup_with_options(opts) {
        tracing::error!("Setup refresh error: {e}");
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
        .map(std::string::ToString::to_string)
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
    let binary_bytes = if std::path::Path::new(asset_name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
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
        // On macOS, rename-over-running-binary causes SIGKILL because the kernel
        // re-validates code pages against the (now different) on-disk file.
        // Unlinking first is safe: the kernel keeps the old memory-mapped pages
        // from the deleted inode, while the new file gets a fresh inode at the path.
        #[cfg(target_os = "macos")]
        {
            let _ = std::fs::remove_file(current_exe);
        }

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

    let script = format!(
        r#"@echo off
echo Waiting for lean-ctx to be released...
:retry
timeout /t 1 /nobreak >nul
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
        target = target_str,
        pending = pending_str,
        old = old_str,
    );

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
            tracing::error!(
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
