//! Command Code proxy wiring (single gateway rail).
//!
//! Binary is `cmd` (package `command-code`). The CLI honours
//! `COMMANDCODE_API_URL` only when `COMMANDCODE_SANDBOX=true`.
//! Auth is a session Bearer in `~/.commandcode/auth.json` (or
//! `COMMAND_CODE_API_KEY`). The proxy must forward
//! `x-command-code-version` or upstream returns 403 `upgrade_required`.
//!
//! MCP: `proxy enable` merges lean-ctx into `~/.commandcode/mcp.json`
//! (Command Code schema: transport/enabled/command/instructions).

use std::path::{Path, PathBuf};

use super::grok::{ShellFlavor, ensure_proxy_provider};
use super::util::{COMMANDCODE_OMITTED_NOTE, is_proxy_reachable};

pub(crate) const COMMANDCODE_PROVIDER_ID: &str = "commandcode";
pub(crate) const COMMANDCODE_UPSTREAM: &str = "https://api.commandcode.ai";

/// Instructions block for Command Code MCP hosts (shadow-mode rules).
pub(crate) const COMMANDCODE_MCP_INSTRUCTIONS: &str = "\
lean-ctx shadow mode: native file/search/shell calls auto-route to ctx_* — no tool-mapping needed.\n\
Exclusive tools (no native trigger): ctx_compose (search + read + symbols), \
ctx_search(action=symbol) (exact symbol), ctx_search(action=semantic) (by meaning), \
ctx_callgraph (callers), ctx_knowledge / ctx_session (memory).\n\
OUTPUT STYLE: concise\n\
- Bullet points over paragraphs\n\
- Skip filler words and hedging (\"I think\", \"probably\", \"it seems\")\n\
- 1-sentence explanations max, then code/action\n\
- No repeating what the user said";

pub(crate) fn commandcode_dir(home: &Path) -> PathBuf {
    home.join(".commandcode")
}

pub(crate) fn commandcode_mcp_path(home: &Path) -> PathBuf {
    commandcode_dir(home).join("mcp.json")
}

/// True when session auth or `COMMAND_CODE_API_KEY` is present.
pub(crate) fn commandcode_auth_available(home: &Path) -> bool {
    if std::env::var("COMMAND_CODE_API_KEY").is_ok_and(|v| !v.trim().is_empty()) {
        return true;
    }
    commandcode_session_auth_available(home)
}

pub(crate) fn commandcode_session_auth_available(home: &Path) -> bool {
    let auth = commandcode_dir(home).join("auth.json");
    let Ok(raw) = std::fs::read_to_string(&auth) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    v.get("apiKey")
        .and_then(|k| k.as_str())
        .is_some_and(|s| !s.trim().is_empty())
}

pub(crate) fn commandcode_proxy_base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/providers/{COMMANDCODE_PROVIDER_ID}")
}

/// Shell exports for Command Code.
///
/// `base` is the bare proxy origin (`http://127.0.0.1:{port}`); the registry
/// rail path is appended. Requires SANDBOX=true for API_URL to apply in the CLI.
pub(crate) fn render_commandcode_shell_exports(
    base: &str,
    include: bool,
    flavor: ShellFlavor,
) -> String {
    if !include {
        return format!("# {COMMANDCODE_OMITTED_NOTE}");
    }
    let url = format!("{base}/providers/{COMMANDCODE_PROVIDER_ID}");
    match flavor {
        ShellFlavor::Posix => format!(
            r#"export COMMANDCODE_SANDBOX="true"
export COMMANDCODE_API_URL="{url}""#
        ),
        ShellFlavor::Fish => format!(
            r#"set -gx COMMANDCODE_SANDBOX "true"
set -gx COMMANDCODE_API_URL "{url}""#
        ),
        ShellFlavor::PowerShell => format!(
            r#"$env:COMMANDCODE_SANDBOX = "true"
$env:COMMANDCODE_API_URL = "{url}""#
        ),
    }
}

/// Seed registry provider, install MCP, print enable guidance.
pub(crate) fn install_commandcode_env(home: &Path, port: u16, quiet: bool, force: bool) {
    let cc_dir = commandcode_dir(home);
    if !cc_dir.is_dir() && !force {
        if !quiet {
            println!(
                "  Command Code: skipped (no ~/.commandcode; install with `npm i -g command-code`)"
            );
        }
        return;
    }

    let auth_ok = commandcode_auth_available(home);
    if !auth_ok && !force {
        if !quiet {
            println!(
                "  Command Code: skipped (no session auth — run `cmd login` or set COMMAND_CODE_API_KEY)"
            );
        }
        return;
    }

    // MCP is independent of the LLM proxy — install whenever auth is ready.
    match install_commandcode_mcp(home) {
        Ok(msg) if !quiet => println!("  Command Code MCP: {msg}"),
        Err(e) if !quiet => println!("  Command Code MCP: failed — {e}"),
        _ => {}
    }

    if !is_proxy_reachable(port) {
        if !quiet {
            println!(
                "  Command Code: proxy not reachable on :{port}; start with `lean-ctx proxy start`"
            );
        }
        return;
    }

    ensure_proxy_provider(COMMANDCODE_PROVIDER_ID, COMMANDCODE_UPSTREAM, quiet);
    if !quiet {
        let base = commandcode_proxy_base_url(port);
        println!(
            "  Command Code: shell exports COMMANDCODE_SANDBOX=true + COMMANDCODE_API_URL={base}"
        );
        if auth_ok {
            println!(
                "  Command Code: session auth detected (~/.commandcode/auth.json or COMMAND_CODE_API_KEY)"
            );
        } else {
            println!(
                "  Command Code: no session auth yet — run `cmd login` (or set COMMAND_CODE_API_KEY)"
            );
        }
    }
}

/// Remove lean-ctx from Command Code MCP config.
pub(crate) fn uninstall_commandcode_env(home: &Path, quiet: bool) {
    match uninstall_commandcode_mcp(home) {
        Ok(Some(msg)) if !quiet => println!("  Command Code MCP: {msg}"),
        Err(e) if !quiet => println!("  Command Code MCP: uninstall failed — {e}"),
        _ => {}
    }
}

/// Merge lean-ctx into `~/.commandcode/mcp.json` (Command Code schema).
pub(crate) fn install_commandcode_mcp(home: &Path) -> Result<String, String> {
    let path = commandcode_mcp_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    let mut root = if path.exists() {
        let raw =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if raw.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?
        }
    } else {
        serde_json::json!({})
    };

    if !root.is_object() {
        return Err(format!("{}: root must be a JSON object", path.display()));
    }

    let servers = root
        .as_object_mut()
        .expect("root verified as JSON object above")
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        return Err(format!(
            "{}: mcpServers must be a JSON object",
            path.display()
        ));
    }

    let entry = serde_json::json!({
        "transport": "stdio",
        "enabled": true,
        "command": "lean-ctx",
        "instructions": COMMANDCODE_MCP_INSTRUCTIONS,
    });
    servers
        .as_object_mut()
        .expect("mcpServers verified as JSON object above")
        .insert("lean-ctx".to_string(), entry);

    let pretty =
        serde_json::to_string_pretty(&root).map_err(|e| format!("serialize mcp.json: {e}"))?;
    std::fs::write(&path, pretty + "\n").map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(format!("wrote lean-ctx → {}", path.display()))
}

/// Remove only the lean-ctx server entry. Leaves other MCP servers intact.
pub(crate) fn uninstall_commandcode_mcp(home: &Path) -> Result<Option<String>, String> {
    let path = commandcode_mcp_path(home);
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut root: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let Some(servers) = root.get_mut("mcpServers").and_then(|s| s.as_object_mut()) else {
        return Ok(None);
    };
    if servers.remove("lean-ctx").is_none() {
        return Ok(None);
    }
    let pretty =
        serde_json::to_string_pretty(&root).map_err(|e| format!("serialize mcp.json: {e}"))?;
    std::fs::write(&path, pretty + "\n").map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(Some(format!("removed lean-ctx from {}", path.display())))
}
