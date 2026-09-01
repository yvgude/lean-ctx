pub(super) fn effective_allowlist() -> Vec<String> {
    // LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE completely replaces the config (for testing)
    if let Ok(ov) = std::env::var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE") {
        return ov
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    let cfg = crate::core::config::Config::load();
    let mut list = cfg.shell_allowlist;
    // `shell_allowlist_extra` is purely additive (written by `lean-ctx allow <cmd>`),
    // so users can permit a command without nuking the built-in defaults. It only
    // matters in restricted mode — when the base list is empty all commands pass anyway.
    if !list.is_empty() {
        for entry in cfg.shell_allowlist_extra {
            if !entry.is_empty() && !list.contains(&entry) {
                list.push(entry);
            }
        }
    }
    if let Ok(env_val) = std::env::var("LEAN_CTX_SHELL_ALLOWLIST") {
        for entry in env_val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            if !list.contains(&entry) {
                list.push(entry);
            }
        }
    }
    list
}

/// Could this token plausibly be an executable name?
///
/// Deliberately permissive — the point is to catch obvious scanner debris
/// (unbalanced parens, quotes, `$`, `&`, embedded spaces), not to validate
/// against the filesystem. A name that would need quoting to type is not a name
/// the user should be told to allowlist.
pub(super) fn is_plausible_command_name(base: &str) -> bool {
    if base.is_empty() {
        return false;
    }
    !base.chars().any(|c| {
        c.is_whitespace() || matches!(c, '(' | ')' | '"' | '\'' | '$' | '&' | '|' | ';' | '`')
    })
}

/// Builds the actionable, self-diagnosing message shown when a command's base binary
/// is not in the allowlist. Unlike a bare "not allowed" string, it tells the user
/// (1) the exact additive fix, (2) the real config path the MCP server reads, and
/// (3) — crucially — whether their `config.toml` silently failed to parse (in which
/// case lean-ctx is on defaults, which is the usual reason an allowlist edit "did
/// nothing"). That last signal is otherwise invisible over an MCP/stdio transport.
pub(super) fn allowlist_block_message(base: &str) -> String {
    let cfg_path = crate::core::config::Config::path().map_or_else(
        || "~/.lean-ctx/config.toml".to_string(),
        |p| p.display().to_string(),
    );

    // A base that cannot be a command name means the scanner mis-split the line,
    // not that the user needs to allow something. Printing
    // `lean-ctx allow print(urllib.parse.quote(sys.argv[1],safe=))` — a real
    // example from GH #1646 — invites the reader to copy a command that cannot
    // work and hides the actual fault. Say which it is.
    if !is_plausible_command_name(base) {
        return format!(
            "[BLOCKED] '{base}' is not in the shell allowlist — but it does not look like a \
             command name, which means lean-ctx split your command line wrongly rather than \
             finding a command to gate.\n\
             Do NOT run `lean-ctx allow` on it; that would allowlist a fragment.\n\
             Quote the fragment differently if you can, and please report the original command \
             at https://github.com/yvgude/lean-ctx/issues — a mis-split is a bug in lean-ctx.\n\
             Config in effect: {cfg_path}"
        );
    }

    let mut msg = format!(
        "[BLOCKED — DO NOT RETRY] '{base}' is not in the shell allowlist. \
         This is a permanent restriction, not a transient error.\n\
         Fix (additive, keeps the defaults): run  lean-ctx allow {base}\n\
         Config in effect: {cfg_path}\n\
         Or disable the allowlist entirely: set  shell_allowlist = []\n\
         Or turn off all shell gating (you own the risk): set  shell_security = \"off\"  \
         (or env LEAN_CTX_SHELL_SECURITY=off) — compression still applies.\n\
         Do NOT reroute through ctx_execute(language=\"shell\"): both tools enforce the same \
         policy. Allow the command explicitly or change shell_security deliberately."
    );

    if crate::core::config::cloud_infra_commands().contains(&base) {
        msg.push_str(
            "\nNote: cloud/infra CLIs (terraform, kubectl, aws, …) are deliberately \
             excluded from the defaults — they mutate remote infrastructure with \
             ambient credentials. Opting in is a deliberate user decision.",
        );
    }

    if let Some(parse_err) = crate::core::config::last_config_parse_error() {
        msg.push_str(&format!(
            "\n\n⚠ Your config.toml currently FAILS to parse, so lean-ctx is running on the \
             built-in defaults — this is almost certainly why editing the allowlist had no \
             effect. Fix the TOML error below, then retry:\n  {parse_err}\n  File: {cfg_path}"
        ));
    } else if let Some(missing) = crate::core::config::Config::missing_config_path() {
        // The resolved config doesn't exist → lean-ctx is on defaults. An edit
        // made to a config.toml in a different dir (XDG vs legacy ~/.lean-ctx) or
        // under a sandboxed/container HOME is never read — say so over MCP (#540).
        msg.push_str(&format!(
            "\n\n⚠ No config file exists at {} — lean-ctx is running on built-in defaults. \
             If you added the command to a config.toml in a DIFFERENT location (XDG \
             ~/.config/lean-ctx vs legacy ~/.lean-ctx, or your MCP client launches lean-ctx \
             in a sandbox/container with a different HOME), the runtime never reads it. \
             `lean-ctx doctor` prints the path actually in effect; pin it with \
             LEAN_CTX_CONFIG_DIR.",
            missing.display()
        ));
    }

    // A project-local `shell_allowlist`/`shell_allowlist_extra` is silently
    // withheld for an untrusted workspace; surface that here so the edit's
    // no-op reason isn't buried in an MCP-invisible stderr warning (#540).
    if let Some(notice) = crate::core::workspace_trust::untrusted_override_notice() {
        msg.push_str("\n\n⚠ ");
        msg.push_str(&notice);
    }

    msg
}
/// Public accessor: the fully-resolved allowlist actually enforced by the MCP tools
/// (base `shell_allowlist` + additive `shell_allowlist_extra` + env), deduplicated.
/// Empty means blocklist-only mode (all commands pass). Used by `lean-ctx allow`
/// and `lean-ctx doctor` to show users exactly what the runtime sees.
#[must_use]
pub fn effective_allowlist_pub() -> Vec<String> {
    effective_allowlist()
}

/// GH #1466: Check whether the user has explicitly allowed an interpreter for
/// inline/heredoc use in Claude Code's own permission system.
///
/// Reads `permissions.allow` from both global (`~/.claude/settings.json`) and
/// project-local (`.claude/settings.local.json`) settings. Entries like
/// `Bash(python3:*)` or `Bash(python3:)` grant the interpreter `python3`
/// inline-execution rights, bypassing the heredoc/eval-flag block.
///
/// Cached per-process to avoid re-reading settings on every shell invocation.
pub(super) fn claude_allows_interpreter_inline(interpreter: &str) -> bool {
    use std::sync::OnceLock;

    static CACHE: OnceLock<Vec<String>> = OnceLock::new();

    let allowed = CACHE.get_or_init(|| {
        let mut interpreters = Vec::new();
        collect_claude_bash_permissions(&mut interpreters);
        interpreters
    });

    allowed.iter().any(|a| a == interpreter)
}

/// Parse `Bash(<cmd>:...)` entries from Claude's `permissions.allow` arrays.
fn collect_claude_bash_permissions(out: &mut Vec<String>) {
    let paths = claude_settings_paths();
    for path in &paths {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(json) = crate::core::jsonc::parse_jsonc(&content) {
                extract_bash_interpreters(&json, out);
            }
        }
    }
}

/// Returns candidate Claude settings paths (global + project-local).
fn claude_settings_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::with_capacity(2);

    if let Some(home) = crate::core::home::resolve_home_dir() {
        paths.push(home.join(".claude").join("settings.json"));
    }

    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join(".claude").join("settings.local.json"));
    }

    paths
}

/// Extract interpreter names from `Bash(<interpreter>:...)` permission entries.
pub(super) fn extract_bash_interpreters(json: &serde_json::Value, out: &mut Vec<String>) {
    let allow = json
        .pointer("/permissions/allow")
        .and_then(|v| v.as_array());

    let Some(arr) = allow else { return };

    for entry in arr {
        let Some(s) = entry.as_str() else { continue };
        if let Some(inner) = parse_bash_permission(s) {
            if !inner.is_empty() && !out.contains(&inner) {
                out.push(inner);
            }
        }
    }
}

/// Parse a Claude permission entry like `Bash(python3:*)` → `Some("python3")`.
/// Accepted formats: `Bash(cmd:)`, `Bash(cmd:*)`, `Bash(cmd:<anything>)`.
pub(super) fn parse_bash_permission(entry: &str) -> Option<String> {
    let rest = entry.strip_prefix("Bash(")?;
    let rest = rest.strip_suffix(')')?;
    let colon_pos = rest.find(':')?;
    let cmd = &rest[..colon_pos];
    if cmd.is_empty() {
        return None;
    }
    let base = cmd.rsplit('/').next().unwrap_or(cmd);
    Some(base.to_string())
}
