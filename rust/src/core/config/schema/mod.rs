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
    let clean: f64 = format!("{v}").parse().unwrap_or(v as f64);
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
    pub fn generate() -> Self {
        let mut sections = BTreeMap::new();
        sections_core::build(&mut sections);
        sections_features::build(&mut sections);
        sections_advanced::build(&mut sections);
        sections.insert("intelligence_runtime".into(), SectionSchema {
            description: "Global-only optional runtime consent and independent staging pins; project overrides never apply".into(),
            keys: [
                ("enabled", key("bool", serde_json::json!(false), "Opt in to the verified local runtime")),
                ("accept_proprietary", key("bool", serde_json::json!(false), "Explicit user acceptance of the separately licensed runtime")),
                ("staging", key("bool", serde_json::json!(false), "Staging-only delivery; not production release approval")),
                ("root", key("string", serde_json::json!(""), "Absolute private installation root")),
                ("manifest_sha256", key("string", serde_json::json!(""), "Independently selected manifest digest")),
                ("trust_key_hex", key("string", serde_json::json!(""), "Independently provisioned Ed25519 public key, lowercase hex")),
                ("channel_url", key("string", serde_json::json!(""), "Explicit staging catalog URL; fetched only after consent")),
                ("channel_signature_url", key("string", serde_json::json!(""), "Detached staging catalog signature URL")),
                ("channel_root_key_hex", key("string", serde_json::json!(""), "Independently provisioned catalog root; never learned from a download")),
                ("license_configuration", key("string", serde_json::json!(""), "Explicit user-global path to private license configuration; saved after installed device provisioning")),
                ("context_policy_apply", key("bool", serde_json::json!(false), "Apply the promoted read-strategy policy in planning instead of recording it in shadow; security and explicit choices still win")),
            ].into_iter().map(|(name, value)| (name.into(), value)).collect(),
        });

        ConfigSchema {
            version: 1,
            sections,
        }
    }

    /// Looks up a key schema by its dot-separated TOML path.
    /// Returns `None` if the key is not part of the schema.
    pub fn lookup(&self, key: &str) -> Option<&KeySchema> {
        if !key.contains('.') {
            return self.sections.get("root")?.keys.get(key);
        }
        // Try progressively longer section prefixes so that dotted sections
        // like `memory.lifecycle` or `solution.commercial.team_policy` resolve
        // correctly: `memory.lifecycle.stale_days` → section=`memory.lifecycle`,
        // field=`stale_days`.
        for (i, _) in key.rmatch_indices('.') {
            let section = &key[..i];
            let field = &key[i + 1..];
            if let Some(schema) = self.sections.get(section) {
                if let Some(ks) = schema.keys.get(field) {
                    return Some(ks);
                }
            }
        }
        None
    }

    /// All known TOML keys (dot-separated) for validation.
    ///
    /// Combines the hand-written schema (which carries descriptions, types and
    /// help text) with the keys derived from the live `Config` struct. The
    /// struct is the source of truth for *what is valid*, so a field added to
    /// `Config` is recognised by `config apply` / `config validate` immediately,
    /// without anyone remembering to mirror it into `sections_*.rs` (#456).
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

    #[test]
    fn proxy_require_token_is_cli_settable() {
        let schema = ConfigSchema::generate();
        assert!(
            schema.lookup("proxy_require_token").is_some(),
            "`lean-ctx config set proxy_require_token <bool>` must be accepted"
        );
    }

    #[test]
    fn dotted_section_keys_are_cli_settable() {
        let schema = ConfigSchema::generate();
        for key in [
            "memory.lifecycle.stale_days",
            "memory.knowledge.max_facts",
            "memory.episodic.max_episodes",
            "solution.commercial.fingerprints_enabled",
            "solution.commercial.cross_project_patterns",
            "solution.commercial.adaptive.enabled",
            "solution.commercial.adaptive.learning_rate",
            "solution.commercial.team_policy.enabled",
            "solution.commercial.team_policy.min_intensity",
            "proxy.role_aggressiveness.system",
        ] {
            assert!(
                schema.lookup(key).is_some(),
                "`lean-ctx config set {key} <v>` fails with \"Unknown config key\" — lookup must handle dotted section names"
            );
        }
    }

    #[test]
    fn decision_loop_keys_are_cli_settable_with_types_and_defaults() {
        let schema = ConfigSchema::generate();
        for (key, ty, default) in [
            ("decision_loop.enabled", "bool", serde_json::json!(true)),
            ("decision_loop.max_filter_level", "u8", serde_json::json!(0)),
        ] {
            let entry = schema
                .lookup(key)
                .unwrap_or_else(|| panic!("config set {key} must be recognized"));
            assert_eq!(entry.ty, ty, "schema type for {key}");
            assert_eq!(entry.default, default, "schema default for {key}");
        }
    }

    #[test]
    fn permission_inheritance_defaults_to_on() {
        let cfg = super::super::Config::default();
        assert_eq!(
            cfg.permission_inheritance.as_deref(),
            Some("on"),
            "permission_inheritance must default to On so IDE permission rules are honored out of the box"
        );
    }

    /// #1605: the `[llm]` schema was hand-written with literal defaults and had
    /// drifted from `LlmConfig` — it named an `api_key` key that no field backs
    /// (so `config set llm.api_key <secret>` printed "Updated" and then dropped
    /// the value in the serde round-trip), advertised `llama3.2` as the model
    /// default, and omitted `base_url` entirely. Every `[llm]` schema key must
    /// name a real serialised field, and its default must be the struct's.
    #[test]
    fn llm_schema_keys_match_the_llm_config_struct() {
        let schema = ConfigSchema::generate();
        let section = schema
            .sections
            .get("llm")
            .expect("[llm] section must exist in the schema");
        let defaults = serde_json::to_value(crate::core::llm_enhance::LlmConfig::default())
            .expect("LlmConfig serialises");
        let defaults = defaults.as_object().expect("LlmConfig is a struct");

        for (key, entry) in &section.keys {
            let actual = defaults.get(key).unwrap_or_else(|| {
                panic!(
                    "schema key `llm.{key}` has no field on LlmConfig — \
                     `config set llm.{key} <v>` would report a save that serde discards"
                )
            });
            // `Option<String>` serialises as null; the schema shows an empty string.
            let expected = if actual.is_null() {
                serde_json::json!("")
            } else {
                actual.clone()
            };
            assert_eq!(
                entry.default, expected,
                "schema default for `llm.{key}` has drifted from LlmConfig::default()"
            );
        }

        assert!(
            schema.lookup("llm.api_key").is_none(),
            "`llm.api_key` is not a config field — the credential comes from \
             OPENROUTER_API_KEY / ANTHROPIC_API_KEY, so `config set` must reject it"
        );
        assert!(
            schema.lookup("llm.base_url").is_some(),
            "`llm.base_url` is a real field and must be settable"
        );
    }
}
