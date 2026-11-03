// SPDX-License-Identifier: Apache-2.0
//! #1879: detect a user-owned Bash hook that already delegates to `lean-ctx hook rewrite`
//! (inline or via a script), so the installer never adds a parallel rewrite hook next to it.

use super::claude::{
    ensure_command_hook, is_lean_ctx_command_for, lean_ctx_action_token, strip_lean_ctx_hooks,
};

/// True if a Claude Code `matcher` applies to the `Bash` tool: absent/empty, `*`, or a regex
/// that matches `Bash` exactly (Claude anchors matchers against the tool name).
fn matcher_covers_bash(matcher: Option<&str>) -> bool {
    match matcher.map(str::trim) {
        None | Some("" | "*") => true,
        Some(m) => regex::Regex::new(&format!("^(?:{m})$")).is_ok_and(|re| re.is_match("Bash")),
    }
}

/// Upper bound for a hook script we inspect for delegation; real wrappers are a few KiB.
const DELEGATION_SCRIPT_MAX_BYTES: u64 = 256 * 1024;

fn calls_lean_ctx_rewrite(text: &str) -> bool {
    text.contains("lean-ctx") && text.contains("hook rewrite")
}

/// Resolve a command token that names a script file: absolute, `~/…`, `$HOME/…`, `${HOME}/…`.
fn script_path_from_token(token: &str, home: &std::path::Path) -> Option<std::path::PathBuf> {
    let token = token.trim_matches(|c| c == '"' || c == '\'');
    for prefix in ["~/", "$HOME/", "${HOME}/"] {
        if let Some(rest) = token.strip_prefix(prefix) {
            return home.is_absolute().then(|| home.join(rest));
        }
    }
    let path = std::path::Path::new(token);
    path.is_absolute().then(|| path.to_path_buf())
}

/// #1879: true when a user-owned PreToolUse hook covering Bash already calls
/// `lean-ctx hook rewrite` itself — inline, or from a script it runs — typically a wrapper that
/// post-processes the rewrite (secret redaction, EXIT traps). Adding lean-ctx's own parallel
/// rewrite hook next to it would make two hooks return `updatedInput` for the same call and
/// silently drop the wrapper whenever lean-ctx's result wins.
fn bash_rewrite_is_delegated(pre_arr: &[serde_json::Value], home: &std::path::Path) -> bool {
    pre_arr
        .iter()
        .filter(|g| matcher_covers_bash(g.get("matcher").and_then(|m| m.as_str())))
        .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
        .flatten()
        .filter(|h| h.get("type").and_then(|t| t.as_str()) == Some("command"))
        .filter(|h| !is_lean_ctx_command_for(h, "hook rewrite"))
        .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
        .any(|cmd| {
            calls_lean_ctx_rewrite(cmd)
                || cmd
                    .split_whitespace()
                    .filter_map(|t| script_path_from_token(t, home))
                    .any(|p| {
                        std::fs::metadata(&p)
                            .is_ok_and(|m| m.is_file() && m.len() <= DELEGATION_SCRIPT_MAX_BYTES)
                            && std::fs::read_to_string(&p).is_ok_and(|s| calls_lean_ctx_rewrite(&s))
                    })
        })
}

/// Install lean-ctx's Bash rewrite hook unless the user delegates it through their own hook
/// ([`bash_rewrite_is_delegated`]); in that case any parallel lean-ctx rewrite entry is removed
/// so exactly one hook rewrites each command.
pub(super) fn ensure_bash_rewrite_hook(
    pre_arr: &mut Vec<serde_json::Value>,
    matcher: &str,
    command: &str,
    home: &std::path::Path,
) {
    if bash_rewrite_is_delegated(pre_arr, home) {
        strip_lean_ctx_hooks(pre_arr, lean_ctx_action_token(command));
        tracing::info!("Bash rewrite delegated to a user hook; not adding lean-ctx's own (#1879)");
        return;
    }
    ensure_command_hook(pre_arr, matcher, command);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BASH: &str = "Bash|bash";

    fn commands_for(pre: &[serde_json::Value], action: &str) -> Vec<String> {
        pre.iter()
            .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
            .flatten()
            .filter(|h| is_lean_ctx_command_for(h, action))
            .filter_map(|h| h.get("command").and_then(|c| c.as_str()).map(String::from))
            .collect()
    }

    fn all_commands(pre: &[serde_json::Value]) -> Vec<String> {
        pre.iter()
            .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
            .flatten()
            .filter_map(|h| h.get("command").and_then(|c| c.as_str()).map(String::from))
            .collect()
    }

    fn cmd_hook(command: &str) -> serde_json::Value {
        json!({ "type": "command", "command": command })
    }

    #[test]
    fn matcher_coverage_of_bash() {
        for m in [
            None,
            Some(""),
            Some("*"),
            Some("Bash"),
            Some("Bash|bash"),
            Some(".*"),
        ] {
            assert!(matcher_covers_bash(m), "{m:?} should cover Bash");
        }
        for m in [
            Some("Read"),
            Some("Grep|Glob"),
            Some("BashOutput"),
            Some("("),
        ] {
            assert!(!matcher_covers_bash(m), "{m:?} must not cover Bash");
        }
    }

    #[test]
    fn inline_delegation_prevents_parallel_rewrite_hook() {
        let home = tempfile::tempdir().unwrap();
        let mut pre = vec![
            json!({ "matcher": "Bash", "hooks": [cmd_hook("redact-wrap lean-ctx hook rewrite")] }),
            // A stale parallel entry from an earlier refresh must be removed, not kept.
            json!({ "matcher": BASH, "hooks": [cmd_hook("/abs/lean-ctx hook rewrite")] }),
        ];
        for _ in 0..3 {
            ensure_bash_rewrite_hook(&mut pre, BASH, "lean-ctx hook rewrite", home.path());
        }
        assert!(commands_for(&pre, "hook rewrite").is_empty(), "{pre:?}");
        assert_eq!(all_commands(&pre), ["redact-wrap lean-ctx hook rewrite"]);
    }

    #[test]
    fn script_delegation_prevents_parallel_rewrite_hook() {
        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("bin/redact-bash.sh");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(
            &script,
            "#!/bin/sh\ntrap cleanup EXIT\nout=$(lean-ctx hook rewrite)\nredact \"$out\"\n",
        )
        .unwrap();
        for command in [
            "~/bin/redact-bash.sh".to_string(),
            "bash \"$HOME/bin/redact-bash.sh\"".to_string(),
            script.display().to_string(),
        ] {
            let mut pre = vec![json!({ "matcher": "Bash", "hooks": [cmd_hook(&command)] })];
            ensure_bash_rewrite_hook(&mut pre, BASH, "lean-ctx hook rewrite", home.path());
            assert_eq!(
                all_commands(&pre),
                std::slice::from_ref(&command),
                "via {command}"
            );
        }
    }

    #[test]
    fn unrelated_bash_hook_still_gets_rewrite() {
        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("audit.sh");
        std::fs::write(&script, "#!/bin/sh\nlogger \"$1\"\n").unwrap();
        let mut pre = vec![
            json!({ "matcher": "Bash", "hooks": [cmd_hook(&script.display().to_string())] }),
            // Mentions the rewrite, but only for Read — not a Bash delegation.
            json!({ "matcher": "Read", "hooks": [cmd_hook("wrap lean-ctx hook rewrite")] }),
        ];
        ensure_bash_rewrite_hook(&mut pre, BASH, "lean-ctx hook rewrite", home.path());
        assert_eq!(
            commands_for(&pre, "hook rewrite"),
            ["lean-ctx hook rewrite"]
        );
    }
}
