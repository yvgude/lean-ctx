use std::collections::HashMap;

const STATS_FILE: &str = "mode_stats.json";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModeOutcome {
    pub mode: String,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub density: f64,
}

impl ModeOutcome {
    pub fn efficiency(&self) -> f64 {
        if self.tokens_out == 0 {
            return 0.0;
        }
        self.density / (self.tokens_out as f64 / self.tokens_in.max(1) as f64)
    }
}

#[derive(Clone, Debug, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSignature {
    pub ext: String,
    pub size_bucket: u8,
}

impl FileSignature {
    pub fn from_path(path: &str, token_count: usize) -> Self {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        let size_bucket = match token_count {
            0..=500 => 0,
            501..=2000 => 1,
            2001..=5000 => 2,
            5001..=20000 => 3,
            _ => 4,
        };
        Self { ext, size_bucket }
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ModePredictor {
    history: HashMap<FileSignature, Vec<ModeOutcome>>,
}

impl ModePredictor {
    pub fn new() -> Self {
        Self::load().unwrap_or_default()
    }

    pub fn record(&mut self, sig: FileSignature, outcome: ModeOutcome) {
        let entries = self.history.entry(sig).or_default();
        entries.push(outcome);
        if entries.len() > 100 {
            entries.drain(0..50);
        }
    }

    /// Returns the best mode based on historical efficiency.
    /// None = no data, use fallback heuristic.
    pub fn predict_best_mode(&self, sig: &FileSignature) -> Option<String> {
        let entries = self.history.get(sig)?;
        if entries.len() < 3 {
            return None;
        }

        let mut mode_scores: HashMap<&str, (f64, usize)> = HashMap::new();
        for entry in entries {
            let (sum, count) = mode_scores.entry(&entry.mode).or_insert((0.0, 0));
            *sum += entry.efficiency();
            *count += 1;
        }

        mode_scores
            .into_iter()
            .max_by(|a, b| {
                let avg_a = a.1 .0 / a.1 .1 as f64;
                let avg_b = b.1 .0 / b.1 .1 as f64;
                avg_a
                    .partial_cmp(&avg_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(mode, _)| mode.to_string())
    }

    pub fn save(&self) {
        let dir = match dirs::home_dir() {
            Some(d) => d.join(".lean-ctx"),
            None => return,
        };
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(STATS_FILE);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    fn load() -> Option<Self> {
        let path = dirs::home_dir()?.join(".lean-ctx").join(STATS_FILE);
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_signature_buckets() {
        assert_eq!(FileSignature::from_path("main.rs", 100).size_bucket, 0);
        assert_eq!(FileSignature::from_path("main.rs", 1000).size_bucket, 1);
        assert_eq!(FileSignature::from_path("main.rs", 3000).size_bucket, 2);
        assert_eq!(FileSignature::from_path("main.rs", 10000).size_bucket, 3);
        assert_eq!(FileSignature::from_path("main.rs", 50000).size_bucket, 4);
    }

    #[test]
    fn predict_returns_none_without_history() {
        let predictor = ModePredictor::default();
        let sig = FileSignature::from_path("test.rs", 500);
        assert!(predictor.predict_best_mode(&sig).is_none());
    }

    #[test]
    fn predict_returns_none_with_too_few_entries() {
        let mut predictor = ModePredictor::default();
        let sig = FileSignature::from_path("test.rs", 500);
        predictor.record(
            sig.clone(),
            ModeOutcome {
                mode: "full".to_string(),
                tokens_in: 100,
                tokens_out: 100,
                density: 0.5,
            },
        );
        assert!(predictor.predict_best_mode(&sig).is_none());
    }

    #[test]
    fn predict_learns_best_mode() {
        let mut predictor = ModePredictor::default();
        let sig = FileSignature::from_path("big.rs", 5000);
        for _ in 0..5 {
            predictor.record(
                sig.clone(),
                ModeOutcome {
                    mode: "full".to_string(),
                    tokens_in: 5000,
                    tokens_out: 5000,
                    density: 0.3,
                },
            );
            predictor.record(
                sig.clone(),
                ModeOutcome {
                    mode: "map".to_string(),
                    tokens_in: 5000,
                    tokens_out: 800,
                    density: 0.6,
                },
            );
        }
        let best = predictor.predict_best_mode(&sig);
        assert_eq!(best, Some("map".to_string()));
    }

    #[test]
    fn history_caps_at_100() {
        let mut predictor = ModePredictor::default();
        let sig = FileSignature::from_path("test.rs", 100);
        for _ in 0..120 {
            predictor.record(
                sig.clone(),
                ModeOutcome {
                    mode: "full".to_string(),
                    tokens_in: 100,
                    tokens_out: 100,
                    density: 0.5,
                },
            );
        }
        assert!(predictor.history.get(&sig).unwrap().len() <= 100);
    }

    #[test]
    fn mode_outcome_efficiency() {
        let o = ModeOutcome {
            mode: "map".to_string(),
            tokens_in: 1000,
            tokens_out: 200,
            density: 0.6,
        };
        assert!(o.efficiency() > 0.0);
    }
}
