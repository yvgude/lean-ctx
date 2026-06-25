use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelCost {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_write_per_m: f64,
    pub cache_read_per_m: f64,
}

impl ModelCost {
    #[must_use]
    pub fn estimate_usd(&self, input: u64, output: u64, cache_write: u64, cache_read: u64) -> f64 {
        (input as f64 / 1_000_000.0 * self.input_per_m)
            + (output as f64 / 1_000_000.0 * self.output_per_m)
            + (cache_write as f64 / 1_000_000.0 * self.cache_write_per_m)
            + (cache_read as f64 / 1_000_000.0 * self.cache_read_per_m)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PricingMatchKind {
    Exact,
    Alias,
    Heuristic,
    Fallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelQuote {
    pub model_key: String,
    pub cost: ModelCost,
    pub match_kind: PricingMatchKind,
}

#[derive(Debug, Clone)]
pub struct ModelPricing {
    models: HashMap<String, ModelCost>,
}

impl ModelPricing {
    #[must_use]
    pub fn load() -> Self {
        let mut p = Self::embedded();
        p.apply_env_override();
        p
    }

    #[must_use]
    pub fn embedded() -> Self {
        let mut models: HashMap<String, ModelCost> = HashMap::new();

        // Anthropic pricing — source: https://platform.claude.com/docs/en/about-claude/pricing
        // (June 2026). One entry per price tier; the 4.5 keys cover the whole
        // 4.5–4.8 generation since Anthropic prices them identically.
        models.insert(
            "claude-fable-5".to_string(),
            ModelCost {
                input_per_m: 10.00,
                output_per_m: 50.00,
                cache_write_per_m: 12.50,
                cache_read_per_m: 1.00,
            },
        );
        models.insert(
            "claude-opus-4.5".to_string(),
            ModelCost {
                input_per_m: 5.00,
                output_per_m: 25.00,
                cache_write_per_m: 6.25,
                cache_read_per_m: 0.50,
            },
        );
        models.insert(
            "claude-sonnet-4.5".to_string(),
            ModelCost {
                input_per_m: 3.00,
                output_per_m: 15.00,
                cache_write_per_m: 3.75,
                cache_read_per_m: 0.30,
            },
        );
        models.insert(
            "claude-haiku-4.5".to_string(),
            ModelCost {
                input_per_m: 1.00,
                output_per_m: 5.00,
                cache_write_per_m: 1.25,
                cache_read_per_m: 0.10,
            },
        );
        // Legacy Claude 3.x tiers (still seen in older configs/logs).
        models.insert(
            "claude-3.5-sonnet".to_string(),
            ModelCost {
                input_per_m: 3.00,
                output_per_m: 15.00,
                cache_write_per_m: 3.75,
                cache_read_per_m: 0.30,
            },
        );
        models.insert(
            "claude-3-opus".to_string(),
            ModelCost {
                input_per_m: 15.00,
                output_per_m: 75.00,
                cache_write_per_m: 18.75,
                cache_read_per_m: 1.50,
            },
        );
        models.insert(
            "claude-3-haiku".to_string(),
            ModelCost {
                input_per_m: 0.25,
                output_per_m: 1.25,
                cache_write_per_m: 0.30,
                cache_read_per_m: 0.03,
            },
        );

        // OpenAI API pricing (Flagship) — source: https://openai.com/api/pricing/
        models.insert(
            "gpt-5.4".to_string(),
            ModelCost {
                input_per_m: 2.50,
                output_per_m: 15.00,
                cache_write_per_m: 2.50,
                cache_read_per_m: 0.25,
            },
        );
        models.insert(
            "gpt-5.4-mini".to_string(),
            ModelCost {
                input_per_m: 0.75,
                output_per_m: 4.50,
                cache_write_per_m: 0.75,
                cache_read_per_m: 0.075,
            },
        );
        models.insert(
            "gpt-5.4-nano".to_string(),
            ModelCost {
                input_per_m: 0.20,
                output_per_m: 1.25,
                cache_write_per_m: 0.20,
                cache_read_per_m: 0.02,
            },
        );

        // Google Gemini API pricing — source: https://ai.google.dev/pricing
        // (No separate cache pricing published → treat cache read/write as input.)
        models.insert(
            "gemini-2.5-pro".to_string(),
            ModelCost {
                input_per_m: 1.25,
                output_per_m: 10.00,
                cache_write_per_m: 1.25,
                cache_read_per_m: 1.25,
            },
        );
        models.insert(
            "gemini-2.5-flash".to_string(),
            ModelCost {
                input_per_m: 0.30,
                output_per_m: 2.50,
                cache_write_per_m: 0.30,
                cache_read_per_m: 0.30,
            },
        );
        models.insert(
            "gemini-2.5-flash-lite".to_string(),
            ModelCost {
                input_per_m: 0.10,
                output_per_m: 0.40,
                cache_write_per_m: 0.10,
                cache_read_per_m: 0.10,
            },
        );

        // Conservative blended fallback (used by legacy stats output).
        models.insert(
            "fallback-blended".to_string(),
            ModelCost {
                input_per_m: 2.50,
                output_per_m: 10.00,
                cache_write_per_m: 2.50,
                cache_read_per_m: 2.50,
            },
        );

        Self { models }
    }

    #[must_use]
    pub fn quote(&self, model: Option<&str>) -> ModelQuote {
        let raw = model.unwrap_or_default();
        if let Some(k) = Self::infer_model_key(raw)
            && let Some(cost) = self.models.get(&k).copied()
        {
            return ModelQuote {
                model_key: k,
                cost,
                match_kind: PricingMatchKind::Exact,
            };
        }

        if let Some((k, kind)) = Self::heuristic_key(raw)
            && let Some(cost) = self.models.get(&k).copied()
        {
            return ModelQuote {
                model_key: k,
                cost,
                match_kind: kind,
            };
        }

        let cost = self
            .models
            .get("fallback-blended")
            .copied()
            .unwrap_or(ModelCost {
                input_per_m: 2.50,
                output_per_m: 10.00,
                cache_write_per_m: 2.50,
                cache_read_per_m: 2.50,
            });
        ModelQuote {
            model_key: "fallback-blended".to_string(),
            cost,
            match_kind: PricingMatchKind::Fallback,
        }
    }

    /// Resolves a pricing model for a client/agent, then quotes it. Resolution
    /// order: `LEAN_CTX_MODEL`/`LCTX_MODEL` env → `[cost.models]` entry →
    /// `[cost] default_model` → the client/agent string as a heuristic hint →
    /// blended fallback (inside [`ModelPricing::quote`]). This is what lets
    /// MCP-only IDEs (Cursor, Copilot, …) be priced with a declared model.
    #[must_use]
    pub fn quote_for_client(&self, client: &str) -> ModelQuote {
        self.quote(Some(&resolve_model_for_client(client)))
    }

    /// Back-compat alias for [`ModelPricing::quote_for_client`]; now also honors
    /// the `[cost]` config, not just the env override.
    #[must_use]
    pub fn quote_from_env_or_agent_type(&self, agent_type: &str) -> ModelQuote {
        self.quote_for_client(agent_type)
    }

    #[must_use]
    pub fn infer_model_key(model: &str) -> Option<String> {
        let m = normalize(model);
        if m.is_empty() {
            return None;
        }

        let exact_keys = [
            "claude-fable-5",
            "claude-opus-4.5",
            "claude-sonnet-4.5",
            "claude-haiku-4.5",
            "claude-3.5-sonnet",
            "claude-3-opus",
            "claude-3-haiku",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            "fallback-blended",
        ];
        for k in exact_keys {
            if m == k {
                return Some(k.to_string());
            }
        }
        None
    }

    fn heuristic_key(model: &str) -> Option<(String, PricingMatchKind)> {
        let m = normalize(model);
        if m.is_empty() {
            return None;
        }

        // Claude family: accept loose naming (e.g. "claude sonnet", "claude-4.6-sonnet").
        // 3.x names map to legacy tiers; everything else gets the current
        // generation's price — defaulting to 3.x would overstate Opus cost 3×.
        if m.contains("claude") || m.contains("fable") || m.contains("mythos") {
            let legacy = m.contains("claude-3");
            if m.contains("fable") || m.contains("mythos") {
                return Some(("claude-fable-5".to_string(), PricingMatchKind::Heuristic));
            }
            if m.contains("sonnet") {
                return Some(if legacy {
                    ("claude-3.5-sonnet".to_string(), PricingMatchKind::Heuristic)
                } else {
                    ("claude-sonnet-4.5".to_string(), PricingMatchKind::Heuristic)
                });
            }
            if m.contains("opus") {
                return Some(if legacy {
                    ("claude-3-opus".to_string(), PricingMatchKind::Heuristic)
                } else {
                    ("claude-opus-4.5".to_string(), PricingMatchKind::Heuristic)
                });
            }
            if m.contains("haiku") {
                return Some(if legacy {
                    ("claude-3-haiku".to_string(), PricingMatchKind::Heuristic)
                } else {
                    ("claude-haiku-4.5".to_string(), PricingMatchKind::Heuristic)
                });
            }
        }

        if m.contains("gemini") {
            if m.contains("2.5") && m.contains("pro") {
                return Some(("gemini-2.5-pro".to_string(), PricingMatchKind::Heuristic));
            }
            if m.contains("2.5") && m.contains("flash-lite") {
                return Some((
                    "gemini-2.5-flash-lite".to_string(),
                    PricingMatchKind::Heuristic,
                ));
            }
            if m.contains("2.5") && m.contains("flash") {
                return Some(("gemini-2.5-flash".to_string(), PricingMatchKind::Heuristic));
            }
        }

        // OpenAI family: accept "gpt-5.4" variants and legacy "gpt-4o" as alias to blended fallback.
        if m.contains("gpt-5.4") && m.contains("mini") {
            return Some(("gpt-5.4-mini".to_string(), PricingMatchKind::Alias));
        }
        if m.contains("gpt-5.4") && m.contains("nano") {
            return Some(("gpt-5.4-nano".to_string(), PricingMatchKind::Alias));
        }
        if m.contains("gpt-5.4") {
            return Some(("gpt-5.4".to_string(), PricingMatchKind::Alias));
        }
        if m.contains("gpt-4o") {
            return Some(("fallback-blended".to_string(), PricingMatchKind::Heuristic));
        }

        None
    }

    fn apply_env_override(&mut self) {
        let raw = std::env::var("LEAN_CTX_MODEL_PRICING_JSON")
            .or_else(|_| std::env::var("LCTX_MODEL_PRICING_JSON"))
            .ok();
        let Some(raw) = raw else { return };

        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };
        let Some(models) = v.get("models").and_then(|m| m.as_object()) else {
            return;
        };
        for (k, vv) in models {
            let Some(obj) = vv.as_object() else { continue };
            let input_per_m = obj.get("input_per_m").and_then(serde_json::Value::as_f64);
            let output_per_m = obj.get("output_per_m").and_then(serde_json::Value::as_f64);
            if input_per_m.is_none() && output_per_m.is_none() {
                continue;
            }

            let key_norm = normalize(k);
            let base = self.models.get(&key_norm).copied().unwrap_or_else(|| {
                self.models
                    .get("fallback-blended")
                    .copied()
                    .unwrap_or(ModelCost {
                        input_per_m: 2.50,
                        output_per_m: 10.00,
                        cache_write_per_m: 2.50,
                        cache_read_per_m: 2.50,
                    })
            });

            let merged = ModelCost {
                input_per_m: input_per_m.unwrap_or(base.input_per_m),
                output_per_m: output_per_m.unwrap_or(base.output_per_m),
                cache_write_per_m: obj
                    .get("cache_write_per_m")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(base.cache_write_per_m),
                cache_read_per_m: obj
                    .get("cache_read_per_m")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(base.cache_read_per_m),
            };
            self.models.insert(key_norm, merged);
        }
    }
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase().replace(' ', "-")
}

fn non_blank(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Pure model resolution: env override → configured model → client hint.
/// Split out for deterministic testing without touching global config/env.
fn resolve_model(client: &str, env_model: Option<&str>, configured: Option<&str>) -> String {
    env_model
        .and_then(non_blank)
        .or_else(|| configured.and_then(non_blank))
        .unwrap_or_else(|| client.to_string())
}

/// Resolves the pricing model id for a client/agent: the `LEAN_CTX_MODEL`/
/// `LCTX_MODEL` env override wins, then the `[cost]` config
/// (`models[client]` → `default_model`), then the client/agent string itself.
/// The returned string is fed to [`ModelPricing::quote`] for the actual price.
#[must_use]
pub fn resolve_model_for_client(client: &str) -> String {
    let env_model = std::env::var("LEAN_CTX_MODEL")
        .or_else(|_| std::env::var("LCTX_MODEL"))
        .ok();
    let configured = crate::core::config::Config::load()
        .cost
        .model_for_client(client);
    resolve_model(client, env_model.as_deref(), configured.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_falls_back() {
        let p = ModelPricing::embedded();
        let q = p.quote(Some("unknown-model"));
        assert_eq!(q.match_kind, PricingMatchKind::Fallback);
    }

    #[test]
    fn claude_sonnet_heuristic_maps_to_current_generation() {
        let p = ModelPricing::embedded();
        let q = p.quote(Some("claude-4.6-sonnet"));
        assert!(matches!(
            q.match_kind,
            PricingMatchKind::Heuristic | PricingMatchKind::Alias
        ));
        assert_eq!(q.model_key, "claude-sonnet-4.5");
        assert!((q.cost.input_per_m - 3.00).abs() < f64::EPSILON);
    }

    #[test]
    fn claude_legacy_names_keep_legacy_pricing() {
        let p = ModelPricing::embedded();
        let q = p.quote(Some("claude-3-opus"));
        assert_eq!(q.model_key, "claude-3-opus");
        assert!((q.cost.input_per_m - 15.00).abs() < f64::EPSILON);
    }

    #[test]
    fn claude_opus_current_generation_is_5_per_m() {
        let p = ModelPricing::embedded();
        for name in ["claude-opus-4.8", "claude-4.7-opus", "claude opus"] {
            let q = p.quote(Some(name));
            assert_eq!(q.model_key, "claude-opus-4.5", "for {name}");
            assert!((q.cost.input_per_m - 5.00).abs() < f64::EPSILON);
            assert!((q.cost.output_per_m - 25.00).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn claude_fable_matches_frontier_tier() {
        let p = ModelPricing::embedded();
        let q = p.quote(Some("claude-fable-5-thinking-high"));
        assert_eq!(q.model_key, "claude-fable-5");
        assert!((q.cost.input_per_m - 10.00).abs() < f64::EPSILON);
    }

    #[test]
    fn resolve_model_precedence() {
        // env override wins over everything.
        assert_eq!(
            resolve_model("cursor", Some("gpt-5.4"), Some("claude-opus-4.5")),
            "gpt-5.4"
        );
        // configured model used when no env override.
        assert_eq!(
            resolve_model("cursor", None, Some("claude-opus-4.5")),
            "claude-opus-4.5"
        );
        // client/agent string is the final hint.
        assert_eq!(
            resolve_model("claude-haiku-4.5", None, None),
            "claude-haiku-4.5"
        );
        // blanks are ignored at each level.
        assert_eq!(resolve_model("cursor", Some("  "), Some("  ")), "cursor");
    }
}
