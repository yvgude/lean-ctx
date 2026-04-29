//! Anomaly detection using Welford's online algorithm for running
//! mean/variance and triggering alerts at >3x standard deviation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const DEFAULT_WINDOW: usize = 50;
const DEFAULT_DEVIATION_THRESHOLD: f64 = 3.0;
const MIN_SAMPLES: usize = 10;

// ---------------------------------------------------------------------------
// Welford online statistics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WelfordState {
    pub count: u64,
    pub mean: f64,
    pub m2: f64,
    #[serde(default = "default_window")]
    window_values: Vec<f64>,
    #[serde(default = "default_window_size")]
    window_size: usize,
}

fn default_window() -> Vec<f64> {
    Vec::new()
}

fn default_window_size() -> usize {
    DEFAULT_WINDOW
}

impl Default for WelfordState {
    fn default() -> Self {
        Self {
            count: 0,
            mean: 0.0,
            m2: 0.0,
            window_values: Vec::new(),
            window_size: DEFAULT_WINDOW,
        }
    }
}

impl WelfordState {
    pub fn with_window(size: usize) -> Self {
        Self {
            window_size: size,
            ..Default::default()
        }
    }

    pub fn update(&mut self, value: f64) {
        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;

        self.window_values.push(value);
        if self.window_values.len() > self.window_size {
            self.window_values.remove(0);
        }
    }

    pub fn variance(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        self.m2 / (self.count - 1) as f64
    }

    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }

    pub fn windowed_mean(&self) -> f64 {
        if self.window_values.is_empty() {
            return self.mean;
        }
        let sum: f64 = self.window_values.iter().sum();
        sum / self.window_values.len() as f64
    }

    pub fn windowed_std_dev(&self) -> f64 {
        if self.window_values.len() < 2 {
            return self.std_dev();
        }
        let mean = self.windowed_mean();
        let variance: f64 = self
            .window_values
            .iter()
            .map(|v| (v - mean).powi(2))
            .sum::<f64>()
            / (self.window_values.len() - 1) as f64;
        variance.sqrt()
    }

    pub fn has_enough_data(&self) -> bool {
        self.count as usize >= MIN_SAMPLES
    }
}

// ---------------------------------------------------------------------------
// Anomaly detector
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyDetector {
    pub metrics: HashMap<String, WelfordState>,
    #[serde(default = "default_threshold")]
    pub deviation_threshold: f64,
}

fn default_threshold() -> f64 {
    DEFAULT_DEVIATION_THRESHOLD
}

impl Default for AnomalyDetector {
    fn default() -> Self {
        Self {
            metrics: HashMap::new(),
            deviation_threshold: DEFAULT_DEVIATION_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AnomalyAlert {
    pub metric: String,
    pub expected: f64,
    pub actual: f64,
    pub std_dev: f64,
    pub deviation_factor: f64,
}

impl AnomalyDetector {
    pub fn record(&mut self, metric: &str, value: f64) -> Option<AnomalyAlert> {
        let state = self
            .metrics
            .entry(metric.to_string())
            .or_insert_with(|| WelfordState::with_window(DEFAULT_WINDOW));

        let alert = if state.has_enough_data() {
            let expected = state.windowed_mean();
            let sd = state.windowed_std_dev();

            if sd > 0.0 {
                let deviation = (value - expected).abs() / sd;
                if deviation > self.deviation_threshold {
                    Some(AnomalyAlert {
                        metric: metric.to_string(),
                        expected,
                        actual: value,
                        std_dev: sd,
                        deviation_factor: deviation,
                    })
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        state.update(value);
        alert
    }

    pub fn summary(&self) -> Vec<MetricSummary> {
        let mut out: Vec<MetricSummary> = self
            .metrics
            .iter()
            .map(|(name, state)| MetricSummary {
                metric: name.clone(),
                count: state.count,
                mean: state.windowed_mean(),
                std_dev: state.windowed_std_dev(),
                last_value: state.window_values.last().copied().unwrap_or(0.0),
            })
            .collect();
        out.sort_by_key(|s| s.metric.clone());
        out
    }

    pub fn save(&self) {
        if let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() {
            let path = dir.join("anomaly_detector.json");
            if let Ok(json) = serde_json::to_string(self) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    pub fn load() -> Self {
        crate::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|d| d.join("anomaly_detector.json"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricSummary {
    pub metric: String,
    pub count: u64,
    pub mean: f64,
    pub std_dev: f64,
    pub last_value: f64,
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static DETECTOR: OnceLock<Mutex<AnomalyDetector>> = OnceLock::new();

fn global_detector() -> &'static Mutex<AnomalyDetector> {
    DETECTOR.get_or_init(|| Mutex::new(AnomalyDetector::load()))
}

pub fn record_metric(metric: &str, value: f64) -> Option<AnomalyAlert> {
    let mut det = global_detector()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let alert = det.record(metric, value);

    if let Some(ref a) = alert {
        crate::core::events::emit_anomaly(&a.metric, a.expected, a.actual, a.deviation_factor);
    }

    alert
}

pub fn summary() -> Vec<MetricSummary> {
    global_detector()
        .lock()
        .map(|d| d.summary())
        .unwrap_or_default()
}

pub fn save() {
    if let Ok(d) = global_detector().lock() {
        d.save();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welford_basic_stats() {
        let mut w = WelfordState::default();
        for v in [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0] {
            w.update(v);
        }
        assert!((w.mean - 5.0).abs() < 0.01);
        // Sample variance (n-1): 32/7 ≈ 4.571
        assert!((w.variance() - 4.571).abs() < 0.01);
        assert!((w.std_dev() - 2.138).abs() < 0.01);
    }

    #[test]
    fn welford_window_limits() {
        let mut w = WelfordState::with_window(5);
        for i in 0..20 {
            w.update(i as f64);
        }
        assert_eq!(w.window_values.len(), 5);
        assert_eq!(w.window_values[0], 15.0);
    }

    #[test]
    fn no_alert_with_few_samples() {
        let mut det = AnomalyDetector::default();
        for i in 0..5 {
            assert!(det.record("test", i as f64).is_none());
        }
    }

    #[test]
    fn alert_on_extreme_value() {
        let mut det = AnomalyDetector::default();
        for i in 0..20 {
            let v = 100.0 + (i % 5) as f64;
            det.record("tokens", v);
        }
        let alert = det.record("tokens", 1000.0);
        assert!(alert.is_some());
        let a = alert.unwrap();
        assert_eq!(a.metric, "tokens");
        assert!(a.deviation_factor > 3.0);
    }

    #[test]
    fn no_alert_on_normal_value() {
        let mut det = AnomalyDetector::default();
        for i in 0..20 {
            let v = 100.0 + (i % 3) as f64;
            assert!(det.record("tokens", v).is_none());
        }
    }

    #[test]
    fn summary_returns_all_metrics() {
        let mut det = AnomalyDetector::default();
        det.record("tokens", 100.0);
        det.record("cost", 0.5);
        det.record("tokens", 120.0);
        let s = det.summary();
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn global_record_works() {
        let _ = record_metric("test_global", 42.0);
    }
}
