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
    /// Exact hit in the live provider price list (OpenRouter models API,
    /// refreshed in the background). Current market data — NOT an estimate.
    Live,
    Alias,
    Heuristic,
    Fallback,
}

impl PricingMatchKind {
    /// True when the priced figure is an estimate (no exact or live price for
    /// the model) and must be surfaced as such, never as a precise number.
    #[must_use]
    pub fn is_estimated(self) -> bool {
        !matches!(self, Self::Exact | Self::Live)
    }
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
        p.apply_config_overrides(&crate::core::config::Config::load().cost.prices);
        p.apply_env_override();
        p
    }

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

        // Azure AI Foundry serverless (Global Standard) — source:
        // https://azure.microsoft.com/en-us/pricing/details/ai-foundry-models/
        // (July 2026). The cheap-OSS tier the gateway router downgrades to
        // (enterprise#14); wrong/missing prices here would overstate savings.
        // Foundry publishes no separate cache price → cache = input rate
        // (same convention as Gemini above).
        models.insert(
            "phi-4".to_string(),
            ModelCost {
                input_per_m: 0.125,
                output_per_m: 0.50,
                cache_write_per_m: 0.125,
                cache_read_per_m: 0.125,
            },
        );
        models.insert(
            "phi-4-mini".to_string(),
            ModelCost {
                input_per_m: 0.075,
                output_per_m: 0.30,
                cache_write_per_m: 0.075,
                cache_read_per_m: 0.075,
            },
        );
        models.insert(
            "deepseek-v3.2".to_string(),
            ModelCost {
                input_per_m: 0.58,
                output_per_m: 1.68,
                cache_write_per_m: 0.58,
                cache_read_per_m: 0.58,
            },
        );
        models.insert(
            "deepseek-v3".to_string(),
            ModelCost {
                input_per_m: 1.14,
                output_per_m: 4.56,
                cache_write_per_m: 1.14,
                cache_read_per_m: 1.14,
            },
        );
        models.insert(
            "llama-3.3-70b".to_string(),
            ModelCost {
                input_per_m: 0.71,
                output_per_m: 0.71,
                cache_write_per_m: 0.71,
                cache_read_per_m: 0.71,
            },
        );
        models.insert(
            "llama-4-maverick".to_string(),
            ModelCost {
                input_per_m: 0.25,
                output_per_m: 1.00,
                cache_write_per_m: 0.25,
                cache_read_per_m: 0.25,
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

    /// Every priced row (embedded table plus operator overrides), for the
    /// cockpit's "Numbers explained" price table.
    pub fn rows(&self) -> Vec<(&str, ModelCost)> {
        self.models.iter().map(|(k, v)| (k.as_str(), *v)).collect()
    }

    pub fn quote(&self, model: Option<&str>) -> ModelQuote {
        let raw = model.unwrap_or_default();
        // Exact = a direct hit in the loaded table (embedded + operator
        // overrides, #1189) — not a fixed key list, so `[cost.prices]` rows
        // with custom names ("internal-llm") price exactly. Dashed API ids
        // (`claude-sonnet-4-5`) retry with dotted versions (`…-4.5`).
        let m = normalize(raw);
        if !m.is_empty() {
            for k in [m.clone(), dot_versions(&m)] {
                if let Some(cost) = self.models.get(&k).copied() {
                    return ModelQuote {
                        model_key: k,
                        cost,
                        match_kind: PricingMatchKind::Exact,
                    };
                }
            }
        }

        // Live provider price list (#1179): exact market prices for models the
        // embedded table doesn't know — checked BEFORE any family heuristic so
        // a new model is never priced by its older, differently-priced kin.
        // No-op unless a run-mode loaded the snapshot (proxy/gateway/spend).
        if let Some((k, cost)) = super::live_pricing::lookup(raw) {
            return ModelQuote {
                model_key: k,
                cost,
                match_kind: PricingMatchKind::Live,
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
    pub fn quote_for_client(&self, client: &str) -> ModelQuote {
        self.quote(Some(&resolve_model_for_client(client)))
    }

    /// Back-compat alias for [`ModelPricing::quote_for_client`]; now also honors
    /// the `[cost]` config, not just the env override.
    pub fn quote_from_env_or_agent_type(&self, agent_type: &str) -> ModelQuote {
        self.quote_for_client(agent_type)
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

        // Foundry OSS families (enterprise#14): deployment names carry suffixes
        // ("Phi-4-reasoning", "DeepSeek-V3-0324", "Llama-3.3-70B-Instruct") —
        // match the family, keep mini/lite variants on their cheaper tier.
        if m.contains("phi-4") {
            return Some(if m.contains("mini") {
                ("phi-4-mini".to_string(), PricingMatchKind::Heuristic)
            } else {
                ("phi-4".to_string(), PricingMatchKind::Heuristic)
            });
        }
        if m.contains("deepseek") {
            return Some(if m.contains("v3.2") {
                ("deepseek-v3.2".to_string(), PricingMatchKind::Heuristic)
            } else {
                ("deepseek-v3".to_string(), PricingMatchKind::Heuristic)
            });
        }
        if m.contains("llama") {
            return Some(if m.contains("maverick") || m.contains("llama-4") {
                ("llama-4-maverick".to_string(), PricingMatchKind::Heuristic)
            } else {
                ("llama-3.3-70b".to_string(), PricingMatchKind::Heuristic)
            });
        }

        None
    }

    /// Merges `[cost.prices]` operator overrides (#1189) into the table as
    /// exact entries. Negotiated enterprise rates override embedded rows and
    /// (via exact-match precedence) every live/heuristic price; only a
    /// provider-measured bill beats them. Rows without any rate are ignored;
    /// omitted fields inherit from the existing row for that key, falling back
    /// to the blended profile — cache rates default to the input rate.
    fn apply_config_overrides(
        &mut self,
        prices: &std::collections::HashMap<String, crate::core::config::PriceOverride>,
    ) {
        for (model, o) in prices {
            if o.input_per_m.is_none()
                && o.output_per_m.is_none()
                && o.cache_write_per_m.is_none()
                && o.cache_read_per_m.is_none()
            {
                continue;
            }
            let key = normalize(model);
            if key.is_empty() {
                continue;
            }
            let base = self.models.get(&key).copied();
            let input = o
                .input_per_m
                .or(base.map(|b| b.input_per_m))
                .unwrap_or(2.50);
            let merged = ModelCost {
                input_per_m: input,
                output_per_m: o
                    .output_per_m
                    .or(base.map(|b| b.output_per_m))
                    .unwrap_or(10.00),
                cache_write_per_m: o
                    .cache_write_per_m
                    .or(base.map(|b| b.cache_write_per_m))
                    .unwrap_or(input),
                cache_read_per_m: o
                    .cache_read_per_m
                    .or(base.map(|b| b.cache_read_per_m))
                    .unwrap_or(input),
            };
            self.models.insert(key, merged);
        }
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

/// Rewrites dashed version tails into dotted ones: `sonnet-4-5` → `sonnet-4.5`.
/// Only digit-digit boundaries are touched, so names like `phi-4-mini` or
/// `llama-4-maverick` stay as they are.
fn dot_versions(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in b.iter().enumerate() {
        if c == b'-'
            && i > 0
            && b[i - 1].is_ascii_digit()
            && b.get(i + 1).is_some_and(u8::is_ascii_digit)
        {
            out.push('.');
        } else {
            out.push(c as char);
        }
    }
    out
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
pub mod tests {
    use super::*;

    #[test]
    fn quote_falls_back() {
        let p = ModelPricing::embedded();
        let q = p.quote(Some("unknown-model"));
        assert_eq!(q.match_kind, PricingMatchKind::Fallback);
    }

    #[test]
    fn live_price_beats_heuristic_but_not_embedded_exact() {
        // #1179: a live-listed model must be priced from the live table (Live),
        // never from a family heuristic; embedded exact matches keep priority.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::core::gain::live_pricing::install(crate::core::gain::live_pricing::LivePriceTable {
            fetched_at: 1,
            models: [
                (
                    "zzz-test/live-only-model".to_string(),
                    ModelCost {
                        input_per_m: 0.07,
                        output_per_m: 0.28,
                        cache_write_per_m: 0.07,
                        cache_read_per_m: 0.007,
                    },
                ),
                (
                    "claude-sonnet-4-5".to_string(),
                    ModelCost {
                        input_per_m: 999.0,
                        output_per_m: 999.0,
                        cache_write_per_m: 999.0,
                        cache_read_per_m: 999.0,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        });

        let p = ModelPricing::embedded();
        let live = p.quote(Some("zzz-test/live-only-model"));
        assert_eq!(live.match_kind, PricingMatchKind::Live);
        assert!(
            !live.match_kind.is_estimated(),
            "live is market data, not a guess"
        );
        assert!((live.cost.input_per_m - 0.07).abs() < 1e-9);

        // Embedded exact match wins over a (bogus) live row for the same key.
        let exact = p.quote(Some("claude-sonnet-4.5"));
        assert_eq!(exact.match_kind, PricingMatchKind::Exact);
        assert!((exact.cost.input_per_m - 3.00).abs() < f64::EPSILON);

        crate::core::gain::live_pricing::clear_for_tests();
        let after = p.quote(Some("zzz-test/live-only-model"));
        assert_eq!(
            after.match_kind,
            PricingMatchKind::Fallback,
            "no snapshot → fallback"
        );
    }

    #[test]
    fn config_price_overrides_are_exact_and_beat_embedded_rows() {
        // #1189: negotiated enterprise rates. A custom model name unknown to
        // any catalog prices exactly; an embedded row is overridden in place.
        let mut p = ModelPricing::embedded();
        let overrides: std::collections::HashMap<String, crate::core::config::PriceOverride> = [
            (
                "internal-llm".to_string(),
                crate::core::config::PriceOverride {
                    input_per_m: Some(0.10),
                    output_per_m: Some(0.40),
                    ..Default::default()
                },
            ),
            (
                "claude-opus-4.5".to_string(),
                crate::core::config::PriceOverride {
                    input_per_m: Some(4.00), // committed-use discount
                    ..Default::default()
                },
            ),
            (
                "empty-row".to_string(),
                crate::core::config::PriceOverride::default(),
            ),
        ]
        .into();
        p.apply_config_overrides(&overrides);

        let custom = p.quote(Some("internal-llm"));
        assert_eq!(custom.match_kind, PricingMatchKind::Exact);
        assert!((custom.cost.input_per_m - 0.10).abs() < 1e-9);
        assert!(
            (custom.cost.cache_read_per_m - 0.10).abs() < 1e-9,
            "omitted cache rates default to the input rate"
        );

        let discounted = p.quote(Some("claude-opus-4.5"));
        assert!((discounted.cost.input_per_m - 4.00).abs() < 1e-9);
        assert!(
            (discounted.cost.output_per_m - 25.00).abs() < 1e-9,
            "omitted fields inherit the embedded row"
        );

        assert_eq!(
            p.quote(Some("empty-row")).match_kind,
            PricingMatchKind::Fallback,
            "a row without any rate is ignored"
        );
    }

    #[test]
    fn dashed_api_ids_hit_their_exact_table_entry() {
        // Anthropic wire ids use dashes ("claude-sonnet-4-5"); the table keys
        // use dots. Same model — must book as Exact list price, not Heuristic.
        let p = ModelPricing::embedded();
        for (api_id, key) in [
            ("claude-sonnet-4-5", "claude-sonnet-4.5"),
            ("claude-opus-4-5", "claude-opus-4.5"),
            ("claude-3-5-sonnet", "claude-3.5-sonnet"),
            ("gemini-2-5-pro", "gemini-2.5-pro"),
        ] {
            let q = p.quote(Some(api_id));
            assert_eq!(q.model_key, key, "{api_id} must map to {key}");
            assert_eq!(
                q.match_kind,
                PricingMatchKind::Exact,
                "{api_id} is the same model as {key} — exact, not heuristic"
            );
        }
        // Dash-digit names that are NOT versions stay untouched.
        let q = p.quote(Some("phi-4-mini"));
        assert_eq!(q.model_key, "phi-4-mini");
        assert_eq!(q.match_kind, PricingMatchKind::Exact);
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
    fn foundry_families_map_deployment_names_to_price_keys() {
        // enterprise#14: Foundry deployment names carry suffixes; the family
        // heuristics must land on the right (cheap) tier — mispricing the
        // downgrade target would corrupt the savings evidence.
        let p = ModelPricing::embedded();
        for (name, key, input) in [
            ("Phi-4", "phi-4", 0.125),
            ("Phi-4-reasoning", "phi-4", 0.125),
            ("Phi-4-mini-instruct", "phi-4-mini", 0.075),
            ("DeepSeek-V3-0324", "deepseek-v3", 1.14),
            ("DeepSeek-V3.2", "deepseek-v3.2", 0.58),
            ("Llama-3.3-70B-Instruct", "llama-3.3-70b", 0.71),
            ("Llama-4-Maverick-17B-128E", "llama-4-maverick", 0.25),
        ] {
            let q = p.quote(Some(name));
            assert_eq!(q.model_key, key, "for {name}");
            assert!(
                (q.cost.input_per_m - input).abs() < f64::EPSILON,
                "for {name}"
            );
            assert_ne!(q.match_kind, PricingMatchKind::Fallback, "for {name}");
        }
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
