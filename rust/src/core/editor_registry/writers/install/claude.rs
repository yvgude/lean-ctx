use serde_json::Value;

#[allow(clippy::wildcard_imports)]
use super::super::shared::*;
use super::super::{WriteAction, WriteOptions, WriteResult};
use crate::core::editor_registry::types::EditorTarget;

pub(crate) fn write_mcp_json(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    let include_aa = supports_auto_approve(target);
    // `agent_key` is the setup key ("claude"), not the constraint id
    // ("claude-code"): fall back to the display name so Claude's rules apply.
    let constraints = crate::core::client_constraints::by_client_id(&target.agent_key)
        .or_else(|| crate::core::client_constraints::by_editor_name(target.name));
    let wants_instructions = constraints.is_none_or(|c| c.supports_config_instructions);
    let mut desired = if target.agent_key.is_empty() || !wants_instructions {
        lean_ctx_server_entry(binary, include_aa)
    } else {
        lean_ctx_server_entry_with_instructions(binary, include_aa, &target.agent_key)
    };
    let is_claude = target.agent_key == "claude" || target.name == "Claude Code";
    if is_claude {
        desired["alwaysLoad"] = Value::Bool(true);
    }

    // Claude Code manages ~/.claude.json and may overwrite it on first start.
    // Prefer the official CLI integration when available.
    // Skip when LEAN_CTX_QUIET=1 (bootstrap --json / setup --json) to avoid
    // spawning `claude mcp add-json` which can stall in non-interactive CI.
    // Never from unit tests: on a machine with a trusted native install it
    // would rewrite the developer's real ~/.claude.json.
    if is_claude
        && !cfg!(test)
        && !matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
        && let Ok(result) = try_claude_mcp_add(&desired)
    {
        return Ok(result);
    }

    if target.config_path.exists() {
        let content = std::fs::read_to_string(&target.config_path).map_err(|e| e.to_string())?;
        let mut json = match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) => v,
            Err(_e) => {
                return handle_invalid_json_write(
                    &target.config_path,
                    &content,
                    "mcpServers",
                    "lean-ctx",
                    &desired,
                    opts.overwrite_invalid,
                );
            }
        };
        let obj = json
            .as_object_mut()
            .ok_or_else(|| "root JSON must be an object".to_string())?;

        let servers = obj
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        let servers_obj = servers
            .as_object_mut()
            .ok_or_else(|| "\"mcpServers\" must be an object".to_string())?;

        let existing = servers_obj.get("lean-ctx").cloned();
        if existing.as_ref() == Some(&desired) {
            return Ok(WriteResult {
                action: WriteAction::Already,
                note: None,
            });
        }
        servers_obj.insert("lean-ctx".to_string(), desired);

        let formatted = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&target.config_path, &formatted)?;
        return Ok(WriteResult {
            action: WriteAction::Updated,
            note: None,
        });
    }

    write_mcp_json_fresh(&target.config_path, &desired, None)
}

pub(crate) fn find_in_path(binary: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The trusted `claude` executable, resolved to its real file on every platform
/// (Windows: `claude.exe`; an npm `claude.cmd` shim cannot be spawned directly
/// and is not trusted). `LEAN_CTX_TRUST_CLAUDE_PATH=1` overrides the location
/// check for unusual installs — only the exact value `1`.
pub(crate) fn validate_claude_binary() -> Result<std::path::PathBuf, String> {
    let name = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let path = find_in_path(name).ok_or_else(|| format!("{name} not found in PATH"))?;

    let canonical =
        std::fs::canonicalize(&path).map_err(|e| format!("cannot resolve claude path: {e}"))?;
    let home = crate::core::home::resolve_home_dir().and_then(|h| std::fs::canonicalize(h).ok());

    if !is_trusted_claude_path(&canonical, home.as_deref())
        && std::env::var("LEAN_CTX_TRUST_CLAUDE_PATH").as_deref() != Ok("1")
    {
        return Err(format!(
            "claude binary resolved to untrusted path: {} — set LEAN_CTX_TRUST_CLAUDE_PATH=1 to override",
            canonical.display()
        ));
    }
    Ok(canonical)
}

/// Install locations of genuine Claude Code builds, matched by path
/// *components* (a substring test accepted look-alikes such as
/// `/tmp/x/.local/share/claude/…`). User-level installs must sit under the real
/// home directory; system installs under their fixed prefixes. The official
/// native installer links `~/.local/bin/claude` to
/// `~/.local/share/claude/versions/<v>`. A project-local `node_modules/.bin`
/// is deliberately not trusted: it would let any checked-out repo supply the
/// `claude` lean-ctx executes.
fn is_trusted_claude_path(canonical: &std::path::Path, home: Option<&std::path::Path>) -> bool {
    const HOME_ROOTS: &[&str] = &[
        ".claude",
        ".local/share/claude",
        ".npm",
        ".npm-global",
        ".nvm",
        ".bun",
        "AppData",
    ];
    const SYSTEM_ROOTS: &[&str] = &[
        "/usr/local/bin",
        "/usr/local/lib/node_modules",
        "/opt/homebrew",
        "/nix/store",
    ];
    home.is_some_and(|h| HOME_ROOTS.iter().any(|r| canonical.starts_with(h.join(r))))
        || SYSTEM_ROOTS.iter().any(|r| canonical.starts_with(r))
}

pub(crate) fn try_claude_mcp_add(desired: &Value) -> Result<WriteResult, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let server_json = serde_json::to_string(desired).map_err(|e| e.to_string())?;

    // Same trusted, canonical executable on every platform — `cmd /C claude`
    // used to run whatever `claude` PATH resolved to on Windows.
    let mut cmd = Command::new(validate_claude_binary()?);
    cmd.args(["mcp", "add-json", "--scope", "user", "lean-ctx"]);

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(server_json.as_bytes());
    }

    let deadline = Duration::from_secs(3);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(WriteResult {
                        action: WriteAction::Updated,
                        note: Some("via claude mcp add-json".to_string()),
                    })
                } else {
                    Err("claude mcp add-json failed".to_string())
                };
            }
            Ok(None) => {
                if start.elapsed() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("claude mcp add-json timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

pub(crate) fn write_mcp_json_fresh(
    path: &std::path::Path,
    desired: &Value,
    note: Option<String>,
) -> Result<WriteResult, String> {
    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": { "lean-ctx": desired }
    }))
    .map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(WriteResult {
        action: if note.is_some() {
            WriteAction::Updated
        } else {
            WriteAction::Created
        },
        note,
    })
}

#[cfg(test)]
mod trust_tests {
    use std::path::Path;

    /// The native installer path is trusted; look-alikes outside the real
    /// home, project-local shims and unrelated dirs are not (component match,
    /// not substring — `/tmp/x/.local/share/claude/fake` used to pass).
    #[test]
    fn trust_is_anchored_to_home_and_system_roots() {
        let home = Some(Path::new("/Users/u"));
        let trusted = |p: &str| super::is_trusted_claude_path(Path::new(p), home);
        assert!(trusted("/Users/u/.local/share/claude/versions/2.1.287"));
        assert!(trusted("/opt/homebrew/Caskroom/claude-code/2.1.287/claude"));
        assert!(!trusted("/tmp/x/.local/share/claude/fake"));
        assert!(!trusted("/Users/u/repo/node_modules/.bin/claude"));
        assert!(!trusted("/Users/other/.claude/local/claude"));
        assert!(!trusted("/tmp/evil/claude"));
    }
}
