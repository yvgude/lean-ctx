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
    pub fn load() -> Self {
        let mut p = Self::embedded();
        p.apply_env_override();
        p
    }

    pub fn embedded() -> Self {
        let mut models: HashMap<String, ModelCost> = HashMap::new();

        // Anthropic prompt caching pricing (public, GA) — source: https://anthropic.com/news/prompt-caching
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

    pub fn quote(&self, model: Option<&str>) -> ModelQuote {
        let raw = model.unwrap_or_default();
        if let Some(k) = Self::infer_model_key(raw) {
            if let Some(cost) = self.models.get(&k).copied() {
                return ModelQuote {
                    model_key: k,
                    cost,
                    match_kind: PricingMatchKind::Exact,
                };
            }
        }

        if let Some((k, kind)) = Self::heuristic_key(raw) {
            if let Some(cost) = self.models.get(&k).copied() {
                return ModelQuote {
                    model_key: k,
                    cost,
                    match_kind: kind,
                };
            }
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

    pub fn quote_from_env_or_agent_type(&self, agent_type: &str) -> ModelQuote {
        let env_model = std::env::var("LEAN_CTX_MODEL")
            .or_else(|_| std::env::var("LCTX_MODEL"))
            .ok();
        self.quote(env_model.as_deref().or(Some(agent_type)))
    }

    pub fn infer_model_key(model: &str) -> Option<String> {
        let m = normalize(model);
        if m.is_empty() {
            return None;
        }

        let exact_keys = [
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
        if m.contains("claude") {
            if m.contains("sonnet") {
                return Some(("claude-3.5-sonnet".to_string(), PricingMatchKind::Heuristic));
            }
            if m.contains("opus") {
                return Some(("claude-3-opus".to_string(), PricingMatchKind::Heuristic));
            }
            if m.contains("haiku") {
                return Some(("claude-3-haiku".to_string(), PricingMatchKind::Heuristic));
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
    fn claude_sonnet_heuristic() {
        let p = ModelPricing::embedded();
        let q = p.quote(Some("claude-4.6-sonnet"));
        assert!(matches!(
            q.match_kind,
            PricingMatchKind::Heuristic | PricingMatchKind::Alias
        ));
        assert_eq!(q.model_key, "claude-3.5-sonnet");
    }
}
