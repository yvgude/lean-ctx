//! Cache-safe, cross-provider reasoning-effort control (#834).
//!
//! Operators pin a single reasoning-effort level (`proxy.effort`) that lean-ctx
//! translates to each provider's native parameter. Unlike per-turn "effort
//! routing" — which changes the effort between turns of one conversation and
//! thereby invalidates the provider prompt cache (`OpenAI` lists "changes to
//! reasoning effort" as a cache-invalidation cause; Anthropic breaks message
//! cache breakpoints on thinking-mode/config changes) — this value is a
//! *constant*: identical on every request, so the cached prefix stays
//! byte-stable (#448/#498) and only the model's reasoning depth changes.
//!
//! Safety rules, enforced by every applier:
//! - **Opt-in:** the caller only invokes an applier when `proxy.effort` is set;
//!   off is a strict no-op that preserves the byte-unchanged meter-only path.
//! - **Never override the client:** an effort the client set explicitly is left
//!   untouched, so the request keeps the client's own cache key.
//! - **Never enable reasoning the client didn't ask for:** the Anthropic applier
//!   only dials an *existing* adaptive request, so it never adds thinking tokens
//!   (or a 400) where the client wanted none. `OpenAI` reasoning models always
//!   reason, so setting the level only ever caps/redirects existing reasoning.
//!   The Gemini applier excludes 2.5 *flash-lite* (thinking off by default) for
//!   the same reason, and never sends both `thinkingLevel` and `thinkingBudget`.
//! - **Model-gated:** models that would reject the parameter are skipped, so the
//!   feature can never turn a working request into a 400. Gemini's generation is
//!   read from the URL path (`thinkingLevel` on 3.x, `thinkingBudget` on 2.5).
//! - **Deterministic:** the rewrite is a pure function of `(document, level)`, so
//!   identical requests stay byte-identical across turns.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::config::Effort;

/// `OpenAI` wire vocabulary for an [`Effort`] (`reasoning_effort` /
/// `reasoning.effort`). The gpt-5 / o-series accept `minimal|low|medium|high`.
fn openai_value(effort: Effort) -> &'static str {
    match effort {
        Effort::Minimal => "minimal",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    }
}

/// Anthropic adaptive-thinking effort (`output_config.effort`). Anthropic has no
/// `minimal` level, so it collapses onto `low` (its lowest-thinking level).
fn anthropic_value(effort: Effort) -> &'static str {
    match effort {
        Effort::Minimal | Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    }
}

/// Whether an `OpenAI` model accepts a reasoning-effort parameter. Reasoning
/// models (the o-series and the gpt-5/gpt-6 families, including any `codex`
/// build) do; the non-reasoning `gpt-4*`/`gpt-3*` models and the
/// `*-chat-latest` non-reasoning variants reject it with a 400, so they are
/// excluded. A vendor-prefixed name from an OpenAI-compatible gateway
/// (`openai/gpt-5.5`, `openrouter/openai/o3`) is reduced to its bare model
/// segment first.
#[must_use]
pub fn openai_supports_effort(model: &str) -> bool {
    let bare = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .trim()
        .to_ascii_lowercase();
    if bare.is_empty() || bare.contains("chat") {
        return false;
    }
    bare.starts_with("o1")
        || bare.starts_with("o3")
        || bare.starts_with("o4")
        || bare.starts_with("gpt-5")
        || bare.starts_with("gpt-6")
        || bare.contains("codex")
}

/// Set `reasoning_effort` on an `OpenAI` **Chat Completions** request. No-op when
/// the client already set it or the model is non-reasoning. Returns whether the
/// document changed.
pub fn apply_openai_chat(doc: &mut Value, effort: Effort) -> bool {
    let Some(obj) = doc.as_object_mut() else {
        return false;
    };
    if obj.contains_key("reasoning_effort") {
        return false; // respect the client's explicit value
    }
    let model = obj.get("model").and_then(Value::as_str).unwrap_or_default();
    if !openai_supports_effort(model) {
        return false;
    }
    obj.insert(
        "reasoning_effort".to_string(),
        Value::String(openai_value(effort).to_string()),
    );
    record(Provider::OpenAi);
    true
}

/// Set `reasoning.effort` on an `OpenAI` **Responses** request (nested object).
/// No-op when the client already pinned `reasoning.effort` or the model is
/// non-reasoning. Any other `reasoning.*` fields (e.g. `summary`) are preserved.
pub fn apply_openai_responses(doc: &mut Value, effort: Effort) -> bool {
    let Some(obj) = doc.as_object_mut() else {
        return false;
    };
    let model = obj.get("model").and_then(Value::as_str).unwrap_or_default();
    if !openai_supports_effort(model) {
        return false;
    }
    if obj.get("reasoning").and_then(|r| r.get("effort")).is_some() {
        return false; // respect the client's explicit value
    }
    let reasoning = obj
        .entry("reasoning")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(map) = reasoning.as_object_mut() else {
        return false; // client sent a non-object `reasoning` — leave it alone
    };
    map.insert(
        "effort".to_string(),
        Value::String(openai_value(effort).to_string()),
    );
    record(Provider::OpenAi);
    true
}

/// Set `output_config.effort` on an Anthropic request, but **only** when the
/// client already requested adaptive thinking (`thinking.type == "adaptive"`).
/// That guard means lean-ctx never enables thinking the client didn't ask for
/// (no surprise reasoning cost) and never sends adaptive config to a model that
/// rejects it (the client already proved the model supports it). No-op when the
/// client already set `output_config.effort`.
pub fn apply_anthropic(doc: &mut Value, effort: Effort) -> bool {
    let Some(obj) = doc.as_object_mut() else {
        return false;
    };
    let is_adaptive = obj
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(Value::as_str)
        == Some("adaptive");
    if !is_adaptive {
        return false;
    }
    if obj
        .get("output_config")
        .and_then(|o| o.get("effort"))
        .is_some()
    {
        return false; // respect the client's explicit value
    }
    let output_config = obj
        .entry("output_config")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(map) = output_config.as_object_mut() else {
        return false;
    };
    map.insert(
        "effort".to_string(),
        Value::String(anthropic_value(effort).to_string()),
    );
    record(Provider::Anthropic);
    true
}

/// Gemini's two mutually-exclusive thinking controls. Sending both in one
/// request is a 400, so each applier path sets exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeminiStyle {
    /// `generationConfig.thinkingConfig.thinkingLevel` — Gemini 3.x and later.
    /// A string enum that maps 1:1 onto [`Effort`].
    Level,
    /// `generationConfig.thinkingConfig.thinkingBudget` — Gemini 2.5 (pro/flash).
    /// An integer token budget.
    Budget,
}

/// Map a Gemini model name (from the request URL path) to its thinking-control
/// style. `3.x`+ → `thinkingLevel` (the go-forward API, also used by future
/// majors); `2.5` pro/flash → `thinkingBudget`. Everything else returns `None`
/// so the applier is a safe no-op: 2.5 *flash-lite* (thinking off by default, so
/// a budget would switch on reasoning the client never asked for), `2.0`/`1.5`
/// (no comparable control), and any unknown name (never risk a 400).
fn gemini_style(model: &str) -> Option<GeminiStyle> {
    let bare = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .trim()
        .to_ascii_lowercase();
    let rest = bare.strip_prefix("gemini-")?;
    let major: u32 = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    if major >= 3 {
        Some(GeminiStyle::Level)
    } else if rest.starts_with("2.5") && !rest.contains("flash-lite") {
        Some(GeminiStyle::Budget)
    } else {
        None
    }
}

/// Gemini 3.x `thinkingLevel` for an [`Effort`] — a 1:1 enum mapping.
fn google_level(effort: Effort) -> &'static str {
    match effort {
        Effort::Minimal => "minimal",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    }
}

/// Gemini 2.5 `thinkingBudget` (thinking tokens) for an [`Effort`]. Every value
/// lies in `[512, 24576]`, which is valid for both 2.5 *pro* (128–32768) and
/// *flash* (1–24576), so the applier can never send an out-of-range budget that
/// 400s. A constant per level keeps the request prefix byte-stable across turns.
fn google_budget(effort: Effort) -> i64 {
    match effort {
        Effort::Minimal => 512,
        Effort::Low => 4096,
        Effort::Medium => 8192,
        Effort::High => 24576,
    }
}

/// Set the Gemini thinking control that matches `model`'s generation on a
/// `generateContent` request (`thinkingLevel` for 3.x, `thinkingBudget` for
/// 2.5). `model` comes from the request URL path — Gemini carries it there, not
/// in the body — threaded in by the Google handler. No-op when the model is
/// unknown/excluded, when the client already pinned either thinking field (never
/// override, and never end up with both → 400), or on an unexpected body shape.
pub fn apply_google(doc: &mut Value, effort: Effort, model: Option<&str>) -> bool {
    let Some(style) = model.and_then(gemini_style) else {
        return false;
    };
    let Some(obj) = doc.as_object_mut() else {
        return false;
    };
    let gen_cfg = obj
        .entry("generationConfig")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(gen_cfg) = gen_cfg.as_object_mut() else {
        return false; // client sent a non-object generationConfig — leave it alone
    };
    let tc = gen_cfg
        .entry("thinkingConfig")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(tc) = tc.as_object_mut() else {
        return false;
    };
    if tc.contains_key("thinkingLevel") || tc.contains_key("thinkingBudget") {
        return false; // respect the client's value; never send both fields
    }
    match style {
        GeminiStyle::Level => {
            tc.insert(
                "thinkingLevel".to_string(),
                Value::String(google_level(effort).to_string()),
            );
        }
        GeminiStyle::Budget => {
            tc.insert(
                "thinkingBudget".to_string(),
                Value::Number(google_budget(effort).into()),
            );
        }
    }
    record(Provider::Google);
    true
}

// --- Telemetry -------------------------------------------------------------

/// Provider whose request had an effort level applied.
#[derive(Debug, Clone, Copy)]
enum Provider {
    OpenAi,
    Anthropic,
    Google,
}

static OPENAI_STEERED: AtomicU64 = AtomicU64::new(0);
static ANTHROPIC_STEERED: AtomicU64 = AtomicU64::new(0);
static GOOGLE_STEERED: AtomicU64 = AtomicU64::new(0);

fn record(provider: Provider) {
    match provider {
        Provider::OpenAi => &OPENAI_STEERED,
        Provider::Anthropic => &ANTHROPIC_STEERED,
        Provider::Google => &GOOGLE_STEERED,
    }
    .fetch_add(1, Ordering::Relaxed);
}

/// Point-in-time view of the effort control for `/status`: the active level
/// (so an operator can confirm `proxy.effort` is live) plus how many requests
/// have been steered per provider. Pair the counters with the per-model
/// `reasoning_tokens` the usage meter already records to see the realized
/// output-token savings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffortStats {
    /// Active level: `off` (no-op) or `minimal|low|medium|high`.
    pub mode: String,
    /// `OpenAI` (Chat + Responses) requests steered, cumulative.
    pub openai_steered: u64,
    /// Anthropic requests steered, cumulative.
    pub anthropic_steered: u64,
    /// Gemini requests steered, cumulative.
    pub google_steered: u64,
}

/// Snapshot the counters together with the currently resolved effort level
/// (`None` → `"off"`).
#[must_use]
pub fn snapshot(active: Option<Effort>) -> EffortStats {
    EffortStats {
        mode: active.map_or("off", Effort::label).to_string(),
        openai_steered: OPENAI_STEERED.load(Ordering::Relaxed),
        anthropic_steered: ANTHROPIC_STEERED.load(Ordering::Relaxed),
        google_steered: GOOGLE_STEERED.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_support_detection() {
        // Reasoning models accept the parameter…
        for m in [
            "gpt-5",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5-codex",
            "gpt-5.1-codex-max",
            "o1",
            "o1-mini",
            "o3",
            "o3-mini",
            "o4-mini",
            "openai/gpt-5.5",       // vendor-prefixed (OpenRouter etc.)
            "openrouter/openai/o3", // doubly-prefixed
        ] {
            assert!(
                openai_supports_effort(m),
                "{m} must support reasoning effort"
            );
        }
        // …non-reasoning models (and chat variants) reject it.
        for m in [
            "gpt-4o",
            "gpt-4.1",
            "gpt-4-turbo",
            "gpt-3.5-turbo",
            "gpt-5-chat-latest", // the non-reasoning chat variant
            "gpt-5.1-chat-latest",
            "",
            "   ",
        ] {
            assert!(
                !openai_supports_effort(m),
                "{m:?} must NOT support reasoning effort"
            );
        }
    }

    #[test]
    fn openai_chat_sets_effort_on_reasoning_model() {
        let mut doc = serde_json::json!({"model": "gpt-5.5", "messages": []});
        assert!(apply_openai_chat(&mut doc, Effort::Low));
        assert_eq!(doc["reasoning_effort"], "low");
    }

    #[test]
    fn openai_chat_respects_client_value() {
        let mut doc = serde_json::json!({
            "model": "gpt-5.5", "reasoning_effort": "high", "messages": []
        });
        assert!(
            !apply_openai_chat(&mut doc, Effort::Low),
            "a client-set reasoning_effort must never be overridden"
        );
        assert_eq!(doc["reasoning_effort"], "high");
    }

    #[test]
    fn openai_chat_skips_non_reasoning_model() {
        let mut doc = serde_json::json!({"model": "gpt-4o", "messages": []});
        assert!(
            !apply_openai_chat(&mut doc, Effort::Low),
            "a non-reasoning model must be skipped (would 400)"
        );
        assert!(doc.get("reasoning_effort").is_none());
    }

    #[test]
    fn openai_responses_sets_nested_effort_and_preserves_siblings() {
        let mut doc = serde_json::json!({
            "model": "gpt-5.5",
            "reasoning": {"summary": "auto"},
            "input": []
        });
        assert!(apply_openai_responses(&mut doc, Effort::Medium));
        assert_eq!(doc["reasoning"]["effort"], "medium");
        assert_eq!(
            doc["reasoning"]["summary"], "auto",
            "existing reasoning.* fields must be preserved"
        );
    }

    #[test]
    fn openai_responses_creates_reasoning_object_when_absent() {
        let mut doc = serde_json::json!({"model": "o3", "input": []});
        assert!(apply_openai_responses(&mut doc, Effort::Minimal));
        assert_eq!(doc["reasoning"]["effort"], "minimal");
    }

    #[test]
    fn openai_responses_respects_client_value() {
        let mut doc = serde_json::json!({
            "model": "gpt-5.5", "reasoning": {"effort": "high"}, "input": []
        });
        assert!(!apply_openai_responses(&mut doc, Effort::Low));
        assert_eq!(doc["reasoning"]["effort"], "high");
    }

    #[test]
    fn anthropic_dials_existing_adaptive_request() {
        let mut doc = serde_json::json!({
            "model": "claude-opus-4-8",
            "thinking": {"type": "adaptive"},
            "messages": []
        });
        assert!(apply_anthropic(&mut doc, Effort::Low));
        assert_eq!(doc["output_config"]["effort"], "low");
    }

    #[test]
    fn anthropic_minimal_collapses_to_low() {
        let mut doc = serde_json::json!({
            "thinking": {"type": "adaptive"}, "messages": []
        });
        assert!(apply_anthropic(&mut doc, Effort::Minimal));
        assert_eq!(
            doc["output_config"]["effort"], "low",
            "Anthropic has no `minimal`; it must collapse onto `low`"
        );
    }

    #[test]
    fn anthropic_skips_when_thinking_absent() {
        // The crucial guard: never enable thinking the client didn't ask for.
        let mut doc = serde_json::json!({"model": "claude-opus-4-8", "messages": []});
        assert!(!apply_anthropic(&mut doc, Effort::Low));
        assert!(doc.get("output_config").is_none());
    }

    #[test]
    fn anthropic_skips_non_adaptive_thinking() {
        // Legacy `enabled` thinking is not adaptive → don't add output_config
        // (output_config.effort only pairs with adaptive thinking).
        let mut doc = serde_json::json!({
            "thinking": {"type": "enabled", "budget_tokens": 4096}, "messages": []
        });
        assert!(!apply_anthropic(&mut doc, Effort::Low));
        assert!(doc.get("output_config").is_none());
    }

    #[test]
    fn anthropic_respects_client_value() {
        let mut doc = serde_json::json!({
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "high"},
            "messages": []
        });
        assert!(!apply_anthropic(&mut doc, Effort::Low));
        assert_eq!(doc["output_config"]["effort"], "high");
    }

    #[test]
    fn gemini_style_detection() {
        use GeminiStyle::{Budget, Level};
        // 3.x and later → thinkingLevel (incl. vendor-prefixed + future majors).
        for m in [
            "gemini-3-pro",
            "gemini-3.5-flash",
            "google/gemini-3-pro",
            "gemini-4-pro",
        ] {
            assert_eq!(gemini_style(m), Some(Level), "{m} → Level");
        }
        // 2.5 pro/flash → thinkingBudget.
        for m in [
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "openrouter/google/gemini-2.5-flash",
        ] {
            assert_eq!(gemini_style(m), Some(Budget), "{m} → Budget");
        }
        // Excluded: flash-lite (thinking off by default), older gens, unknowns.
        for m in [
            "gemini-2.5-flash-lite",
            "gemini-2.0-flash",
            "gemini-1.5-pro",
            "gpt-5",
            "",
        ] {
            assert_eq!(gemini_style(m), None, "{m} → None");
        }
    }

    #[test]
    fn google_3x_sets_thinking_level() {
        let mut doc = serde_json::json!({"contents": []});
        assert!(apply_google(&mut doc, Effort::Medium, Some("gemini-3-pro")));
        assert_eq!(
            doc["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "medium"
        );
    }

    #[test]
    fn google_3x_minimal_maps_directly() {
        let mut doc = serde_json::json!({"contents": []});
        assert!(apply_google(
            &mut doc,
            Effort::Minimal,
            Some("gemini-3.5-flash")
        ));
        assert_eq!(
            doc["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "minimal"
        );
    }

    #[test]
    fn google_25_sets_thinking_budget_in_range() {
        let mut doc = serde_json::json!({"contents": []});
        assert!(apply_google(&mut doc, Effort::Low, Some("gemini-2.5-pro")));
        assert_eq!(
            doc["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            4096
        );
    }

    #[test]
    fn google_preserves_existing_generation_config() {
        let mut doc = serde_json::json!({
            "contents": [],
            "generationConfig": {"temperature": 0.2}
        });
        assert!(apply_google(&mut doc, Effort::High, Some("gemini-3-pro")));
        assert_eq!(doc["generationConfig"]["temperature"], 0.2);
        assert_eq!(
            doc["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );
    }

    #[test]
    fn google_skips_flash_lite_to_avoid_enabling_thinking() {
        // flash-lite has thinking OFF by default — adding a budget would switch on
        // reasoning the client never asked for.
        let mut doc = serde_json::json!({"contents": []});
        assert!(!apply_google(
            &mut doc,
            Effort::Low,
            Some("gemini-2.5-flash-lite")
        ));
        assert!(doc.get("generationConfig").is_none());
    }

    #[test]
    fn google_skips_unknown_model_and_missing_model() {
        let mut a = serde_json::json!({"contents": []});
        assert!(!apply_google(&mut a, Effort::Low, Some("gemini-2.0-flash")));
        assert!(a.get("generationConfig").is_none());
        let mut b = serde_json::json!({"contents": []});
        assert!(!apply_google(&mut b, Effort::Low, None));
        assert!(b.get("generationConfig").is_none());
    }

    #[test]
    fn google_respects_client_thinking_level() {
        let mut doc = serde_json::json!({
            "contents": [],
            "generationConfig": {"thinkingConfig": {"thinkingLevel": "high"}}
        });
        assert!(!apply_google(&mut doc, Effort::Low, Some("gemini-3-pro")));
        assert_eq!(
            doc["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );
    }

    #[test]
    fn google_never_sends_both_fields() {
        // Client pinned a (legacy) budget on a 3.x model → adding thinkingLevel
        // would 400. The applier must bail.
        let mut doc = serde_json::json!({
            "contents": [],
            "generationConfig": {"thinkingConfig": {"thinkingBudget": 1024}}
        });
        assert!(!apply_google(&mut doc, Effort::Low, Some("gemini-3-pro")));
        assert!(
            doc["generationConfig"]["thinkingConfig"]
                .get("thinkingLevel")
                .is_none()
        );
    }

    #[test]
    fn google_is_deterministic_across_turns() {
        let mk = || serde_json::json!({"contents": []});
        let (mut a, mut b) = (mk(), mk());
        apply_google(&mut a, Effort::Medium, Some("gemini-3-pro"));
        apply_google(&mut b, Effort::Medium, Some("gemini-3-pro"));
        assert_eq!(
            serde_json::to_vec(&a).unwrap(),
            serde_json::to_vec(&b).unwrap()
        );
    }

    #[test]
    fn snapshot_reports_active_mode() {
        // Counters are process-global (other tests mutate them), so assert only
        // on the self-describing `mode` field that /status exposes.
        assert_eq!(snapshot(None).mode, "off");
        assert_eq!(snapshot(Some(Effort::Minimal)).mode, "minimal");
        assert_eq!(snapshot(Some(Effort::High)).mode, "high");
    }

    #[test]
    fn appliers_are_deterministic_across_turns() {
        // #498/#448: the same request + level must yield byte-identical output,
        // so a constant effort never perturbs the provider cache prefix.
        let mk_chat = || serde_json::json!({"model": "gpt-5.5", "messages": []});
        let (mut a, mut b) = (mk_chat(), mk_chat());
        apply_openai_chat(&mut a, Effort::Low);
        apply_openai_chat(&mut b, Effort::Low);
        assert_eq!(
            serde_json::to_vec(&a).unwrap(),
            serde_json::to_vec(&b).unwrap()
        );

        let mk_anthropic = || serde_json::json!({"thinking": {"type": "adaptive"}, "messages": []});
        let (mut c, mut d) = (mk_anthropic(), mk_anthropic());
        apply_anthropic(&mut c, Effort::Medium);
        apply_anthropic(&mut d, Effort::Medium);
        assert_eq!(
            serde_json::to_vec(&c).unwrap(),
            serde_json::to_vec(&d).unwrap()
        );
    }
}
