//! Auto-generated config schema from `Config` struct metadata.
//!
//! Used by `lean-ctx config schema` to emit JSON and by
//! `lean-ctx config validate` to check user config.toml files.

use serde::Serialize;
use std::collections::BTreeMap;
mod sections_advanced;
mod sections_core;
mod sections_features;

#[derive(Debug, Clone, Serialize)]
pub struct ConfigSchema {
    pub version: u32,
    pub sections: BTreeMap<String, SectionSchema>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionSchema {
    pub description: String,
    pub keys: BTreeMap<String, KeySchema>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeySchema {
    #[serde(rename = "type")]
    pub ty: String,
    pub default: serde_json::Value,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_override: Option<String>,
}

fn clean_f32(v: f32) -> serde_json::Value {
    let clean: f64 = format!("{v}").parse().unwrap_or(f64::from(v));
    serde_json::json!(clean)
}

fn key(ty: &str, default: serde_json::Value, desc: &str) -> KeySchema {
    KeySchema {
        ty: ty.to_string(),
        default,
        description: desc.to_string(),
        values: None,
        env_override: None,
    }
}

fn key_enum(values: &[&str], default: &str, desc: &str) -> KeySchema {
    KeySchema {
        ty: "enum".to_string(),
        default: serde_json::Value::String(default.to_string()),
        description: desc.to_string(),
        values: Some(values.iter().map(ToString::to_string).collect()),
        env_override: None,
    }
}

fn key_with_env(ty: &str, default: serde_json::Value, desc: &str, env: &str) -> KeySchema {
    KeySchema {
        ty: ty.to_string(),
        default,
        description: desc.to_string(),
        values: None,
        env_override: Some(env.to_string()),
    }
}

fn key_enum_with_env(values: &[&str], default: &str, desc: &str, env: &str) -> KeySchema {
    KeySchema {
        ty: "enum".to_string(),
        default: serde_json::Value::String(default.to_string()),
        description: desc.to_string(),
        values: Some(values.iter().map(ToString::to_string).collect()),
        env_override: Some(env.to_string()),
    }
}

impl ConfigSchema {
    #[must_use]
    pub fn generate() -> Self {
        let mut sections = BTreeMap::new();
        sections_core::build(&mut sections);
        sections_features::build(&mut sections);
        sections_advanced::build(&mut sections);

        ConfigSchema {
            version: 1,
            sections,
        }
    }

    /// Looks up a key schema by its dot-separated TOML path.
    /// Returns `None` if the key is not part of the schema.
    #[must_use]
    pub fn lookup(&self, key: &str) -> Option<&KeySchema> {
        if let Some(dot_pos) = key.find('.') {
            let section = &key[..dot_pos];
            let field = &key[dot_pos + 1..];
            self.sections.get(section)?.keys.get(field)
        } else {
            self.sections.get("root")?.keys.get(key)
        }
    }

    /// All known TOML keys (dot-separated) for validation.
    ///
    /// Combines the hand-written schema (which carries descriptions, types and
    /// help text) with the keys derived from the live `Config` struct. The
    /// struct is the source of truth for *what is valid*, so a field added to
    /// `Config` is recognised by `config apply` / `config validate` immediately,
    /// without anyone remembering to mirror it into `sections_*.rs` (#456).
    #[must_use]
    pub fn known_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for (section, schema) in &self.sections {
            if section == "root" {
                for key_name in schema.keys.keys() {
                    keys.push(key_name.clone());
                }
            } else {
                if schema.keys.is_empty() {
                    keys.push(section.clone());
                }
                for key_name in schema.keys.keys() {
                    keys.push(format!("{section}.{key_name}"));
                }
            }
        }
        keys.extend(config_derived_keys());
        keys.sort();
        keys.dedup();
        keys
    }
}

/// Every TOML key the `Config` struct serialises to, in dot-separated form
/// (e.g. `proxy_require_token`, `memory`, `memory.episodic`). Derived from
/// `Config::default()` so validation tracks the struct automatically (#456).
///
/// Option fields that default to `None` are omitted by serde and therefore not
/// listed here; those keys still come from the hand-written schema. Emitting the
/// bare section name (e.g. `memory`) lets the `starts_with("section.")` rule in
/// the validators accept the whole section, matching how empty schema sections
/// already behave.
fn config_derived_keys() -> Vec<String> {
    fn walk(table: &toml::value::Table, prefix: &str, out: &mut Vec<String>) {
        for (k, v) in table {
            let full = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if let toml::Value::Table(sub) = v {
                out.push(full.clone());
                walk(sub, &full, out);
            } else {
                out.push(full);
            }
        }
    }

    let mut out = Vec::new();
    if let Ok(toml::Value::Table(table)) =
        toml::Value::try_from(crate::core::config::Config::default())
    {
        walk(&table, "", &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the acceptance rule used by `config validate` / `config apply`:
    /// a key is valid if it is listed verbatim or sits under a known section.
    fn accepted(known: &[String], key: &str) -> bool {
        known.iter().any(|k| k == key) || known.iter().any(|k| key.starts_with(&format!("{k}.")))
    }

    /// #456: every field the `Config` struct actually serialises must be
    /// accepted by validation. Before the fix, 38 real keys/sections
    /// (`proxy_require_token`, `memory.*`, `providers.*`, `proxy`, …) were
    /// flagged "unknown" because the hand-written schema had drifted.
    #[test]
    fn known_keys_cover_every_config_struct_field() {
        let known = ConfigSchema::generate().known_keys();
        let missing: Vec<_> = config_derived_keys()
            .into_iter()
            .filter(|k| !accepted(&known, k))
            .collect();
        assert!(
            missing.is_empty(),
            "config struct fields not recognised by validation (schema drift): {missing:?}"
        );
    }

    /// Spot-check the concrete keys from the #456 report so a future schema/struct
    /// refactor that reintroduces the drift fails loudly.
    #[test]
    fn known_keys_recognise_reported_456_keys() {
        let known = ConfigSchema::generate().known_keys();
        for key in [
            "proxy_require_token",
            "allow_ide_config_dirs",
            "memory.episodic",
            "providers.github",
            "proxy",
        ] {
            assert!(
                accepted(&known, key),
                "validation must recognise '{key}' (#456)"
            );
        }
    }

    /// `config set` resolves keys via [`ConfigSchema::lookup`] — the hand-written
    /// schema only, NOT `known_keys()` (which also folds in `config_derived_keys`).
    /// An `Option<_>` scalar field defaults to `None`, so serde omits it from
    /// `Config::default()` and it never appears in `config_derived_keys`: such a
    /// field is settable via `config set` **only** if it was hand-added to a
    /// `sections_*.rs` schema. Forgetting that is the `Unknown config key: <x>`
    /// regression a user hit for `path_jail` before #507 (and `persona` /
    /// `bypass_hints` here). Guard the whole class so a new `Option` knob can't
    /// silently become un-settable again — if you add an `Option` scalar to
    /// `Config`, register it in `sections_*.rs` and list it here.
    #[test]
    fn option_scalar_keys_are_cli_settable() {
        let schema = ConfigSchema::generate();
        for key in [
            "path_jail",
            "persona",
            "bypass_hints",
            "shell_security",
            "cache_policy",
            "profile",
            "tool_profile",
            "rules_scope",
            "rules_injection",
            "permission_inheritance",
            "proxy_enabled",
            "proxy_port",
            "proxy_timeout_ms",
        ] {
            assert!(
                schema.lookup(key).is_some(),
                "`lean-ctx config set {key} <v>` fails with 'Unknown config key' — \
                 add `{key}` to a sections_*.rs schema"
            );
        }
    }
}
