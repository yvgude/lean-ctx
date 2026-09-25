//! Claude Code `statusLine`: lean-ctx's segment, set without clobbering.
//!
//! `init --agent claude` sets `statusLine` only when there is none or it is
//! already lean-ctx's. A status line the user wrote stays untouched; they get
//! a one-line hint to chain it through `lean-ctx statusline --wrap`. Uninstall
//! gives a wrapped command back and removes only an entry lean-ctx owns.

use serde_json::Value;

use super::super::{mcp_server_quiet_mode, resolve_hook_command_binary, write_file};
use crate::core::config::{Config, ValueDisplayMode};

/// What [`merge_statusline`] did to the settings object.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StatuslineMerge {
    /// Added, or refreshed an entry lean-ctx already owned.
    Set,
    /// Already current.
    Unchanged,
    /// The user has their own status line; this is its command.
    Foreign(String),
}

pub(crate) fn install_claude_statusline(home: &std::path::Path) {
    if Config::load().value_display.effective_mode() == ValueDisplayMode::Off {
        return;
    }
    let settings_path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
    let content = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let mut settings = if content.trim().is_empty() {
        serde_json::json!({})
    } else {
        match crate::core::jsonc::parse_jsonc(&content) {
            Ok(v) if v.is_object() => v,
            _ => return,
        }
    };
    let binary = resolve_hook_command_binary();
    match merge_statusline(&mut settings, &binary) {
        StatuslineMerge::Set => {
            write_file(
                &settings_path,
                &serde_json::to_string_pretty(&settings).unwrap_or_default(),
            );
        }
        StatuslineMerge::Unchanged => {}
        StatuslineMerge::Foreign(existing) => {
            if !mcp_server_quiet_mode() {
                eprintln!(
                    "Kept your Claude Code status line. To add lean-ctx's segment to it, set \
                     statusLine.command to: {binary} statusline --wrap {}",
                    sh_single_quote(&existing)
                );
            }
        }
    }
}

/// Sets lean-ctx's status line in `settings` unless the user has their own.
/// An entry lean-ctx owns keeps its arguments (a `--wrap` the user added);
/// only the binary in front is brought up to date.
pub(crate) fn merge_statusline(settings: &mut Value, binary: &str) -> StatuslineMerge {
    let Some(root) = settings.as_object_mut() else {
        return StatuslineMerge::Unchanged;
    };
    let current = root.get("statusLine");
    let existing_cmd = current
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str);
    let desired_cmd = match existing_cmd {
        // A status line without a command string is not ours to touch, and
        // there is no command to suggest wrapping.
        None if current.is_some_and(|s| !s.is_null()) => return StatuslineMerge::Unchanged,
        None => format!("{binary} statusline"),
        Some(cmd) if is_lean_ctx_statusline(cmd) => {
            let args = statusline_args(cmd).unwrap_or_default();
            format!("{binary} statusline{args}")
        }
        Some(cmd) => return StatuslineMerge::Foreign(cmd.to_string()),
    };
    if existing_cmd == Some(desired_cmd.as_str()) {
        return StatuslineMerge::Unchanged;
    }
    let entry = root
        .entry("statusLine".to_string())
        .or_insert_with(|| serde_json::json!({ "type": "command", "padding": 0 }));
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("type".to_string(), Value::from("command"));
        obj.insert("command".to_string(), Value::from(desired_cmd));
    }
    StatuslineMerge::Set
}

/// Uninstall: an owned status line gives its `--wrap` command back, or goes.
/// Returns whether `settings` changed.
pub(crate) fn remove_lean_ctx_statusline(settings: &mut Value) -> bool {
    let Some(root) = settings.as_object_mut() else {
        return false;
    };
    let Some(cmd) = root
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
        .filter(|cmd| is_lean_ctx_statusline(cmd))
        .map(str::to_string)
    else {
        return false;
    };
    match wrapped_command(&cmd) {
        Some(original) => {
            if let Some(obj) = root.get_mut("statusLine").and_then(Value::as_object_mut) {
                obj.insert("command".to_string(), Value::from(original));
            }
        }
        None => {
            root.remove("statusLine");
        }
    }
    true
}

/// `… lean-ctx … statusline [args]` — the binary may be quoted or portable.
pub(crate) fn is_lean_ctx_statusline(cmd: &str) -> bool {
    statusline_args(cmd).is_some()
}

/// The part after the `statusline` subcommand (with its leading space), when
/// `cmd` runs lean-ctx's status line.
fn statusline_args(cmd: &str) -> Option<&str> {
    let idx = cmd.find(" statusline")?;
    let (bin, rest) = cmd.split_at(idx);
    let rest = &rest[" statusline".len()..];
    let ends_token = rest.is_empty() || rest.starts_with(' ');
    (bin.contains("lean-ctx") && ends_token).then_some(rest)
}

/// The user's command inside `statusline --wrap "<cmd>"`, unquoted.
pub(crate) fn wrapped_command(cmd: &str) -> Option<String> {
    let args = statusline_args(cmd)?.trim();
    let raw = args
        .strip_prefix("--wrap=")
        .or_else(|| args.strip_prefix("--wrap "))?
        .trim();
    let unquoted = if let Some(inner) = raw.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        inner.replace("'\\''", "'")
    } else if let Some(inner) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        inner.replace("\\\"", "\"").replace("\\\\", "\\")
    } else {
        raw.to_string()
    };
    (!unquoted.trim().is_empty()).then_some(unquoted)
}

fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BIN: &str = "/usr/local/bin/lean-ctx";

    #[test]
    fn sets_the_status_line_when_there_is_none() {
        let mut s = json!({ "hooks": {} });
        assert_eq!(merge_statusline(&mut s, BIN), StatuslineMerge::Set);
        assert_eq!(
            s["statusLine"],
            json!({ "type": "command", "command": format!("{BIN} statusline"), "padding": 0 })
        );
        assert_eq!(merge_statusline(&mut s, BIN), StatuslineMerge::Unchanged);
    }

    #[test]
    fn never_clobbers_a_foreign_status_line() {
        let mut s = json!({ "statusLine": { "type": "command", "command": "~/bin/my-line.sh" } });
        let before = s.clone();
        assert_eq!(
            merge_statusline(&mut s, BIN),
            StatuslineMerge::Foreign("~/bin/my-line.sh".into())
        );
        assert_eq!(s, before);
    }

    #[test]
    fn refreshes_its_own_binary_and_keeps_the_wrap() {
        let mut s = json!({ "statusLine": {
            "type": "command",
            "command": "/old/lean-ctx statusline --wrap 'starship prompt'",
            "padding": 2
        }});
        assert_eq!(merge_statusline(&mut s, BIN), StatuslineMerge::Set);
        assert_eq!(
            s["statusLine"]["command"],
            format!("{BIN} statusline --wrap 'starship prompt'")
        );
        assert_eq!(s["statusLine"]["padding"], 2, "the user's padding stays");
    }

    #[test]
    fn recognises_only_lean_ctx_statuslines() {
        assert!(is_lean_ctx_statusline("lean-ctx statusline"));
        assert!(is_lean_ctx_statusline(
            "\"$HOME/.local/bin/lean-ctx\" statusline --wrap=x"
        ));
        assert!(!is_lean_ctx_statusline("lean-ctx statuslinex"));
        assert!(!is_lean_ctx_statusline("my-tool statusline"));
        assert!(!is_lean_ctx_statusline("bash ~/line.sh"));
    }

    #[test]
    fn unwraps_the_original_command() {
        let w = |c: &str| wrapped_command(c);
        assert_eq!(
            w("lean-ctx statusline --wrap 'a b'").as_deref(),
            Some("a b")
        );
        assert_eq!(
            w("lean-ctx statusline --wrap 'it'\\''s'").as_deref(),
            Some("it's")
        );
        assert_eq!(
            w("lean-ctx statusline --wrap \"say \\\"hi\\\"\"").as_deref(),
            Some("say \"hi\"")
        );
        assert_eq!(
            w("lean-ctx statusline --wrap=line.sh").as_deref(),
            Some("line.sh")
        );
        assert_eq!(w("lean-ctx statusline"), None);
    }

    #[test]
    fn uninstall_restores_the_wrapped_line_or_removes_its_own() {
        let mut wrapped = json!({ "statusLine": {
            "type": "command", "command": "lean-ctx statusline --wrap 'bash ~/l.sh'"
        }});
        assert!(remove_lean_ctx_statusline(&mut wrapped));
        assert_eq!(wrapped["statusLine"]["command"], "bash ~/l.sh");

        let mut own = json!({ "model": "x", "statusLine": { "type": "command", "command": "lean-ctx statusline" } });
        assert!(remove_lean_ctx_statusline(&mut own));
        assert_eq!(own, json!({ "model": "x" }));

        let mut foreign = json!({ "statusLine": { "type": "command", "command": "bash ~/l.sh" } });
        assert!(!remove_lean_ctx_statusline(&mut foreign));
    }
}
