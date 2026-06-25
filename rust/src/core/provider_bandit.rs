//! Provider Bandit — Thompson Sampling for provider selection.
//!
//! Extends the existing bandit system to learn which data providers are
//! most informative for different task types. When multiple providers
//! are available, the bandit samples from Beta distributions to select
//! the provider most likely to yield useful context.
//!
//! Scientific basis: Dopaminergic prediction errors (Schultz 1997;
//! Nature Neurosci 2025). Positive prediction errors (provider was more
//! useful than expected) increase the Beta alpha parameter. Negative
//! errors decrease it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A provider-specific bandit arm (simplified Beta-Bernoulli).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderArm {
    pub name: String,
    pub alpha: f64,
    pub beta: f64,
    pub pulls: u64,
}

impl ProviderArm {
    #[must_use]
    pub fn sample(&self) -> f64 {
        beta_sample(self.alpha, self.beta)
    }

    pub fn update_success(&mut self) {
        self.alpha += 1.0;
        self.pulls += 1;
    }

    pub fn update_failure(&mut self) {
        self.beta += 1.0;
        self.pulls += 1;
    }

    #[must_use]
    pub fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }
}

/// Per-provider arms, keyed by task type (e.g., "bugfix", "feature", "refactor").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderBandit {
    pub arms: HashMap<String, ProviderArm>,
}

impl Default for ProviderBandit {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderBandit {
    #[must_use]
    pub fn new() -> Self {
        Self {
            arms: HashMap::new(),
        }
    }

    /// Select the best provider for a given task type using Thompson Sampling.
    /// Returns the `provider_id` with the highest sampled value.
    pub fn select_provider(
        &mut self,
        task_type: &str,
        available_providers: &[String],
    ) -> Option<String> {
        if available_providers.is_empty() {
            return None;
        }

        if available_providers.len() == 1 {
            return Some(available_providers[0].clone());
        }

        let mut best_sample = f64::NEG_INFINITY;
        let mut best_provider = &available_providers[0];

        for provider_id in available_providers {
            let key = arm_key(task_type, provider_id);
            let arm = self.arms.entry(key).or_insert_with(|| ProviderArm {
                name: provider_id.clone(),
                alpha: 1.0,
                beta: 1.0,
                pulls: 0,
            });

            let sample = arm.sample();
            if sample > best_sample {
                best_sample = sample;
                best_provider = provider_id;
            }
        }

        Some(best_provider.clone())
    }

    /// Update the bandit after observing the outcome of a provider query.
    pub fn update(&mut self, task_type: &str, provider_id: &str, was_useful: bool) {
        let key = arm_key(task_type, provider_id);
        let arm = self.arms.entry(key).or_insert_with(|| ProviderArm {
            name: provider_id.to_string(),
            alpha: 1.0,
            beta: 1.0,
            pulls: 0,
        });

        if was_useful {
            arm.update_success();
        } else {
            arm.update_failure();
        }
    }

    /// Get the estimated success probability for a provider on a task type.
    pub fn estimated_probability(&self, task_type: &str, provider_id: &str) -> f64 {
        let key = arm_key(task_type, provider_id);
        self.arms.get(&key).map_or(0.5, ProviderArm::mean)
    }

    /// Load the persisted bandit for a project, or a fresh one if none exists.
    /// Persistence is what turns the preloader from a per-call heuristic into a
    /// model that genuinely learns which providers pay off for which task types.
    #[must_use]
    pub fn load(project_root: &str) -> Self {
        let path = provider_bandit_path(project_root);
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(bandit) = serde_json::from_str::<ProviderBandit>(&content)
        {
            return bandit;
        }
        Self::new()
    }

    /// Persist the bandit's learned arms for this project.
    pub fn save(&self, project_root: &str) -> Result<(), String> {
        let path = provider_bandit_path(project_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    /// Format a summary of all arms for debugging/logging.
    #[must_use]
    pub fn format_report(&self) -> String {
        let mut out = String::from("Provider Bandit Arms:\n");
        let mut keys: Vec<_> = self.arms.keys().collect();
        keys.sort();

        for key in keys {
            let arm = &self.arms[key];
            out.push_str(&format!(
                "  {} — alpha={:.1} beta={:.1} mean={:.3} pulls={}\n",
                key,
                arm.alpha,
                arm.beta,
                arm.mean(),
                arm.pulls,
            ));
        }
        out
    }
}

fn arm_key(task_type: &str, provider_id: &str) -> String {
    format!("{task_type}:{provider_id}")
}

fn provider_bandit_path(project_root: &str) -> std::path::PathBuf {
    let hash = crate::core::project_hash::hash_project_root(project_root);
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("projects")
        .join(hash)
        .join("provider_bandit.json")
}

/// Simple Beta distribution sample using the ratio of two Gamma samples.
fn beta_sample(alpha: f64, beta: f64) -> f64 {
    let x = gamma_sample(alpha);
    let y = gamma_sample(beta);
    if x + y == 0.0 {
        return 0.5;
    }
    (x / (x + y)).clamp(0.0, 1.0)
}

/// Gamma(shape, 1) sample using Marsaglia & Tsang's method.
#[allow(clippy::many_single_char_names)]
fn gamma_sample(shape: f64) -> f64 {
    if shape < 1.0 {
        return gamma_sample(shape + 1.0) * rng_f64().powf(1.0 / shape);
    }
    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();
    loop {
        let x = standard_normal();
        let v_base = 1.0 + c * x;
        if v_base <= 0.0 {
            continue;
        }
        let v = v_base * v_base * v_base;
        let u = rng_f64();
        if u < 1.0 - 0.0331 * (x * x) * (x * x) || u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
            return d * v;
        }
    }
}

fn standard_normal() -> f64 {
    let u1: f64 = rng_f64().max(1e-10);
    let u2: f64 = rng_f64();
    (-2.0_f64 * u1.ln()).sqrt() * (2.0_f64 * std::f64::consts::PI * u2).cos()
}

fn rng_f64() -> f64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::time::Instant::now().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    (hasher.finish() as f64) / (u64::MAX as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_from_single_provider() {
        let mut bandit = ProviderBandit::new();
        let providers = vec!["github".into()];

        let selected = bandit.select_provider("bugfix", &providers);
        assert_eq!(selected.as_deref(), Some("github"));
    }

    #[test]
    fn select_from_empty_returns_none() {
        let mut bandit = ProviderBandit::new();
        let selected = bandit.select_provider("bugfix", &[]);
        assert!(selected.is_none());
    }

    #[test]
    fn update_shifts_distribution() {
        let mut bandit = ProviderBandit::new();
        let providers = vec!["github".into(), "jira".into()];

        // Train: github is always useful for bugfix
        for _ in 0..20 {
            bandit.update("bugfix", "github", true);
            bandit.update("bugfix", "jira", false);
        }

        let gh_prob = bandit.estimated_probability("bugfix", "github");
        let jira_prob = bandit.estimated_probability("bugfix", "jira");
        assert!(gh_prob > 0.8);
        assert!(jira_prob < 0.2);

        // Should strongly prefer github for bugfix tasks.
        let mut github_selected = 0;
        for _ in 0..100 {
            let selected = bandit.select_provider("bugfix", &providers).unwrap();
            if selected == "github" {
                github_selected += 1;
            }
        }
        assert!(github_selected > 80);
    }

    #[test]
    fn different_task_types_have_independent_arms() {
        let mut bandit = ProviderBandit::new();

        bandit.update("bugfix", "github", true);
        bandit.update("feature", "jira", true);

        assert!(bandit.estimated_probability("bugfix", "github") > 0.5);
        assert!(bandit.estimated_probability("feature", "jira") > 0.5);
        assert!((bandit.estimated_probability("bugfix", "jira") - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn format_report_shows_all_arms() {
        let mut bandit = ProviderBandit::new();
        bandit.update("bugfix", "github", true);
        bandit.update("bugfix", "jira", false);

        let report = bandit.format_report();
        assert!(report.contains("bugfix:github"));
        assert!(report.contains("bugfix:jira"));
    }

    #[test]
    fn persistence_roundtrip_preserves_learning() {
        let _env = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
        let project = "/tmp/provider-bandit-roundtrip";

        let mut bandit = ProviderBandit::new();
        for _ in 0..10 {
            bandit.update("bugfix", "github", true);
        }
        bandit.save(project).expect("save");

        let reloaded = ProviderBandit::load(project);
        assert!(
            reloaded.estimated_probability("bugfix", "github") > 0.8,
            "reloaded bandit must retain the learned preference"
        );
        // A fresh project starts unbiased (no cross-project leakage).
        let fresh = ProviderBandit::load("/tmp/provider-bandit-unseen");
        assert!((fresh.estimated_probability("bugfix", "github") - 0.5).abs() < f64::EPSILON);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
