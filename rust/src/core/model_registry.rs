use std::collections::HashMap;
use std::sync::OnceLock;

static BUNDLED_REGISTRY: &str = include_str!("../../data/model_registry.json");

static PARSED_BUNDLED: OnceLock<Registry> = OnceLock::new();
static PARSED_LOCAL: OnceLock<Option<Registry>> = OnceLock::new();

#[derive(Debug, Clone)]
struct ModelEntry {
    context_window: usize,
}

#[derive(Debug, Clone, Default)]
struct Registry {
    models: HashMap<String, ModelEntry>,
    families: HashMap<String, usize>,
}

fn parse_registry(json: &str) -> Option<Registry> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut models = HashMap::new();
    if let Some(obj) = v.get("models").and_then(|m| m.as_object()) {
        for (key, entry) in obj {
            if let Some(window) = entry
                .get("context_window")
                .and_then(serde_json::Value::as_u64)
            {
                models.insert(
                    key.to_lowercase(),
                    ModelEntry {
                        context_window: window as usize,
                    },
                );
            }
        }
    }
    let mut families = HashMap::new();
    if let Some(obj) = v.get("families").and_then(|f| f.as_object()) {
        for (key, val) in obj {
            if let Some(window) = val.as_u64() {
                families.insert(key.to_lowercase(), window as usize);
            }
        }
    }
    Some(Registry { models, families })
}

fn bundled() -> &'static Registry {
    PARSED_BUNDLED.get_or_init(|| parse_registry(BUNDLED_REGISTRY).unwrap_or_default())
}

fn local_registry() -> Option<&'static Registry> {
    PARSED_LOCAL
        .get_or_init(|| {
            let data_dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
            let path = data_dir.join("model_registry.json");
            let content = std::fs::read_to_string(path).ok()?;
            parse_registry(&content)
        })
        .as_ref()
}

fn user_config_override(model: &str) -> Option<usize> {
    let cfg = crate::core::config::Config::load();
    cfg.model_context_windows
        .get(model)
        .or_else(|| cfg.model_context_windows.get(&model.to_lowercase()))
        .copied()
}

fn registry_lookup(model: &str, registry: &Registry) -> Option<usize> {
    let m = model.to_lowercase();

    // Exact match
    if let Some(entry) = registry.models.get(&m) {
        return Some(entry.context_window);
    }

    // Prefix match: "gpt-5.5-0513" should match "gpt-5.5"
    let mut best_match: Option<(usize, usize)> = None; // (key_len, window)
    for (key, entry) in &registry.models {
        if m.starts_with(key.as_str()) && m[key.len()..].starts_with(['-', '_', '.']) || m == *key {
            let key_len = key.len();
            if best_match.is_none_or(|(bl, _)| key_len > bl) {
                best_match = Some((key_len, entry.context_window));
            }
        }
    }
    if let Some((_, window)) = best_match {
        return Some(window);
    }

    // Family match (substring)
    let mut best_family: Option<(usize, usize)> = None;
    for (family, window) in &registry.families {
        if m.contains(family.as_str()) {
            let flen = family.len();
            if best_family.is_none_or(|(bl, _)| flen > bl) {
                best_family = Some((flen, *window));
            }
        }
    }
    best_family.map(|(_, w)| w)
}

/// Look up context window for a model name.
/// Layers: User Config → Local Registry → Bundled Registry → 200k default.
#[must_use]
pub fn context_window_for_model(model: &str) -> usize {
    // Layer 1: User config override
    if let Some(w) = user_config_override(model) {
        return w;
    }

    // Layer 2: Local registry (auto-updated via lean-ctx update)
    if let Some(local) = local_registry()
        && let Some(w) = registry_lookup(model, local)
    {
        return w;
    }

    // Layer 3: Bundled registry (compiled into binary)
    if let Some(w) = registry_lookup(model, bundled()) {
        return w;
    }

    // Fallback
    200_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_registry_parses() {
        let reg = bundled();
        assert!(!reg.models.is_empty());
        assert!(!reg.families.is_empty());
    }

    #[test]
    fn exact_match_gpt55() {
        assert_eq!(context_window_for_model("gpt-5.5"), 1_048_576);
    }

    #[test]
    fn prefix_match_gpt55_variant() {
        assert_eq!(context_window_for_model("gpt-5.5-0513"), 1_048_576);
    }

    #[test]
    fn exact_match_gpt41() {
        assert_eq!(context_window_for_model("gpt-4.1"), 1_047_576);
    }

    #[test]
    fn family_match_gpt5() {
        assert_eq!(context_window_for_model("gpt-5.3-turbo"), 128_000);
    }

    #[test]
    fn family_match_claude() {
        assert_eq!(context_window_for_model("claude-unknown-version"), 200_000);
    }

    #[test]
    fn family_match_gemini() {
        assert_eq!(context_window_for_model("gemini-future-model"), 1_048_576);
    }

    #[test]
    fn unknown_model_returns_default() {
        assert_eq!(
            context_window_for_model("totally-unknown-model-xyz"),
            200_000
        );
    }
}
