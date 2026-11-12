// SPDX-License-Identifier: Apache-2.0

//! `lean-ctx hook write-guard`: a native Write/Edit/apply_patch must not
//! reconfigure LeanCTX.
//!
//! The shell refuses an agent's writes into lean-ctx's own config and data
//! directories (`shell_allowlist::self_config`), but a native file tool
//! never reaches the shell: `Write ~/.config/lean-ctx/config.toml` with
//! `shell_security = "off"`, or an edit of the host's hook file
//! (`~/.claude/settings.json`, `~/.codex/hooks.json`, Cursor's
//! `hooks.json`), removed every gate. The installers route each host's write
//! tools here; a write that targets lean-ctx's state or a host hook
//! configuration is denied with the reason, every other write passes (`{}`).
//!
//! No config switch turns this off: the config it guards is the thing an
//! agent would change to turn it off.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

/// Payload keys that never name a write target.
const SKIPPED_KEYS: &[&str] = &[
    "transcript_path",
    "cwd",
    "workspace_roots",
    "hook_event_name",
];
/// Argument keys whose text is the file content a tool writes.
const CONTENT_KEYS: &[&str] = &[
    "content",
    "contents",
    "new_string",
    "old_string",
    "new_source",
    "text",
];

/// Handle `lean-ctx hook write-guard`. An empty or unreadable payload passes:
/// the guard judges only the write it can see.
pub fn handle_write_guard() {
    let stdin_payload = super::deny::read_stdin_with_timeout();
    let Ok(payload) = serde_json::from_str::<Value>(&stdin_payload) else {
        println!("{{}}");
        return;
    };
    match guarded_write(&payload) {
        Some(refusal) => super::deny::emit_deny(&refusal, &stdin_payload),
        None => println!("{{}}"),
    }
}

/// The refusal for a write that would change LeanCTX's own configuration or
/// state, or a host's hook configuration; `None` for any other payload.
pub(super) fn guarded_write(payload: &Value) -> Option<String> {
    let tool = super::payload::resolve_tool_name(payload).unwrap_or_default();
    if !tool.is_empty() && !is_write_like(&tool) {
        return None;
    }
    let home = lean_ctx::core::home::resolve_home_dir();
    let base = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let target = write_targets(payload)
        .into_iter()
        .filter_map(|raw| absolute(&raw, home.as_deref(), base.as_deref()))
        .find(|path| is_guarded(path, home.as_deref()))?;
    let tool = if tool.is_empty() { "write" } else { &tool };
    Some(format!(
        "[BLOCKED — DO NOT RETRY] `{tool}` would change {}: LeanCTX's own configuration and \
         state, and the host's hook configuration, cannot be changed by an agent. Ask the \
         user to make this change in their own editor or terminal if they want it.",
        target.display()
    ))
}

/// A tool name that may create, change, move or delete a file.
fn is_write_like(tool: &str) -> bool {
    let t = tool.to_ascii_lowercase();
    [
        "write", "edit", "replace", "patch", "delete", "create", "notebook", "move", "rename",
        "remove",
    ]
    .iter()
    .any(|w| t.contains(w))
}

/// Every path a write payload names: values of `*path`/`*file` keys (also in
/// arrays) and the file headers of an `apply_patch` text, wherever the host
/// puts its tool arguments (`toolArgs` may be a JSON string).
fn write_targets(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect(payload, None, &mut out);
    out
}

fn collect(value: &Value, key: Option<&str>, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if !SKIPPED_KEYS.contains(&k.as_str()) {
                    collect(v, Some(k), out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect(v, key, out)),
        Value::String(s) => {
            let k = key.unwrap_or_default().to_ascii_lowercase();
            if k == "toolargs"
                && let Ok(inner) = serde_json::from_str::<Value>(s)
            {
                collect(&inner, None, out);
            }
            if ["path", "paths", "file", "filename", "file_name"]
                .iter()
                .any(|suffix| k.ends_with(suffix))
            {
                out.push(s.clone());
            }
            // The file body of a write is data, not a patch to apply.
            if !CONTENT_KEYS.contains(&k.as_str()) {
                out.extend(lean_ctx::core::shell_file_effects::apply_patch_targets(s));
            }
        }
        _ => {}
    }
}

/// `raw` as an absolute, lexically normalised path: `~/` against `home`, a
/// relative path against `base` (the payload's `cwd`).
fn absolute(raw: &str, home: Option<&Path>, base: Option<&Path>) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let path = if raw == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        home?.join(rest)
    } else if Path::new(raw).is_absolute() || raw.starts_with('/') {
        PathBuf::from(raw)
    } else {
        base?.join(raw)
    };
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    Some(normal)
}

/// `path` and, when its parent exists, the same file under the resolved
/// parent (a symlinked directory names the file it reaches).
fn spellings(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(real) = std::fs::canonicalize(parent)
    {
        out.push(real.join(name));
    }
    if let Ok(real) = std::fs::canonicalize(path)
        && !out.contains(&real)
    {
        out.push(real);
    }
    out
}

/// Path text compared as the filesystem does.
fn key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if cfg!(any(target_os = "macos", windows)) {
        text.to_lowercase()
    } else {
        text
    }
}

fn is_guarded(path: &Path, home: Option<&Path>) -> bool {
    if lean_ctx_shell_policy::is_leanctx_state_path(path) {
        return true;
    }
    let files: Vec<String> = home
        .map(host_hook_files)
        .unwrap_or_default()
        .iter()
        .flat_map(|f| spellings(f))
        .map(|f| key(&f))
        .collect();
    spellings(path)
        .iter()
        .any(|p| files.contains(&key(p)) || is_hook_config_shape(p))
}

/// The hook configuration files the installers write under `home`.
fn host_hook_files(home: &Path) -> Vec<PathBuf> {
    use lean_ctx::core::governor_hosts::{codebuddy, codex, copilot, cursor, gemini};
    let claude = lean_ctx::core::editor_registry::claude_state_dir(home);
    let codebuddy_dir = codebuddy::state_dir(home);
    vec![
        claude.join("settings.json"),
        claude.join("settings.local.json"),
        codebuddy_dir.join("settings.json"),
        codebuddy_dir.join("settings.local.json"),
        cursor::hooks_path(home),
        gemini::settings_path(home),
        codex::hooks_path(home),
        codex::config_path(home),
        copilot::hooks_path(home),
    ]
}

/// A host hook configuration by its shape, in the home or in a project
/// (`.claude/settings.local.json` carries project hooks and
/// `disableAllHooks`), and LeanCTX's hook scripts under a host's `hooks/`.
fn is_hook_config_shape(path: &Path) -> bool {
    let parts: Vec<String> = path
        .components()
        .rev()
        .take(3)
        .map(|c| key(Path::new(c.as_os_str())))
        .collect();
    let (Some(name), Some(dir)) = (parts.first(), parts.get(1)) else {
        return false;
    };
    let host_file = matches!(
        (dir.as_str(), name.as_str()),
        (
            ".claude" | ".codebuddy",
            "settings.json" | "settings.local.json"
        ) | (".cursor", "hooks.json")
            | (".gemini", "settings.json")
            | (".codex", "hooks.json" | "config.toml")
    );
    let host_script = dir == "hooks"
        && name.starts_with("lean-ctx")
        && parts.get(2).is_some_and(|host| {
            matches!(
                host.as_str(),
                ".claude" | ".codebuddy" | ".cursor" | ".gemini" | ".codex" | ".copilot"
            )
        });
    host_file || host_script
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_toml() -> String {
        lean_ctx::core::paths::config_dir_read_only()
            .expect("config dir")
            .join("config.toml")
            .display()
            .to_string()
    }

    /// One row per host dialect: the payload each host sends for a native
    /// write, aimed at lean-ctx's config, at the host's hook file and at an
    /// ordinary project file.
    #[test]
    fn native_writes_to_leanctx_or_hook_config_are_denied_per_host() {
        // Other tests point HOME and the config dir elsewhere while they hold
        // this lock; without it the rows below can judge a moved config dir.
        let _env = lean_ctx::core::data_dir::test_env_lock();
        let cfg = config_toml();
        let project = std::env::temp_dir().join("wg-project/src/main.rs");
        let project = project.display().to_string();
        let patch = |path: &str| {
            format!("*** Begin Patch\n*** Update File: {path}\n@@\n-a\n+b\n*** End Patch\n")
        };
        let claude = |tool: &str, path: &str| {
            serde_json::json!({
                "hook_event_name": "PreToolUse", "transcript_path": "/t", "cwd": "/w",
                "tool_name": tool, "tool_input": {"file_path": path, "content": "x"}
            })
        };
        let codex = |path: &str| {
            serde_json::json!({
                "hook_event_name": "PreToolUse", "cwd": "/w", "tool_name": "apply_patch",
                "tool_input": {"command": ["apply_patch", patch(path)]}
            })
        };
        let cursor = |tool: &str, path: &str| serde_json::json!({"tool_name": tool, "tool_input": {"path": path, "old_string": "a"}});
        let gemini = |path: &str| {
            serde_json::json!({
                "hook_event_name": "BeforeTool", "tool_name": "write_file",
                "tool_input": {"file_path": path, "content": "shell_security = \"off\""}
            })
        };
        let codebuddy = |path: &str| {
            serde_json::json!({
                "hook_event_name": "PreToolUse", "transcript_path": "/t",
                "tool_name": "MultiEdit", "tool_input": {"file_path": path, "edits": []}
            })
        };
        let rows = [
            ("claude", claude("Write", &cfg), true),
            (
                "claude-hooks",
                claude("Edit", "/w/.claude/settings.json"),
                true,
            ),
            ("claude-ok", claude("Write", &project), false),
            ("codex", codex(&cfg), true),
            ("codex-hooks", codex("/h/.codex/hooks.json"), true),
            ("codex-ok", codex(&project), false),
            ("cursor", cursor("StrReplace", &cfg), true),
            (
                "cursor-hooks",
                cursor("Write", "/h/.cursor/hooks.json"),
                true,
            ),
            ("cursor-ok", cursor("Write", &project), false),
            ("gemini", gemini(&cfg), true),
            ("gemini-hooks", gemini("/h/.gemini/settings.json"), true),
            ("gemini-ok", gemini(&project), false),
            ("codebuddy", codebuddy(&cfg), true),
            (
                "codebuddy-hooks",
                codebuddy("/h/.codebuddy/settings.local.json"),
                true,
            ),
            ("codebuddy-ok", codebuddy(&project), false),
        ];
        for (name, payload, denied) in rows {
            let verdict = guarded_write(&payload);
            assert_eq!(verdict.is_some(), denied, "{name}: {verdict:?}");
            if let Some(reason) = verdict {
                assert!(reason.starts_with("[BLOCKED — DO NOT RETRY]"), "{reason}");
            }
        }
    }

    #[test]
    fn relative_and_parent_paths_resolve_against_the_payload_cwd() {
        let _env = lean_ctx::core::data_dir::test_env_lock();
        let cfg = lean_ctx::core::paths::config_dir_read_only().expect("config dir");
        let cwd = cfg.join("sub");
        let payload = serde_json::json!({
            "tool_name": "Write", "cwd": cwd.display().to_string(),
            "tool_input": {"file_path": "../config.toml"}
        });
        assert!(guarded_write(&payload).is_some());
        let read = serde_json::json!({
            "tool_name": "Read", "tool_input": {"file_path": config_toml()}
        });
        assert!(guarded_write(&read).is_none(), "reads are not judged here");
    }
}
