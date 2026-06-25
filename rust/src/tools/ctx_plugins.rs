use crate::core::plugins::{PluginManager, executor::HookPoint, registry::PluginRegistry};

#[must_use]
pub fn handle(action: &str, name: Option<&str>) -> String {
    match action {
        "list" => handle_list(),
        "enable" => handle_enable(name),
        "disable" => handle_disable(name),
        "info" => handle_info(name),
        "hooks" => handle_hooks(),
        _ => format!("Unknown action: {action}. Valid: list, enable, disable, info, hooks"),
    }
}

fn handle_list() -> String {
    let mut registry = PluginRegistry::from_default_dir();
    let errors = registry.discover();

    let mut out = String::new();
    if !errors.is_empty() {
        for err in &errors {
            out.push_str(&format!("⚠ {}: {}\n", err.path.display(), err.error));
        }
        out.push('\n');
    }

    let plugins = registry.list();
    if plugins.is_empty() {
        out.push_str("No plugins installed.\n");
        out.push_str(&format!(
            "Plugin directory: {}\n",
            registry.plugin_dir().display()
        ));
        return out;
    }

    out.push_str(&format!("{} plugin(s):\n\n", plugins.len()));
    for plugin in &plugins {
        let status = if plugin.enabled {
            "enabled"
        } else {
            "disabled"
        };
        let hooks_count = plugin.manifest.hooks.len();
        out.push_str(&format!(
            "• {} v{} [{status}] ({hooks_count} hook{})\n",
            plugin.manifest.plugin.name,
            plugin.manifest.plugin.version,
            if hooks_count == 1 { "" } else { "s" }
        ));
        if !plugin.manifest.plugin.description.is_empty() {
            out.push_str(&format!("  {}\n", plugin.manifest.plugin.description));
        }
    }
    out
}

fn handle_enable(name: Option<&str>) -> String {
    let Some(name) = name else {
        return "Error: 'name' parameter required for enable action".to_string();
    };
    PluginManager::init();
    match PluginManager::with_registry_mut(|reg| reg.enable(name)) {
        Some(Ok(())) => format!("Enabled plugin: {name}"),
        Some(Err(e)) => format!("Error: {e}"),
        None => "Error: plugin registry not initialized".to_string(),
    }
}

fn handle_disable(name: Option<&str>) -> String {
    let Some(name) = name else {
        return "Error: 'name' parameter required for disable action".to_string();
    };
    PluginManager::init();
    match PluginManager::with_registry_mut(|reg| reg.disable(name)) {
        Some(Ok(())) => format!("Disabled plugin: {name}"),
        Some(Err(e)) => format!("Error: {e}"),
        None => "Error: plugin registry not initialized".to_string(),
    }
}

fn handle_info(name: Option<&str>) -> String {
    let Some(name) = name else {
        return "Error: 'name' parameter required for info action".to_string();
    };
    let mut registry = PluginRegistry::from_default_dir();
    registry.discover();

    match registry.get(name) {
        Some(plugin) => {
            let mut out = String::new();
            out.push_str(&format!("Plugin: {}\n", plugin.manifest.plugin.name));
            out.push_str(&format!("Version: {}\n", plugin.manifest.plugin.version));
            if !plugin.manifest.plugin.description.is_empty() {
                out.push_str(&format!(
                    "Description: {}\n",
                    plugin.manifest.plugin.description
                ));
            }
            if !plugin.manifest.plugin.author.is_empty() {
                out.push_str(&format!("Author: {}\n", plugin.manifest.plugin.author));
            }
            out.push_str(&format!("Enabled: {}\n", plugin.enabled));
            out.push_str(&format!("Path: {}\n", plugin.path.display()));
            if !plugin.manifest.hooks.is_empty() {
                out.push_str("\nHooks:\n");
                for (hook_name, entry) in &plugin.manifest.hooks {
                    out.push_str(&format!(
                        "  {hook_name}: {} (timeout: {}ms)\n",
                        entry.command, entry.timeout_ms
                    ));
                }
            }
            out
        }
        None => format!("Plugin not found: {name}"),
    }
}

fn handle_hooks() -> String {
    let mut out = String::from("Available hook points:\n\n");
    for name in HookPoint::all_hook_names() {
        let desc = match *name {
            "on_session_start" => "Called when a new session begins",
            "on_session_end" => "Called when a session ends",
            "pre_read" => {
                "Called before a file is read (stdin: {\"hook\":\"pre_read\",\"path\":\"...\"})"
            }
            "post_compress" => {
                "Called after compression (stdin: {\"hook\":\"post_compress\",\"path\":\"...\",\"original_tokens\":N,\"compressed_tokens\":N})"
            }
            "on_knowledge_update" => {
                "Called when knowledge is updated (stdin: {\"hook\":\"on_knowledge_update\",\"fact_id\":\"...\"})"
            }
            _ => "",
        };
        out.push_str(&format!("• {name}\n  {desc}\n\n"));
    }
    out
}
