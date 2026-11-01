use std::path::PathBuf;

use super::super::{
    HookMode, make_executable, mcp_server_quiet_mode, resolve_binary_path_for_bash,
    resolve_hook_command_binary, shell_quoted_binary, write_file, write_wrapper_file,
};
use super::shared::{
    install_standard_hook_scripts, read_agent_json_config, report_unreadable_agent_config,
};

fn ensure_pretooluse_hook(
    pre: &mut Vec<serde_json::Value>,
    matcher_variants: &[&str],
    desired_matcher: &str,
    desired_command: &str,
) {
    if let Some(existing) = pre.iter_mut().find(|v| {
        v.get("matcher")
            .and_then(|m| m.as_str())
            .is_some_and(|m| matcher_variants.contains(&m))
    }) {
        if let Some(obj) = existing.as_object_mut() {
            obj.insert(
                "matcher".to_string(),
                serde_json::Value::String(desired_matcher.to_string()),
            );
            obj.insert(
                "command".to_string(),
                serde_json::Value::String(desired_command.to_string()),
            );
        }
        return;
    }
    pre.push(serde_json::json!({
        "matcher": desired_matcher,
        "command": desired_command
    }));
}

fn ensure_observe_hook(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    event: &str,
    observe_cmd: &str,
) {
    let arr = hooks_obj
        .entry(event.to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !arr.is_array() {
        *arr = serde_json::json!([]);
    }
    let Some(entries) = arr.as_array_mut() else {
        return;
    };
    let already = entries.iter().any(|e| {
        e.get("command")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains("hook observe"))
    });
    if !already {
        entries.push(serde_json::json!({ "command": observe_cmd }));
    }
}

fn merge_cursor_hooks(existing: &mut serde_json::Value, rewrite_cmd: &str, redirect_cmd: &str) {
    if !existing.is_object() {
        *existing = serde_json::json!({});
    }
    let Some(root) = existing.as_object_mut() else {
        return;
    };
    root.insert("version".to_string(), serde_json::json!(1));

    let hooks = root
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return;
    };

    // PreToolUse hooks (rewrite + redirect)
    let pre = hooks_obj
        .entry("preToolUse".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !pre.is_array() {
        *pre = serde_json::json!([]);
    }
    let Some(pre_arr) = pre.as_array_mut() else {
        return;
    };

    ensure_pretooluse_hook(pre_arr, &["Shell"], "Shell", rewrite_cmd);
    // Smart Read redirect re-enabled: edit-probe PoC (2026-07) proved that
    // StrReplace does NOT fire a Read PreToolUse — only "Write" — so Read
    // redirect is safe. Uses `auto` mode for full reads (87-97% compression)
    // and `full-compact` for windowed reads (offset/limit).
    // GH #1250 was caused by an older Cursor version or misattribution.
    ensure_pretooluse_hook(
        pre_arr,
        &["Read|Grep|Glob", "Read|Grep", "Grep|Glob", "Grep", "Read"],
        "Read|Grep|Glob",
        redirect_cmd,
    );

    // Observe hooks — only essential ones (#1200). postToolUse caused
    // Cursor to append hook stdout to edited files, corrupting source code.
    let observe_cmd = rewrite_cmd.replace("hook rewrite", "hook observe");
    ensure_observe_hook(hooks_obj, "sessionStart", &observe_cmd);
    ensure_observe_hook(hooks_obj, "preCompact", &observe_cmd);

    // Clean up previously installed problematic hooks
    for stale in &[
        "postToolUse",
        "afterShellExecution",
        "afterMCPExecution",
        "beforeReadFile",
        "afterAgentResponse",
        "afterAgentThought",
        "beforeSubmitPrompt",
        "sessionEnd",
    ] {
        remove_observe_hook(hooks_obj, stale, &observe_cmd);
    }
}

fn remove_observe_hook(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    event: &str,
    _observe_cmd: &str,
) {
    let Some(arr) = hooks_obj.get_mut(event).and_then(|v| v.as_array_mut()) else {
        return;
    };
    arr.retain(|e| {
        !e.get("command")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains("hook observe"))
    });
    if arr.is_empty() {
        hooks_obj.remove(event);
    }
}

pub fn install_cursor_hook(global: bool) {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_cursor_hook_scripts(&home);
    install_cursor_hook_config(&home);

    let scope = crate::core::config::Config::load().rules_scope_effective();
    let skip_project = global || scope == crate::core::config::RulesScope::Global;

    if skip_project {
        if !mcp_server_quiet_mode() {
            eprintln!(
                "Global mode: skipping project-local .cursor/rules/ (use without --global in a project)."
            );
        }
    } else {
        let rules_dir = PathBuf::from(".cursor").join("rules");
        let _ = std::fs::create_dir_all(&rules_dir);
        let rule_path = rules_dir.join("lean-ctx.mdc");
        if rule_path.exists() {
            if !mcp_server_quiet_mode() {
                eprintln!("Cursor rule already exists.");
            }
        } else {
            write_file(&rule_path, &cursor_mdc_content(&home));
            if !mcp_server_quiet_mode() {
                eprintln!("Created .cursor/rules/lean-ctx.mdc in current project.");
            }
        }
    }

    if !mcp_server_quiet_mode() {
        eprintln!("Restart Cursor to activate.");
    }
}

pub(crate) fn install_cursor_hook_with_mode(global: bool, mode: HookMode) {
    match mode {
        HookMode::Mcp => install_cursor_hook(global),
        HookMode::Hybrid => {
            install_cursor_hook(global);
            install_cursor_rules_for_mode(global, mode);
        }
        HookMode::Replace => {
            install_cursor_hook(global);
            install_cursor_deny_hook(global);
            install_cursor_rules_for_mode(global, mode);
        }
    }
}

fn install_cursor_rules_for_mode(global: bool, mode: HookMode) {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return;
    };
    let content = cursor_mdc_content(&home);
    let mode_name = match mode {
        HookMode::Hybrid => "hybrid",
        HookMode::Mcp => "mcp",
        HookMode::Replace => "replace",
    };

    if global {
        let global_rules_dir = home.join(".cursor").join("rules");
        let _ = std::fs::create_dir_all(&global_rules_dir);
        let global_path = global_rules_dir.join("lean-ctx.mdc");
        write_file(&global_path, &content);
        if !mcp_server_quiet_mode() {
            eprintln!(
                "Installed Cursor rules in {mode_name} mode at {}",
                global_path.display()
            );
        }
    } else {
        let rules_dir = PathBuf::from(".cursor").join("rules");
        let _ = std::fs::create_dir_all(&rules_dir);
        let rule_path = rules_dir.join("lean-ctx.mdc");
        write_file(&rule_path, &content);
        if !mcp_server_quiet_mode() {
            eprintln!("Installed Cursor rules in {mode_name} mode at .cursor/rules/lean-ctx.mdc");
        }
    }
}

/// The Cursor mdc document this installer writes. Config-driven (GL #1156 —
/// previously hardcoded `shadow=false`, `CompressionLevel::Off`) and
/// hook-aware (GL #1153): right after `install_cursor_hook_config` wrote the
/// rewrite+redirect hooks, the coverage check selects the honest HookCovered
/// profile; without hooks it falls back to the full Dedicated mapping.
fn cursor_mdc_content(home: &std::path::Path) -> String {
    let cfg = crate::core::config::Config::load();
    let wrapper = if crate::core::rules_channel::cursor_hooks_cover_native_tools(home) {
        crate::core::rules_canonical::Wrapper::HookCovered
    } else {
        crate::core::rules_canonical::Wrapper::Dedicated
    };
    let profile = crate::core::tool_profiles::ToolProfile::from_config(&cfg);
    let body = crate::core::rules_canonical::render(
        cfg.shadow_mode,
        wrapper,
        crate::core::config::CompressionLevel::effective(&cfg),
        &profile,
    );
    crate::rules_inject::cursor_mdc_document(&body)
}

pub(crate) fn install_cursor_hook_scripts(home: &std::path::Path) {
    let hooks_dir = home.join(".cursor").join("hooks");
    install_standard_hook_scripts(
        &hooks_dir,
        home,
        "lean-ctx-rewrite.sh",
        "lean-ctx-redirect.sh",
    );

    // #719: quoted + heal-safe like the Claude wrappers; bash-compatible path
    // because these are `#!/bin/sh` scripts.
    let native_binary = shell_quoted_binary(&resolve_binary_path_for_bash());
    let rewrite_native = hooks_dir.join("lean-ctx-rewrite-native");
    write_wrapper_file(
        &rewrite_native,
        &format!("#!/bin/sh\nexec {native_binary} hook rewrite\n"),
        home,
    );
    make_executable(&rewrite_native);

    let redirect_native = hooks_dir.join("lean-ctx-redirect-native");
    write_wrapper_file(
        &redirect_native,
        &format!("#!/bin/sh\nexec {native_binary} hook redirect\n"),
        home,
    );
    make_executable(&redirect_native);
}

/// In Replace mode, swap the redirect hook for a deny hook that blocks
/// native Read/Grep and instructs the agent to use ctx_read/ctx_search.
pub(crate) fn install_cursor_deny_hook(_global: bool) {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return;
    };
    let binary = resolve_hook_command_binary();
    let deny_cmd = format!("{binary} hook deny");

    let hooks_json = home.join(".cursor").join("hooks.json");
    let mut existing = match read_agent_json_config(&hooks_json, "cursor") {
        Ok(existing) => existing,
        Err(error) => return report_unreadable_agent_config("Cursor", &error),
    };

    if !existing.is_object() {
        existing = serde_json::json!({});
    }

    let root = existing
        .as_object_mut()
        .expect("existing normalized to JSON object above");
    root.insert("version".to_string(), serde_json::json!(1));

    let hooks = root
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    let hooks_obj = hooks
        .as_object_mut()
        .expect("hooks normalized to JSON object above");

    let pre = hooks_obj
        .entry("preToolUse".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !pre.is_array() {
        *pre = serde_json::json!([]);
    }
    let pre_arr = pre
        .as_array_mut()
        .expect("preToolUse normalized to JSON array above");

    // Read: redirect with smart compression (auto mode). Safe because
    // StrReplace does NOT fire Read PreToolUse (edit-probe PoC 2026-07).
    let redirect_cmd = deny_cmd.replace("hook deny", "hook redirect");
    ensure_pretooluse_hook(pre_arr, &["Read"], "Read", &redirect_cmd);

    // Grep + Glob: deny (must use ctx_search / ctx_glob instead).
    ensure_pretooluse_hook(
        pre_arr,
        &["Read|Grep|Glob", "Read|Grep", "Grep", "Grep|Glob"],
        "Grep|Glob",
        &deny_cmd,
    );

    // Write tools: deny guard blocks payloads containing compression markers.
    // Without this, an agent that reads compressed ctx_read output and pastes
    // it into StrReplace silently corrupts the file (#1302, #1323).
    ensure_pretooluse_hook(
        pre_arr,
        &["StrReplace|Write|Edit|EditNotebook|MultiEdit"],
        "StrReplace|Write|Edit|EditNotebook|MultiEdit",
        &deny_cmd,
    );

    let formatted = serde_json::to_string_pretty(&existing).unwrap_or_default();
    write_file(&hooks_json, &formatted);

    if !mcp_server_quiet_mode() {
        eprintln!("  \x1b[32m✓\x1b[0m Cursor deny hook installed (Replace mode)");
    }
}

pub(crate) fn install_cursor_hook_config(home: &std::path::Path) {
    let binary = resolve_hook_command_binary();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let hooks_json = home.join(".cursor").join("hooks.json");
    let mut existing = match read_agent_json_config(&hooks_json, "cursor") {
        Ok(existing) => existing,
        Err(error) => return report_unreadable_agent_config("Cursor", &error),
    };

    if !existing.is_object() {
        existing = serde_json::json!({});
    }

    // Merge-based: preserve other hooks/plugins. Only upsert lean-ctx entries.
    merge_cursor_hooks(&mut existing, &rewrite_cmd, &redirect_cmd);

    let formatted = serde_json::to_string_pretty(&existing).unwrap_or_default();
    write_file(&hooks_json, &formatted);

    if !mcp_server_quiet_mode() {
        eprintln!("Installed Cursor hooks at {}", hooks_json.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // An unparseable hooks.json was replaced with `{}` plus the lean-ctx
    // hooks, dropping the user's own hooks. It is now refused and left
    // byte-identical.
    #[test]
    fn unparseable_hooks_json_is_refused_and_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks_json = tmp.path().join(".cursor").join("hooks.json");
        std::fs::create_dir_all(hooks_json.parent().unwrap()).unwrap();
        let broken = "{\"version\": 1, \"hooks\": {\"stop\": [\n";
        std::fs::write(&hooks_json, broken).unwrap();

        install_cursor_hook_config(tmp.path());

        assert_eq!(std::fs::read_to_string(&hooks_json).unwrap(), broken);
        let error = read_agent_json_config(&hooks_json, "cursor").unwrap_err();
        assert!(error.contains("hooks.json is not valid JSON"), "{error}");
        assert!(error.contains("lean-ctx init --agent cursor"), "{error}");
    }

    #[test]
    fn cursor_hooks_merge_preserves_other_entries() {
        let mut v = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    { "matcher": "Shell", "command": "/old/bin hook rewrite" },
                    { "matcher": "Other", "command": "do-something" }
                ],
                "postToolUse": [
                    { "matcher": "Shell", "command": "post" }
                ]
            },
            "otherKey": { "x": 1 }
        });

        merge_cursor_hooks(&mut v, "/new/bin hook rewrite", "/new/bin hook redirect");

        assert!(v.get("otherKey").is_some());
        assert!(v.pointer("/hooks/postToolUse").is_some());

        let pre = v
            .pointer("/hooks/preToolUse")
            .and_then(|x| x.as_array())
            .unwrap();
        assert!(
            pre.iter()
                .any(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("Other"))
        );
        assert!(pre.iter().any(|e| {
            e.get("matcher").and_then(|m| m.as_str()) == Some("Shell")
                && e.get("command").and_then(|c| c.as_str()) == Some("/new/bin hook rewrite")
        }));
        assert!(pre.iter().any(|e| {
            e.get("matcher").and_then(|m| m.as_str()) == Some("Read|Grep|Glob")
                && e.get("command").and_then(|c| c.as_str()) == Some("/new/bin hook redirect")
        }));
    }

    #[test]
    fn cursor_redirect_matcher_migrates_legacy_to_read_grep_glob() {
        // Smart Read redirect re-enabled: legacy "Read|Grep" or "Grep" matcher
        // is upgraded to "Read|Grep|Glob" with smart auto-mode compression.
        let mut v = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    { "matcher": "Grep", "command": "/old/bin hook redirect" }
                ]
            }
        });

        merge_cursor_hooks(&mut v, "/new/bin hook rewrite", "/new/bin hook redirect");

        let pre = v
            .pointer("/hooks/preToolUse")
            .and_then(|x| x.as_array())
            .unwrap();
        let redirects: Vec<_> = pre
            .iter()
            .filter(|e| e.get("command").and_then(|c| c.as_str()) == Some("/new/bin hook redirect"))
            .collect();
        assert_eq!(redirects.len(), 1, "must migrate in place, not duplicate");
        assert_eq!(
            redirects[0].get("matcher").and_then(|m| m.as_str()),
            Some("Read|Grep|Glob"),
            "matcher must be Read|Grep|Glob (smart redirect for all)"
        );
    }

    #[test]
    fn replace_mode_redirects_read_and_denies_grep_glob() {
        let mut v = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    { "matcher": "Shell", "command": "/bin/lean-ctx hook rewrite" },
                    { "matcher": "Read|Grep", "command": "/bin/lean-ctx hook redirect" }
                ]
            }
        });

        let deny_cmd = "/bin/lean-ctx hook deny";
        let redirect_cmd = "/bin/lean-ctx hook redirect";
        let pre = v
            .pointer_mut("/hooks/preToolUse")
            .and_then(|x| x.as_array_mut())
            .unwrap();

        // Read: redirect with smart compression (StrReplace doesn't fire Read PreToolUse)
        ensure_pretooluse_hook(pre, &["Read"], "Read", redirect_cmd);
        // Grep|Glob: deny
        ensure_pretooluse_hook(
            pre,
            &["Read|Grep|Glob", "Read|Grep", "Grep", "Grep|Glob"],
            "Grep|Glob",
            deny_cmd,
        );

        let pre = v
            .pointer("/hooks/preToolUse")
            .and_then(|x| x.as_array())
            .unwrap();

        // Read should be redirected (smart compression via auto mode)
        assert!(pre.iter().any(|e| {
            e.get("matcher").and_then(|m| m.as_str()) == Some("Read")
                && e.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains("redirect"))
        }));
        // Grep|Glob should use deny
        assert!(pre.iter().any(|e| {
            e.get("matcher").and_then(|m| m.as_str()) == Some("Grep|Glob")
                && e.get("command").and_then(|c| c.as_str()) == Some("/bin/lean-ctx hook deny")
        }));
        // Old combined Read|Grep should be gone
        assert!(!pre.iter().any(|e| {
            e.get("matcher")
                .and_then(|m| m.as_str())
                .is_some_and(|m| m == "Read|Grep")
        }));
    }

    #[test]
    fn replace_mode_registers_write_tool_deny_guard() {
        // #1302/#1323: StrReplace/Write/Edit must be hooked for compression guard
        let mut v = serde_json::json!({
            "version": 1,
            "hooks": { "preToolUse": [] }
        });

        let deny_cmd = "/bin/lean-ctx hook deny";
        let redirect_cmd = "/bin/lean-ctx hook redirect";
        let pre = v
            .pointer_mut("/hooks/preToolUse")
            .and_then(|x| x.as_array_mut())
            .unwrap();

        ensure_pretooluse_hook(pre, &["Read"], "Read", redirect_cmd);
        ensure_pretooluse_hook(
            pre,
            &["Read|Grep|Glob", "Read|Grep", "Grep", "Grep|Glob"],
            "Grep|Glob",
            deny_cmd,
        );
        ensure_pretooluse_hook(
            pre,
            &["StrReplace|Write|Edit|EditNotebook|MultiEdit"],
            "StrReplace|Write|Edit|EditNotebook|MultiEdit",
            deny_cmd,
        );

        let pre = v
            .pointer("/hooks/preToolUse")
            .and_then(|x| x.as_array())
            .unwrap();

        assert!(
            pre.iter().any(|e| {
                e.get("matcher")
                    .and_then(|m| m.as_str())
                    .is_some_and(|m| m.contains("StrReplace"))
                    && e.get("command").and_then(|c| c.as_str()) == Some(deny_cmd)
            }),
            "write tools must be registered on hook deny"
        );
    }
}
