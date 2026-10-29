//! Configuration for proxy response shaping (#1125).

use serde::{Deserialize, Serialize};

/// Response-shaping configuration (`[response_shaping]`).
///
/// The proxy applies the patterns of the selected `mode`. The former
/// `preamble`, `code_repetition`, `narration` and `confirmation` sub-sections
/// were never read; they are retired and ignored when an older config loads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ResponseShapingConfig {
    /// Enable proxy response shaping. Defaults to the conservative gentle mode.
    pub enabled: bool,
    /// `off`, `gentle`, or `aggressive`.
    pub mode: String,
}

impl Default for ResponseShapingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: "gentle".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_sub_sections_still_load() {
        let cfg: ResponseShapingConfig = toml::from_str(
            r#"
            enabled = true
            mode = "aggressive"
            [preamble]
            max_pattern_length = 42
            [code_repetition]
            enabled = true
            [narration]
            max_summary_tokens = 24
            [confirmation]
            strip_trailing = false
            "#,
        )
        .expect("response-shaping TOML");
        assert_eq!(cfg.mode, "aggressive");
        assert!(cfg.enabled);
    }
}
