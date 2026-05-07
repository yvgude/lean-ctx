use std::path::Path;

use crate::marked_block;

const PROXY_ENV_START: &str = "# >>> lean-ctx proxy env >>>";
const PROXY_ENV_END: &str = "# <<< lean-ctx proxy env <<<";

const DEFAULT_PROXY_PORT: u16 = 4444;

pub fn install_proxy_env(home: &Path, port: u16, quiet: bool) {
    install_shell_exports(home, port, quiet);
    install_claude_env(home, port, quiet);
    install_codex_env(home, port, quiet);
}

pub fn uninstall_proxy_env(home: &Path, quiet: bool) {
    for rc in &[home.join(".zshrc"), home.join(".bashrc")] {
        let label = format!(
            "proxy env from ~/{}",
            rc.file_name().unwrap_or_default().to_string_lossy()
        );
        marked_block::remove_from_file(rc, PROXY_ENV_START, PROXY_ENV_END, quiet, &label);
    }
}

fn install_shell_exports(home: &Path, port: u16, quiet: bool) {
    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping shell proxy exports (proxy not running on port {port})");
        }
        return;
    }

    let base = format!("http://127.0.0.1:{port}");

    let block = format!(
        r#"{PROXY_ENV_START}
export GEMINI_API_BASE_URL="{base}"
{PROXY_ENV_END}"#
    );

    for rc in &[home.join(".zshrc"), home.join(".bashrc")] {
        if rc.exists() {
            let label = format!(
                "proxy env in ~/{}",
                rc.file_name().unwrap_or_default().to_string_lossy()
            );
            marked_block::upsert(rc, PROXY_ENV_START, PROXY_ENV_END, &block, quiet, &label);
        }
    }
}

fn install_claude_env(home: &Path, port: u16, quiet: bool) {
    let base = format!("http://127.0.0.1:{port}");
    let mut cfg = crate::core::config::Config::load_global();
    let capture = capture_claude_upstream_from_settings(home, &base, quiet, &mut cfg);
    if capture.captured {
        let _ = cfg.save();
    }

    if capture.already_local_proxy {
        return;
    }

    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping Claude Code proxy env (proxy not running on port {port})");
        }
        return;
    }

    write_claude_proxy_env(home, &base, quiet);
}

#[derive(Default)]
struct ClaudeUpstreamCapture {
    captured: bool,
    already_local_proxy: bool,
}

fn capture_claude_upstream_from_settings(
    home: &Path,
    proxy_base: &str,
    quiet: bool,
    cfg: &mut crate::core::config::Config,
) -> ClaudeUpstreamCapture {
    let settings_dir = crate::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&existing) else {
        return ClaudeUpstreamCapture::default();
    };

    let current = doc
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if current == proxy_base {
        if !quiet {
            println!("  Claude Code proxy env already configured");
            if !has_custom_anthropic_upstream(cfg) {
                println!(
                    "  Anthropic upstream not configured; run: lean-ctx config set proxy.anthropic_upstream <url>"
                );
            }
        }
        return ClaudeUpstreamCapture {
            captured: false,
            already_local_proxy: true,
        };
    }

    let captured = capture_claude_upstream_in_config(current, proxy_base, cfg);
    if captured && !quiet {
        let path = crate::core::config::Config::path().map_or_else(
            || "~/.lean-ctx/config.toml".to_string(),
            |p| p.to_string_lossy().to_string(),
        );
        println!("  Saved Claude Code upstream to {path}");
        println!("    proxy.anthropic_upstream = {current}");
        println!("    Change later with: lean-ctx config set proxy.anthropic_upstream <url>");
    }

    ClaudeUpstreamCapture {
        captured,
        already_local_proxy: false,
    }
}

fn write_claude_proxy_env(home: &Path, base: &str, quiet: bool) {
    let settings_dir = crate::core::editor_registry::claude_state_dir(home);
    let settings_path = settings_dir.join("settings.json");
    let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();

    let mut doc: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(&existing) {
            Ok(v) => v,
            Err(_) => return,
        }
    };

    if doc
        .as_object_mut()
        .and_then(|o| {
            o.entry("env")
                .or_insert(serde_json::json!({}))
                .as_object_mut()
                .map(|_| ())
        })
        .is_none()
    {
        return;
    }

    if let Some(env_obj) = doc.get_mut("env").and_then(|e| e.as_object_mut()) {
        env_obj.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            serde_json::Value::String(base.to_string()),
        );
    }

    let _ = std::fs::create_dir_all(&settings_dir);
    let content = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let _ = std::fs::write(&settings_path, content + "\n");
    if !quiet {
        println!("  Configured ANTHROPIC_BASE_URL in Claude Code settings");
    }
}

fn has_custom_anthropic_upstream(cfg: &crate::core::config::Config) -> bool {
    cfg.proxy
        .anthropic_upstream
        .as_deref()
        .is_some_and(|v| normalize_captured_upstream(v).is_some_and(|u| !is_local_proxy_url(&u)))
}

fn capture_claude_upstream_in_config(
    current: &str,
    proxy_base: &str,
    cfg: &mut crate::core::config::Config,
) -> bool {
    let Some(upstream) = normalize_captured_upstream(current) else {
        return false;
    };
    if upstream == normalize_url(proxy_base) || is_local_proxy_url(&upstream) {
        return false;
    }

    if cfg
        .proxy
        .anthropic_upstream
        .as_deref()
        .is_some_and(|v| normalize_captured_upstream(v).is_some())
    {
        return false;
    }

    cfg.proxy.anthropic_upstream = Some(upstream);
    true
}

fn normalize_captured_upstream(value: &str) -> Option<String> {
    let trimmed = normalize_url(value);
    if trimmed.is_empty() {
        return None;
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return None;
    }
    Some(trimmed)
}

fn normalize_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn is_local_proxy_url(value: &str) -> bool {
    value.starts_with("http://127.0.0.1:")
        || value.starts_with("http://localhost:")
        || value.starts_with("http://[::1]:")
}

fn is_proxy_reachable(port: u16) -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}")
            .parse()
            .expect("BUG: invalid hardcoded socket address"),
        Duration::from_millis(200),
    )
    .is_ok()
}

fn install_codex_env(home: &Path, port: u16, quiet: bool) {
    let base = format!("http://127.0.0.1:{port}");

    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping Codex CLI proxy env (proxy not running on port {port})");
        }
        return;
    }

    let config_dir = home.join(".codex");
    let config_path = config_dir.join("config.toml");

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();

    if existing.contains("OPENAI_BASE_URL") && existing.contains(&base) {
        if !quiet {
            println!("  Codex CLI proxy env already configured");
        }
        return;
    }

    if !config_dir.exists() {
        return;
    }

    let mut content = existing;

    if content.contains("[env]") {
        if !content.contains("OPENAI_BASE_URL") {
            content = content.replace("[env]", &format!("[env]\nOPENAI_BASE_URL = \"{base}\""));
        }
    } else {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("\n[env]\nOPENAI_BASE_URL = \"{base}\"\n"));
    }

    let _ = std::fs::write(&config_path, &content);
    if !quiet {
        println!("  Configured OPENAI_BASE_URL in Codex CLI config");
    }
}

pub fn default_port() -> u16 {
    DEFAULT_PROXY_PORT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_existing_claude_upstream_into_proxy_config() {
        let mut cfg = crate::core::config::Config::default();

        let captured = capture_claude_upstream_in_config(
            "https://gateway.example.test/api/code",
            "http://127.0.0.1:4444",
            &mut cfg,
        );

        assert!(captured);
        assert_eq!(
            cfg.proxy.anthropic_upstream.as_deref(),
            Some("https://gateway.example.test/api/code")
        );
    }

    #[test]
    fn captures_existing_claude_upstream_even_when_proxy_is_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let settings_dir = crate::core::editor_registry::claude_state_dir(&home);
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.example.test/api/code"}}"#,
        )
        .unwrap();
        let mut cfg = crate::core::config::Config::default();

        let capture =
            capture_claude_upstream_from_settings(&home, "http://127.0.0.1:9", true, &mut cfg);

        assert!(capture.captured);
        assert!(!capture.already_local_proxy);
        assert_eq!(
            cfg.proxy.anthropic_upstream.as_deref(),
            Some("https://gateway.example.test/api/code")
        );
        let settings = std::fs::read_to_string(settings_dir.join("settings.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&settings).unwrap();
        assert_eq!(
            doc["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("https://gateway.example.test/api/code")
        );
    }

    #[test]
    fn preserves_existing_custom_upstream() {
        let mut cfg = crate::core::config::Config::default();
        cfg.proxy.anthropic_upstream = Some("https://existing.example.test".to_string());

        let captured = capture_claude_upstream_in_config(
            "https://gateway.example.test/api/code",
            "http://127.0.0.1:4444",
            &mut cfg,
        );

        assert!(!captured);
        assert_eq!(
            cfg.proxy.anthropic_upstream.as_deref(),
            Some("https://existing.example.test")
        );
    }

    #[test]
    fn ignores_local_proxy_when_capturing_claude_upstream() {
        let mut cfg = crate::core::config::Config::default();

        let captured = capture_claude_upstream_in_config(
            "http://127.0.0.1:4444",
            "http://127.0.0.1:4444",
            &mut cfg,
        );

        assert!(!captured);
        assert!(cfg.proxy.anthropic_upstream.is_none());
    }
}
