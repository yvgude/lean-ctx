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

    if !is_proxy_reachable(port) {
        if !quiet {
            println!("  Skipping Claude Code proxy env (proxy not running on port {port})");
        }
        return;
    }

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

    let env = doc
        .as_object_mut()
        .and_then(|o| {
            o.entry("env")
                .or_insert(serde_json::json!({}))
                .as_object_mut()
                .map(|_| ())
        })
        .is_some();

    if env {
        if let Some(env_obj) = doc.get_mut("env").and_then(|e| e.as_object_mut()) {
            let current = env_obj
                .get("ANTHROPIC_BASE_URL")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if current == base {
                if !quiet {
                    println!("  Claude Code proxy env already configured");
                }
                return;
            }
            env_obj.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                serde_json::Value::String(base),
            );
        }
    }

    let _ = std::fs::create_dir_all(&settings_dir);
    let content = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let _ = std::fs::write(&settings_path, content + "\n");
    if !quiet {
        println!("  Configured ANTHROPIC_BASE_URL in Claude Code settings");
    }
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
