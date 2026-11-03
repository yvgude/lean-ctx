// SPDX-License-Identifier: Apache-2.0
//! Grok Build MCP registration.
//!
//! Grok Build natively discovers `AGENTS.md`, skills and MCP servers. Its
//! documented user config is `$GROK_HOME/config.toml` (or `~/.grok/config.toml`)
//! with stdio servers under `[mcp_servers.<name>]`, so no unsupported hook
//! format is installed here.

use super::super::{
    mcp_server_env_pairs, mcp_server_quiet_mode, resolve_binary_path, should_register_mcp,
    write_file,
};

pub(crate) fn install_grok_mcp() {
    if !should_register_mcp() {
        return;
    }
    let Some(home) = lean_ctx::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };
    let grok_home = std::env::var_os("GROK_HOME")
        .filter(|path| !path.is_empty())
        .map_or_else(|| home.join(".grok"), std::path::PathBuf::from);
    let path = grok_home.join("config.toml");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    super::super::project_pins::migrate_extra_roots(&existing);
    let Some(updated) =
        ensure_grok_mcp_server(&existing, &resolve_binary_path(), &mcp_server_env_pairs())
    else {
        return;
    };
    write_file(&path, &updated);
    if !mcp_server_quiet_mode() {
        eprintln!("Installed Grok Build MCP server at {}", path.display());
    }
}

fn ensure_grok_mcp_server(
    config_content: &str,
    binary: &str,
    env_pairs: &[(String, String)],
) -> Option<String> {
    let mut doc = config_content.parse::<toml_edit::DocumentMut>().ok()?;
    let original = doc.to_string();
    let servers = doc["mcp_servers"].or_insert(toml_edit::table());
    servers.as_table_mut()?.set_implicit(true);
    let lean = servers["lean-ctx"].or_insert(toml_edit::table());
    let lean_tbl = lean.as_table_mut()?;
    lean_tbl.set_implicit(false);
    if !lean_tbl.contains_key("command") {
        lean_tbl["command"] = toml_edit::value(binary);
    }
    if !lean_tbl.contains_key("args") {
        lean_tbl["args"] = toml_edit::value(toml_edit::Array::new());
    }
    super::super::strip_project_scoped_env_toml(lean_tbl);
    if !env_pairs.is_empty() {
        let env = lean_tbl["env"].or_insert(toml_edit::table());
        let env_tbl = env.as_table_mut()?;
        for (key, value) in env_pairs {
            if env_tbl.get(key).and_then(toml_edit::Item::as_str) != Some(value.as_str()) {
                env_tbl[key] = toml_edit::value(value);
            }
        }
    }
    let updated = doc.to_string();
    (updated != original).then_some(updated)
}

#[cfg(test)]
mod tests {
    use super::ensure_grok_mcp_server;

    fn env_pairs() -> Vec<(String, String)> {
        vec![("LEAN_CTX_QUIET".to_string(), "1".to_string())]
    }

    #[test]
    fn adds_valid_grok_mcp_server() {
        let output = ensure_grok_mcp_server("", "/usr/local/bin/lean-ctx", &env_pairs())
            .expect("empty config must gain the lean-ctx server");
        let doc = output
            .parse::<toml_edit::DocumentMut>()
            .expect("generated Grok config must be valid TOML");
        assert_eq!(
            doc["mcp_servers"]["lean-ctx"]["command"].as_str(),
            Some("/usr/local/bin/lean-ctx")
        );
        assert_eq!(
            doc["mcp_servers"]["lean-ctx"]["env"]["LEAN_CTX_QUIET"].as_str(),
            Some("1")
        );
    }

    #[test]
    fn heals_a_global_project_pin() {
        let input = "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\nargs = []\n\
                     [mcp_servers.lean-ctx.env]\nLEAN_CTX_PROJECT_ROOT = \"/work/repo\"\n";
        let output = ensure_grok_mcp_server(input, "lean-ctx", &[]).expect("pin must be healed");
        assert!(!output.contains("LEAN_CTX_PROJECT_ROOT"), "{output}");
        assert!(ensure_grok_mcp_server(&output, "lean-ctx", &[]).is_none());
    }

    #[test]
    fn preserves_user_config_and_is_idempotent() {
        let input =
            "[models]\ndefault = \"grok-build\"\n\n[mcp_servers.other]\ncommand = \"other\"\n";
        let output = ensure_grok_mcp_server(input, "lean-ctx", &env_pairs()).unwrap();
        assert!(output.contains("default = \"grok-build\""));
        assert!(output.contains("[mcp_servers.other]"));
        assert!(ensure_grok_mcp_server(&output, "lean-ctx", &env_pairs()).is_none());
    }

    #[test]
    fn never_overwrites_user_managed_lean_ctx_command() {
        let input = "[mcp_servers.lean-ctx]\ncommand = \"custom-lean-ctx\"\n";
        let output = ensure_grok_mcp_server(input, "lean-ctx", &[]).unwrap();
        let doc = output.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            doc["mcp_servers"]["lean-ctx"]["command"].as_str(),
            Some("custom-lean-ctx")
        );
    }
}
