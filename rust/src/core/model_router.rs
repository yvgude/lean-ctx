//! Thompson-sampling model selection with durable per-task-class outcomes.
//!
//! Routing candidates are model aliases supplied by the proxy configuration.
//! Statistics are stored per `(task_class, model)` pair, so a model that is
//! effective for code changes does not automatically dominate research tasks.

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

const ROUTING_FILE: &str = "model_routing.jsonl";
const ARM_KEY_SEPARATOR: char = '\u{1f}';
static RNG_STATE: AtomicU64 = AtomicU64::new(0);
static GLOBAL_ROUTER: OnceLock<Mutex<ModelRouter>> = OnceLock::new();

/// Cumulative outcome data for one model and task class.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArmStats {
    pub successes: f64,
    pub failures: f64,
    pub total_cost: f64,
    pub total_tasks: u64,
}

/// Selects models by sampling each arm's Beta posterior.
#[derive(Debug, Default)]
pub struct ModelRouter {
    pub arms: HashMap<String, ArmStats>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedArm {
    model: String,
    task_class: String,
    stats: ArmStats,
}

impl ModelRouter {
    /// Reconstructs the most recent state of each arm from the JSONL journal.
    #[must_use]
    pub fn load() -> Self {
        let mut router = Self::default();
        let Ok(contents) = fs::read_to_string(routing_path()) else {
            return router;
        };

        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            let Ok(record) = serde_json::from_str::<PersistedArm>(line) else {
                continue;
            };
            if valid_stats(&record.stats) {
                router
                    .arms
                    .insert(arm_key(&record.task_class, &record.model), record.stats);
            }
        }

        router
    }

    /// Draws a Thompson-sampling score for every candidate and returns the best.
    ///
    /// # Panics
    ///
    /// Panics when `available_models` is empty, because the proxy cannot route
    /// a request without at least one configured candidate.
    #[must_use]
    pub fn select_model<'a>(&self, task_class: &str, available_models: &'a [&'a str]) -> &'a str {
        let (&first, rest) = available_models
            .split_first()
            .expect("model routing requires at least one available model");
        let task_class = normalized_task_class(task_class);
        let mut selected = first;
        let mut best_sample = self.sample_for(&task_class, first);

        for &model in rest {
            let sample = self.sample_for(&task_class, model);
            if sample > best_sample {
                selected = model;
                best_sample = sample;
            }
        }

        selected
    }

    /// Records an outcome and appends the resulting arm snapshot to the journal.
    pub fn record_outcome(&mut self, model: &str, task_class: &str, success: bool, cost: f64) {
        if model.trim().is_empty() {
            return;
        }

        let task_class = normalized_task_class(task_class);
        let key = arm_key(&task_class, model);
        let stats = self.arms.entry(key).or_default();
        if success {
            stats.successes += 1.0;
        } else {
            stats.failures += 1.0;
        }
        if cost.is_finite() && cost > 0.0 {
            stats.total_cost += cost;
        }
        stats.total_tasks = stats.total_tasks.saturating_add(1);

        let record = PersistedArm {
            model: model.to_owned(),
            task_class,
            stats: stats.clone(),
        };
        append_record(&record);
    }

    fn sample_for(&self, task_class: &str, model: &str) -> f64 {
        let stats = self.arms.get(&arm_key(task_class, model));
        let successes = stats.map_or(0.0, |stats| stats.successes.max(0.0));
        let failures = stats.map_or(0.0, |stats| stats.failures.max(0.0));
        sample_beta(successes + 1.0, failures + 1.0)
    }
}

/// Process-wide proxy router, initialized from disk once on first use.
#[must_use]
pub fn global_model_router() -> &'static Mutex<ModelRouter> {
    GLOBAL_ROUTER.get_or_init(|| Mutex::new(ModelRouter::load()))
}

fn routing_path() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(ROUTING_FILE)
}

fn append_record(record: &PersistedArm) {
    let path = routing_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_string(record) else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{json}");
}

fn normalized_task_class(task_class: &str) -> String {
    let task_class = task_class.trim();
    if task_class.is_empty() {
        "unknown".to_owned()
    } else {
        task_class.to_owned()
    }
}

fn arm_key(task_class: &str, model: &str) -> String {
    format!("{task_class}{ARM_KEY_SEPARATOR}{model}")
}

fn valid_stats(stats: &ArmStats) -> bool {
    stats.successes.is_finite()
        && stats.successes >= 0.0
        && stats.failures.is_finite()
        && stats.failures >= 0.0
        && stats.total_cost.is_finite()
        && stats.total_cost >= 0.0
}

fn sample_beta(alpha: f64, beta: f64) -> f64 {
    let left = sample_gamma(alpha);
    let right = sample_gamma(beta);
    let total = left + right;
    if total.is_finite() && total > 0.0 {
        left / total
    } else {
        0.5
    }
}

/// Marsaglia and Tsang's gamma sampler; Beta(a, b) is Gamma(a)/(Gamma(a)+Gamma(b)).
fn sample_gamma(shape: f64) -> f64 {
    debug_assert!(shape > 0.0);
    if shape < 1.0 {
        return sample_gamma(shape + 1.0) * uniform_open().powf(1.0 / shape);
    }

    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();
    loop {
        let normal = standard_normal();
        let base = 1.0 + c * normal;
        if base <= 0.0 {
            continue;
        }
        let value = base * base * base;
        let uniform = uniform_open();
        if uniform < 1.0 - 0.0331 * normal.powi(4)
            || uniform.ln() < 0.5 * normal * normal + d * (1.0 - value + value.ln())
        {
            return d * value;
        }
    }
}

fn standard_normal() -> f64 {
    let radius = (-2.0 * uniform_open().ln()).sqrt();
    radius * (std::f64::consts::TAU * uniform_open()).cos()
}

fn uniform_open() -> f64 {
    let bits = next_random_u64() >> 11;
    ((bits as f64) + 0.5) * (1.0 / ((1_u64 << 53) as f64))
}

fn next_random_u64() -> u64 {
    let mut state = RNG_STATE.load(Ordering::Relaxed);
    loop {
        if state == 0 {
            let seed = initial_seed();
            let _ = RNG_STATE.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed);
            state = RNG_STATE.load(Ordering::Relaxed);
            continue;
        }
        let mut next = state;
        next ^= next >> 12;
        next ^= next << 25;
        next ^= next >> 27;
        if RNG_STATE
            .compare_exchange_weak(state, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return next.wrapping_mul(2_685_821_657_736_338_717);
        }
        state = RNG_STATE.load(Ordering::Relaxed);
    }
}

fn initial_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
    (nanos ^ u64::from(std::process::id()) ^ 0x9e37_79b9_7f4a_7c15).max(1)
}

#[cfg(test)]
mod tests {
    use super::ModelRouter;

    #[test]
    fn more_successful_model_is_selected_more_often() {
        let _sandbox = crate::core::data_dir::isolated_data_dir();
        let mut router = ModelRouter::default();
        for _ in 0..10 {
            router.record_outcome("model_a", "coding", true, 0.0);
        }
        for _ in 0..2 {
            router.record_outcome("model_a", "coding", false, 0.0);
        }
        for _ in 0..5 {
            router.record_outcome("model_b", "coding", true, 0.0);
            router.record_outcome("model_b", "coding", false, 0.0);
        }

        let candidates = ["model_a", "model_b"];
        let mut model_a_choices = 0;
        let selections = 2_000;
        for _ in 0..selections {
            if router.select_model("coding", &candidates) == "model_a" {
                model_a_choices += 1;
            }
        }

        assert!(
            model_a_choices > selections / 2,
            "model_a should win more Thompson samples: {model_a_choices}/{selections}"
        );
    }

    #[test]
    fn load_reconstructs_latest_persisted_arm() {
        let _sandbox = crate::core::data_dir::isolated_data_dir();
        let mut router = ModelRouter::default();
        router.record_outcome("model_a", "coding", true, 0.25);
        router.record_outcome("model_a", "coding", false, 0.75);

        let restored = ModelRouter::load();
        let stats = restored
            .arms
            .get(&super::arm_key("coding", "model_a"))
            .expect("persisted arm should load");
        assert_eq!(stats.successes, 1.0);
        assert_eq!(stats.failures, 1.0);
        assert_eq!(stats.total_cost, 1.0);
        assert_eq!(stats.total_tasks, 2);
    }
}
