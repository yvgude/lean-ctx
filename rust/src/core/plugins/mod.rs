pub mod executor;
pub mod manifest;
pub mod registry;
pub mod sandbox;
pub mod tools;

use executor::{HookPoint, HookResult, execute_hooks_for_point};
use registry::PluginRegistry;
use std::sync::Mutex;
use std::sync::OnceLock;

static GLOBAL_REGISTRY: OnceLock<Mutex<PluginRegistry>> = OnceLock::new();

pub struct PluginManager;

impl PluginManager {
    pub fn init() {
        let _ = GLOBAL_REGISTRY.get_or_init(|| {
            let mut reg = PluginRegistry::from_default_dir();
            let errors = reg.discover();
            for err in &errors {
                tracing::warn!(
                    "plugin discovery error at {}: {}",
                    err.path.display(),
                    err.error
                );
            }
            Mutex::new(reg)
        });
    }

    pub fn with_registry<F, R>(f: F) -> Option<R>
    where
        F: FnOnce(&PluginRegistry) -> R,
    {
        GLOBAL_REGISTRY
            .get()
            .and_then(|m| m.lock().ok())
            .map(|reg| f(&reg))
    }

    pub fn with_registry_mut<F, R>(f: F) -> Option<R>
    where
        F: FnOnce(&mut PluginRegistry) -> R,
    {
        GLOBAL_REGISTRY
            .get()
            .and_then(|m| m.lock().ok())
            .map(|mut reg| f(&mut reg))
    }

    #[must_use]
    pub fn fire_hook(hook: &HookPoint) -> Vec<HookResult> {
        Self::with_registry(|reg| {
            let plugins: Vec<_> = reg.enabled_plugins();
            execute_hooks_for_point(&plugins, hook)
        })
        .unwrap_or_default()
    }

    pub fn fire_hook_background(hook: HookPoint) {
        std::thread::spawn(move || {
            let results = Self::fire_hook(&hook);
            for r in &results {
                if !r.success {
                    tracing::warn!(
                        "plugin hook failed: {} - {}",
                        r.plugin_name,
                        r.error.as_deref().unwrap_or("unknown")
                    );
                }
            }
        });
    }

    /// True if any enabled plugin declares `hook_name`. A cheap guard so the hot
    /// path never spawns a hook thread when nothing would run — the default
    /// (no plugins installed → registry uninitialized → `false`).
    #[must_use]
    pub fn has_listener(hook_name: &str) -> bool {
        Self::with_registry(|reg| any_enabled_listener(reg, hook_name)).unwrap_or(false)
    }

    /// Fire a hook in the background, but only when a plugin is actually
    /// listening for it. Call sites should prefer this over
    /// `fire_hook_background` so they stay zero-cost without plugins.
    pub fn notify(hook: HookPoint) {
        if Self::has_listener(hook.hook_name()) {
            Self::fire_hook_background(hook);
        }
    }

    /// Flattened `[[tools]]` from all enabled plugins (EPIC 12.11). Empty unless
    /// plugins are installed + enabled, so it is zero-cost by default.
    #[must_use]
    pub fn tool_specs() -> Vec<tools::PluginToolSpec> {
        Self::with_registry(|reg| {
            reg.enabled_plugins()
                .iter()
                .flat_map(|p| {
                    let policy = p.manifest.trust.policy();
                    p.manifest.tools.iter().map(move |t| tools::PluginToolSpec {
                        plugin_name: p.manifest.plugin.name.clone(),
                        plugin_dir: p.path.clone(),
                        name: t.name.clone(),
                        description: t.description.clone(),
                        command: t.command.clone(),
                        timeout_ms: t.timeout_ms,
                        input_schema: t.input_schema.clone(),
                        policy,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    }
}

fn any_enabled_listener(reg: &PluginRegistry, hook_name: &str) -> bool {
    reg.enabled_plugins()
        .iter()
        .any(|p| p.manifest.hooks.contains_key(hook_name))
}

pub fn init_plugin_template(name: &str, dir: &std::path::Path) -> std::io::Result<()> {
    let plugin_dir = dir.join(name);
    std::fs::create_dir_all(&plugin_dir)?;

    let manifest = format!(
        r#"[plugin]
name = "{name}"
version = "0.1.0"
description = "Description of what this plugin does"
author = "Your Name"

[hooks.on_session_start]
command = "{name} start"
timeout_ms = 5000

[hooks.on_session_end]
command = "{name} stop"

# [hooks.pre_read]
# command = "{name} pre-read"
# timeout_ms = 2000

# [hooks.post_compress]
# command = "{name} post-compress"

# [hooks.on_knowledge_update]
# command = "{name} knowledge-updated"

# Native MCP tools (no fork needed). Each [[tools]] entry becomes a tool the
# agent can call; arguments arrive as JSON on stdin, the result is stdout.
# [[tools]]
# name = "{name}_lookup"
# description = "What this tool does"
# command = "{name} tool lookup"
# timeout_ms = 5000
# input_schema = {{ type = "object", properties = {{ query = {{ type = "string" }} }}, required = ["query"] }}

# Trust & sandbox (least privilege by default). Hooks/tools run with a scrubbed
# environment and a working-dir jail. Declare only what you need:
#   network         — you make outbound network calls (surfaced for consent)
#   fs_write        — you write files outside the plugin dir (surfaced)
#   env_passthrough — you need the full host env (disables env scrubbing)
# [trust]
# permissions = ["network"]
"#
    );

    std::fs::write(plugin_dir.join("plugin.toml"), manifest)?;

    let readme = format!(
        "# {name}\n\n\
         A lean-ctx plugin.\n\n\
         ## Installation\n\n\
         Copy this directory to `~/.config/lean-ctx/plugins/{name}/`\n\n\
         ## Hook Points\n\n\
         - `on_session_start` — Called when a new session begins\n\
         - `on_session_end` — Called when a session ends\n\
         - `pre_read` — Called before a file is read (receives path via stdin JSON)\n\
         - `post_compress` — Called after compression (receives stats via stdin JSON)\n\
         - `on_knowledge_update` — Called when knowledge is updated (receives fact_id via stdin JSON)\n\n\
         ## Protocol\n\n\
         Hook data is passed as JSON via stdin. Your command should:\n\
         1. Read JSON from stdin\n\
         2. Process the hook\n\
         3. Write optional JSON response to stdout\n\
         4. Exit with code 0 on success, non-zero on failure\n"
    );

    std::fs::write(plugin_dir.join("README.md"), readme)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_template_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        init_plugin_template("test-plugin", dir.path()).unwrap();
        let plugin_dir = dir.path().join("test-plugin");
        assert!(plugin_dir.join("plugin.toml").exists());
        assert!(plugin_dir.join("README.md").exists());

        let manifest = manifest::PluginManifest::from_file(&plugin_dir.join("plugin.toml"));
        assert!(manifest.is_ok());
        let m = manifest.unwrap();
        assert_eq!(m.plugin.name, "test-plugin");
    }

    #[test]
    fn fire_hook_with_no_plugins_returns_empty() {
        let results = PluginManager::fire_hook(&HookPoint::OnSessionStart);
        assert!(results.is_empty());
    }

    #[test]
    fn any_enabled_listener_detects_declared_hook() {
        use registry::PluginRegistry;
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("p");
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("plugin.toml"),
            "[plugin]\nname = \"p\"\nversion = \"1.0.0\"\n\n\
             [hooks.pre_read]\ncommand = \"echo hi\"\n",
        )
        .unwrap();

        let mut reg = PluginRegistry::new(dir.path().to_path_buf());
        reg.discover();

        assert!(any_enabled_listener(&reg, "pre_read"));
        assert!(!any_enabled_listener(&reg, "post_compress"));
    }

    #[test]
    fn any_enabled_listener_ignores_disabled_plugin() {
        use registry::PluginRegistry;
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("p");
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("plugin.toml"),
            "[plugin]\nname = \"p\"\nversion = \"1.0.0\"\n\n\
             [hooks.pre_read]\ncommand = \"echo hi\"\n",
        )
        .unwrap();

        let mut reg = PluginRegistry::new(dir.path().to_path_buf());
        reg.discover();
        reg.disable("p").unwrap();

        assert!(!any_enabled_listener(&reg, "pre_read"));
    }
}
