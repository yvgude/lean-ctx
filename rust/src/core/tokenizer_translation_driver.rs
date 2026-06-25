use crate::core::profiles::TranslationConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationRulesetV1 {
    Legacy,
    Ascii,
}

#[derive(Debug, Clone)]
pub struct TranslationSelectionV1 {
    pub ruleset: TranslationRulesetV1,
    pub reason_code: String,
    pub reason: String,
    pub model_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TranslationApplyResultV1 {
    pub output: String,
    pub selection: TranslationSelectionV1,
    pub changed: bool,
    pub skipped_json: bool,
}

#[must_use]
pub fn translate_tool_output(text: &str, cfg: &TranslationConfig) -> TranslationApplyResultV1 {
    let model_key = active_model_key_from_env();
    let selection = select_ruleset(cfg, model_key.as_deref());

    if selection.ruleset == TranslationRulesetV1::Legacy {
        return TranslationApplyResultV1 {
            output: text.to_string(),
            selection,
            changed: false,
            skipped_json: false,
        };
    }

    if looks_like_json(text) {
        return TranslationApplyResultV1 {
            output: text.to_string(),
            selection,
            changed: false,
            skipped_json: true,
        };
    }

    let out = translate_text(text, selection.ruleset);
    TranslationApplyResultV1 {
        changed: out != text,
        output: out,
        selection,
        skipped_json: false,
    }
}

#[must_use]
pub fn translate_text(text: &str, ruleset: TranslationRulesetV1) -> String {
    match ruleset {
        TranslationRulesetV1::Legacy => text.to_string(),
        TranslationRulesetV1::Ascii => translate_ascii(text),
    }
}

fn normalize_ruleset(s: &str) -> String {
    s.trim().to_lowercase().replace(['_', ' '], "-")
}

fn active_model_key_from_env() -> Option<String> {
    let raw = std::env::var("LEAN_CTX_MODEL")
        .or_else(|_| std::env::var("LCTX_MODEL"))
        .unwrap_or_default();
    let m = raw.trim();
    if m.is_empty() {
        return None;
    }
    Some(m.to_lowercase().replace(['_', ' '], "-"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelFamilyV1 {
    OpenAiGpt,
    AnthropicClaude,
    GoogleGemini,
    Unknown,
}

fn infer_model_family(model_key: &str) -> ModelFamilyV1 {
    let m = model_key.trim().to_lowercase();
    if m.contains("gpt") || m.contains("openai") {
        return ModelFamilyV1::OpenAiGpt;
    }
    if m.contains("claude") {
        return ModelFamilyV1::AnthropicClaude;
    }
    if m.contains("gemini") {
        return ModelFamilyV1::GoogleGemini;
    }
    ModelFamilyV1::Unknown
}

pub fn select_ruleset(cfg: &TranslationConfig, model_key: Option<&str>) -> TranslationSelectionV1 {
    let model_key = model_key.map(str::trim).filter(|s| !s.is_empty());
    let model_key = model_key.map(std::string::ToString::to_string);

    if !cfg.enabled_effective() {
        return TranslationSelectionV1 {
            ruleset: TranslationRulesetV1::Legacy,
            reason_code: "disabled".to_string(),
            reason: "translation disabled by profile".to_string(),
            model_key,
        };
    }

    let ruleset = normalize_ruleset(cfg.ruleset_effective());
    match ruleset.as_str() {
        "legacy" | "unicode" => TranslationSelectionV1 {
            ruleset: TranslationRulesetV1::Legacy,
            reason_code: "legacy".to_string(),
            reason: "legacy ruleset selected".to_string(),
            model_key,
        },
        "ascii" => TranslationSelectionV1 {
            ruleset: TranslationRulesetV1::Ascii,
            reason_code: "ascii".to_string(),
            reason: "ascii ruleset selected".to_string(),
            model_key,
        },
        "auto" => {
            let family = model_key
                .as_deref()
                .map_or(ModelFamilyV1::Unknown, infer_model_family);
            match family {
                ModelFamilyV1::OpenAiGpt => TranslationSelectionV1 {
                    ruleset: TranslationRulesetV1::Ascii,
                    reason_code: "auto_openai_gpt".to_string(),
                    reason: "auto: OpenAI/GPT tokenizer prefers ASCII over Unicode symbols"
                        .to_string(),
                    model_key,
                },
                _ => TranslationSelectionV1 {
                    ruleset: TranslationRulesetV1::Legacy,
                    reason_code: "auto_unknown".to_string(),
                    reason: "auto: unknown tokenizer family; preserve legacy format".to_string(),
                    model_key,
                },
            }
        }
        other => TranslationSelectionV1 {
            ruleset: TranslationRulesetV1::Legacy,
            reason_code: "unknown_ruleset".to_string(),
            reason: format!("unknown ruleset '{other}'; using legacy"),
            model_key,
        },
    }
}

fn looks_like_json(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if !(t.starts_with('{') || t.starts_with('[')) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(t).is_ok()
}

// Prefer deterministic, minimal symbol substitutions.
const ASCII_SYMBOL_RULES: &[(&str, &str)] = &[
    // Signature/TDD glyphs (empirically expensive on GPT tokenizers)
    ("⊛ ", "+ "),
    ("⊛", "+"),
    ("λ", "fn"),
    ("§", "cl"),
    ("∂", "if"),
    ("τ", "ty"),
    ("ε", "en"),
    ("ν", "val"),
    // Common CRP/TDD symbols
    ("→", "->"),
    ("≠", "!="),
    ("≈", "~"),
    ("∴", "thus"),
    ("✓", "ok"),
    ("✗", "fail"),
    ("⚠", "warn"),
];

fn translate_ascii(text: &str) -> String {
    let mut out = text.to_string();
    for (from, to) in ASCII_SYMBOL_RULES {
        if out.contains(from) {
            out = out.replace(from, to);
        }
    }

    // Apply TokenOptimizer only on synthetic TDD signature lines (verifier-safe).
    let opt = crate::core::neural::token_optimizer::TokenOptimizer::with_defaults();
    let mut changed = false;
    let mut lines: Vec<String> = Vec::new();
    for line in out.lines() {
        if is_synthetic_tdd_signature_line(line) {
            let optimized = opt.optimize_line(line);
            if optimized != line {
                changed = true;
            }
            lines.push(optimized);
        } else {
            lines.push(line.to_string());
        }
    }
    if changed {
        out = lines.join("\n");
    }

    out
}

fn is_synthetic_tdd_signature_line(line: &str) -> bool {
    let mut t = line.trim_start();
    if let Some(rest) = t.strip_prefix('~') {
        t = rest;
    }

    // Unicode TDD signature markers: λ/§/∂/τ/ε/ν + visibility +/-.
    if let Some(first) = t.chars().next()
        && matches!(first, 'λ' | '§' | '∂' | 'τ' | 'ε' | 'ν')
    {
        let mut it = t.chars();
        let _ = it.next();
        if matches!(it.next(), Some('+' | '-')) {
            return true;
        }
    }

    // ASCII translated variants (after symbol mapping).
    let ascii_prefixes = [
        "fn+", "fn-", "cl+", "cl-", "if+", "if-", "ty+", "ty-", "en+", "en-", "val+", "val-",
    ];
    ascii_prefixes.iter().any(|p| t.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn ruleset_disabled_is_legacy() {
        let _lock = env_lock();
        crate::test_env::remove_var("LEAN_CTX_MODEL");
        let cfg = TranslationConfig {
            enabled: Some(false),
            ruleset: Some("auto".to_string()),
        };
        let sel = select_ruleset(&cfg, Some("gpt-5.4"));
        assert_eq!(sel.ruleset, TranslationRulesetV1::Legacy);
        assert!(sel.reason_code.contains("disabled"));
    }

    #[test]
    fn ruleset_ascii_forced() {
        let cfg = TranslationConfig {
            enabled: Some(true),
            ruleset: Some("ascii".to_string()),
        };
        let sel = select_ruleset(&cfg, Some("claude-3.5-sonnet"));
        assert_eq!(sel.ruleset, TranslationRulesetV1::Ascii);
    }

    #[test]
    fn ruleset_auto_openai_gpt() {
        let cfg = TranslationConfig {
            enabled: Some(true),
            ruleset: Some("auto".to_string()),
        };
        let sel = select_ruleset(&cfg, Some("gpt-5.4-mini"));
        assert_eq!(sel.ruleset, TranslationRulesetV1::Ascii);
        assert!(sel.reason_code.contains("auto_openai_gpt"));
    }

    #[test]
    fn ruleset_auto_unknown_falls_back_to_legacy() {
        let cfg = TranslationConfig {
            enabled: Some(true),
            ruleset: Some("auto".to_string()),
        };
        let sel = select_ruleset(&cfg, Some("claude-3.5-sonnet"));
        assert_eq!(sel.ruleset, TranslationRulesetV1::Legacy);
        assert!(sel.reason_code.contains("auto_unknown"));
    }

    #[test]
    fn translation_skips_json_outputs() {
        let _lock = env_lock();
        crate::test_env::set_var("LEAN_CTX_MODEL", "gpt-5.4");
        let cfg = TranslationConfig {
            enabled: Some(true),
            ruleset: Some("auto".to_string()),
        };
        let json = r#"{"ok":"✓","arrow":"→"}"#;
        let r = translate_tool_output(json, &cfg);
        assert!(r.skipped_json);
        assert_eq!(r.output, json);
    }

    #[test]
    fn translation_ascii_converts_signature_markers_and_optimizes_types() {
        let cfg = TranslationConfig {
            enabled: Some(true),
            ruleset: Some("ascii".to_string()),
        };
        let input = "λ+foo(x)→Vec<String>";
        let r = translate_tool_output(input, &cfg);
        assert!(!r.skipped_json);
        assert!(r.output.contains("fn+foo"));
        assert!(r.output.contains("->Vec"));
        assert!(!r.output.contains("λ"));
        assert!(!r.output.contains("→"));
    }
}
