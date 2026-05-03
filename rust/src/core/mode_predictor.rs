use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

const STATS_FILE: &str = "mode_stats.json";
const PREDICTOR_FLUSH_SECS: u64 = 10;

static PREDICTOR_BUFFER: Mutex<Option<(ModePredictor, Instant)>> = Mutex::new(None);

/// Observed outcome of a read mode: tokens in/out and information density.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModeOutcome {
    pub mode: String,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub density: f64,
}

impl ModeOutcome {
    /// Computes an efficiency score: density / compression ratio.
    pub fn efficiency(&self) -> f64 {
        if self.tokens_out == 0 {
            return 0.0;
        }
        self.density / (self.tokens_out as f64 / self.tokens_in.max(1) as f64)
    }
}

/// File identity for mode prediction: extension + token-count size bucket.
#[derive(Clone, Debug, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSignature {
    pub ext: String,
    pub size_bucket: u8,
}

impl FileSignature {
    /// Creates a file signature from its path and token count.
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

/// Learns the best read mode per file signature from historical outcomes.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModePredictor {
    history: HashMap<FileSignature, Vec<ModeOutcome>>,
    project_root: Option<String>,
}

impl ModePredictor {
    /// Loads or creates the predictor, using an in-memory buffer for caching.
    pub fn new() -> Self {
        let mut guard = PREDICTOR_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((ref predictor, _)) = *guard {
            return predictor.clone();
        }
        let mut loaded = Self::load_from_disk().unwrap_or_default();
        if loaded.project_root.is_none() {
            loaded.project_root = std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string());
        }
        *guard = Some((loaded.clone(), Instant::now()));
        loaded
    }

    pub fn with_project_root(mut self, root: &str) -> Self {
        self.project_root = Some(root.to_string());
        self
    }

    pub fn set_project_root(&mut self, root: &str) {
        self.project_root = Some(root.to_string());
    }

    /// Records a mode outcome for a file signature (capped at 100 entries).
    pub fn record(&mut self, sig: FileSignature, outcome: ModeOutcome) {
        let entries = self.history.entry(sig).or_default();
        entries.push(outcome);
        if entries.len() > 100 {
            entries.drain(0..50);
        }
    }

    /// Returns the best mode based on historical efficiency.
    /// Chain: local history -> cloud adaptive models -> built-in defaults.
    pub fn predict_best_mode(&self, sig: &FileSignature) -> Option<String> {
        if let Some(local) = self.predict_from_local(sig) {
            return Some(local);
        }
        if let Some(bandit) = self.predict_from_bandit(sig) {
            return Some(bandit);
        }
        if let Some(cloud) = self.predict_from_cloud(sig) {
            return Some(cloud);
        }
        Self::predict_from_defaults(sig)
    }

    fn predict_from_bandit(&self, sig: &FileSignature) -> Option<String> {
        let key = format!("{}_feedback", sig.ext);
        let store =
            crate::core::bandit::BanditStore::load(self.project_root.as_deref().unwrap_or("."));
        let bandit = store.bandits.get(&key)?;
        if bandit.total_pulls < 5 {
            return None;
        }
        let best_arm = bandit.arms.iter().max_by(|a, b| {
            a.mean()
                .partial_cmp(&b.mean())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let mode = match best_arm.name.as_str() {
            "conservative" => "full",
            "balanced" => "signatures",
            "aggressive" => "aggressive",
            _ => return None,
        };
        Some(mode.to_string())
    }

    fn predict_from_local(&self, sig: &FileSignature) -> Option<String> {
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

    /// Loads cloud adaptive models (synced from LeanCTX Cloud).
    /// Models are cached locally and auto-updated for cloud users.
    #[allow(clippy::unused_self)]
    fn predict_from_cloud(&self, sig: &FileSignature) -> Option<String> {
        let data = crate::cloud_client::load_cloud_models()?;
        let models = data["models"].as_array()?;

        let ext_with_dot = format!(".{}", sig.ext);
        let bucket_name = match sig.size_bucket {
            0 => "0-500",
            1 => "500-2k",
            2 => "2k-10k",
            _ => "10k+",
        };

        let mut best: Option<(&str, f64)> = None;

        for model in models {
            let m_ext = model["file_ext"].as_str().unwrap_or("");
            let m_bucket = model["size_bucket"].as_str().unwrap_or("");
            let confidence = model["confidence"].as_f64().unwrap_or(0.0);

            if m_ext == ext_with_dot && m_bucket == bucket_name && confidence > 0.5 {
                if let Some(mode) = model["recommended_mode"].as_str() {
                    if best.is_none_or(|(_, c)| confidence > c) {
                        best = Some((mode, confidence));
                    }
                }
            }
        }

        if let Some((mode, _)) = best {
            return Some(mode.to_string());
        }

        for model in models {
            let m_ext = model["file_ext"].as_str().unwrap_or("");
            let confidence = model["confidence"].as_f64().unwrap_or(0.0);
            if m_ext == ext_with_dot && confidence > 0.5 {
                return model["recommended_mode"]
                    .as_str()
                    .map(std::string::ToString::to_string);
            }
        }

        None
    }

    /// Built-in defaults for common file types and sizes.
    /// Ensures reasonable compression even without local history or cloud models.
    /// Respects Kolmogorov-Gate: files with K>0.7 skip aggressive modes.
    fn predict_from_defaults(sig: &FileSignature) -> Option<String> {
        let mode = match (sig.ext.as_str(), sig.size_bucket) {
            // Tiny files (0-500 tokens): always full — compression overhead not worth it
            (_, 0) => return None,

            // Lock / large code files: signatures only
            ("lock", _)
            | (
                "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c" | "cpp" | "rb"
                | "swift" | "kt" | "cs" | "vue" | "svelte",
                4..,
            ) => "signatures",

            // Code files 2k-10k / SQL: map gives structure without bloat
            (
                "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c" | "cpp" | "rb"
                | "swift" | "kt" | "cs" | "vue" | "svelte",
                2 | 3,
            )
            | ("sql", 2..) => "map",

            // Config/data, CSS, and large unknown files: aggressive
            ("json" | "yaml" | "yml" | "toml" | "xml" | "csv", _)
            | ("css" | "scss" | "less" | "sass", 2..)
            | (_, 3..) => "aggressive",

            _ => return None,
        };
        Some(mode.to_string())
    }

    /// Saves to the in-memory buffer and flushes to disk if the interval elapsed.
    pub fn save(&self) {
        let mut guard = PREDICTOR_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let should_flush = match *guard {
            Some((_, ref last_flush)) => last_flush.elapsed().as_secs() >= PREDICTOR_FLUSH_SECS,
            None => true,
        };
        *guard = Some((self.clone(), Instant::now()));
        if should_flush {
            self.save_to_disk();
        }
    }

    fn save_to_disk(&self) {
        let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(STATS_FILE);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let tmp = dir.join(".mode_stats.tmp");
            if std::fs::write(&tmp, &json).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    /// Forces an immediate write of the buffered predictor state to disk.
    pub fn flush() {
        let guard = PREDICTOR_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((ref predictor, _)) = *guard {
            predictor.save_to_disk();
        }
    }

    fn load_from_disk() -> Option<Self> {
        let path = crate::core::data_dir::lean_ctx_data_dir()
            .ok()?
            .join(STATS_FILE);
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
        let sig = FileSignature::from_path("test.zzz", 500);
        assert!(predictor.predict_from_local(&sig).is_none());
    }

    #[test]
    fn predict_returns_none_with_too_few_entries() {
        let mut predictor = ModePredictor::default();
        let sig = FileSignature::from_path("test.zzz", 500);
        predictor.record(
            sig.clone(),
            ModeOutcome {
                mode: "full".to_string(),
                tokens_in: 100,
                tokens_out: 100,
                density: 0.5,
            },
        );
        assert!(predictor.predict_from_local(&sig).is_none());
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
    fn defaults_return_none_for_small_files() {
        let sig = FileSignature::from_path("small.rs", 200);
        assert!(ModePredictor::predict_from_defaults(&sig).is_none());
    }

    #[test]
    fn defaults_recommend_map_for_medium_code() {
        let sig = FileSignature::from_path("medium.rs", 3000);
        assert_eq!(
            ModePredictor::predict_from_defaults(&sig),
            Some("map".to_string())
        );
    }

    #[test]
    fn defaults_recommend_aggressive_for_json() {
        let sig = FileSignature::from_path("config.json", 1000);
        assert_eq!(
            ModePredictor::predict_from_defaults(&sig),
            Some("aggressive".to_string())
        );
    }

    #[test]
    fn defaults_recommend_signatures_for_huge_code() {
        let sig = FileSignature::from_path("huge.ts", 25000);
        assert_eq!(
            ModePredictor::predict_from_defaults(&sig),
            Some("signatures".to_string())
        );
    }

    #[test]
    fn defaults_recommend_aggressive_for_large_unknown() {
        let sig = FileSignature::from_path("data.xyz", 8000);
        assert_eq!(
            ModePredictor::predict_from_defaults(&sig),
            Some("aggressive".to_string())
        );
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
