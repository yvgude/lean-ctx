use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::llm_feedback::LlmFeedbackEvent;

const POLICY_FILE: &str = "adaptive_mode_policy.json";
const EMA_ALPHA: f64 = 0.2;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdaptiveModePolicyStore {
    pub global: ModePenaltyTable,
    #[serde(default)]
    pub by_intent: HashMap<String, ModePenaltyTable>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModePenaltyTable {
    #[serde(default)]
    pub modes: BTreeMap<String, ModePenalty>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModePenalty {
    pub ema_badness: f64,
    pub samples: u64,
    pub last_ts: Option<String>,
}

impl AdaptiveModePolicyStore {
    pub fn load() -> Self {
        let path = policy_path();
        let Ok(s) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&s).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = policy_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create_dir_all {}: {e}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))?;
        Ok(())
    }

    pub fn reset() -> Result<(), String> {
        let path = policy_path();
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
        }
        Ok(())
    }

    pub fn update_from_feedback(&mut self, ev: &LlmFeedbackEvent) {
        let ratio = ev.llm_output_tokens as f64 / ev.llm_input_tokens.max(1) as f64;
        let mut badness = ((ratio - 1.2) / 1.2).clamp(0.0, 1.0);
        if ev.llm_output_tokens >= 6000 {
            badness = badness.max(0.8);
        } else if ev.llm_output_tokens >= 3000 {
            badness = badness.max(0.5);
        }
        if badness <= 0.0 {
            return;
        }

        let modes = ev.ctx_read_modes.clone().unwrap_or_default();
        if modes.is_empty() {
            if let Some(m) = ev.ctx_read_last_mode.as_ref() {
                if let Some(key) = normalize_mode_key(m) {
                    Self::apply_update(&mut self.global, key, badness, ev.timestamp.as_str());
                    if let Some(k) = normalized_intent_key(ev.intent.as_deref()) {
                        let table = self.by_intent.entry(k).or_default();
                        Self::apply_update(table, key, badness, ev.timestamp.as_str());
                    }
                }
            }
            return;
        }

        let total: u64 = modes.values().sum();
        if total == 0 {
            return;
        }

        for (mode, count) in modes {
            let Some(key) = normalize_mode_key(&mode) else {
                continue;
            };
            let w = count as f64 / total as f64;
            let b = badness * w;
            Self::apply_update(&mut self.global, key, b, ev.timestamp.as_str());
            if let Some(k) = normalized_intent_key(ev.intent.as_deref()) {
                let table = self.by_intent.entry(k).or_default();
                Self::apply_update(table, key, b, ev.timestamp.as_str());
            }
        }
    }

    fn apply_update(table: &mut ModePenaltyTable, mode: &str, badness: f64, ts: &str) {
        let entry = table.modes.entry(mode.to_string()).or_default();
        entry.ema_badness = entry.ema_badness * (1.0 - EMA_ALPHA) + badness * EMA_ALPHA;
        entry.samples = entry.samples.saturating_add(1);
        entry.last_ts = Some(ts.to_string());
    }

    pub fn penalty(&self, intent: Option<&str>, mode: &str) -> f64 {
        let Some(key) = normalize_mode_key(mode) else {
            return 0.0;
        };
        if let Some(k) = normalized_intent_key(intent) {
            if let Some(t) = self.by_intent.get(&k) {
                if let Some(p) = t.modes.get(key) {
                    return p.ema_badness.clamp(0.0, 1.0);
                }
            }
        }
        self.global
            .modes
            .get(key)
            .map_or(0.0, |p| p.ema_badness.clamp(0.0, 1.0))
    }

    pub fn choose_auto_mode(&self, intent: Option<&str>, predicted: &str) -> String {
        let candidates = auto_candidates(predicted);
        let mut best_mode = predicted.to_string();
        let mut best_score = f64::NEG_INFINITY;
        for (i, mode) in candidates.into_iter().enumerate() {
            let base = 1.0 - (i as f64 * 0.05);
            let score = base - self.penalty(intent, &mode);
            if score > best_score {
                best_score = score;
                best_mode = mode;
            }
        }
        best_mode
    }
}

fn mode_group(mode: &str) -> &str {
    match mode {
        "entropy" | "aggressive" => "aggressive",
        other => other,
    }
}

fn normalize_mode_key(mode: &str) -> Option<&str> {
    if mode == "diff" {
        return None;
    }
    if mode.starts_with("lines:") {
        return None;
    }
    Some(mode_group(mode))
}

fn auto_candidates(predicted: &str) -> Vec<String> {
    match predicted {
        "aggressive" | "entropy" => vec![
            "aggressive".to_string(),
            "entropy".to_string(),
            "map".to_string(),
            "signatures".to_string(),
            "full".to_string(),
        ],
        "map" => vec![
            "map".to_string(),
            "signatures".to_string(),
            "full".to_string(),
            "aggressive".to_string(),
        ],
        "signatures" => vec![
            "signatures".to_string(),
            "map".to_string(),
            "full".to_string(),
            "aggressive".to_string(),
        ],
        "full" => vec![
            "full".to_string(),
            "map".to_string(),
            "signatures".to_string(),
            "aggressive".to_string(),
        ],
        other => vec![
            other.to_string(),
            "map".to_string(),
            "signatures".to_string(),
            "full".to_string(),
            "aggressive".to_string(),
        ],
    }
}

fn normalized_intent_key(intent: Option<&str>) -> Option<String> {
    let s = intent?.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_lowercase();
    Some(lower.chars().take(80).collect())
}

fn policy_path() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(POLICY_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn penalty_defaults_to_zero() {
        let p = AdaptiveModePolicyStore::default();
        assert_eq!(p.penalty(None, "aggressive"), 0.0);
    }

    #[test]
    fn choose_auto_avoids_penalized_aggressive() {
        let mut store = AdaptiveModePolicyStore::default();
        AdaptiveModePolicyStore::apply_update(&mut store.global, "aggressive", 1.0, "t");
        let chosen = store.choose_auto_mode(Some("fix bug"), "aggressive");
        assert_ne!(chosen, "aggressive");
        assert_ne!(chosen, "entropy");
    }
}
