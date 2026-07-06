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

/// Parse a trailing long-context marker like `[1m]` / `[1M]` / `[200k]` into
/// its token window (GH #739). Clients append these suffixes for context-beta
/// variants (e.g. `claude-opus-4-8[1m]`); the marker is an explicit statement
/// of the window, so it wins over any registry entry for the base model.
fn window_from_suffix(model: &str) -> Option<usize> {
    let (_, marker) = split_window_suffix(model)?;
    let digits: String = marker.chars().take_while(char::is_ascii_digit).collect();
    let n: usize = digits.parse().ok()?;
    let unit = &marker[digits.len()..];
    match unit {
        "m" => Some(n.checked_mul(1_000_000)?),
        "k" => Some(n.checked_mul(1_000)?),
        _ => None,
    }
}

/// Split `name[marker]` into `(name, lowercased marker)` when the model ends
/// in a bracketed suffix. Returns `None` for models without one.
fn split_window_suffix(model: &str) -> Option<(&str, String)> {
    let stripped = model.strip_suffix(']')?;
    let open = stripped.rfind('[')?;
    let marker = stripped[open + 1..].to_lowercase();
    if marker.is_empty() {
        return None;
    }
    Some((&model[..open], marker))
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
/// Layers: User Config → `[1m]`-style suffix → Local Registry → Bundled
/// Registry → 200k default.
pub fn context_window_for_model(model: &str) -> usize {
    // Layer 1: User config override ([model_context_windows] in config.toml)
    if let Some(w) = user_config_override(model) {
        return w;
    }

    // Layer 2: explicit window marker in the model name itself (GH #739).
    // `claude-opus-4-8[1m]` means the client runs the 1M-context variant —
    // registries would only ever know the base model's window.
    if let Some(w) = window_from_suffix(model) {
        return w;
    }

    // Registry lookups see the base name so `foo[1m]` variants of unknown
    // markers still match their base entry instead of falling through.
    let base = split_window_suffix(model).map_or(model, |(base, _)| base);

    // Layer 3: Local registry (auto-updated via lean-ctx update)
    if let Some(local) = local_registry()
        && let Some(w) = registry_lookup(base, local)
    {
        return w;
    }

    // Layer 4: Bundled registry (compiled into binary)
    if let Some(w) = registry_lookup(base, bundled()) {
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

    #[test]
    fn long_context_suffix_wins_over_registry() {
        // GH #739: the [1m] marker is the client's explicit window statement.
        assert_eq!(context_window_for_model("claude-opus-4-8[1m]"), 1_000_000);
        assert_eq!(context_window_for_model("claude-opus-4-8[1M]"), 1_000_000);
        assert_eq!(context_window_for_model("some-future-model[200k]"), 200_000);
        assert_eq!(context_window_for_model("gpt-5.5[2m]"), 2_000_000);
    }

    #[test]
    fn base_model_of_suffix_variant_resolves_normally() {
        // Without the marker the registry (exact/prefix/family) decides.
        assert_eq!(context_window_for_model("claude-opus-4-8"), 200_000);
    }

    #[test]
    fn unknown_marker_falls_back_to_base_lookup() {
        // A non-window bracket suffix must not break base-model resolution.
        assert_eq!(context_window_for_model("gpt-5.5[thinking]"), 1_048_576);
    }

    #[test]
    fn suffix_parsing_is_strict() {
        assert_eq!(window_from_suffix("model[1m]"), Some(1_000_000));
        assert_eq!(window_from_suffix("model[128k]"), Some(128_000));
        assert_eq!(window_from_suffix("model[]"), None);
        assert_eq!(window_from_suffix("model[m]"), None);
        assert_eq!(window_from_suffix("model[1x]"), None);
        assert_eq!(window_from_suffix("model"), None);
        assert_eq!(window_from_suffix("model[1m"), None);
    }
}
