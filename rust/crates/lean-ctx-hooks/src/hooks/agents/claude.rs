// SPDX-License-Identifier: Apache-2.0
use super::super::{
    HookMode, REDIRECT_SCRIPT_CLAUDE, ensure_state_dir, generate_rewrite_script, make_executable,
    mcp_server_quiet_mode, resolve_binary_path_for_bash, resolve_hook_command_binary,
    shell_quoted_binary, write_file, write_wrapper_file,
};
use super::claude_delegation::ensure_bash_rewrite_hook;
#[cfg(test)]
use lean_ctx::core::claude_md::{
    CLAUDE_MD_BLOCK_CONTENT_MCP, CLAUDE_MD_BLOCK_END, CLAUDE_MD_BLOCK_VERSION,
    strip_claude_md_block,
};
pub use lean_ctx::core::claude_md::{CLAUDE_MD_BLOCK_START, sync_claude_global_rules_block};
use lean_ctx::core::claude_md::{
    install_claude_global_claude_md_for_mode, remove_claude_rules_file,
};

pub(crate) fn install_claude_hook_with_mode(global: bool, mode: HookMode) {
    let Some(home) = lean_ctx::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };
    // Opt-in governor profile is sticky until `init --mode <standard>` reverts it.
    if super::claude_governor::is_installed(&home) {
        super::claude_governor::refresh(&home);
        return;
    }

    install_claude_hook_scripts(&home);
    install_claude_hook_config(&home);
    super::claude_statusline::install_claude_statusline(&home);

    // #281: register the MCP server only when MCP updates are enabled. The hook
    // scripts/config above and the rules/skill below still install, so an
    // MCP-disabled setup keeps the CLI integration without an MCP entry.
    if matches!(mode, HookMode::Hybrid | HookMode::Mcp | HookMode::Replace)
        && super::super::should_register_mcp()
    {
        install_claude_mcp_server(&home);
    }

    if super::super::claude_hides_native_search(mode) {
        install_claude_permissions_deny_replace(&home);
    }

    // Auto-approve lean-ctx MCP tools so Claude Code's auto mode / classifier
    // does not gate or skip them.  Without this, auto mode silently falls back
    // to native Read (uncompressed) and the model avoids MCP calls.  (#1467)
    if matches!(mode, HookMode::Hybrid | HookMode::Mcp | HookMode::Replace) {
        install_claude_permissions_allow_mcp(&home);
    }

    let scope = lean_ctx::core::config::Config::load().rules_scope_effective();
    if scope != lean_ctx::core::config::RulesScope::Project {
        remove_claude_rules_file(&home);
        install_claude_global_claude_md_for_mode(&home, mode);
        // rules_injection=off (#361): the functional hooks above still install
        // (off opts out of *instructions*, not compression), but the on-demand
        // skill is lean-ctx-authored steering — suppress it, and remove one a
        // previous shared/dedicated install left behind.
        if lean_ctx::core::config::Config::load().rules_injection_effective()
            == lean_ctx::core::config::RulesInjection::Off
        {
            remove_claude_skill(&home);
        } else {
            install_claude_skill(&home);
        }
    }

    let _ = global;
}

fn install_claude_mcp_server(home: &std::path::Path) {
    let config_path = lean_ctx::core::editor_registry::claude_mcp_json_path(home);
    let binary = super::super::resolve_binary_path();

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    if existing.contains("\"lean-ctx\"") && existing.contains("mcpServers") {
        // Entry already exists — ensure autoApprove (#1467) and alwaysLoad.
        ensure_claude_mcp_entry_defaults(&config_path, &existing);
        return;
    }

    let parsed: Result<serde_json::Value, _> = if existing.trim().is_empty() {
        Ok(serde_json::json!({}))
    } else {
        lean_ctx::core::jsonc::parse_jsonc(&existing)
    };

    let auto_approve = lean_ctx::core::editor_registry::writers::auto_approve_tools();

    if let Ok(mut root) = parsed
        && let Some(obj) = root.as_object_mut()
    {
        let servers = obj
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(servers_obj) = servers.as_object_mut()
            && !servers_obj.contains_key("lean-ctx")
        {
            servers_obj.insert(
                "lean-ctx".to_string(),
                serde_json::json!({
                    "command": binary,
                    "args": [],
                    "autoApprove": auto_approve,
                    "alwaysLoad": true
                }),
            );
            write_file(
                &config_path,
                &serde_json::to_string_pretty(&root).unwrap_or_default(),
            );
            if !super::super::mcp_server_quiet_mode() {
                eprintln!("Added lean-ctx MCP server to {}", config_path.display());
            }
        }
    }
}

/// Backfill `autoApprove` and `alwaysLoad` on an existing entry. Without
/// `alwaysLoad`, Claude Code lists lean-ctx tools by name only behind
/// ToolSearch: agents pay a round trip to load a schema and fall back to the
/// one generic tool they already have (`ctx_shell`) for everything.
fn ensure_claude_mcp_entry_defaults(config_path: &std::path::Path, content: &str) {
    let Ok(mut root) = lean_ctx::core::jsonc::parse_jsonc(content) else {
        return;
    };
    let Some(entry) = root
        .pointer_mut("/mcpServers/lean-ctx")
        .and_then(|v| v.as_object_mut())
    else {
        return;
    };
    let desired = lean_ctx::core::editor_registry::writers::auto_approve_tools();
    let desired_json = serde_json::json!(desired);
    let always_load = serde_json::Value::Bool(true);
    if entry.get("autoApprove") == Some(&desired_json)
        && entry.get("alwaysLoad") == Some(&always_load)
    {
        return;
    }
    entry.insert("autoApprove".to_string(), desired_json);
    entry.insert("alwaysLoad".to_string(), always_load);
    if let Ok(out) = serde_json::to_string_pretty(&root) {
        write_file(config_path, &out);
    }
}

/// Whenever native search is hidden (`claude_hides_native_search`: Replace and
/// the default Hybrid mode), deny Grep/Glob via Claude Code's `permissions.deny` so
/// search/listing go through `ctx_*`. Native `Read` is intentionally NOT denied:
/// Claude Code's read-before-write edit gate and auto memory
/// (`~/.claude/projects/<slug>/memory/`) both require native Read (GH #637,
/// #1091, #1228). Deny always wins over allow, so a bare `Read` deny cannot
/// carve out memory paths — remove it instead and steer exploration via
/// CLAUDE.md + hooks.
pub(crate) fn install_claude_permissions_deny_replace(home: &std::path::Path) {
    if super::claude_governor::is_installed(home) {
        return; // the governor keeps the host's own tool list
    }
    let settings_path = home.join(".claude").join("settings.json");
    let read_mtime = lean_ctx::config_io::snapshot_mtime(&settings_path);

    let mut json = match read_claude_settings(&settings_path) {
        Ok(json) => json,
        Err(error) => return report_unreadable_settings(&error),
    };

    let Some(obj) = json.as_object_mut() else {
        return;
    };

    let permissions = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let Some(perm_obj) = permissions.as_object_mut() else {
        return;
    };
    let deny = perm_obj
        .entry("deny")
        .or_insert_with(|| serde_json::json!([]));
    let Some(arr) = deny.as_array_mut() else {
        return;
    };

    // Bash intentionally NOT denied: permissions.deny blocks globally including
    // plugin commands (codex-companion etc.). PreToolUse hook handles agent Bash. (GH #799)
    // Read intentionally NOT denied: auto memory + edit gate need native Read (#1228).
    let deny_tools = ["Grep", "Glob"];
    let mut changed = false;
    for tool in deny_tools {
        let val = serde_json::Value::String(tool.to_string());
        if !arr.contains(&val) {
            arr.push(val);
            changed = true;
        }
    }

    // Remove stale bare "Read" / "Bash" entries left by older versions
    // (GH #799 Bash; GH #1228 Read).
    for stale in ["Read", "Bash"] {
        let stale_val = serde_json::Value::String(stale.to_string());
        if let Some(pos) = arr.iter().position(|v| v == &stale_val) {
            arr.remove(pos);
            changed = true;
        }
    }

    if changed
        && write_claude_settings(&settings_path, &json, read_mtime)
        && !mcp_server_quiet_mode()
    {
        eprintln!(
            "  \x1b[32m✓\x1b[0m Claude Code: denied native Grep/Glob (Read kept for edit gate + auto memory)"
        );
    }
}

/// `settings.json` as JSON, or `{}` when it is missing (its directory is
/// created) or empty. A file that cannot be read or parsed is an error and is
/// never replaced: the caller leaves it untouched and reports why.
fn read_claude_settings(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            return Ok(serde_json::json!({}));
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    if content.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    lean_ctx::core::jsonc::parse_jsonc(&content).map_err(|error| unparseable_settings(path, &error))
}

fn unparseable_settings(path: &std::path::Path, error: &serde_json::Error) -> String {
    format!(
        "{} is not valid JSON ({error}); it was left unchanged. Fix it (or move it aside) and \
         run `lean-ctx init --agent claude` again.",
        path.display()
    )
}

/// A refused `settings.json` is reported even in quiet mode: the hooks or
/// permissions it should carry are missing until the file is fixed.
fn report_unreadable_settings(error: &str) {
    tracing::error!("Claude Code settings not updated: {error}");
    eprintln!("  \x1b[31m✗\x1b[0m Claude Code settings not updated: {error}");
}

/// Write `settings.json` back like every other config writer: atomically,
/// with a backup, under the config write lock, and refused when the file
/// changed after `read_mtime` was taken (another writer came in between, its
/// edit is kept). A failure is reported instead of swallowed; `true` when
/// written.
fn write_claude_settings(
    path: &std::path::Path,
    json: &serde_json::Value,
    read_mtime: Option<std::time::SystemTime>,
) -> bool {
    let written = serde_json::to_string_pretty(json)
        .map_err(|error| error.to_string())
        .and_then(|out| {
            lean_ctx::config_io::write_atomic_with_backup_checked(path, &out, read_mtime)
        });
    if let Err(error) = &written {
        tracing::error!("Error writing {}: {error}", path.display());
    }
    written.is_ok()
}

/// Auto-approve lean-ctx MCP tools in Claude Code's `permissions.allow` so
/// auto mode and the classifier do not block or avoid them.  This is the
/// `settings.json` counterpart to the `autoApprove` field in `~/.claude.json`.
pub(crate) fn install_claude_permissions_allow_mcp(home: &std::path::Path) {
    let settings_path = home.join(".claude").join("settings.json");
    let read_mtime = lean_ctx::config_io::snapshot_mtime(&settings_path);

    let mut json = match read_claude_settings(&settings_path) {
        Ok(json) => json,
        Err(error) => return report_unreadable_settings(&error),
    };

    let Some(obj) = json.as_object_mut() else {
        return;
    };

    let permissions = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let Some(perm_obj) = permissions.as_object_mut() else {
        return;
    };
    let allow = perm_obj
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]));
    let Some(arr) = allow.as_array_mut() else {
        return;
    };

    let tools = lean_ctx::core::editor_registry::writers::auto_approve_tools();
    let mut changed = false;
    for tool in &tools {
        let perm = format!("mcp__lean-ctx__{tool}");
        let val = serde_json::Value::String(perm);
        if !arr.iter().any(|v| v == &val) {
            arr.push(val);
            changed = true;
        }
    }

    if changed
        && write_claude_settings(&settings_path, &json, read_mtime)
        && !mcp_server_quiet_mode()
    {
        eprintln!(
            "  \x1b[32m\u{2713}\x1b[0m Claude Code: auto-approved {} lean-ctx MCP tools in permissions.allow",
            tools.len()
        );
    }
}

fn install_claude_skill(home: &std::path::Path) {
    // Honor CLAUDE_CONFIG_DIR like every other Claude state path (CLAUDE.md,
    // hooks, settings.json) instead of hardcoding `~/.claude` (#596).
    let skill_dir = lean_ctx::core::editor_registry::claude_state_dir(home).join("skills/lean-ctx");
    if !ensure_state_dir(&skill_dir.join("scripts")) {
        return;
    }

    let skill_md = lean_ctx::core::agent_templates::SKILL_MD;
    let install_sh = lean_ctx::core::agent_templates::SKILL_INSTALL_SH;

    let skill_path = skill_dir.join("SKILL.md");
    let script_path = skill_dir.join("scripts/install.sh");

    write_file(&skill_path, skill_md);
    write_file(&script_path, install_sh);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(mut perms) = std::fs::metadata(&script_path).map(|m| m.permissions()) {
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&script_path, perms);
        }
    }
}

/// Remove the lean-ctx skill directory (`rules_injection=off`, GH #361). Only the
/// lean-ctx-owned `lean-ctx` skill folder is touched, so a user's other skills
/// are never affected.
fn remove_claude_skill(home: &std::path::Path) {
    let skill_dir = lean_ctx::core::editor_registry::claude_state_dir(home).join("skills/lean-ctx");
    if skill_dir.exists()
        && std::fs::remove_dir_all(&skill_dir).is_ok()
        && !super::super::mcp_server_quiet_mode()
    {
        eprintln!(
            "Removed {} (rules_injection=off — instructions intentionally not installed)",
            skill_dir.display()
        );
    }
}

pub(crate) fn install_claude_hook_scripts(home: &std::path::Path) {
    let hooks_dir = lean_ctx::core::editor_registry::claude_state_dir(home).join("hooks");
    if !ensure_state_dir(&hooks_dir) {
        return;
    }

    // #719: wrapper writes go through write_wrapper_file — a synced portable
    // wrapper ($HOME/… that resolves here) is never re-stamped with this
    // machine's absolute path, and the binary token is shell-quoted.
    let rewrite_path = hooks_dir.join("lean-ctx-rewrite.sh");
    let rewrite_script = generate_rewrite_script(&resolve_binary_path_for_bash());
    write_wrapper_file(&rewrite_path, &rewrite_script, home);
    make_executable(&rewrite_path);

    let redirect_path = hooks_dir.join("lean-ctx-redirect.sh");
    write_file(&redirect_path, REDIRECT_SCRIPT_CLAUDE);
    make_executable(&redirect_path);

    let rewrite_native = hooks_dir.join("lean-ctx-rewrite-native");
    write_wrapper_file(
        &rewrite_native,
        &format!(
            "#!/bin/sh\nexec {} hook rewrite\n",
            shell_quoted_binary(&resolve_binary_path_for_bash())
        ),
        home,
    );
    make_executable(&rewrite_native);

    let redirect_native = hooks_dir.join("lean-ctx-redirect-native");
    write_wrapper_file(
        &redirect_native,
        &format!(
            "#!/bin/sh\nexec {} hook redirect\n",
            shell_quoted_binary(&resolve_binary_path_for_bash())
        ),
        home,
    );
    make_executable(&redirect_native);
}

/// The `Read|…` tool matcher shared by every Claude redirect hook (global + project).
/// Claude Code tool names are case-sensitive: lowercase aliases never match
/// (reimplements community PR #2169 by ZacKienzle2).
const REDIRECT_MATCHER: &str = "Read|Grep|Glob";

/// PostToolUse matcher for the guard-safe re-read dedup (GL #1140): the native
/// Read and lean-ctx's own `ctx_read`, whose server-side stub is withheld on
/// Claude Code (#1801) — the hook knows the reading agent, the server does not.
/// The handler mirrors each result shape and passes every other tool through.
pub(super) const READ_DEDUP_MATCHER: &str = "Read|mcp__lean-ctx__ctx_read|mcp__lean_ctx__ctx_read";

/// Ensure the PostToolUse `hook read-dedup` entry exists exactly once (GL #1140).
///
/// Runs alongside the observe hook on PostToolUse; only read-dedup emits
/// `updatedToolOutput`, so there is no competing-rewriter race. The handler is
/// inert off guard hosts and for first reads (`read_dedup = auto`), so
/// installing it unconditionally is safe.
fn ensure_read_dedup_hook(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    read_dedup_cmd: &str,
) {
    let post = hooks_obj
        .entry("PostToolUse".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if let Some(post_arr) = post.as_array_mut() {
        ensure_command_hook(post_arr, READ_DEDUP_MATCHER, read_dedup_cmd);
    }
}

/// The trailing action token of a lean-ctx hook command, e.g. `"hook rewrite"`.
///
/// Commands are rendered as `<binary> hook <action>`, where `<binary>` is either a bare
/// `lean-ctx` (when it is on `PATH`) or an absolute path (when it is not). The action token
/// is therefore the only path-independent way to recognise a lean-ctx hook.
pub(super) fn lean_ctx_action_token(command: &str) -> &str {
    match command.rfind(" hook ") {
        Some(i) => command[i + 1..].trim_end(),
        None => command.trim_end(),
    }
}

/// True if `hook` is a lean-ctx command hook for `action`, regardless of how the binary path
/// was rendered (bare `lean-ctx` vs an absolute path) and including the legacy script form
/// (`…/lean-ctx-rewrite.sh`) written by pre-3.x installs.
pub(super) fn is_lean_ctx_command_for(hook: &serde_json::Value, action: &str) -> bool {
    if hook.get("type").and_then(|t| t.as_str()) != Some("command") {
        return false;
    }
    let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) else {
        return false;
    };
    if !cmd.contains("lean-ctx") {
        return false;
    }
    // Only `<lean-ctx binary> hook <action>` is ours. A user wrapper that merely *mentions*
    // lean-ctx (`redact-wrap lean-ctx hook rewrite`) ends with the same action but must never
    // be stripped (#1879).
    if let Some(program) = cmd.trim_end().strip_suffix(action) {
        if let Some(program) = program.strip_suffix(' ') {
            return is_lean_ctx_program(program);
        }
    }
    let legacy = if action.ends_with("rewrite") {
        "lean-ctx-rewrite"
    } else if action.ends_with("redirect") {
        "lean-ctx-redirect"
    } else {
        return false;
    };
    cmd.contains(legacy)
}

/// True if `program` (the part of a hook command before ` hook <action>`) is the lean-ctx
/// binary: a bare `lean-ctx`, any path to it (quoted or not, with spaces, `.exe`, `$HOME/…`
/// portable forms), the configured `hook_binary` override verbatim, or the binary path the
/// installer currently renders (covers renamed binaries, which would otherwise be re-added on
/// every refresh).
fn is_lean_ctx_program(program: &str) -> bool {
    let program = program.trim().trim_matches(|c| c == '"' || c == '\'');
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name.eq_ignore_ascii_case("lean-ctx")
        || name.eq_ignore_ascii_case("lean-ctx.exe")
        || lean_ctx::core::portable_binary::hook_binary_override().is_some_and(|o| o == program)
        || crate::hooks::resolve_binary_path() == program
}

/// Remove every lean-ctx hook for `action` (any path, any matcher group), dropping groups
/// left empty. Non-lean-ctx hooks are untouched.
pub(super) fn strip_lean_ctx_hooks(pre_arr: &mut Vec<serde_json::Value>, action: &str) {
    for group in pre_arr.iter_mut() {
        if let Some(hooks) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
            hooks.retain(|h| !is_lean_ctx_command_for(h, action));
        }
    }
    pre_arr.retain(|g| {
        g.get("hooks")
            .and_then(|h| h.as_array())
            .is_none_or(|hooks| !hooks.is_empty())
    });
}

/// Ensure exactly one lean-ctx hook for `command`'s action exists under `matcher`.
///
/// Earlier versions deduped on the *full* command string, but `resolve_binary_path()` renders
/// the binary as a bare `lean-ctx` (on `PATH`) or an absolute path (off `PATH`), so the
/// rendering flips between `install` and `update` and the exact-string compare missed the
/// existing hook — appending a duplicate on every run. We now strip every stale lean-ctx hook
/// for this action (any path, any matcher group, including legacy `.sh` hooks) and re-add a
/// single fresh one. This is idempotent across re-runs *and* self-heals settings files already
/// duplicated by older versions, while leaving any non-lean-ctx hooks untouched.
pub(super) fn ensure_command_hook(
    pre_arr: &mut Vec<serde_json::Value>,
    matcher: &str,
    command: &str,
) {
    strip_lean_ctx_hooks(pre_arr, lean_ctx_action_token(command));

    let desired = serde_json::json!({ "type": "command", "command": command });
    if let Some(group) = pre_arr
        .iter_mut()
        .find(|g| g.get("matcher").and_then(|m| m.as_str()) == Some(matcher))
    {
        if let Some(obj) = group.as_object_mut() {
            match obj
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!([]))
                .as_array_mut()
            {
                Some(hooks) => hooks.insert(0, desired),
                None => {
                    obj.insert("hooks".to_string(), serde_json::json!([desired]));
                }
            }
        }
        return;
    }
    pre_arr.push(serde_json::json!({ "matcher": matcher, "hooks": [desired] }));
}

fn ensure_claude_observe_hooks(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    observe_cmd: &str,
) {
    let observe_hook = serde_json::json!([{
        "matcher": ".*",
        "hooks": [{ "type": "command", "command": observe_cmd }]
    }]);

    let observe_events = [
        "PostToolUse",
        "UserPromptSubmit",
        "Stop",
        "PreCompact",
        "SessionStart",
        "SessionEnd",
    ];

    for event in observe_events {
        let entry = hooks_obj
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]));

        if let Some(arr) = entry.as_array() {
            let already = arr.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|hook| {
                            hook.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c.contains("hook observe"))
                        })
                    })
            });
            if already {
                continue;
            }
        }

        if let Some(arr) = entry.as_array_mut() {
            arr.push(serde_json::json!({
                "matcher": ".*",
                "hooks": [{ "type": "command", "command": observe_cmd }]
            }));
        } else {
            *entry = observe_hook.clone();
        }
    }
}

pub(crate) fn install_claude_hook_config(home: &std::path::Path) {
    if super::claude_governor::is_installed(home) {
        return super::claude_governor::refresh(home);
    }
    let hooks_dir = lean_ctx::core::editor_registry::claude_state_dir(home).join("hooks");
    let binary = resolve_hook_command_binary();

    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");
    let observe_cmd = format!("{binary} hook observe");
    let read_dedup_cmd = format!("{binary} hook read-dedup");

    let settings_path =
        lean_ctx::core::editor_registry::claude_state_dir(home).join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    let bash_matcher = if cfg!(windows) {
        "Bash|PowerShell"
    } else {
        "Bash"
    };

    let desired_pretooluse = serde_json::json!([
        {
            "matcher": bash_matcher,
            "hooks": [{
                "type": "command",
                "command": rewrite_cmd
            }]
        },
        {
            "matcher": REDIRECT_MATCHER,
            "hooks": [{
                "type": "command",
                "command": redirect_cmd
            }]
        }
    ]);

    if !settings_content.trim().is_empty()
        && let Err(error) = lean_ctx::core::jsonc::parse_jsonc(&settings_content)
    {
        return report_unreadable_settings(&unparseable_settings(&settings_path, &error));
    }
    if settings_content.trim().is_empty() {
        let mut hook_map = serde_json::Map::new();
        hook_map.insert("PreToolUse".to_string(), desired_pretooluse);
        ensure_claude_observe_hooks(&mut hook_map, &observe_cmd);
        ensure_read_dedup_hook(&mut hook_map, &read_dedup_cmd);
        let hook_entry = serde_json::json!({ "hooks": serde_json::Value::Object(hook_map) });
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_entry).unwrap_or_default(),
        );
    } else if let Ok(mut existing) = lean_ctx::core::jsonc::parse_jsonc(&settings_content) {
        let before = serde_json::to_string_pretty(&existing).unwrap_or_default();
        if let Some(root) = existing.as_object_mut() {
            let hooks = root
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(hooks_obj) = hooks.as_object_mut() {
                let pre = hooks_obj
                    .entry("PreToolUse".to_string())
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(pre_arr) = pre.as_array_mut() {
                    ensure_bash_rewrite_hook(pre_arr, bash_matcher, &rewrite_cmd, home);
                    ensure_command_hook(pre_arr, REDIRECT_MATCHER, &redirect_cmd);
                }
                ensure_claude_observe_hooks(hooks_obj, &observe_cmd);
                ensure_read_dedup_hook(hooks_obj, &read_dedup_cmd);
            }
        }
        // Only rewrite (with backup) when the merge actually changed something, so a
        // no-op `update` doesn't churn the user's settings.json or pile up backups.
        let after = serde_json::to_string_pretty(&existing).unwrap_or_default();
        if after != before {
            write_file(&settings_path, &after);
        }
    }
    if !mcp_server_quiet_mode() {
        eprintln!("Installed Claude Code hooks at {}", hooks_dir.display());
    }
}

pub(crate) fn install_claude_project_hooks(cwd: &std::path::Path) {
    let binary = resolve_hook_command_binary();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");
    let observe_cmd = format!("{binary} hook observe");
    let read_dedup_cmd = format!("{binary} hook read-dedup");

    let settings_path = cwd.join(".claude").join("settings.local.json");
    let _ = std::fs::create_dir_all(cwd.join(".claude"));

    let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let bash_matcher = if cfg!(windows) {
        "Bash|PowerShell"
    } else {
        "Bash"
    };

    let desired_pretooluse = serde_json::json!([
        {
            "matcher": bash_matcher,
            "hooks": [{
                "type": "command",
                "command": rewrite_cmd
            }]
        },
        {
            "matcher": REDIRECT_MATCHER,
            "hooks": [{
                "type": "command",
                "command": redirect_cmd
            }]
        }
    ]);

    if !existing.trim().is_empty()
        && let Err(error) = lean_ctx::core::jsonc::parse_jsonc(&existing)
    {
        return report_unreadable_settings(&unparseable_settings(&settings_path, &error));
    }
    if existing.trim().is_empty() {
        let mut hook_map = serde_json::Map::new();
        hook_map.insert("PreToolUse".to_string(), desired_pretooluse);
        ensure_claude_project_observe_hooks(&mut hook_map, &observe_cmd);
        ensure_read_dedup_hook(&mut hook_map, &read_dedup_cmd);
        let hook_entry = serde_json::json!({ "hooks": serde_json::Value::Object(hook_map) });
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_entry).unwrap_or_default(),
        );
    } else if let Ok(mut json) = lean_ctx::core::jsonc::parse_jsonc(&existing) {
        let before = serde_json::to_string_pretty(&json).unwrap_or_default();
        if let Some(root) = json.as_object_mut() {
            let hooks = root
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(hooks_obj) = hooks.as_object_mut() {
                let pre = hooks_obj
                    .entry("PreToolUse".to_string())
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(pre_arr) = pre.as_array_mut() {
                    // Project wrappers reference `~/…` scripts too; without a resolvable home
                    // only inline delegation is detected.
                    let home = lean_ctx::core::home::resolve_home_dir().unwrap_or_default();
                    ensure_bash_rewrite_hook(pre_arr, bash_matcher, &rewrite_cmd, &home);
                    ensure_command_hook(pre_arr, REDIRECT_MATCHER, &redirect_cmd);
                }
                ensure_claude_project_observe_hooks(hooks_obj, &observe_cmd);
                ensure_read_dedup_hook(hooks_obj, &read_dedup_cmd);
            }
        }
        let after = serde_json::to_string_pretty(&json).unwrap_or_default();
        if after != before {
            write_file(&settings_path, &after);
        }
    }
    if !mcp_server_quiet_mode() {
        eprintln!("Created .claude/settings.local.json (project-local hooks with observe).");
    }
}

/// Project-level observe hooks — excludes SessionStart/SessionEnd (global-only).
fn ensure_claude_project_observe_hooks(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    observe_cmd: &str,
) {
    let project_events = ["PostToolUse", "UserPromptSubmit", "Stop", "PreCompact"];
    for event in project_events {
        let entry = hooks_obj
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]));

        if let Some(arr) = entry.as_array() {
            let already = arr.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|hook| {
                            hook.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c.contains("hook observe"))
                        })
                    })
            });
            if already {
                continue;
            }
        }

        if let Some(arr) = entry.as_array_mut() {
            arr.push(serde_json::json!({
                "matcher": ".*",
                "hooks": [{ "type": "command", "command": observe_cmd }]
            }));
        } else {
            *entry = serde_json::json!([{
                "matcher": ".*",
                "hooks": [{ "type": "command", "command": observe_cmd }]
            }]);
        }
    }
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
