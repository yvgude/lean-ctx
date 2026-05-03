use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BanditArm {
    pub name: String,
    pub alpha: f64,
    pub beta: f64,
    pub entropy_threshold: f64,
    pub jaccard_threshold: f64,
    pub budget_ratio: f64,
}

impl BanditArm {
    fn sample(&self) -> f64 {
        beta_sample(self.alpha, self.beta)
    }

    pub fn update_from_feedback(&mut self, outcome: &crate::core::feedback::CompressionOutcome) {
        let efficiency = if outcome.tokens_original > 0 {
            outcome.tokens_saved as f64 / outcome.tokens_original as f64
        } else {
            0.0
        };
        let success = efficiency > 0.3 && outcome.task_completed;
        if success {
            self.update_success();
        } else {
            self.update_failure();
        }
    }

    pub fn update_success(&mut self) {
        self.alpha += 1.0;
    }

    pub fn update_failure(&mut self) {
        self.beta += 1.0;
    }

    pub fn decay(&mut self, factor: f64) {
        self.alpha = (self.alpha * factor).max(1.0);
        self.beta = (self.beta * factor).max(1.0);
    }

    pub fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdBandit {
    pub arms: Vec<BanditArm>,
    pub total_pulls: u64,
}

impl Default for ThresholdBandit {
    fn default() -> Self {
        Self {
            arms: vec![
                BanditArm {
                    name: "conservative".to_string(),
                    alpha: 2.0,
                    beta: 1.0,
                    entropy_threshold: 1.2,
                    jaccard_threshold: 0.8,
                    budget_ratio: 0.5,
                },
                BanditArm {
                    name: "balanced".to_string(),
                    alpha: 2.0,
                    beta: 1.0,
                    entropy_threshold: 0.9,
                    jaccard_threshold: 0.7,
                    budget_ratio: 0.35,
                },
                BanditArm {
                    name: "aggressive".to_string(),
                    alpha: 2.0,
                    beta: 1.0,
                    entropy_threshold: 0.6,
                    jaccard_threshold: 0.55,
                    budget_ratio: 0.2,
                },
            ],
            total_pulls: 0,
        }
    }
}

impl ThresholdBandit {
    pub fn select_arm(&mut self) -> &BanditArm {
        self.total_pulls += 1;

        let epsilon = (0.1 / (1.0 + self.total_pulls as f64 / 100.0)).max(0.02);
        if rng_f64() < epsilon {
            let idx = rng_usize(self.arms.len());
            return &self.arms[idx];
        }

        let samples: Vec<f64> = self.arms.iter().map(BanditArm::sample).collect();
        let best_idx = samples
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map_or(0, |(i, _)| i);

        &self.arms[best_idx]
    }

    pub fn update(&mut self, arm_name: &str, success: bool) {
        if let Some(arm) = self.arms.iter_mut().find(|a| a.name == arm_name) {
            if success {
                arm.update_success();
            } else {
                arm.update_failure();
            }
        }
    }

    pub fn decay_all(&mut self, factor: f64) {
        for arm in &mut self.arms {
            arm.decay(factor);
        }
    }

    pub fn update_from_session(&mut self, outcomes: &[crate::core::feedback::CompressionOutcome]) {
        for outcome in outcomes {
            let efficiency = if outcome.tokens_original > 0 {
                outcome.tokens_saved as f64 / outcome.tokens_original as f64
            } else {
                0.0
            };
            let success = efficiency > 0.3 && outcome.task_completed;

            let arm_name = if outcome.entropy_threshold >= 1.0 {
                "conservative"
            } else if outcome.entropy_threshold >= 0.7 {
                "balanced"
            } else {
                "aggressive"
            };

            self.update(arm_name, success);
        }

        if !outcomes.is_empty() {
            self.decay_all(0.98);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BanditStore {
    pub bandits: HashMap<String, ThresholdBandit>,
}

impl BanditStore {
    pub fn get_or_create(&mut self, key: &str) -> &mut ThresholdBandit {
        self.bandits.entry(key.to_string()).or_default()
    }

    pub fn load(project_root: &str) -> Self {
        let path = bandit_path(project_root);
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(store) = serde_json::from_str::<BanditStore>(&content) {
                    return store;
                }
            }
        }
        Self::default()
    }

    pub fn save(&self, project_root: &str) -> Result<(), String> {
        let path = bandit_path(project_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn format_report(&self) -> String {
        if self.bandits.is_empty() {
            return "No bandit data yet.".to_string();
        }
        let mut lines = vec!["Threshold Bandits (Thompson Sampling):".to_string()];
        for (key, bandit) in &self.bandits {
            lines.push(format!("  {key} (pulls: {}):", bandit.total_pulls));
            for arm in &bandit.arms {
                let mean = arm.mean();
                lines.push(format!(
                    "    {}: α={:.1} β={:.1} mean={:.0}% entropy={:.2} jaccard={:.2} budget={:.0}%",
                    arm.name,
                    arm.alpha,
                    arm.beta,
                    mean * 100.0,
                    arm.entropy_threshold,
                    arm.jaccard_threshold,
                    arm.budget_ratio * 100.0
                ));
            }
        }
        lines.join("\n")
    }
}

fn bandit_path(project_root: &str) -> std::path::PathBuf {
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        project_root.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    };
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("projects")
        .join(hash)
        .join("bandits.json")
}

fn rng_f64() -> f64 {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).unwrap_or(());
    let val = u64::from_le_bytes(bytes);
    (val >> 11) as f64 / ((1u64 << 53) as f64)
}

fn rng_usize(bound: usize) -> usize {
    if bound == 0 {
        return 0;
    }
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).unwrap_or(());
    let val = u64::from_le_bytes(bytes);
    (val as usize) % bound
}

fn beta_sample(alpha: f64, beta: f64) -> f64 {
    let x = gamma_sample(alpha);
    let y = gamma_sample(beta);
    if x + y == 0.0 {
        return 0.5;
    }
    x / (x + y)
}

#[allow(clippy::many_single_char_names)] // Marsaglia's algorithm uses standard math notation
fn gamma_sample(shape: f64) -> f64 {
    if shape < 1.0 {
        let u = rng_f64().max(1e-10);
        gamma_sample(shape + 1.0) * u.powf(1.0 / shape)
    } else {
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0_f64 * d).sqrt();
        loop {
            let x = standard_normal();
            let v = (1.0 + c * x).powi(3);
            if v <= 0.0 {
                continue;
            }
            let u = rng_f64().max(1e-10);
            if u < 1.0 - 0.0331 * x.powi(4) || u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
                return d * v;
            }
        }
    }
}

fn standard_normal() -> f64 {
    let u1: f64 = rng_f64().max(1e-10);
    let u2: f64 = rng_f64();
    (-2.0_f64 * u1.ln()).sqrt() * (2.0_f64 * std::f64::consts::PI * u2).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bandit_default_has_three_arms() {
        let b = ThresholdBandit::default();
        assert_eq!(b.arms.len(), 3);
        assert_eq!(b.arms[0].name, "conservative");
        assert_eq!(b.arms[1].name, "balanced");
        assert_eq!(b.arms[2].name, "aggressive");
    }

    #[test]
    fn bandit_selection_works() {
        let mut b = ThresholdBandit::default();
        for _ in 0..10 {
            let arm = b.select_arm();
            let _ = arm.name.clone();
        }
        assert_eq!(b.total_pulls, 10);
    }

    #[test]
    fn bandit_update_shifts_distribution() {
        let mut b = ThresholdBandit::default();
        for _ in 0..20 {
            b.update("aggressive", true);
        }
        for _ in 0..20 {
            b.update("conservative", false);
        }
        let agg = b.arms.iter().find(|a| a.name == "aggressive").unwrap();
        let con = b.arms.iter().find(|a| a.name == "conservative").unwrap();
        assert!(agg.mean() > con.mean());
    }

    #[test]
    fn beta_sample_in_range() {
        for _ in 0..100 {
            let s = beta_sample(2.0, 2.0);
            assert!((0.0..=1.0).contains(&s), "got {s}");
        }
    }

    #[test]
    fn store_save_load_roundtrip() {
        let dir = std::env::temp_dir().join("bandit-test");
        let root = dir.to_string_lossy().to_string();
        let mut store = BanditStore::default();
        store.get_or_create("rs_medium");
        store.save(&root).unwrap();
        let loaded = BanditStore::load(&root);
        assert!(loaded.bandits.contains_key("rs_medium"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
