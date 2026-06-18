//! Dashboard settings API (#427) — read + flip the four high-impact, mid-session
//! switches (compression level, tool profile, `structure_first`, terse agent)
//! without dropping to the terminal.
//!
//! Security: this is the dashboard's only *write* surface besides the existing
//! memory/knowledge POSTs, and it inherits the same protection — every `/api/*`
//! request is Bearer-token gated and (for POST) CSRF-`Origin` checked *before*
//! the router runs (see `dashboard/mod.rs::handle_request`). On top of that, the
//! value is validated against a fixed allow-list here and again by the schema
//! round-trip in `config::setter::set_by_key`, so an authenticated client can
//! only ever land a known-good value into `config.toml`.

use serde::Deserialize;

use super::helpers::json_err;
use crate::core::config::{CompressionLevel, Config, TerseAgent};
use crate::core::tool_profiles::ToolProfile;

const COMPRESSION_OPTIONS: &[&str] = &["off", "lite", "standard", "max"];
const TERSE_OPTIONS: &[&str] = &["off", "lite", "full", "ultra"];
// `lean` unpins the profile (clears the key) — same as `lean-ctx tools lean`.
const TOOL_PROFILE_OPTIONS: &[&str] = &["minimal", "standard", "power", "lean"];

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    method: &str,
    body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/settings" if method.eq_ignore_ascii_case("POST") => Some(post_settings(body)),
        "/api/settings" => Some(("200 OK", "application/json", settings_payload())),
        _ => None,
    }
}

#[derive(Deserialize)]
struct SettingReq {
    key: String,
    value: serde_json::Value,
}

fn post_settings(body: &str) -> (&'static str, &'static str, String) {
    let req: SettingReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!("invalid JSON: {e}")),
            );
        }
    };

    let Some(value) = normalize_value(&req.value) else {
        return (
            "400 Bad Request",
            "application/json",
            json_err("value must be a string or boolean"),
        );
    };

    match apply_setting(&req.key, &value) {
        // Echo the fresh state so the UI repaints from the source of truth.
        Ok(()) => ("200 OK", "application/json", settings_payload()),
        Err(e) => ("400 Bad Request", "application/json", json_err(&e)),
    }
}

/// Coerce a JSON setting value into the canonical string the setters expect.
/// Booleans (the `structure_first` toggle) become `"true"`/`"false"`.
fn normalize_value(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.trim().to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Validate `key`/`value` against the fixed allow-list. Pure (no disk I/O) so it
/// is unit-testable and acts as the first of two validation gates.
fn validate_setting(key: &str, value: &str) -> Result<(), String> {
    match key {
        "compression_level" => ensure_in(value, COMPRESSION_OPTIONS),
        "terse_agent" => ensure_in(value, TERSE_OPTIONS),
        "tool_profile" => ensure_in(value, TOOL_PROFILE_OPTIONS),
        "structure_first" => ensure_bool(value),
        _ => Err(format!("unknown or non-editable setting: '{key}'")),
    }
}

fn apply_setting(key: &str, value: &str) -> Result<(), String> {
    validate_setting(key, value)?;
    match key {
        "compression_level" => apply_compression(value),
        "tool_profile" => apply_tool_profile(value),
        "structure_first" => {
            crate::core::config::setter::set_by_key("structure_first", value).map(|_| ())
        }
        "terse_agent" => apply_terse_agent(value),
        // validate_setting already rejected anything else.
        _ => Err(format!("unknown or non-editable setting: '{key}'")),
    }
}

/// Mirror a `terse_agent` change: persist it *and* re-inject the agent rules.
/// `terse_agent` is a legacy input to `CompressionLevel::effective`, and the
/// injected rules are derived from that effective level — so without a re-inject
/// the change would not reach the agent (and the UI footer's "terse changes
/// re-inject the agent rules" claim would be false).
fn apply_terse_agent(value: &str) -> Result<(), String> {
    crate::core::config::setter::set_by_key("terse_agent", value)?;
    let cfg = Config::load();
    let _ = crate::core::terse::rules_inject::inject(&CompressionLevel::effective(&cfg));
    Ok(())
}

/// Mirror `lean-ctx compression <level>`: persist the level *and* re-inject the
/// compression prompt into the agent rules files so the change actually lands.
fn apply_compression(value: &str) -> Result<(), String> {
    let level = CompressionLevel::from_str_label(value)
        .ok_or_else(|| format!("invalid compression level '{value}'"))?;
    let cfg = Config::update_global(move |c| c.compression_level = level)
        .map_err(|e| format!("Error saving config: {e}"))?;
    let _ = crate::core::terse::rules_inject::inject(&cfg.compression_level);
    Ok(())
}

/// Mirror `lean-ctx tools <profile>`: pin minimal/standard/power, or unpin on
/// `lean` (clear the key so the default unpinned behaviour returns).
fn apply_tool_profile(value: &str) -> Result<(), String> {
    match value {
        "minimal" | "standard" | "power" => {
            crate::core::tool_profiles::set_profile_in_config(value)
        }
        "lean" => crate::core::tool_profiles::clear_profile_in_config(),
        other => Err(format!("invalid tool profile '{other}'")),
    }
}

fn ensure_in(value: &str, allowed: &[&str]) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "invalid value '{value}'. Allowed: {}",
            allowed.join(", ")
        ))
    }
}

fn ensure_bool(value: &str) -> Result<(), String> {
    match value {
        "true" | "false" | "1" | "0" | "yes" | "no" | "on" | "off" => Ok(()),
        _ => Err(format!("expected a boolean, got '{value}'")),
    }
}

/// Snapshot of the four settings as the UI needs them: the active value, the
/// selectable options, and whether an environment variable or a project-local
/// `.lean-ctx.toml` currently overrides the persisted config (so the UI can warn
/// that a toggle won't take effect until that source is removed).
///
/// GH #450: the top-level `config_path`/`config_exists`/`parse_error` fields let
/// the UI show *which* `config.toml` is being read — the missing piece that made
/// "my settings keep resetting" undiagnosable. `local_override` mirrors the keys
/// `Config::merge_local` honors (compression/terse/tool profile; `structure_first`
/// is never merged from local config, so it carries no `local_override`).
fn settings_payload() -> String {
    let cfg = Config::load();
    let prov = Config::provenance();
    let local = |key: &str| prov.local_overrides(key);
    let payload = serde_json::json!({
        "config_path": prov.config_path.as_ref().map(|p| p.display().to_string()),
        "config_exists": prov.config_exists,
        "parse_error": prov.parse_error,
        "settings": {
            "compression_level": {
                "value": compression_canon(&CompressionLevel::effective(&cfg)),
                "options": COMPRESSION_OPTIONS,
                "env_override": env_present("LEAN_CTX_COMPRESSION"),
                "local_override": local("compression_level"),
            },
            "tool_profile": {
                "value": tool_profile_value(&cfg),
                "options": TOOL_PROFILE_OPTIONS,
                "env_override": env_present("LEAN_CTX_TOOL_PROFILE"),
                "local_override": local("tool_profile"),
            },
            "structure_first": {
                "value": cfg.structure_first_effective(),
                "env_override": env_present("LEAN_CTX_STRUCTURE_FIRST"),
            },
            "terse_agent": {
                "value": terse_canon(&cfg.terse_agent),
                "options": TERSE_OPTIONS,
                "env_override": env_present("LEAN_CTX_TERSE_AGENT"),
                "local_override": local("terse_agent"),
            },
        }
    });
    payload.to_string()
}

fn env_present(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

fn compression_canon(level: &CompressionLevel) -> &'static str {
    match level {
        CompressionLevel::Off => "off",
        CompressionLevel::Lite => "lite",
        CompressionLevel::Standard => "standard",
        CompressionLevel::Max => "max",
    }
}

fn terse_canon(t: &TerseAgent) -> &'static str {
    match t {
        TerseAgent::Off => "off",
        TerseAgent::Lite => "lite",
        TerseAgent::Full => "full",
        TerseAgent::Ultra => "ultra",
    }
}

fn tool_profile_canon(p: &ToolProfile) -> &'static str {
    match p {
        ToolProfile::Minimal => "minimal",
        ToolProfile::Standard => "standard",
        ToolProfile::Power => "power",
        ToolProfile::Custom(_) => "custom",
    }
}

/// Canonical value for the dashboard's tool-profile toggle.
///
/// The unpinned default and an explicit `power` pin both resolve to
/// `ToolProfile::Power` internally, so reporting the *effective* profile made
/// the UI snap "Lean" back to "Power" the instant it was selected (#431). This
/// mirrors `ToolProfile::from_config`'s precedence but maps the unpinned state
/// to the `lean` sentinel the UI understands.
fn tool_profile_value(cfg: &Config) -> &'static str {
    // An env override wins and is surfaced separately via `env_override`.
    if env_present("LEAN_CTX_TOOL_PROFILE") {
        return tool_profile_canon(&cfg.tool_profile_effective());
    }
    // A real pin (minimal/standard/power) takes precedence. Unpin aliases
    // (`lean`/`lazy`/`reset`) and unknown literals fail to parse and fall
    // through to the same default resolution `from_config` uses.
    if let Some(name) = cfg.tool_profile.as_deref()
        && let Some(profile) = ToolProfile::parse(name)
    {
        return tool_profile_canon(&profile);
    }
    if !cfg.tools_enabled.is_empty() {
        return "custom";
    }
    "lean"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_known_enum_values() {
        assert!(validate_setting("compression_level", "max").is_ok());
        assert!(validate_setting("terse_agent", "ultra").is_ok());
        assert!(validate_setting("tool_profile", "power").is_ok());
        assert!(validate_setting("tool_profile", "lean").is_ok());
        assert!(validate_setting("structure_first", "true").is_ok());
        assert!(validate_setting("structure_first", "false").is_ok());
    }

    #[test]
    fn validate_rejects_bad_values() {
        assert!(validate_setting("compression_level", "ultra").is_err());
        assert!(validate_setting("terse_agent", "max").is_err());
        assert!(validate_setting("tool_profile", "turbo").is_err());
        assert!(validate_setting("structure_first", "maybe").is_err());
    }

    #[test]
    fn validate_rejects_unknown_or_dangerous_keys() {
        assert!(validate_setting("proxy.anthropic_upstream", "http://evil").is_err());
        assert!(validate_setting("allow_paths", "/etc").is_err());
        assert!(validate_setting("", "x").is_err());
    }

    #[test]
    fn normalize_bool_and_string_values() {
        assert_eq!(
            normalize_value(&serde_json::json!(true)).as_deref(),
            Some("true")
        );
        assert_eq!(
            normalize_value(&serde_json::json!(false)).as_deref(),
            Some("false")
        );
        assert_eq!(
            normalize_value(&serde_json::json!("max")).as_deref(),
            Some("max")
        );
        assert_eq!(
            normalize_value(&serde_json::json!("  power  ")).as_deref(),
            Some("power")
        );
        assert!(normalize_value(&serde_json::json!(42)).is_none());
        assert!(normalize_value(&serde_json::json!(null)).is_none());
    }

    /// GH #431: the unpinned default and a real `power` pin both resolve to
    /// `ToolProfile::Power`, but the UI must tell them apart so selecting "Lean"
    /// does not snap back to "Power".
    #[test]
    fn tool_profile_value_distinguishes_lean_from_power() {
        // Avoid env interference from the host running the suite.
        crate::test_env::remove_var("LEAN_CTX_TOOL_PROFILE");

        let unpinned = Config {
            tool_profile: None,
            tools_enabled: vec![],
            ..Default::default()
        };
        assert_eq!(tool_profile_value(&unpinned), "lean");

        // A persisted unpin alias self-heals to lean instead of "power".
        let aliased = Config {
            tool_profile: Some("lean".into()),
            ..Default::default()
        };
        assert_eq!(tool_profile_value(&aliased), "lean");

        let pinned = Config {
            tool_profile: Some("power".into()),
            ..Default::default()
        };
        assert_eq!(tool_profile_value(&pinned), "power");

        let minimal = Config {
            tool_profile: Some("minimal".into()),
            ..Default::default()
        };
        assert_eq!(tool_profile_value(&minimal), "minimal");

        let custom = Config {
            tool_profile: None,
            tools_enabled: vec!["ctx_read".into()],
            ..Default::default()
        };
        assert_eq!(tool_profile_value(&custom), "custom");
    }

    #[test]
    fn payload_is_valid_json_with_all_four_settings() {
        let raw = settings_payload();
        let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        let s = &v["settings"];
        for key in [
            "compression_level",
            "tool_profile",
            "structure_first",
            "terse_agent",
        ] {
            assert!(s.get(key).is_some(), "missing setting {key}");
            assert!(
                s[key].get("env_override").is_some(),
                "missing env_override for {key}"
            );
        }
    }

    /// GH #450: the payload must carry the resolved config provenance so the UI
    /// can show *which* config.toml is read and warn on a project-local override.
    #[test]
    fn payload_exposes_config_provenance() {
        let raw = settings_payload();
        let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");

        assert!(v.get("config_path").is_some(), "missing config_path");
        assert!(
            v.get("config_exists")
                .is_some_and(serde_json::Value::is_boolean),
            "config_exists must be a bool"
        );
        assert!(v.get("parse_error").is_some(), "missing parse_error key");

        let s = &v["settings"];
        // The three locally-mergeable settings expose local_override…
        for key in ["compression_level", "tool_profile", "terse_agent"] {
            assert!(
                s[key]
                    .get("local_override")
                    .is_some_and(serde_json::Value::is_boolean),
                "missing local_override bool for {key}"
            );
        }
        // …structure_first is never merged from local config, so it has none.
        assert!(
            s["structure_first"].get("local_override").is_none(),
            "structure_first must not carry local_override"
        );
    }
}
