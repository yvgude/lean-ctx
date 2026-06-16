use super::super::{mcp_server_quiet_mode, resolve_binary_path, write_file};
use super::shared::install_standard_hook_scripts;
use crate::core::config::{Config, RulesInjection, RulesScope};

pub(crate) fn install_gemini_hook() {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_gemini_hook_scripts(&home);
    install_gemini_hook_config(&home);

    // Dedicated rules-injection mode (#343): register the lean-ctx-owned rules
    // file via .gemini/settings.json `context.fileName` (Gemini discovers context
    // files by name; the file itself is written by rules_inject) and strip any
    // block a prior shared install left in GEMINI.md. Shared mode reverses it.
    let cfg = Config::load();
    let dedicated_global = cfg.rules_injection_effective() == RulesInjection::Dedicated
        && cfg.rules_scope_effective() != RulesScope::Project;
    if dedicated_global {
        register_gemini_context_filename(&home);
        strip_gemini_md_block(&home);
    } else {
        unregister_gemini_context_filename(&home);
    }
}

fn gemini_settings_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".gemini").join("settings.json")
}

fn read_gemini_settings(home: &std::path::Path) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(gemini_settings_path(home)).ok()?;
    crate::core::jsonc::parse_jsonc(&content).ok()
}

/// Normalize `context.fileName` to a string array and return a mutable handle.
/// Seeds the default `["GEMINI.md"]` when absent so the user's GEMINI.md keeps
/// being discovered after we add our entry.
fn context_filename_array(root: &mut serde_json::Value) -> Option<&mut Vec<serde_json::Value>> {
    let obj = root.as_object_mut()?;
    let context = obj
        .entry("context".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !context.is_object() {
        *context = serde_json::json!({});
    }
    let ctx_obj = context.as_object_mut()?;
    let entry = ctx_obj
        .entry("fileName".to_string())
        .or_insert_with(|| serde_json::json!(["GEMINI.md"]));
    match entry {
        serde_json::Value::String(s) => {
            *entry = serde_json::json!([s.clone()]);
        }
        serde_json::Value::Array(_) => {}
        _ => *entry = serde_json::json!(["GEMINI.md"]),
    }
    entry.as_array_mut()
}

/// Add `LEANCTX.md` to `context.fileName` (idempotent), preserving GEMINI.md.
fn register_gemini_context_filename(home: &std::path::Path) {
    let name = crate::rules_inject::GEMINI_DEDICATED_CONTEXT_FILENAME;
    let mut json = read_gemini_settings(home).unwrap_or_else(|| serde_json::json!({}));
    let Some(arr) = context_filename_array(&mut json) else {
        return;
    };
    if arr.iter().any(|v| v.as_str() == Some(name)) {
        return;
    }
    arr.push(serde_json::Value::String(name.to_string()));

    let path = gemini_settings_path(home);
    if let (Some(parent), Ok(formatted)) = (path.parent(), serde_json::to_string_pretty(&json)) {
        let _ = std::fs::create_dir_all(parent);
        write_file(&path, &formatted);
        if !mcp_server_quiet_mode() {
            eprintln!(
                "  \x1b[32m✓\x1b[0m Gemini rules registered in settings.json context.fileName"
            );
        }
    }
}

/// Remove `LEANCTX.md` from `context.fileName` (shared-mode cleanup / toggle-back
/// and uninstall). Collapses back to the implicit default when only
/// `["GEMINI.md"]` remains.
pub(crate) fn unregister_gemini_context_filename(home: &std::path::Path) {
    let name = crate::rules_inject::GEMINI_DEDICATED_CONTEXT_FILENAME;
    let Some(mut json) = read_gemini_settings(home) else {
        return;
    };
    let Some(context) = json.get_mut("context").and_then(|c| c.as_object_mut()) else {
        return;
    };
    let Some(arr) = context.get_mut("fileName").and_then(|v| v.as_array_mut()) else {
        return;
    };
    let before = arr.len();
    arr.retain(|v| v.as_str() != Some(name));
    if arr.len() == before {
        return;
    }
    if arr.len() == 1 && arr[0].as_str() == Some("GEMINI.md") {
        context.remove("fileName");
        if context.is_empty()
            && let Some(obj) = json.as_object_mut()
        {
            obj.remove("context");
        }
    }
    if let Ok(formatted) = serde_json::to_string_pretty(&json) {
        write_file(&gemini_settings_path(home), &formatted);
    }
}

/// Strip the lean-ctx block from the global GEMINI.md (dedicated mode).
fn strip_gemini_md_block(home: &std::path::Path) {
    let gemini_md = home.join(".gemini").join("GEMINI.md");
    if gemini_md
        .metadata()
        .is_ok_and(|m| m.is_file())
        .then(|| std::fs::read_to_string(&gemini_md).ok())
        .flatten()
        .is_some_and(|c| c.contains(crate::rules_inject::RULES_MARKER))
    {
        crate::marked_block::remove_from_file(
            &gemini_md,
            crate::rules_inject::RULES_MARKER,
            crate::rules_inject::RULES_END_MARKER,
            true,
            "Gemini GEMINI.md lean-ctx block",
        );
    }
}

pub(crate) fn install_gemini_hook_scripts(home: &std::path::Path) {
    let hooks_dir = home.join(".gemini").join("hooks");
    install_standard_hook_scripts(
        &hooks_dir,
        "lean-ctx-rewrite-gemini.sh",
        "lean-ctx-redirect-gemini.sh",
    );
}

pub(crate) fn install_gemini_hook_config(home: &std::path::Path) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let settings_path = home.join(".gemini").join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    let has_new_format = settings_content.contains("hook rewrite")
        && settings_content.contains("hook redirect")
        && settings_content.contains("\"type\"")
        && settings_content.contains("\"matcher\"");
    let has_old_hooks = settings_content.contains("lean-ctx-rewrite")
        || settings_content.contains("lean-ctx-redirect")
        || (settings_content.contains("hook rewrite") && !settings_content.contains("\"matcher\""));
    let missing_observe = !settings_content.contains("hook observe");

    if has_new_format && !has_old_hooks && !missing_observe {
        return;
    }

    let observe_cmd = format!("{binary} hook observe");
    let hook_config = serde_json::json!({
        "hooks": {
            "BeforeTool": [
                {
                    "matcher": "shell|execute_command|run_shell_command",
                    "hooks": [{
                        "type": "command",
                        "command": rewrite_cmd
                    }]
                },
                {
                    "matcher": "read_file|read_many_files|grep|search|list_dir",
                    "hooks": [{
                        "type": "command",
                        "command": redirect_cmd
                    }]
                }
            ],
            "AfterTool": [
                {
                    "matcher": ".*",
                    "hooks": [{
                        "type": "command",
                        "command": observe_cmd
                    }]
                }
            ]
        }
    });

    if settings_content.is_empty() {
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_config).unwrap_or_default(),
        );
    } else if let Ok(mut existing) = crate::core::jsonc::parse_jsonc(&settings_content)
        && let Some(obj) = existing.as_object_mut()
    {
        obj.insert("hooks".to_string(), hook_config["hooks"].clone());
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&existing).unwrap_or_default(),
        );
    }
    if !mcp_server_quiet_mode() {
        eprintln!(
            "Installed Gemini CLI hooks at {}",
            settings_path.parent().unwrap_or(&settings_path).display()
        );
    }
}

#[cfg(test)]
mod dedicated_tests {
    use super::*;

    fn temp_home(tag: &str) -> std::path::PathBuf {
        let home =
            std::env::temp_dir().join(format!("leanctx_gemini_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        home
    }

    fn read_filenames(home: &std::path::Path) -> Vec<String> {
        let content = std::fs::read_to_string(gemini_settings_path(home)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        json["context"]["fileName"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn register_seeds_default_and_adds_leanctx() {
        let home = temp_home("seed");
        register_gemini_context_filename(&home);
        assert_eq!(read_filenames(&home), vec!["GEMINI.md", "LEANCTX.md"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn register_is_idempotent() {
        let home = temp_home("idem");
        register_gemini_context_filename(&home);
        register_gemini_context_filename(&home);
        assert_eq!(read_filenames(&home), vec!["GEMINI.md", "LEANCTX.md"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn register_preserves_existing_user_entries() {
        let home = temp_home("preserve");
        std::fs::write(
            gemini_settings_path(&home),
            r#"{"context":{"fileName":["AGENTS.md","GEMINI.md"]}}"#,
        )
        .unwrap();
        register_gemini_context_filename(&home);
        assert_eq!(
            read_filenames(&home),
            vec!["AGENTS.md", "GEMINI.md", "LEANCTX.md"]
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn register_normalizes_string_form() {
        let home = temp_home("string");
        std::fs::write(
            gemini_settings_path(&home),
            r#"{"context":{"fileName":"GEMINI.md"}}"#,
        )
        .unwrap();
        register_gemini_context_filename(&home);
        assert_eq!(read_filenames(&home), vec!["GEMINI.md", "LEANCTX.md"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unregister_collapses_to_default() {
        let home = temp_home("collapse");
        register_gemini_context_filename(&home);
        unregister_gemini_context_filename(&home);
        let content = std::fs::read_to_string(gemini_settings_path(&home)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            json.get("context").is_none(),
            "context should be removed when only default remained: {content}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unregister_preserves_other_entries() {
        let home = temp_home("unreg_preserve");
        std::fs::write(
            gemini_settings_path(&home),
            r#"{"context":{"fileName":["AGENTS.md","LEANCTX.md"]}}"#,
        )
        .unwrap();
        unregister_gemini_context_filename(&home);
        assert_eq!(read_filenames(&home), vec!["AGENTS.md"]);
        let _ = std::fs::remove_dir_all(&home);
    }
}
