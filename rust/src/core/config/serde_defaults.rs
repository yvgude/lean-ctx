//! Serde helpers and `#[serde(default = "...")]` fns for `Config`.

use serde::Deserialize;

use super::TeeMode;

pub(super) fn deserialize_tee_mode<'de, D>(deserializer: D) -> Result<TeeMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(deserializer)?;
    match &v {
        serde_json::Value::Bool(true) => Ok(TeeMode::Failures),
        serde_json::Value::Bool(false) => Ok(TeeMode::Never),
        serde_json::Value::String(s) => match s.as_str() {
            "never" => Ok(TeeMode::Never),
            "failures" => Ok(TeeMode::Failures),
            "highcompression" | "high_compression" => Ok(TeeMode::HighCompression),
            "always" => Ok(TeeMode::Always),
            other => Err(D::Error::custom(format!("unknown tee_mode: {other}"))),
        },
        _ => Err(D::Error::custom("tee_mode must be string or bool")),
    }
}

pub(super) fn default_theme() -> String {
    "default".to_string()
}

/// Default compact output formats preserved verbatim instead of recompressed (#342).
pub(super) fn default_preserve_compact_formats() -> Vec<String> {
    vec!["toon".to_string()]
}

pub(super) fn default_buddy_enabled() -> bool {
    true
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn default_bm25_max_cache_mb() -> u64 {
    128
}

pub(super) fn default_graph_index_max_files() -> u64 {
    0 // 0 = unlimited (safety enforced by scan timeout + entry visitor limit)
}

pub(super) fn default_max_ram_percent() -> u8 {
    5
}

pub(super) fn default_cognition_loop_interval() -> u64 {
    3600
}

pub(super) fn default_cognition_loop_max_steps() -> u8 {
    9
}

pub(super) fn default_cognition_synthesis_min_cluster() -> usize {
    3
}
