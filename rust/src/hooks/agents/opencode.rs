use super::super::{HookMode, mcp_server_quiet_mode, resolve_binary_path};
use crate::core::config::{Config, RulesInjection, RulesScope};

/// The JSON document a config update starts from (#1586).
///
/// `Err` means "leave the file alone": an existing config we cannot parse is
/// never replaced with a fresh lean-ctx-only scaffold. Starting from an empty
/// object there wiped providers, models, plugins and foreign MCP entries — and
/// the command still reported success. Only a genuinely absent file may start
/// empty.
fn starting_document(file_existed: bool, content: &str) -> Result<serde_json::Value, String> {
    match crate::core::jsonc::parse_jsonc(content) {
        Ok(v) => Ok(v),
        Err(e) if file_existed => Err(e.to_string()),
        Err(_) => Ok(serde_json::Value::Object(serde_json::Map::new())),
    }
}

pub(crate) fn install_opencode_hook_with_mode(mode: HookMode) {
    let binary = resolve_binary_path();
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    // #1585: write into the user's existing opencode.jsonc when that is the file
    // they actually have, rather than creating a competing opencode.json.
    let resolved = crate::core::opencode_config::resolve(&home);
    let display_path = resolved.display(&home);
    let display_path = display_path.as_str();
    let config_path = resolved.path;

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let desired = serde_json::json!({
        "type": "local",
        "command": [&binary],
        "enabled": true,
        "environment": super::super::mcp_server_env_json()
    });

    // #313: `shadow_mode` (default off) controls whether native tools (read,
    // grep, glob, bash) are set to "ask" at the permission level in opencode.json,
    // steering the agent to ctx_* equivalents via the MCP server without
    // removing the tools from the request (#1864).
    // In Replace mode, shadow permissions are always applied regardless of config.
    let cfg = Config::load();
    let shadow = cfg.shadow_mode || mode == HookMode::Replace;

    let should_reg_mcp = super::super::should_register_mcp();
    let mcp_needed =
        should_reg_mcp && matches!(mode, HookMode::Mcp | HookMode::Hybrid | HookMode::Replace);

    let file_existed = config_path.exists();
    let content = if file_existed {
        std::fs::read_to_string(&config_path).unwrap_or_default()
    } else {
        String::new()
    };
    let has_lean_ctx = content.contains("lean-ctx");

    let mut json = match starting_document(file_existed, &content) {
        Ok(v) => v,
        Err(e) => {
            if !mcp_server_quiet_mode() {
                eprintln!(
                    "  \x1b[33m!\x1b[0m OpenCode config at {display_path} could not be parsed ({e}) — left untouched. Fix the file, then re-run."
                );
            }
            return;
        }
    };
    let Some(obj) = json.as_object_mut() else {
        return;
    };

    // 1. Apply or remove shadow permissions
    let perm_changed = if shadow {
        apply_shadow_permissions_inplace(obj)
    } else {
        remove_shadow_permissions_inplace(obj)
    };

    // 2. Register MCP server (if needed and not already present)
    let mcp_written = if mcp_needed && !has_lean_ctx {
        if !file_existed {
            obj.insert(
                "$schema".to_string(),
                serde_json::json!("https://opencode.ai/config.json"),
            );
        }
        let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
        if let Some(mcp_obj) = mcp.as_object_mut() {
            mcp_obj.insert("lean-ctx".to_string(), desired);
            true
        } else {
            false
        }
    } else {
        false
    };

    // 3. Single write if anything changed
    if perm_changed || mcp_written {
        if file_existed {
            let backup = config_path.with_extension("json.bak");
            let _ = std::fs::copy(&config_path, &backup);
        }
        if let Ok(formatted) = serde_json::to_string_pretty(&json) {
            let _ = std::fs::write(&config_path, formatted);
            if !mcp_server_quiet_mode() {
                if perm_changed && shadow {
                    eprintln!(
                        "  \x1b[32m✓\x1b[0m Shadow mode: native tools set to ask at {display_path}"
                    );
                } else if perm_changed {
                    eprintln!(
                        "  \x1b[32m✓\x1b[0m Shadow mode: native tool permissions restored at {display_path}"
                    );
                }
                if mcp_written {
                    eprintln!("  \x1b[32m✓\x1b[0m OpenCode MCP configured at {display_path}");
                }
            }
        }
    } else if has_lean_ctx && !mcp_server_quiet_mode() {
        eprintln!("OpenCode MCP already configured at {display_path}");
    }

    // #442: inject the "prefer ctx_*" rules block so the agent knows to use
    // lean-ctx tools. In shadow mode, native tools require approval — the agent
    // must use ctx_* tools, so rules are even more important.
    if super::super::should_register_mcp() && cfg.setup.auto_inject_rules != Some(false) {
        let _ = crate::rules_inject::inject_rules_for_agent(&home, "OpenCode");
    }

    // Dedicated rules-injection mode (#343): register the lean-ctx-owned rules
    // file via opencode.json `instructions[]` (absolute path — OpenCode resolves
    // relative entries against the CWD, not the config dir) and strip any block a
    // prior shared install left in the global AGENTS.md. The rules file itself is
    // written by rules_inject. Shared mode (default) reverses the registration.
    // Gate on should_register_mcp() so auto_update_mcp=false prevents ALL
    // opencode.json modifications (#sb-feedback).
    let dedicated_global = should_reg_mcp
        && cfg.rules_injection_effective() == RulesInjection::Dedicated
        && cfg.rules_scope_effective() != RulesScope::Project;
    if dedicated_global {
        register_opencode_instructions(&home);
        strip_opencode_agents_block(&home);
    } else {
        unregister_opencode_instructions(&home);
    }
}

fn opencode_config_path(home: &std::path::Path) -> std::path::PathBuf {
    // #1585: resolve .json/.jsonc instead of hardcoding one name, so
    // registration and unregistration touch the same file the installer wrote.
    crate::core::opencode_config::resolve(home).path
}

/// Add the dedicated rules file to opencode.json `instructions[]` (idempotent).
fn register_opencode_instructions(home: &std::path::Path) {
    let config_path = opencode_config_path(home);
    let rules_str = crate::rules_inject::opencode_dedicated_rules_path(home)
        .to_string_lossy()
        .into_owned();

    let mut json = match std::fs::read_to_string(&config_path) {
        // #1586: same rule as the installer — an existing config we cannot parse
        // is left alone, never overwritten with a fresh scaffold.
        Ok(content) => match starting_document(true, &content) {
            Ok(v) => v,
            Err(e) => {
                if !mcp_server_quiet_mode() {
                    eprintln!(
                        "  \x1b[33m!\x1b[0m OpenCode config at {} could not be parsed ({e}) — instructions[] not registered.",
                        config_path.display()
                    );
                }
                return;
            }
        },
        Err(_) => serde_json::json!({ "$schema": "https://opencode.ai/config.json" }),
    };

    let Some(obj) = json.as_object_mut() else {
        return;
    };
    let instr = obj
        .entry("instructions".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !instr.is_array() {
        *instr = serde_json::json!([]);
    }
    let arr = instr.as_array_mut().expect("instructions is an array");
    if arr.iter().any(|v| v.as_str() == Some(rules_str.as_str())) {
        return;
    }
    arr.push(serde_json::Value::String(rules_str));

    if let (Some(parent), Ok(formatted)) =
        (config_path.parent(), serde_json::to_string_pretty(&json))
    {
        let _ = std::fs::create_dir_all(parent);
        let _ = std::fs::write(&config_path, formatted);
        if !mcp_server_quiet_mode() {
            eprintln!(
                "  \x1b[32m✓\x1b[0m OpenCode rules registered in opencode.json instructions[]"
            );
        }
    }
}

/// Remove the lean-ctx `instructions[]` entry from opencode.json. Used for
/// shared-mode toggle-back and uninstall cleanup.
pub(crate) fn unregister_opencode_instructions(home: &std::path::Path) {
    let config_path = opencode_config_path(home);
    let Ok(content) = std::fs::read_to_string(&config_path) else {
        return;
    };
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return;
    };
    let Some(obj) = json.as_object_mut() else {
        return;
    };
    let Some(arr) = obj.get_mut("instructions").and_then(|v| v.as_array_mut()) else {
        return;
    };
    let rules_str = crate::rules_inject::opencode_dedicated_rules_path(home)
        .to_string_lossy()
        .into_owned();
    let before = arr.len();
    arr.retain(|v| v.as_str() != Some(rules_str.as_str()));
    if arr.len() == before {
        return;
    }
    if arr.is_empty() {
        obj.remove("instructions");
    }
    if let Ok(formatted) = serde_json::to_string_pretty(&json) {
        let _ = std::fs::write(&config_path, formatted);
    }
}

/// Strip the lean-ctx block from the global OpenCode AGENTS.md (dedicated mode).
fn strip_opencode_agents_block(home: &std::path::Path) {
    let agents = home.join(".config/opencode/AGENTS.md");
    if let Ok(meta) = agents.metadata()
        && meta.is_file()
        && let Ok(content) = std::fs::read_to_string(&agents)
        && content.contains(crate::core::rules_canonical::START_MARK)
    {
        crate::marked_block::remove_from_file(
            &agents,
            crate::core::rules_canonical::START_MARK,
            crate::core::rules_canonical::END_MARK,
            true,
            "OpenCode AGENTS.md lean-ctx block",
        );
    }
}

/// Native tools that shadow mode steers away from via the opencode.json
/// `permission` object.
const SHADOW_DENIED_TOOLS: &[&str] = &["read", "grep", "glob", "bash"];

/// `ask`, not `deny`: opencode drops a denied tool from the request, and
/// OpenCode Zen's free tier rejects any request whose tool list lacks `bash` or
/// `read` (#1864). An asked tool stays declared and still needs the user's
/// approval, so the agent keeps reaching for the ctx_* tools.
const SHADOW_ACTION: &str = "ask";

/// Values shadow mode has written: the current one, and `deny` from releases
/// before #1864.
fn is_shadow_value(v: Option<&serde_json::Value>) -> bool {
    matches!(v.and_then(|v| v.as_str()), Some("ask" | "deny"))
}

/// Apply the shadow permissions in-place on a JSON object.
/// Returns true if any changes were made.
fn apply_shadow_permissions_inplace(obj: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    let perms = obj
        .entry("permission".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(perms_obj) = perms.as_object_mut() else {
        return false;
    };

    let mut changed = false;
    for &tool in SHADOW_DENIED_TOOLS {
        if perms_obj.get(tool).and_then(|v| v.as_str()) != Some(SHADOW_ACTION) {
            perms_obj.insert(tool.to_string(), serde_json::json!(SHADOW_ACTION));
            changed = true;
        }
    }
    changed
}

/// Apply permission denies for native tools in opencode.json — forces the
/// agent to use ctx_* equivalents from the MCP server. Always overwrites
/// regardless of any user-set permission values.
#[cfg(test)]
fn apply_shadow_permissions(config_path: &std::path::Path, display_path: &str) {
    let content = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut json = crate::core::jsonc::parse_jsonc(&content)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
    let Some(obj) = json.as_object_mut() else {
        return;
    };

    if apply_shadow_permissions_inplace(obj)
        && let Ok(formatted) = serde_json::to_string_pretty(&json)
    {
        let _ = std::fs::write(config_path, formatted);
        if !mcp_server_quiet_mode() {
            eprintln!("  \x1b[32m✓\x1b[0m Shadow mode: native tools set to ask at {display_path}");
        }
    }
}

/// Remove shadow-mode permission denies in-place on a JSON object.
/// Returns true if any changes were made.
fn remove_shadow_permissions_inplace(obj: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    let Some(perms) = obj.get_mut("permission").and_then(|p| p.as_object_mut()) else {
        return false;
    };

    let mut changed = false;
    for &tool in SHADOW_DENIED_TOOLS {
        if is_shadow_value(perms.get(tool)) {
            perms.remove(tool);
            changed = true;
        }
    }

    if changed && perms.is_empty() {
        obj.remove("permission");
    }

    changed
}

/// Remove shadow-mode permission denies from opencode.json. Only removes
/// entries WE set — tools with a shadow value ("ask", or legacy "deny") in our list.
/// Leaves other permission entries and other values for these tools untouched.
#[cfg(test)]
fn remove_shadow_permissions(config_path: &std::path::Path, display_path: &str) {
    let Ok(content) = std::fs::read_to_string(config_path) else {
        return;
    };
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return;
    };
    let Some(obj) = json.as_object_mut() else {
        return;
    };

    if remove_shadow_permissions_inplace(obj)
        && let Ok(formatted) = serde_json::to_string_pretty(&json)
    {
        let _ = std::fs::write(config_path, formatted);
        if !mcp_server_quiet_mode() {
            eprintln!(
                "  \x1b[32m✓\x1b[0m Shadow mode: native tool permissions restored at {display_path}"
            );
        }
    }
}

#[cfg(test)]
mod dedicated_tests {
    use super::*;

    fn temp_home(tag: &str) -> std::path::PathBuf {
        let home =
            std::env::temp_dir().join(format!("leanctx_opencode_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
        home
    }

    fn read_instructions(home: &std::path::Path) -> Vec<String> {
        let content = std::fs::read_to_string(opencode_config_path(home)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        json["instructions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn register_adds_absolute_dedicated_path() {
        let home = temp_home("add");
        register_opencode_instructions(&home);
        let expected = crate::rules_inject::opencode_dedicated_rules_path(&home)
            .to_string_lossy()
            .into_owned();
        assert_eq!(read_instructions(&home), vec![expected]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn register_is_idempotent() {
        let home = temp_home("idem");
        register_opencode_instructions(&home);
        register_opencode_instructions(&home);
        assert_eq!(read_instructions(&home).len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn register_preserves_user_instructions() {
        let home = temp_home("preserve");
        std::fs::write(
            opencode_config_path(&home),
            r#"{"instructions":["./CONTRIBUTING.md"]}"#,
        )
        .unwrap();
        register_opencode_instructions(&home);
        let instrs = read_instructions(&home);
        assert!(instrs.contains(&"./CONTRIBUTING.md".to_string()));
        assert_eq!(instrs.len(), 2);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unregister_removes_only_our_entry() {
        let home = temp_home("unreg");
        std::fs::write(
            opencode_config_path(&home),
            r#"{"instructions":["./CONTRIBUTING.md"]}"#,
        )
        .unwrap();
        register_opencode_instructions(&home);
        unregister_opencode_instructions(&home);
        assert_eq!(read_instructions(&home), vec!["./CONTRIBUTING.md"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unregister_drops_empty_instructions_key() {
        let home = temp_home("empty");
        register_opencode_instructions(&home);
        unregister_opencode_instructions(&home);
        let content = std::fs::read_to_string(opencode_config_path(&home)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json.get("instructions").is_none(), "got: {content}");
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[cfg(test)]
mod shadow_permission_tests {
    use super::*;

    fn temp_home(tag: &str) -> std::path::PathBuf {
        let home =
            std::env::temp_dir().join(format!("leanctx_shadow_perm_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
        home
    }

    /// Helper: create a temp dir with an opencode.json config file.
    /// Each test MUST use a unique tag to avoid parallel-execution races.
    fn temp_cfg_with_tag(
        tag: &str,
        initial_content: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = temp_home(tag);
        let cfg = dir.join(".config/opencode/opencode.json");
        if !initial_content.is_empty() {
            std::fs::write(&cfg, initial_content).unwrap();
        }
        (dir, cfg)
    }

    fn read_json(cfg: &std::path::Path) -> serde_json::Value {
        let content = std::fs::read_to_string(cfg).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    // --- apply_shadow_permissions ---

    #[test]
    fn apply_adds_ask_for_all_tools() {
        let (_dir, cfg) = temp_cfg_with_tag("apply_adds", r#"{"mcp":{"other":{"type":"local"}}}"#);
        apply_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        let perms = json["permission"].as_object().unwrap();
        for tool in &["read", "grep", "glob", "bash"] {
            assert_eq!(perms[*tool], "ask", "{tool} should be ask");
        }
        assert_eq!(
            json["mcp"]["other"]["type"], "local",
            "other keys preserved"
        );
    }

    #[test]
    fn apply_creates_permission_when_missing() {
        let (_dir, cfg) = temp_cfg_with_tag("create_perm", r#"{"mcp":{"other":{"type":"local"}}}"#);
        apply_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        assert!(json.get("permission").is_some(), "permission key created");
    }

    #[test]
    fn apply_overwrites_user_allow() {
        let (_dir, cfg) = temp_cfg_with_tag(
            "overwrite_allow",
            r#"{"permission":{"read":"allow","edit":"allow"}}"#,
        );
        apply_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        assert_eq!(json["permission"]["read"], "ask", "user allow overwritten");
        assert_eq!(
            json["permission"]["edit"], "allow",
            "non-shadow tool preserved"
        );
    }

    #[test]
    fn apply_is_idempotent() {
        let (_dir, cfg) = temp_cfg_with_tag("apply_idem", r"{}");
        apply_shadow_permissions(&cfg, "test");
        let first = read_json(&cfg);
        apply_shadow_permissions(&cfg, "test");
        let second = read_json(&cfg);
        assert_eq!(first, second, "second apply should not change output");
    }

    #[test]
    fn apply_handles_missing_file() {
        let (_dir, cfg) = temp_cfg_with_tag("missing_file", ""); // file doesn't exist yet
        apply_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        let perms = json["permission"].as_object().unwrap();
        for tool in &["read", "grep", "glob", "bash"] {
            assert_eq!(perms[*tool], "ask", "{tool} should be ask in new file");
        }
    }

    #[test]
    fn apply_handles_corrupt_json() {
        let (_dir, cfg) = temp_cfg_with_tag("corrupt", "{corrupt json!!}");
        apply_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        let perms = json["permission"].as_object().unwrap();
        for tool in &["read", "grep", "glob", "bash"] {
            assert_eq!(
                perms[*tool], "ask",
                "{tool} should be ask after corrupt apply"
            );
        }
    }

    // --- remove_shadow_permissions ---

    #[test]
    fn remove_clears_our_deny_entries() {
        let (_dir, cfg) = temp_cfg_with_tag(
            "rm_clears",
            r#"{"permission":{"read":"deny","grep":"deny","glob":"deny","bash":"deny","edit":"allow"}}"#,
        );
        remove_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        let perms = json["permission"].as_object().unwrap();
        for tool in &["read", "grep", "glob", "bash"] {
            assert!(perms.get(*tool).is_none(), "{tool} should be removed");
        }
        assert_eq!(perms["edit"], "allow", "non-shadow tool preserved");
    }

    #[test]
    fn remove_drops_empty_permission_object() {
        let (_dir, cfg) = temp_cfg_with_tag(
            "rm_drops",
            r#"{"mcp":{"other":{"type":"local"}},"permission":{"read":"deny","grep":"deny","glob":"deny","bash":"deny"}}"#,
        );
        remove_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        assert!(
            json.get("permission").is_none(),
            "empty permission should be dropped"
        );
        assert_eq!(
            json["mcp"]["other"]["type"], "local",
            "other keys preserved"
        );
    }

    #[test]
    fn remove_preserves_user_allow_values() {
        let (_dir, cfg) = temp_cfg_with_tag(
            "rm_preserve",
            r#"{"permission":{"read":"allow","bash":"allow","edit":"deny"}}"#,
        );
        remove_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        assert_eq!(json["permission"]["read"], "allow", "user allow preserved");
        assert_eq!(json["permission"]["bash"], "allow", "user allow preserved");
        assert_eq!(
            json["permission"]["edit"], "deny",
            "non-shadow deny preserved"
        );
    }

    #[test]
    fn remove_is_idempotent() {
        let (_dir, cfg) = temp_cfg_with_tag(
            "rm_idem",
            r#"{"permission":{"read":"deny","grep":"deny","glob":"deny","bash":"deny"}}"#,
        );
        remove_shadow_permissions(&cfg, "test");
        let after_first = read_json(&cfg);
        remove_shadow_permissions(&cfg, "test");
        let after_second = read_json(&cfg);
        assert_eq!(
            after_first, after_second,
            "second remove should not change output"
        );
    }

    #[test]
    fn remove_noop_when_no_permission_key() {
        let (_dir, cfg) = temp_cfg_with_tag("rm_noop", r#"{"mcp":{"other":{"type":"local"}}}"#);
        let before = read_json(&cfg);
        remove_shadow_permissions(&cfg, "test");
        let after = read_json(&cfg);
        assert_eq!(before, after, "noop when no permission key");
    }

    #[test]
    fn remove_noop_when_file_missing() {
        let (_dir, cfg) = temp_cfg_with_tag("rm_noop_file", ""); // no file
        remove_shadow_permissions(&cfg, "test"); // must not panic
        assert!(!cfg.exists(), "file should not be created");
    }

    #[test]
    fn remove_noop_on_corrupt_json() {
        let (_dir, cfg) = temp_cfg_with_tag("rm_corrupt", "{corrupt json!!}");
        let before = std::fs::read_to_string(&cfg).unwrap();
        remove_shadow_permissions(&cfg, "test");
        let after = std::fs::read_to_string(&cfg).unwrap();
        assert_eq!(before, after, "corrupt file left unchanged");
    }

    // --- #1864: legacy "deny" written by older releases ---

    #[test]
    fn apply_migrates_legacy_deny_to_ask() {
        let (_dir, cfg) = temp_cfg_with_tag(
            "legacy_migrate",
            r#"{"permission":{"read":"deny","grep":"deny","glob":"deny","bash":"deny","edit":"deny"}}"#,
        );
        apply_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        let perms = json["permission"].as_object().unwrap();
        for tool in &["read", "grep", "glob", "bash"] {
            assert_eq!(perms[*tool], "ask", "{tool} must no longer be denied");
        }
        assert_eq!(perms["edit"], "deny", "non-shadow tool untouched");
    }

    #[test]
    fn remove_clears_current_ask_entries() {
        let (_dir, cfg) = temp_cfg_with_tag(
            "rm_ask",
            r#"{"permission":{"read":"ask","grep":"ask","glob":"ask","bash":"ask","edit":"allow"}}"#,
        );
        remove_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        let perms = json["permission"].as_object().unwrap();
        for tool in &["read", "grep", "glob", "bash"] {
            assert!(perms.get(*tool).is_none(), "{tool} should be removed");
        }
        assert_eq!(perms["edit"], "allow");
    }

    // --- State transitions ---

    #[test]
    fn apply_then_remove_restores_permission() {
        let (_dir, cfg) = temp_cfg_with_tag("apply_rm", r#"{"permission":{"edit":"allow"}}"#);
        apply_shadow_permissions(&cfg, "test");
        remove_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        let perms = json["permission"].as_object().unwrap();
        assert_eq!(perms["edit"], "allow", "edit preserved");
        for tool in &["read", "grep", "glob", "bash"] {
            assert!(perms.get(*tool).is_none(), "{tool} should be gone");
        }
    }

    #[test]
    fn remove_then_apply_adds_asks() {
        let (_dir, cfg) = temp_cfg_with_tag("rm_then_apply", r"{}");
        remove_shadow_permissions(&cfg, "test");
        apply_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        let perms = json["permission"].as_object().unwrap();
        for tool in &["read", "grep", "glob", "bash"] {
            assert_eq!(perms[*tool], "ask", "{tool} should be ask");
        }
    }

    // --- Node: confirm opencode.json uses "permission" not "permissions" ---
    #[test]
    fn permission_key_is_singular() {
        let (_dir, cfg) = temp_cfg_with_tag("key_singular", r"{}");
        apply_shadow_permissions(&cfg, "test");
        let json = read_json(&cfg);
        assert!(
            json.get("permission").is_some(),
            "key should be 'permission' (singular)"
        );
        assert!(
            json.get("permissions").is_none(),
            "'permissions' (plural) should not exist"
        );
    }
}

/// #1585 / #1586: the OpenCode global config must survive a lean-ctx install.
#[cfg(test)]
mod config_preservation_tests {
    use super::*;

    const REAL_WORLD: &str = r#"{
  // my setup
  "$schema": "https://opencode.ai/config.json",
  "model": "anthropic/claude-opus-4",
  "provider": {
    "anthropic": { "options": { "apiKey": "{env:ANTHROPIC_API_KEY}" } }
  },
  "plugin": ["my-plugin"],
  "permission": { "bash": "ask" },
  "mcp": {
    "other-tool": { "type": "local", "command": ["other"], "enabled": true }
  },
}"#;

    #[test]
    fn unparseable_existing_config_is_never_replaced_by_a_scaffold() {
        // The #1586 regression: this used to yield an empty object, which the
        // installer then wrote back over the user's file.
        let broken = r#"{ "model": "x" "provider": {} }"#;
        let err = starting_document(true, broken)
            .expect_err("a config we cannot parse must abort the write, not reset it");
        assert!(!err.is_empty(), "the user needs to know what was wrong");
    }

    #[test]
    fn absent_config_may_start_from_an_empty_object() {
        let v = starting_document(false, "").expect("a missing file is created from scratch");
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_still_parse() {
        let v = starting_document(true, REAL_WORLD).expect("JSONC is valid OpenCode config");
        assert_eq!(v["model"], "anthropic/claude-opus-4");
        assert!(v["mcp"]["other-tool"]["enabled"].as_bool().unwrap());
    }

    #[test]
    fn bom_prefixed_config_parses_instead_of_being_reset() {
        // Windows editors emit a UTF-8 BOM. serde_json rejects it, and that
        // rejection used to be read as "corrupt, scaffold a fresh one".
        let with_bom = format!("\u{feff}{REAL_WORLD}");
        let v = starting_document(true, &with_bom)
            .expect("a BOM is an encoding artefact, not a corrupt config");
        assert_eq!(v["model"], "anthropic/claude-opus-4");
        assert_eq!(v["plugin"][0], "my-plugin");
    }

    #[test]
    fn user_content_survives_the_lean_ctx_mcp_insertion() {
        let mut v = starting_document(true, REAL_WORLD).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.entry("mcp")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .unwrap()
            .insert("lean-ctx".to_string(), serde_json::json!({"enabled": true}));

        assert_eq!(v["model"], "anthropic/claude-opus-4");
        assert!(v["provider"]["anthropic"]["options"]["apiKey"].is_string());
        assert_eq!(v["plugin"][0], "my-plugin");
        assert_eq!(v["permission"]["bash"], "ask");
        assert!(
            v["mcp"]["other-tool"]["enabled"].as_bool().unwrap(),
            "a foreign MCP server must not be dropped"
        );
        assert!(v["mcp"]["lean-ctx"]["enabled"].as_bool().unwrap());
    }

    #[test]
    fn config_path_follows_the_users_jsonc_file() {
        // #1585: with only opencode.jsonc present we must not create a second,
        // competing opencode.json.
        let dir = tempfile::tempdir().unwrap();
        let cfg_dir = dir.path().join(".config/opencode");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("opencode.jsonc"), REAL_WORLD).unwrap();

        let resolved = crate::core::opencode_config::resolve_in_dir(&cfg_dir);
        assert_eq!(resolved.path, cfg_dir.join("opencode.jsonc"));
        assert!(!cfg_dir.join("opencode.json").exists());
    }
}
