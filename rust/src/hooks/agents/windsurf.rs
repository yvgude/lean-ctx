use super::super::{
    install_mcp_json_agent, mcp_server_quiet_mode, resolve_hook_command_binary, write_file,
};
use super::shared::{
    prepare_project_rules_path, read_agent_json_config, report_unreadable_agent_config,
};

pub(crate) fn install_windsurf_rules(global: bool) {
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();

    // hooks.json + MCP config are always global (they live in ~/.codeium/windsurf/)
    if global {
        let config_path = home
            .join(".codeium")
            .join("windsurf")
            .join("mcp_config.json");
        install_mcp_json_agent(
            "Windsurf",
            "~/.codeium/windsurf/mcp_config.json",
            &config_path,
        );
    }
    install_windsurf_hooks(&home);

    let Some(rules_path) = prepare_project_rules_path(global, ".windsurfrules") else {
        return;
    };

    let rules = include_str!("../../templates/windsurfrules.txt");
    write_file(&rules_path, rules);
    if !mcp_server_quiet_mode() {
        eprintln!("Installed .windsurfrules in current project.");
    }
}

pub(crate) fn install_windsurf_hooks(home: &std::path::Path) {
    let hooks_json = home.join(".codeium").join("windsurf").join("hooks.json");
    let binary = resolve_hook_command_binary();
    let observe_cmd = format!("{binary} hook observe");
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let mut root = match read_agent_json_config(&hooks_json, "windsurf") {
        Ok(root) => root,
        Err(error) => return report_unreadable_agent_config("Windsurf", &error),
    };

    if !root.is_object() {
        root = serde_json::json!({});
    }

    let Some(root_obj) = root.as_object_mut() else {
        return;
    };

    let hooks = root_obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return;
    };

    ensure_windsurf_hook_entry(hooks_obj, "pre_mcp_tool_use", &rewrite_cmd, "hook rewrite");
    ensure_windsurf_hook_entry(
        hooks_obj,
        "pre_mcp_tool_use",
        &redirect_cmd,
        "hook redirect",
    );

    let observe_events = [
        "post_mcp_tool_use",
        "post_run_command",
        "post_cascade_response",
        "pre_user_prompt",
    ];

    for event in observe_events {
        ensure_windsurf_hook_entry(hooks_obj, event, &observe_cmd, "hook observe");
    }

    let formatted = serde_json::to_string_pretty(&root).unwrap_or_default();
    let _ = std::fs::create_dir_all(hooks_json.parent().unwrap_or(home));
    write_file(&hooks_json, &formatted);

    if !mcp_server_quiet_mode() {
        eprintln!("Installed Windsurf hooks at {}", hooks_json.display());
    }
}

fn ensure_windsurf_hook_entry(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    event: &str,
    command: &str,
    marker: &str,
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
            .is_some_and(|c| c.contains(marker))
    });
    if !already {
        entries.push(serde_json::json!({ "command": command }));
    }
}

/// Install deny hook for Windsurf in Replace mode. Adds a `hook deny` entry
/// to `pre_mcp_tool_use` that blocks native Read/Grep/Shell.
pub(crate) fn install_windsurf_deny_hook(home: &std::path::Path) {
    let hooks_json = home.join(".codeium").join("windsurf").join("hooks.json");
    let binary = resolve_hook_command_binary();
    let deny_cmd = format!("{binary} hook deny");

    let mut root = match read_agent_json_config(&hooks_json, "windsurf") {
        Ok(root) => root,
        Err(error) => return report_unreadable_agent_config("Windsurf", &error),
    };

    if !root.is_object() {
        root = serde_json::json!({});
    }

    let Some(root_obj) = root.as_object_mut() else {
        return;
    };

    let hooks = root_obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return;
    };

    // Replace the redirect hook with deny
    let entries = hooks_obj
        .entry("pre_mcp_tool_use".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if let Some(arr) = entries.as_array_mut() {
        arr.retain(|e| {
            !e.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.contains("hook redirect"))
        });
        let has_deny = arr.iter().any(|e| {
            e.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.contains("hook deny"))
        });
        if !has_deny {
            arr.push(serde_json::json!({ "command": deny_cmd }));
        }
    }

    let formatted = serde_json::to_string_pretty(&root).unwrap_or_default();
    let _ = std::fs::create_dir_all(hooks_json.parent().unwrap_or(home));
    write_file(&hooks_json, &formatted);

    if !mcp_server_quiet_mode() {
        eprintln!("  \x1b[32m✓\x1b[0m Windsurf deny hook installed (Replace mode)");
    }
}

/// Called by the hook mode dispatcher for Windsurf in Replace mode.
pub(crate) fn install_windsurf_hooks_replace(home: &std::path::Path) {
    install_windsurf_hooks(home);
    install_windsurf_deny_hook(home);
}

#[cfg(test)]
mod tests {
    use super::*;

    // An unparseable hooks.json was replaced with `{}` plus the lean-ctx
    // hooks, dropping the user's own hooks. Both writers now refuse it and
    // leave it byte-identical.
    #[test]
    fn unparseable_hooks_json_is_refused_and_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks_json = tmp
            .path()
            .join(".codeium")
            .join("windsurf")
            .join("hooks.json");
        std::fs::create_dir_all(hooks_json.parent().unwrap()).unwrap();
        let broken = "{\"hooks\": {\"pre_user_prompt\": [\n";
        std::fs::write(&hooks_json, broken).unwrap();

        install_windsurf_hooks_replace(tmp.path());

        assert_eq!(std::fs::read_to_string(&hooks_json).unwrap(), broken);
        let error = read_agent_json_config(&hooks_json, "windsurf").unwrap_err();
        assert!(error.contains("hooks.json is not valid JSON"), "{error}");
        assert!(error.contains("lean-ctx init --agent windsurf"), "{error}");
    }
}
