// SPDX-License-Identifier: Apache-2.0
//! Project pins in user-global agent MCP configs.
//!
//! Builds that baked the installing session's `LEAN_CTX_PROJECT_ROOT` /
//! `LEAN_CTX_EXTRA_ROOTS` into an agent's *global* MCP entry bound every later
//! session of that agent — in any repository — to one project (see
//! [`super::mcp_server_env_pairs`]). This module finds those pins (doctor) and
//! removes them (agent refresh, `doctor --fix`). Extra roots are moved into the
//! global `extra_roots` first, so access a user granted that way is kept; the
//! project pin itself is the bug and is dropped.

use std::path::{Path, PathBuf};

/// Env keys that bind a single project and therefore must never live in a
/// user-global agent MCP entry.
pub(crate) const PROJECT_SCOPED_ENV_KEYS: &[&str] =
    &["LEAN_CTX_PROJECT_ROOT", "LEAN_CTX_EXTRA_ROOTS"];

/// Removes project-scoped pins from a TOML `[mcp_servers.lean-ctx]` table's
/// `env`, dropping `env` once it is empty. Returns the removed `(key, value)`s.
pub(crate) fn strip_project_scoped_env_toml(
    lean_tbl: &mut toml_edit::Table,
) -> Vec<(&'static str, String)> {
    let Some(env_tbl) = lean_tbl.get_mut("env").and_then(|e| e.as_table_like_mut()) else {
        return Vec::new();
    };
    let removed: Vec<(&'static str, String)> = PROJECT_SCOPED_ENV_KEYS
        .iter()
        .filter_map(|key| {
            env_tbl
                .remove(key)
                .map(|v| (*key, v.as_str().unwrap_or_default().to_string()))
        })
        .collect();
    if env_tbl.is_empty() {
        lean_tbl.remove("env");
    }
    removed
}

/// Moves `LEAN_CTX_EXTRA_ROOTS` values found in a lean-ctx MCP entry of
/// `config_content` into the global `extra_roots` (which the server reads
/// itself), so stripping the env pin keeps that access. Best effort.
pub(crate) fn migrate_extra_roots(config_content: &str) {
    let value = config_content
        .parse::<toml_edit::DocumentMut>()
        .ok()
        .and_then(|doc| {
            doc.get("mcp_servers")?
                .get("lean-ctx")?
                .get("env")?
                .get("LEAN_CTX_EXTRA_ROOTS")?
                .as_str()
                .map(str::to_string)
        })
        .or_else(|| json_lean_ctx_env(config_content, "LEAN_CTX_EXTRA_ROOTS"));
    let Some(value) = value else {
        return;
    };
    let mut roots = crate::cli::global_string_list("extra_roots");
    let before = roots.len();
    for path in std::env::split_paths(&value) {
        let entry = path.to_string_lossy().to_string();
        if !entry.trim().is_empty() && !entry.contains(',') && !roots.contains(&entry) {
            roots.push(entry);
        }
    }
    if roots.len() > before
        && let Err(e) = crate::core::config::setter::set_by_key("extra_roots", &roots.join(","))
    {
        tracing::warn!("could not move LEAN_CTX_EXTRA_ROOTS into extra_roots: {e}");
    }
}

/// First `env[key]` of a top-level `lean-ctx` server entry in a JSON config.
fn json_lean_ctx_env(content: &str, key: &str) -> Option<String> {
    let json = crate::core::jsonc::parse_jsonc(content).ok()?;
    let mut found = None;
    visit_lean_ctx_entries(&json, &mut |entry| {
        for env_key in ["env", "environment"] {
            if let Some(v) = entry
                .get(env_key)
                .and_then(|e| e.get(key))
                .and_then(|v| v.as_str())
            {
                found.get_or_insert_with(|| v.to_string());
            }
        }
    });
    found
}

/// Calls `f` for every `lean-ctx` server entry, at any nesting (`mcpServers`,
/// `servers`, `mcp.servers`, `context_servers`, …) except below a `projects`
/// key: per-project entries (`~/.claude.json`) are legitimately project-scoped.
fn visit_lean_ctx_entries(value: &serde_json::Value, f: &mut dyn FnMut(&serde_json::Value)) {
    let Some(obj) = value.as_object() else {
        return;
    };
    for (key, child) in obj {
        if key == "lean-ctx" {
            f(child);
        } else if key != "projects" {
            visit_lean_ctx_entries(child, f);
        }
    }
}

/// `value` with the project pins removed from every visited lean-ctx entry.
fn without_pins(mut value: serde_json::Value) -> serde_json::Value {
    fn strip(value: &mut serde_json::Value) {
        let Some(obj) = value.as_object_mut() else {
            return;
        };
        for (key, child) in obj.iter_mut() {
            if key == "lean-ctx" {
                for env_key in ["env", "environment"] {
                    if let Some(env) = child.get_mut(env_key).and_then(|e| e.as_object_mut()) {
                        for pin in PROJECT_SCOPED_ENV_KEYS {
                            env.remove(*pin);
                        }
                    }
                }
            } else if key != "projects" {
                strip(child);
            }
        }
    }
    strip(&mut value);
    value
}

/// JSON/JSONC heal by deleting the pin *lines*, so key order, formatting and
/// comments survive. The result must parse to exactly the original minus the
/// pins of top-level lean-ctx entries; anything else (a pin on the same line
/// as other keys, pins under `projects`) leaves the file for a manual edit.
fn heal_json_lines(content: &str) -> Option<String> {
    let original = crate::core::jsonc::parse_jsonc(content).ok()?;
    let expected = without_pins(original.clone());
    if expected == original {
        return None;
    }
    let is_pin = |line: &str| {
        let t = line.trim_start();
        PROJECT_SCOPED_ENV_KEYS
            .iter()
            .any(|k| t.starts_with(&format!("\"{k}\"")))
    };
    let mut kept: Vec<String> = Vec::new();
    for line in content.lines() {
        if is_pin(line) {
            continue;
        }
        // A pin that was the last member leaves a dangling comma behind.
        if line.trim_start().starts_with('}')
            && let Some(prev) = kept.iter_mut().rev().find(|l| !l.trim().is_empty())
            && prev.trim_end().ends_with(',')
        {
            let trimmed = prev.trim_end().trim_end_matches(',').to_string();
            *prev = trimmed;
        }
        kept.push(line.to_string());
    }
    let mut healed = kept.join("\n");
    if content.ends_with('\n') {
        healed.push('\n');
    }
    (crate::core::jsonc::parse_jsonc(&healed).ok()? == expected).then_some(healed)
}

/// Heals one config file's text. `None` when there is nothing to remove, or
/// when the file cannot be rewritten without touching other content.
fn heal_content(path: &Path, content: &str) -> Option<String> {
    if !PROJECT_SCOPED_ENV_KEYS.iter().any(|k| content.contains(k)) {
        return None;
    }
    if path.extension().is_some_and(|e| e == "toml") {
        let mut doc = content.parse::<toml_edit::DocumentMut>().ok()?;
        let lean = doc
            .get_mut("mcp_servers")?
            .get_mut("lean-ctx")?
            .as_table_mut()?;
        if strip_project_scoped_env_toml(lean).is_empty() {
            return None;
        }
        return Some(doc.to_string());
    }
    heal_json_lines(content)
}

/// Global agent MCP configs that may carry a lean-ctx entry: every editor
/// target plus the TOML agents the registry does not model (Codex, Grok).
/// `~/.claude.json` is skipped: Claude Code rewrites it concurrently and
/// lean-ctx never put an env block there.
fn candidate_configs(home: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = crate::core::editor_registry::detect::build_targets(home)
        .into_iter()
        .map(|t| (t.name.to_string(), t.config_path))
        .collect();
    // Codex: the base config plus every profile overlay (`<name>.config.toml`).
    let codex_dir = std::env::var_os("CODEX_HOME")
        .filter(|p| !p.is_empty())
        .map_or_else(|| home.join(".codex"), PathBuf::from);
    if let Ok(entries) = std::fs::read_dir(&codex_dir) {
        for path in entries.filter_map(Result::ok).map(|e| e.path()) {
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with("config.toml"))
            {
                out.push(("Codex".to_string(), path));
            }
        }
    }
    let grok_home = std::env::var_os("GROK_HOME")
        .filter(|p| !p.is_empty())
        .map_or_else(|| home.join(".grok"), PathBuf::from);
    out.push(("Grok Build".to_string(), grok_home.join("config.toml")));
    // Some registry targets resolve through platform dirs rather than `home`;
    // only files under `home` are this user's global configs to inspect.
    out.retain(|(_, path)| {
        path.starts_with(home) && path.file_name().is_none_or(|n| n != ".claude.json")
    });
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out.dedup_by(|a, b| a.1 == b.1);
    out
}

/// Agents whose global config pins a project, as `(agent, config path)`.
pub(crate) fn pinned_agent_configs(home: &Path) -> Vec<(String, PathBuf)> {
    candidate_configs(home)
        .into_iter()
        .filter(|(_, path)| {
            std::fs::read_to_string(path).is_ok_and(|c| {
                c.contains("lean-ctx") && PROJECT_SCOPED_ENV_KEYS.iter().any(|k| c.contains(k))
            })
        })
        .collect()
}

/// Removes project pins from every global agent config (`doctor --fix`).
/// Returns `(healed, left_for_manual_edit)`.
pub(crate) fn heal_global_project_pins(home: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut healed = Vec::new();
    let mut manual = Vec::new();
    for (_, path) in pinned_agent_configs(home) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        match heal_content(&path, &content) {
            Some(updated) => {
                migrate_extra_roots(&content);
                if crate::config_io::write_atomic_with_backup(&path, &updated).is_ok() {
                    healed.push(path);
                } else {
                    manual.push(path);
                }
            }
            None => manual.push(path),
        }
    }
    (healed, manual)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heals_toml_and_keeps_everything_else() {
        let toml = "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\n\n\
                    [mcp_servers.lean-ctx.env]\nLEAN_CTX_PROJECT_ROOT = \"/a\"\n\
                    LEAN_CTX_EXTRA_ROOTS = \"/b\"\nLEAN_CTX_QUIET = \"1\"\n\n\
                    [mcp_servers.other]\ncommand = \"x\"\n";
        let out = heal_content(Path::new("config.toml"), toml).expect("pins removed");
        assert!(!out.contains("LEAN_CTX_PROJECT_ROOT") && !out.contains("LEAN_CTX_EXTRA_ROOTS"));
        assert!(out.contains("LEAN_CTX_QUIET") && out.contains("[mcp_servers.other]"));
        assert!(heal_content(Path::new("config.toml"), &out).is_none());
    }

    #[test]
    fn strip_drops_an_env_table_left_empty() {
        let mut doc = "[mcp_servers.lean-ctx]\ncommand = \"x\"\n[mcp_servers.lean-ctx.env]\n\
                       LEAN_CTX_PROJECT_ROOT = \"/a\"\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        let lean = doc["mcp_servers"]["lean-ctx"].as_table_mut().unwrap();
        let removed = strip_project_scoped_env_toml(lean);
        assert_eq!(removed, vec![("LEAN_CTX_PROJECT_ROOT", "/a".to_string())]);
        assert!(!doc.to_string().contains("env"), "{doc}");
    }

    #[test]
    fn json_heal_keeps_order_comments_and_other_servers() {
        let jsonc = "{\n  // user comment\n  \"servers\": {\n    \"zeta\": {\"command\": \"z\"},\n    \
                     \"lean-ctx\": {\n      \"command\": \"lean-ctx\",\n      \"env\": {\n        \
                     \"KEEP\": \"1\",\n        \"LEAN_CTX_PROJECT_ROOT\": \"/a\"\n      }\n    },\n    \
                     \"other\": {\"env\": {\"LEAN_CTX_PROJECT_ROOT\": \"/x\"}}\n  }\n}\n";
        let out = heal_content(Path::new("mcp.json"), jsonc).expect("pin removed");
        assert!(out.contains("// user comment"), "{out}");
        assert!(
            out.find("zeta").unwrap() < out.find("lean-ctx").unwrap(),
            "order kept"
        );
        assert!(
            out.contains("\"KEEP\": \"1\"\n"),
            "dangling comma removed: {out}"
        );
        // Another server's env is not ours to touch.
        assert!(out.contains("\"other\": {\"env\": {\"LEAN_CTX_PROJECT_ROOT\": \"/x\"}}"));
        assert_eq!(
            crate::core::jsonc::parse_jsonc(&out).unwrap(),
            without_pins(crate::core::jsonc::parse_jsonc(jsonc).unwrap())
        );
    }

    #[test]
    fn json_heal_never_touches_project_scoped_entries() {
        let claude = r#"{"projects": {"/repo": {"mcpServers": {"lean-ctx": {"env": {
            "LEAN_CTX_PROJECT_ROOT": "/repo"}}}}}}"#;
        assert!(heal_content(Path::new("settings.json"), claude).is_none());
        // A pin sharing a line with other keys cannot be removed line-wise.
        let inline = r#"{"lean-ctx": {"env": {"LEAN_CTX_PROJECT_ROOT": "/a", "K": "1"}}}"#;
        assert!(heal_content(Path::new("mcp.json"), inline).is_none());
    }

    #[test]
    fn heal_rewrites_only_pinned_files_and_keeps_extra_roots() {
        let iso = crate::core::data_dir::isolated_data_dir();
        let home = iso.path().join("home");
        let grok = home.join(".grok/config.toml");
        std::fs::create_dir_all(grok.parent().unwrap()).unwrap();
        std::fs::write(
            &grok,
            "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\n[mcp_servers.lean-ctx.env]\n\
             LEAN_CTX_PROJECT_ROOT = \"/a\"\nLEAN_CTX_EXTRA_ROOTS = \"/work/b\"\n",
        )
        .unwrap();
        let codex = home.join(".codex/config.toml");
        std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
        std::fs::write(&codex, "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\n").unwrap();
        crate::test_env::remove_var("GROK_HOME");
        crate::test_env::remove_var("CODEX_HOME");

        assert_eq!(pinned_agent_configs(&home).len(), 1);
        let (healed, manual) = heal_global_project_pins(&home);
        assert_eq!(healed, vec![grok.clone()]);
        assert!(manual.is_empty(), "{manual:?}");
        {
            let found = &pinned_agent_configs(&home);
            assert!(found.is_empty(), "{found:?}");
        }
        assert_eq!(
            crate::cli::global_string_list("extra_roots"),
            vec!["/work/b".to_string()],
            "the env-granted extra root moves into config.toml"
        );
    }
}
