use std::io::Read;

const GITHUB_API_RELEASES: &str = "https://api.github.com/repos/yvgude/lean-ctx/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(args: &[String]) {
    let check_only = args.iter().any(|a| a == "--check");
    let insecure = args.iter().any(|a| a == "--insecure");

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
        tracing::error!("No binary found for this platform ({asset_name}). Download manually: https://github.com/yvgude/lean-ctx/releases/latest");
        std::process::exit(1);
    };

    let bytes = match download_bytes(&download_url) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("Download failed: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = verify_download_integrity(&release, &asset_name, &bytes) {
        if insecure {
            tracing::warn!("Integrity verification failed: {e}");
            tracing::warn!("Proceeding due to --insecure");
        } else {
            tracing::error!("Integrity verification failed: {e}");
            tracing::error!("Refusing to install an unverifiable binary. Re-run with `lean-ctx update --insecure` or download manually: https://github.com/yvgude/lean-ctx/releases/latest");
            std::process::exit(1);
        }
    }

    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Cannot locate current executable: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = replace_binary(&bytes, &asset_name, &current_exe) {
        tracing::error!("Failed to replace binary: {e}");
        tracing::warn!("Continuing with a setup refresh so your wiring stays correct");
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

fn verify_download_integrity(
    release: &serde_json::Value,
    asset_name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    #[cfg(not(feature = "secure-update"))]
    {
        let _ = (release, asset_name, bytes);
        return Err("secure-update feature disabled (sha256 verification unavailable)".to_string());
    }

    #[cfg(feature = "secure-update")]
    {
        let computed = sha256_hex(bytes);

        let Some((checksum_url, kind)) = find_checksum_asset_url(release, asset_name) else {
            return Err(
                "no checksum asset found for this release (expected SHA256SUMS or *.sha256)"
                    .to_string(),
            );
        };
        let checksum_bytes = download_bytes(&checksum_url)?;
        let checksum_text = String::from_utf8_lossy(&checksum_bytes).to_string();

        let expected = match kind {
            ChecksumAssetKind::SingleSha256 => parse_single_sha256(&checksum_text),
            ChecksumAssetKind::Sha256Sums => parse_sha256sums(&checksum_text, asset_name),
        }
        .ok_or_else(|| format!("checksum file did not contain an entry for {asset_name}"))?;

        if !constant_time_eq(computed.as_bytes(), expected.as_bytes()) {
            return Err(format!(
                "sha256 mismatch for {asset_name}: expected {expected}, got {computed}"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum ChecksumAssetKind {
    Sha256Sums,
    SingleSha256,
}

fn find_checksum_asset_url(
    release: &serde_json::Value,
    asset_name: &str,
) -> Option<(String, ChecksumAssetKind)> {
    // Prefer per-asset checksum (asset.ext.sha256) if present.
    let candidates = [
        format!("{asset_name}.sha256"),
        format!("{asset_name}.sha256.txt"),
        "SHA256SUMS".to_string(),
        "SHA256SUMS.txt".to_string(),
        "sha256sums.txt".to_string(),
        "checksums.txt".to_string(),
    ];

    for c in candidates {
        if let Some(url) = find_asset_url(release, &c) {
            let kind = if c.to_lowercase().contains("sha256sums")
                || c.to_uppercase() == "SHA256SUMS"
                || c.to_lowercase().contains("checksums")
            {
                ChecksumAssetKind::Sha256Sums
            } else {
                ChecksumAssetKind::SingleSha256
            };
            return Some((url, kind));
        }
    }
    None
}

fn parse_single_sha256(text: &str) -> Option<String> {
    let t = text.trim();
    let first = t.split_whitespace().next().unwrap_or("").trim();
    if first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(first.to_ascii_lowercase())
    } else {
        None
    }
}

fn parse_sha256sums(text: &str, asset_name: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let mut parts = l.split_whitespace();
        let hash = parts.next().unwrap_or("");
        let file = parts.next().unwrap_or("");
        if file == asset_name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    hex_lower(&out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn post_update_rewire() {
    // run_setup_with_options now handles daemon restart internally,
    // so no separate restart_daemon_if_running() call needed.
    let opts = crate::setup::SetupOptions {
        non_interactive: true,
        yes: true,
        fix: true,
        ..Default::default()
    };
    if let Err(e) = crate::setup::run_setup_with_options(opts) {
        tracing::error!("Setup refresh error: {e}");
    }

    restart_proxy_if_running();
}

fn restart_proxy_if_running() {
    let port = crate::proxy_setup::default_port();

    if restart_managed_proxy() {
        return;
    }

    if is_proxy_reachable(port) {
        println!(
            "  \x1b[33m⟳\x1b[0m Proxy running on port {port} — restart it to use the new binary:"
        );
        println!("    \x1b[1mlean-ctx proxy start --port={port}\x1b[0m");
    }
}

/// Restart proxy managed by launchd (macOS) or systemd (Linux).
/// Returns `true` if a managed service was found and restarted.
fn restart_managed_proxy() -> bool {
    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir()
            .unwrap_or_default()
            .join("Library/LaunchAgents/com.leanctx.proxy.plist");
        if plist_path.exists() {
            let plist_str = plist_path.to_string_lossy().to_string();
            let _ = std::process::Command::new("launchctl")
                .args(["unload", &plist_str])
                .output();
            let result = std::process::Command::new("launchctl")
                .args(["load", &plist_str])
                .output();
            match result {
                Ok(o) if o.status.success() => {
                    println!("  \x1b[32m✓\x1b[0m Proxy restarted (LaunchAgent)");
                }
                _ => {
                    println!("  \x1b[33m⚠\x1b[0m Could not restart proxy LaunchAgent");
                }
            }
            return true;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let service_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".config/systemd/user/lean-ctx-proxy.service");
        if service_path.exists() {
            let result = std::process::Command::new("systemctl")
                .args(["--user", "restart", "lean-ctx-proxy"])
                .output();
            match result {
                Ok(o) if o.status.success() => {
                    println!("  \x1b[32m✓\x1b[0m Proxy restarted (systemd)");
                }
                _ => {
                    println!("  \x1b[33m⚠\x1b[0m Could not restart proxy systemd service");
                }
            }
            return true;
        }
    }

    false
}

fn is_proxy_reachable(port: u16) -> bool {
    ureq::get(&format!("http://127.0.0.1:{port}/health"))
        .call()
        .is_ok()
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
