//! Config provenance (GH #450) — *where does each effective setting come from?*
//!
//! The "quick settings keep resetting themselves" reports were impossible to
//! diagnose because nothing surfaced *which* source actually backed an effective
//! value. A value typed into the dashboard lands in the global `config.toml`, but
//! the effective value can be silently shadowed by:
//!
//!   - an environment variable (`LEAN_CTX_COMPRESSION`, …) — wins in `effective()`;
//!   - a project-local `.lean-ctx.toml` — overrides `compression_level`,
//!     `terse_agent` and `tool_profile` in [`Config::merge_local`];
//!   - a divergent *resolved config dir* (launchd vs. terminal env) — the
//!     dashboard writes path X while the runtime reads path Y, so the global file
//!     "does not exist" from the reader's view;
//!   - a parse error — `load()` falls back to defaults (only a stderr warning).
//!
//! [`Config::provenance`] captures all four mechanisms in one snapshot, consumed
//! by both `lean-ctx config validate` and the dashboard `/api/settings` endpoint
//! so each becomes visible — and therefore fixable.

use std::path::PathBuf;

use super::Config;

/// Editable quick-settings a project-local `.lean-ctx.toml` can override via
/// [`Config::merge_local`]. `structure_first` is intentionally absent: the local
/// merge never touches it.
const LOCAL_OVERRIDABLE_KEYS: &[&str] = &["compression_level", "terse_agent", "tool_profile"];

/// Editable quick-settings paired with the environment variable that overrides
/// each in `effective()`. Keep in sync with the dashboard allow-list and the
/// per-field `*_effective()` readers.
const ENV_OVERRIDABLE: &[(&str, &str)] = &[
    ("compression_level", "LEAN_CTX_COMPRESSION"),
    ("terse_agent", "LEAN_CTX_TERSE_AGENT"),
    ("tool_profile", "LEAN_CTX_TOOL_PROFILE"),
    ("structure_first", "LEAN_CTX_STRUCTURE_FIRST"),
];

/// An active environment variable shadowing a persisted setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvOverride {
    /// The setting key (e.g. `"compression_level"`).
    pub setting: &'static str,
    /// The environment variable currently pinning it (e.g. `"LEAN_CTX_COMPRESSION"`).
    pub var: &'static str,
}

/// Where the effective config comes from — the four shadowing mechanisms behind
/// GH #450, captured in one snapshot.
#[derive(Debug, Clone)]
pub struct ConfigProvenance {
    /// Resolved global `config.toml` path (`None` only when no config base
    /// resolves, e.g. no `HOME`).
    pub config_path: Option<PathBuf>,
    /// Whether that global file currently exists on disk.
    pub config_exists: bool,
    /// Whether this install is committed to the XDG four-dir layout.
    pub xdg_pinned: bool,
    /// The global-config parse error, if `config.toml` exists but is unparseable
    /// (mirrors the fallback-to-defaults `Config::load` takes).
    pub parse_error: Option<String>,
    /// Resolved project-local `.lean-ctx.toml` path, if a project root resolves.
    pub local_path: Option<PathBuf>,
    /// Whether that project-local file exists and is readable.
    pub local_exists: bool,
    /// Editable keys the project-local file overrides (subset of
    /// `compression_level` / `terse_agent` / `tool_profile`).
    pub local_keys: Vec<&'static str>,
    /// Active environment overrides among the editable settings.
    pub env_overrides: Vec<EnvOverride>,
}

impl ConfigProvenance {
    /// `true` when at least one shadowing source (env override, project-local
    /// override, or parse error) could make a saved global value appear to reset.
    #[must_use]
    pub fn has_shadow(&self) -> bool {
        !self.env_overrides.is_empty() || !self.local_keys.is_empty() || self.parse_error.is_some()
    }

    /// `true` when `setting` is overridden by a project-local `.lean-ctx.toml`.
    // `contains` would require a `&&'static str` argument; accepting a plain
    // `&str` keeps the call site lifetime-agnostic, so the manual compare stays.
    #[must_use]
    #[allow(clippy::manual_contains)]
    pub fn local_overrides(&self, setting: &str) -> bool {
        self.local_keys.iter().any(|k| *k == setting)
    }
}

impl Config {
    /// Snapshot the provenance of the editable settings (GH #450).
    ///
    /// Reads the same sources as [`Config::load`] (honoring the `#356` TCC guard
    /// for the project-local file) plus the live environment, so the result
    /// matches what a fresh `load()` would resolve. Pure: it never mutates the
    /// config cache or writes to disk.
    #[must_use]
    pub fn provenance() -> ConfigProvenance {
        let config_path = Self::path();
        let config_exists = config_path.as_ref().is_some_and(|p| p.exists());

        // Mirror `load()`: a present-but-unparseable global file means the runtime
        // silently runs on defaults. Parse into `Config` (not a bare `Table`) to
        // match exactly what `load()` rejects.
        let parse_error = config_path.as_ref().and_then(|p| {
            let raw = std::fs::read_to_string(p).ok()?;
            toml::from_str::<Config>(&raw).err().map(|e| e.to_string())
        });

        let local_path = Self::find_project_root().map(|r| Self::local_path(&r));
        let local_content = local_path
            .as_ref()
            .filter(|p| crate::core::pathutil::may_probe_path(p.as_path()))
            .and_then(|p| std::fs::read_to_string(p).ok());
        let local_exists = local_content.is_some();
        let local_keys = local_content
            .as_deref()
            .map(local_override_keys)
            .unwrap_or_default();

        let env_overrides = ENV_OVERRIDABLE
            .iter()
            .filter(|(_, var)| env_is_set(var))
            .map(|&(setting, var)| EnvOverride { setting, var })
            .collect();

        ConfigProvenance {
            config_path,
            config_exists,
            xdg_pinned: crate::core::layout_pin::is_xdg_pinned(),
            parse_error,
            local_path,
            local_exists,
            local_keys,
            env_overrides,
        }
    }
}

/// Editable keys explicitly set in a project-local `.lean-ctx.toml`. Mirrors the
/// keys [`Config::merge_local`] honors, detected via parsed top-level table keys
/// (a comment that merely mentions the key does not count).
fn local_override_keys(local_toml: &str) -> Vec<&'static str> {
    let Ok(table) = local_toml.parse::<toml::Table>() else {
        return Vec::new();
    };
    LOCAL_OVERRIDABLE_KEYS
        .iter()
        .filter(|k| table.contains_key(**k))
        .copied()
        .collect()
}

/// `true` when `var` is set to a non-empty value. Matches the dashboard's
/// `env_present` so the two surfaces never disagree about an override.
fn env_is_set(var: &str) -> bool {
    std::env::var_os(var).is_some_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_override_keys_detects_editable_keys() {
        let toml =
            "compression_level = \"max\"\nterse_agent = \"ultra\"\ntool_profile = \"power\"\n";
        let keys = local_override_keys(toml);
        assert!(keys.contains(&"compression_level"));
        assert!(keys.contains(&"terse_agent"));
        assert!(keys.contains(&"tool_profile"));
    }

    #[test]
    fn local_override_keys_ignores_structure_first_and_unrelated() {
        // structure_first is not merged from local config, so it must not appear.
        let toml = "structure_first = true\nultra_compact = true\n";
        assert!(local_override_keys(toml).is_empty());
    }

    #[test]
    fn local_override_keys_ignores_comment_mentions() {
        // A bare comment mentioning the key must not be reported as an override.
        let toml = "# compression_level = \"max\" was considered\nultra_compact = false\n";
        assert!(local_override_keys(toml).is_empty());
    }

    #[test]
    fn local_override_keys_empty_on_parse_error() {
        assert!(local_override_keys("this is = = not toml").is_empty());
    }

    #[test]
    fn has_shadow_reflects_each_source() {
        let clean = ConfigProvenance {
            config_path: None,
            config_exists: false,
            xdg_pinned: true,
            parse_error: None,
            local_path: None,
            local_exists: false,
            local_keys: vec![],
            env_overrides: vec![],
        };
        assert!(!clean.has_shadow());

        let with_local = ConfigProvenance {
            local_keys: vec!["compression_level"],
            ..clean.clone()
        };
        assert!(with_local.has_shadow());
        assert!(with_local.local_overrides("compression_level"));
        assert!(!with_local.local_overrides("terse_agent"));

        let with_env = ConfigProvenance {
            env_overrides: vec![EnvOverride {
                setting: "compression_level",
                var: "LEAN_CTX_COMPRESSION",
            }],
            ..clean.clone()
        };
        assert!(with_env.has_shadow());

        let with_parse_err = ConfigProvenance {
            parse_error: Some("bad".into()),
            ..clean
        };
        assert!(with_parse_err.has_shadow());
    }

    #[test]
    fn provenance_reports_active_env_override() {
        let _guard = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_COMPRESSION", "lite");
        let prov = Config::provenance();
        crate::test_env::remove_var("LEAN_CTX_COMPRESSION");

        assert!(
            prov.env_overrides
                .iter()
                .any(|e| e.setting == "compression_level" && e.var == "LEAN_CTX_COMPRESSION"),
            "expected LEAN_CTX_COMPRESSION to be reported as an env override"
        );
    }
}
