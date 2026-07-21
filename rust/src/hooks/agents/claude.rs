use super::super::{
    HookMode, REDIRECT_SCRIPT_CLAUDE, ensure_state_dir, generate_rewrite_script, make_executable,
    mcp_server_quiet_mode, resolve_binary_path_for_bash, resolve_hook_command_binary,
    shell_quoted_binary, write_file, write_wrapper_file,
};
use super::shared::remove_all_blocks;

pub(crate) fn install_claude_hook_with_mode(global: bool, mode: HookMode) {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_claude_hook_scripts(&home);
    install_claude_hook_config(&home);

    // #281: register the MCP server only when MCP updates are enabled. The hook
    // scripts/config above and the rules/skill below still install, so an
    // MCP-disabled setup keeps the CLI integration without an MCP entry.
    if matches!(mode, HookMode::Hybrid | HookMode::Mcp | HookMode::Replace)
        && super::super::should_register_mcp()
    {
        install_claude_mcp_server(&home);
    }

    if mode == HookMode::Replace {
        install_claude_permissions_deny_replace(&home);
    }

    let scope = crate::core::config::Config::load().rules_scope_effective();
    if scope != crate::core::config::RulesScope::Project {
        remove_claude_rules_file(&home);
        install_claude_global_claude_md_for_mode(&home, mode);
        // rules_injection=off (#361): the functional hooks above still install
        // (off opts out of *instructions*, not compression), but the on-demand
        // skill is lean-ctx-authored steering — suppress it, and remove one a
        // previous shared/dedicated install left behind.
        if crate::core::config::Config::load().rules_injection_effective()
            == crate::core::config::RulesInjection::Off
        {
            remove_claude_skill(&home);
        } else {
            install_claude_skill(&home);
        }
    }

    let _ = global;
}

fn install_claude_mcp_server(home: &std::path::Path) {
    let config_path = crate::core::editor_registry::claude_mcp_json_path(home);
    let binary = super::super::resolve_binary_path();

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    if existing.contains("\"lean-ctx\"") && existing.contains("mcpServers") {
        return;
    }

    let parsed: Result<serde_json::Value, _> = if existing.trim().is_empty() {
        Ok(serde_json::json!({}))
    } else {
        crate::core::jsonc::parse_jsonc(&existing)
    };

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
                    "args": []
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

/// In Replace mode, add Read/Grep/Glob/Bash to Claude Code's `permissions.deny`
/// so native tools are completely unavailable and the agent must use ctx_* MCP tools.
pub(crate) fn install_claude_permissions_deny_replace(home: &std::path::Path) {
    let settings_path = home.join(".claude").join("settings.json");

    let mut json = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path).unwrap_or_default();
        if content.trim().is_empty() {
            serde_json::json!({})
        } else {
            crate::core::jsonc::parse_jsonc(&content).unwrap_or_else(|_| serde_json::json!({}))
        }
    } else {
        let _ = std::fs::create_dir_all(home.join(".claude"));
        serde_json::json!({})
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
    let deny_tools = ["Read", "Grep", "Glob"];
    let mut changed = false;
    for tool in deny_tools {
        let val = serde_json::Value::String(tool.to_string());
        if !arr.contains(&val) {
            arr.push(val);
            changed = true;
        }
    }

    // Remove stale "Bash" entry left by older versions (GH #799).
    let bash_val = serde_json::Value::String("Bash".to_string());
    if let Some(pos) = arr.iter().position(|v| v == &bash_val) {
        arr.remove(pos);
        changed = true;
    }

    if changed {
        if let Ok(out) = serde_json::to_string_pretty(&json) {
            let _ = std::fs::write(&settings_path, out);
        }
        if !mcp_server_quiet_mode() {
            eprintln!(
                "  \x1b[32m✓\x1b[0m Claude Code: denied native Read/Grep/Glob (Replace mode)"
            );
        }
    }
}

/// Shared with `doctor` so the instructions check recognises the same block
/// this installer writes (GH #396: doctor must not demand the retired rules file).
///
/// The CLAUDE.md block is a lightweight *pointer* block, so it is delimited by
/// `AGENTS_BLOCK_START`/`END` (`<!-- lean-ctx -->`), NOT the full-rules markers
/// `START_MARK`/`END_MARK` (`<!-- lean-ctx-rules -->`). Pointing these at the
/// rules markers made `contains()` never match the block lean-ctx actually
/// writes, so `doctor` reported it missing and every `setup`/`doctor --fix`
/// appended a duplicate (GH #549).
pub(crate) const CLAUDE_MD_BLOCK_START: &str = crate::core::rules_canonical::AGENTS_BLOCK_START;
const CLAUDE_MD_BLOCK_END: &str = crate::core::rules_canonical::AGENTS_BLOCK_END;
const CLAUDE_MD_BLOCK_VERSION: &str = "lean-ctx-claude-v7";

// v3 (GL #555): self-contained, no `@rules/lean-ctx.md` import. Claude Code
// expands `@` imports inline at launch ("imports do not reduce context usage"
// — code.claude.com/docs/en/memory), so the old pointer silently tripled the
// per-session footprint. Detail docs now live in the lean-ctx skill, which
// loads on demand only.
//
// v4 (GH #637 / GL #1138): MCP-aware guidance. The old block recommended
// `ctx_edit` unconditionally — in sessions where the lean-ctx MCP server is
// not connected the advertised fallback does not exist, and agents strand on
// shell heredocs. It also predated `read_redirect=auto`, which makes native
// Read → Edit a guard-safe editing path under Claude Code (the read-before-
// write gate is path-keyed, so the edited file must be *natively* read).
//
// v5 (#1008 / GL #1144): anchored editing routes to `ctx_patch` (now advertised
// in the lazy core for Claude Code) — line+hash anchors instead of old_string
// reproduction; `ctx_edit` demoted to a legacy power-profile mention. Native
// Read → Edit stays fully supported (v4's guard semantics unchanged).
//
// v7 (#1091): fix the Replace-mode block's write guidance. "Write and Delete —
// use native tools normally" was wrong under Replace mode: native `Edit` needs
// a prior native `Read` (denied), and native `Write` only succeeds for
// brand-new files, so `ctx_patch` is the actual edit path. The MCP block is
// unchanged; the shared version tag bumps so existing blocks get rewritten.
const CLAUDE_MD_BLOCK_CONTENT_MCP: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-claude-v7 -->
## lean-ctx — Context Runtime

When the `ctx_*` MCP tools are listed in this session, prefer them over native equivalents:
- `ctx_read` instead of `Read` / `cat` for exploration (cached, 10 modes, re-reads ~13 tokens)
- `ctx_shell` instead of `bash` / `Shell` (95+ compression patterns)
- `ctx_search` instead of `Grep` / `rg` (compact results)
- `ctx_tree` instead of `ls` / `find` (compact directory maps)
- Edits: `ctx_read(mode=\"anchored\")` → `ctx_patch` (line+hash anchors, never echo old text; `op=create` for new files). `ctx_edit` (str_replace) is the legacy power-profile fallback.

Native `Read` → `Edit`/`StrReplace` stays fully supported — the edit gate requires a
prior native Read of the same file path. Write, Delete, Glob — use normally.
If no `ctx_*` tools are listed in this session, use the native tools throughout.

Read modes: anchored (edit), full (verbatim), map (overview), signatures (API), diff (post-edit), lines:N-M (range), auto.
Details live in the `lean-ctx` skill (loads on demand — keep this file lean).
<!-- /lean-ctx -->";

const CLAUDE_MD_BLOCK_CONTENT_REPLACE: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-claude-v7 -->
## lean-ctx — Replace Mode (native tools denied)

Native Read/Grep/Glob/Bash are denied by policy. Use ONLY `ctx_*` MCP tools:
- `ctx_read` for ALL file reads (cached, 10 modes, re-reads ~13 tokens)
- `ctx_shell` for ALL shell commands (95+ compression patterns)
- `ctx_search` instead of Grep/rg (compact results)
- `ctx_tree` instead of ls/find (compact directory maps)
- `ctx_glob` instead of Glob (file pattern matching)
- Edits: `ctx_read(mode=\"anchored\")` → `ctx_patch` (line+hash anchors, never echo old text; `op=create` for new files).

`ctx_patch` is the edit path: native `Edit` needs a prior native `Read` (denied), and native `Write` only succeeds for brand-new files. Use `op=create` for new files; native Delete is fine.
Do NOT attempt native Read, Grep, Glob, or Bash — they will be denied.

Read modes: anchored (edit), full (verbatim), map (overview), signatures (API), diff (post-edit), lines:N-M (range), auto.
Details live in the `lean-ctx` skill (loads on demand — keep this file lean).
<!-- /lean-ctx -->";

fn install_claude_global_claude_md_for_mode(home: &std::path::Path, mode: HookMode) {
    let claude_dir = crate::core::editor_registry::claude_state_dir(home);
    if !ensure_state_dir(&claude_dir) {
        return;
    }
    let claude_md_path = claude_dir.join("CLAUDE.md");

    // Neither dedicated nor off keep a lean-ctx block in the user's CLAUDE.md:
    //  - dedicated (#343): the SessionStart hook injects the compact summary;
    //  - off (#361): the user opted out of lean-ctx steering entirely.
    // Strip any block a previous shared install left so switching modes is clean.
    if matches!(
        crate::core::config::Config::load().rules_injection_effective(),
        crate::core::config::RulesInjection::Dedicated | crate::core::config::RulesInjection::Off
    ) {
        strip_claude_md_block(&claude_md_path);
        return;
    }

    let existing = std::fs::read_to_string(&claude_md_path).unwrap_or_default();
    let block = match mode {
        HookMode::Replace => CLAUDE_MD_BLOCK_CONTENT_REPLACE,
        HookMode::Mcp | HookMode::Hybrid => CLAUDE_MD_BLOCK_CONTENT_MCP,
    };
    let block_version = CLAUDE_MD_BLOCK_VERSION;

    // A single up-to-date block needs no rewrite. Check both version tag AND
    // mode-specific content — the Replace and MCP blocks share the version tag
    // but have different instructions (GH #1250 follow-up).
    let block_count = existing.matches(CLAUDE_MD_BLOCK_START).count();
    let is_replace_block = existing.contains("denied by policy");
    let mode_matches = matches!(mode, HookMode::Replace) == is_replace_block;
    if block_count == 1 && existing.contains(block_version) && mode_matches {
        return;
    }
    let cleaned = remove_all_blocks(&existing, CLAUDE_MD_BLOCK_START, CLAUDE_MD_BLOCK_END);
    let cleaned = cleaned.trim();
    let updated = if cleaned.is_empty() {
        format!("{block}\n")
    } else {
        format!("{cleaned}\n\n{block}\n")
    };
    write_file(&claude_md_path, &updated);
}

/// Remove the lean-ctx block from `CLAUDE.md` (dedicated mode). Deletes the file
/// entirely if it becomes empty (i.e. lean-ctx was its only content).
fn strip_claude_md_block(claude_md_path: &std::path::Path) {
    let Ok(existing) = std::fs::read_to_string(claude_md_path) else {
        return;
    };
    if !existing.contains(CLAUDE_MD_BLOCK_START) {
        return;
    }
    let cleaned = remove_all_blocks(&existing, CLAUDE_MD_BLOCK_START, CLAUDE_MD_BLOCK_END);
    if cleaned.trim().is_empty() {
        let _ = std::fs::remove_file(claude_md_path);
    } else {
        write_file(claude_md_path, &format!("{}\n", cleaned.trim_end()));
    }
}

fn install_claude_skill(home: &std::path::Path) {
    // Honor CLAUDE_CONFIG_DIR like every other Claude state path (CLAUDE.md,
    // hooks, settings.json) instead of hardcoding `~/.claude` (#596).
    let skill_dir = crate::core::editor_registry::claude_state_dir(home).join("skills/lean-ctx");
    if !ensure_state_dir(&skill_dir.join("scripts")) {
        return;
    }

    let skill_md = include_str!("../../templates/SKILL.md");
    let install_sh = include_str!("../../templates/skill_install.sh");

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
    let skill_dir = crate::core::editor_registry::claude_state_dir(home).join("skills/lean-ctx");
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

/// Remove the lean-ctx-owned `~/.claude/rules/lean-ctx.md` (GL #555).
///
/// Claude Code loads every rules file without `paths:` frontmatter
/// unconditionally at session start (code.claude.com/docs/en/memory), so this
/// file duplicated the CLAUDE.md block in *every* session — users reported
/// 12k+ tokens of memory files from stacked lean-ctx instructions. The
/// CLAUDE.md block is self-contained and detail docs live in the on-demand
/// skill; only files carrying our rules marker are touched.
fn remove_claude_rules_file(home: &std::path::Path) {
    let rules_path = crate::core::editor_registry::claude_rules_dir(home).join("lean-ctx.md");
    let Ok(existing) = std::fs::read_to_string(&rules_path) else {
        return;
    };
    if existing.contains(crate::core::rules_canonical::RULES_MARKER_PREFIX)
        && std::fs::remove_file(&rules_path).is_ok()
        && !super::super::mcp_server_quiet_mode()
    {
        eprintln!(
            "Removed {} (always-loaded duplicate; the CLAUDE.md block + on-demand skill replace it)",
            rules_path.display()
        );
    }
}

pub(crate) fn install_claude_hook_scripts(home: &std::path::Path) {
    let hooks_dir = crate::core::editor_registry::claude_state_dir(home).join("hooks");
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
const REDIRECT_MATCHER: &str = "Read|read|ReadFile|read_file|View|view|Grep|grep|Search|search|ListFiles|list_files|ListDirectory|list_directory|Glob|glob";

/// PostToolUse matcher for the guard-safe re-read dedup (GL #1140): Read only —
/// the handler mirrors the Read result shape and must never see another tool's.
const READ_DEDUP_MATCHER: &str = "Read";

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
fn lean_ctx_action_token(command: &str) -> &str {
    match command.rfind(" hook ") {
        Some(i) => command[i + 1..].trim_end(),
        None => command.trim_end(),
    }
}

/// True if `hook` is a lean-ctx command hook for `action`, regardless of how the binary path
/// was rendered (bare `lean-ctx` vs an absolute path) and including the legacy script form
/// (`…/lean-ctx-rewrite.sh`) written by pre-3.x installs.
fn is_lean_ctx_command_for(hook: &serde_json::Value, action: &str) -> bool {
    if hook.get("type").and_then(|t| t.as_str()) != Some("command") {
        return false;
    }
    let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) else {
        return false;
    };
    if !cmd.contains("lean-ctx") {
        return false;
    }
    if cmd.trim_end().ends_with(action) {
        return true;
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

/// Ensure exactly one lean-ctx hook for `command`'s action exists under `matcher`.
///
/// Earlier versions deduped on the *full* command string, but `resolve_binary_path()` renders
/// the binary as a bare `lean-ctx` (on `PATH`) or an absolute path (off `PATH`), so the
/// rendering flips between `install` and `update` and the exact-string compare missed the
/// existing hook — appending a duplicate on every run. We now strip every stale lean-ctx hook
/// for this action (any path, any matcher group, including legacy `.sh` hooks) and re-add a
/// single fresh one. This is idempotent across re-runs *and* self-heals settings files already
/// duplicated by older versions, while leaving any non-lean-ctx hooks untouched.
fn ensure_command_hook(pre_arr: &mut Vec<serde_json::Value>, matcher: &str, command: &str) {
    let action = lean_ctx_action_token(command);

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
    let hooks_dir = crate::core::editor_registry::claude_state_dir(home).join("hooks");
    let binary = resolve_hook_command_binary();

    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");
    let observe_cmd = format!("{binary} hook observe");
    let read_dedup_cmd = format!("{binary} hook read-dedup");

    let settings_path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    let bash_matcher = if cfg!(windows) {
        "Bash|bash|PowerShell|powershell"
    } else {
        "Bash|bash"
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

    if settings_content.is_empty() {
        let mut hook_map = serde_json::Map::new();
        hook_map.insert("PreToolUse".to_string(), desired_pretooluse);
        ensure_claude_observe_hooks(&mut hook_map, &observe_cmd);
        ensure_read_dedup_hook(&mut hook_map, &read_dedup_cmd);
        let hook_entry = serde_json::json!({ "hooks": serde_json::Value::Object(hook_map) });
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_entry).unwrap_or_default(),
        );
    } else if let Ok(mut existing) = crate::core::jsonc::parse_jsonc(&settings_content) {
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
                    ensure_command_hook(pre_arr, bash_matcher, &rewrite_cmd);
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
        "Bash|bash|PowerShell|powershell"
    } else {
        "Bash|bash"
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

    if existing.is_empty() {
        let mut hook_map = serde_json::Map::new();
        hook_map.insert("PreToolUse".to_string(), desired_pretooluse);
        ensure_claude_project_observe_hooks(&mut hook_map, &observe_cmd);
        ensure_read_dedup_hook(&mut hook_map, &read_dedup_cmd);
        let hook_entry = serde_json::json!({ "hooks": serde_json::Value::Object(hook_map) });
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_entry).unwrap_or_default(),
        );
    } else if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&existing) {
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
                    ensure_command_hook(pre_arr, bash_matcher, &rewrite_cmd);
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

    #[test]
    fn action_token_is_path_independent() {
        assert_eq!(
            lean_ctx_action_token("lean-ctx hook rewrite"),
            "hook rewrite"
        );
        assert_eq!(
            lean_ctx_action_token("/Users/x/.local/bin/lean-ctx hook redirect"),
            "hook redirect"
        );
    }

    #[test]
    fn path_flip_refreshes_instead_of_duplicating() {
        // Installed once off PATH (absolute path), then `update` runs on PATH (bare name).
        let mut pre = vec![json!({
            "matcher": BASH,
            "hooks": [{ "type": "command", "command": "/abs/bin/lean-ctx hook rewrite" }]
        })];
        ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
        assert_eq!(
            commands_for(&pre, "hook rewrite"),
            ["lean-ctx hook rewrite"]
        );
    }

    #[test]
    fn repeated_calls_are_idempotent() {
        let mut pre = vec![];
        for _ in 0..5 {
            ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
        }
        assert_eq!(
            commands_for(&pre, "hook rewrite"),
            ["lean-ctx hook rewrite"]
        );
    }

    #[test]
    fn heals_preexisting_duplicates_across_groups() {
        // Two stale rewrite hooks left by older buggy versions, different matchers/paths.
        let mut pre = vec![
            json!({ "matcher": "Bash", "hooks": [{ "type": "command", "command": "/a/lean-ctx hook rewrite" }] }),
            json!({ "matcher": BASH, "hooks": [{ "type": "command", "command": "lean-ctx hook rewrite" }] }),
        ];
        ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
        assert_eq!(
            commands_for(&pre, "hook rewrite"),
            ["lean-ctx hook rewrite"]
        );
    }

    #[test]
    fn migrates_legacy_script_hook() {
        let mut pre = vec![json!({
            "matcher": BASH,
            "hooks": [{ "type": "command", "command": "/home/u/.claude/hooks/lean-ctx-rewrite.sh" }]
        })];
        ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
        assert_eq!(
            commands_for(&pre, "hook rewrite"),
            ["lean-ctx hook rewrite"]
        );
    }

    #[test]
    fn preserves_foreign_hooks() {
        let mut pre = vec![json!({
            "matcher": BASH,
            "hooks": [
                { "type": "command", "command": "my-own-tool --flag" },
                { "type": "command", "command": "/abs/lean-ctx hook rewrite" }
            ]
        })];
        ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
        assert_eq!(
            commands_for(&pre, "hook rewrite"),
            ["lean-ctx hook rewrite"]
        );
        let all: Vec<String> = pre
            .iter()
            .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
            .flatten()
            .filter_map(|h| h.get("command").and_then(|c| c.as_str()).map(String::from))
            .collect();
        assert!(
            all.iter().any(|c| c == "my-own-tool --flag"),
            "foreign hook dropped: {all:?}"
        );
    }

    #[test]
    fn rewrite_and_redirect_coexist_after_reruns() {
        let mut pre = vec![];
        ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
        ensure_command_hook(&mut pre, REDIRECT_MATCHER, "lean-ctx hook redirect");
        ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite"); // re-run (update)
        assert_eq!(
            commands_for(&pre, "hook rewrite"),
            ["lean-ctx hook rewrite"]
        );
        assert_eq!(
            commands_for(&pre, "hook redirect"),
            ["lean-ctx hook redirect"]
        );
    }

    #[test]
    fn redirect_matcher_covers_glob() {
        // #556: shadow mode must intercept the Glob tool, so it has to be part
        // of the redirect matcher Claude installs.
        assert!(REDIRECT_MATCHER.contains("Glob"));
        assert!(REDIRECT_MATCHER.contains("glob"));
    }

    #[test]
    fn read_dedup_hook_installs_once_alongside_observe() {
        // GL #1140: the PostToolUse array carries observe (matcher .*) AND
        // read-dedup (matcher Read); repeated installs must not duplicate either.
        let mut hooks_obj = serde_json::Map::new();
        ensure_claude_observe_hooks(&mut hooks_obj, "lean-ctx hook observe");
        ensure_read_dedup_hook(&mut hooks_obj, "lean-ctx hook read-dedup");
        ensure_read_dedup_hook(&mut hooks_obj, "lean-ctx hook read-dedup"); // re-run

        let post = hooks_obj
            .get("PostToolUse")
            .and_then(|p| p.as_array())
            .expect("PostToolUse array");
        let dedup_entries: Vec<&serde_json::Value> = post
            .iter()
            .filter(|g| {
                g.get("hooks").and_then(|h| h.as_array()).is_some_and(|h| {
                    h.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.ends_with("hook read-dedup"))
                    })
                })
            })
            .collect();
        assert_eq!(dedup_entries.len(), 1, "exactly one read-dedup group");
        assert_eq!(
            dedup_entries[0].get("matcher").and_then(|m| m.as_str()),
            Some("Read"),
            "read-dedup must match ONLY the Read tool (shape safety)"
        );
        let observe_present = post.iter().any(|g| {
            g.get("hooks").and_then(|h| h.as_array()).is_some_and(|h| {
                h.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.contains("hook observe"))
                })
            })
        });
        assert!(
            observe_present,
            "observe hook must survive read-dedup install"
        );
    }

    #[test]
    fn rules_injection_off_strips_claude_md_block() {
        // #361: with instructions opted out, the installer must not leave a
        // lean-ctx block in CLAUDE.md (and must strip one left by a prior install).
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = crate::core::editor_registry::claude_state_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("CLAUDE.md");
        std::fs::write(
            &md,
            format!("# my notes\n\n{CLAUDE_MD_BLOCK_CONTENT_MCP}\n"),
        )
        .unwrap();

        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "off");
        install_claude_global_claude_md_for_mode(home, HookMode::Mcp);
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

        let after = std::fs::read_to_string(&md).unwrap_or_default();
        assert!(
            !after.contains(CLAUDE_MD_BLOCK_START),
            "rules_injection=off must strip the CLAUDE.md block, got:\n{after}"
        );
        assert!(after.contains("# my notes"), "user content must survive");
    }

    #[test]
    fn skill_install_then_remove_roundtrips() {
        // The off path removes a previously installed skill; install+remove must
        // touch only the lean-ctx skill folder. The skill dir now honors
        // CLAUDE_CONFIG_DIR (#596); pin it off so the path is deterministic.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("CLAUDE_CONFIG_DIR");
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        install_claude_skill(home);
        assert!(home.join(".claude/skills/lean-ctx/SKILL.md").exists());
        remove_claude_skill(home);
        assert!(!home.join(".claude/skills/lean-ctx").exists());
    }

    #[test]
    fn claude_block_content_carries_detection_markers() {
        // GH #549 regression guard: the written block MUST contain the markers the
        // doctor + installer detect with — otherwise detection silently fails and
        // every `setup`/`doctor --fix` appends a fresh duplicate.
        assert!(
            CLAUDE_MD_BLOCK_CONTENT_MCP.contains(CLAUDE_MD_BLOCK_START),
            "block content must contain its own start marker"
        );
        assert!(
            CLAUDE_MD_BLOCK_CONTENT_MCP.contains(CLAUDE_MD_BLOCK_END),
            "block content must contain its own end marker"
        );
    }

    #[test]
    fn installer_writes_single_claude_block_and_is_idempotent() {
        // GH #549: repeated installs must converge on exactly one block.
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let md = crate::core::editor_registry::claude_state_dir(home).join("CLAUDE.md");

        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
        install_claude_global_claude_md_for_mode(home, HookMode::Mcp);
        install_claude_global_claude_md_for_mode(home, HookMode::Mcp);
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

        let after = std::fs::read_to_string(&md).unwrap();
        assert_eq!(
            after.matches(CLAUDE_MD_BLOCK_START).count(),
            1,
            "exactly one block after repeated installs, got:\n{after}"
        );
        assert!(after.contains(CLAUDE_MD_BLOCK_VERSION));
    }

    #[test]
    fn installer_heals_duplicate_claude_blocks() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = crate::core::editor_registry::claude_state_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("CLAUDE.md");
        let b = CLAUDE_MD_BLOCK_CONTENT_MCP;
        let dupes = format!("# my notes\n\n{b}\n\n{b}\n\n{b}\n");
        std::fs::write(&md, dupes).unwrap();

        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
        install_claude_global_claude_md_for_mode(home, HookMode::Mcp);
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

        let after = std::fs::read_to_string(&md).unwrap();
        assert_eq!(
            after.matches(CLAUDE_MD_BLOCK_START).count(),
            1,
            "duplicates must collapse to one, got:\n{after}"
        );
        assert!(after.contains("# my notes"), "user content must survive");
    }

    #[test]
    fn replace_mode_adds_permissions_deny() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        // Start with existing settings
        let settings = home.join(".claude").join("settings.json");
        std::fs::write(&settings, r#"{"hooks": {}}"#).unwrap();

        install_claude_permissions_deny_replace(home);

        let content = std::fs::read_to_string(&settings).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        let deny = json
            .pointer("/permissions/deny")
            .and_then(|d| d.as_array())
            .unwrap();

        assert!(deny.contains(&serde_json::json!("Read")));
        assert!(deny.contains(&serde_json::json!("Grep")));
        assert!(deny.contains(&serde_json::json!("Glob")));
        // Bash must NOT be in permissions.deny — it blocks plugins (GH #799)
        assert!(!deny.contains(&serde_json::json!("Bash")));
        // Original content preserved
        assert!(json.get("hooks").is_some());
    }

    #[test]
    fn replace_mode_permissions_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        let settings = home.join(".claude").join("settings.json");
        std::fs::write(&settings, r"{}").unwrap();

        install_claude_permissions_deny_replace(home);
        install_claude_permissions_deny_replace(home);

        let content = std::fs::read_to_string(&settings).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        let deny = json
            .pointer("/permissions/deny")
            .and_then(|d| d.as_array())
            .unwrap();

        // Each tool should appear exactly once
        let read_count = deny.iter().filter(|v| v.as_str() == Some("Read")).count();
        assert_eq!(read_count, 1, "idempotent: Read must appear exactly once");
    }

    #[test]
    fn replace_mode_removes_stale_bash_deny() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        // Simulate a settings.json left by an older lean-ctx version
        let settings = home.join(".claude").join("settings.json");
        std::fs::write(
            &settings,
            r#"{"permissions": {"deny": ["Read", "Grep", "Glob", "Bash"]}}"#,
        )
        .unwrap();

        install_claude_permissions_deny_replace(home);

        let content = std::fs::read_to_string(&settings).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        let deny = json
            .pointer("/permissions/deny")
            .and_then(|d| d.as_array())
            .unwrap();

        assert!(deny.contains(&serde_json::json!("Read")));
        assert!(deny.contains(&serde_json::json!("Grep")));
        assert!(deny.contains(&serde_json::json!("Glob")));
        assert!(
            !deny.contains(&serde_json::json!("Bash")),
            "stale Bash must be removed (GH #799)"
        );
    }
}
