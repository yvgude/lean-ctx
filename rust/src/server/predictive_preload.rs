#![allow(clippy::case_sensitive_file_extension_comparisons)]
//! Predict file reads before the agent asks for them.
//!
//! The predictor deliberately stays small and deterministic: task triage plus
//! file topology provide useful first-read predictions without a remote model.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_RECENT_FILES: usize = 3;
const MAX_WARM_BYTES: u64 = 256 * 1024;

type PredictionKey = (String, Vec<String>);

#[derive(Debug)]
pub struct PredictivePreloader {
    /// Learned and heuristic predictions, keyed by triage class and read context.
    model: HashMap<PredictionKey, Vec<String>>,
    pending: HashSet<String>,
    last_key: Option<PredictionKey>,
    predicted: u64,
    used: u64,
    history_path: PathBuf,
}

impl Default for PredictivePreloader {
    fn default() -> Self {
        Self::with_history_path(default_history_path())
    }
}

impl PredictivePreloader {
    #[must_use]
    pub fn with_history_path(history_path: PathBuf) -> Self {
        Self {
            model: HashMap::new(),
            pending: HashSet::new(),
            last_key: None,
            predicted: 0,
            used: 0,
            history_path,
        }
    }

    /// Returns deterministic candidates for the next read and records them for
    /// later accuracy accounting.
    #[must_use]
    pub fn predict_next(&mut self, task_class: &str, recent_files: &[&str]) -> Vec<String> {
        let recent = normalize_recent_files(recent_files);
        if recent.is_empty() {
            return Vec::new();
        }

        let key = (task_class.to_ascii_lowercase(), recent.clone());
        let mut predictions = self.model.get(&key).cloned().unwrap_or_default();
        for path in &recent {
            predictions.extend(file_predictions(path));
        }
        if is_debugging_task(task_class) {
            predictions.extend(debugging_predictions());
        }

        predictions = unique_predictions(predictions, &recent);
        self.model.insert(key.clone(), predictions.clone());
        self.last_key = Some(key);
        for path in &predictions {
            if self.pending.insert(path.clone()) {
                self.predicted += 1;
            }
        }
        self.append_history("predict", None, &predictions);
        predictions
    }

    /// Records an observed file read, learns it for the preceding context, and
    /// marks a pending prediction as used when applicable.
    pub fn on_file_read(&mut self, path: &str) {
        let path = normalize_path(path);
        let hit = self.pending.remove(&path);
        if hit {
            self.used += 1;
        }
        if let Some(key) = self.last_key.take() {
            let learned = self.model.entry(key).or_default();
            if !learned.contains(&path) {
                learned.push(path.clone());
            }
        }
        self.append_history("read", Some(hit), &[path]);
    }

    #[must_use]
    pub fn accuracy(&self) -> f32 {
        if self.predicted == 0 {
            0.0
        } else {
            self.used as f32 / self.predicted as f32
        }
    }

    fn append_history(&self, event: &str, hit: Option<bool>, paths: &[String]) {
        let Some(parent) = self.history_path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.history_path)
        else {
            return;
        };
        let record = serde_json::json!({
            "event": event,
            "hit": hit,
            "paths": paths,
        });
        let _ = writeln!(file, "{record}");
    }
}

static PRELOADER: OnceLock<Mutex<PredictivePreloader>> = OnceLock::new();

/// Record a completed read and produce paths safe to warm in the background.
#[must_use]
pub fn record_read_and_predict(task_class: &str, path: &str) -> Vec<String> {
    let Ok(mut preloader) = global_preloader().lock() else {
        tracing::debug!("predictive preload lock poisoned; skipping prediction");
        return Vec::new();
    };
    preloader.on_file_read(path);
    preloader.predict_next(task_class, &[path])
}

/// Predict the next reads for callers that do not need access to the model.
#[must_use]
pub fn predict_next(task_class: &str, recent_files: &[&str]) -> Vec<String> {
    let Ok(mut preloader) = global_preloader().lock() else {
        tracing::debug!("predictive preload lock poisoned; skipping prediction");
        return Vec::new();
    };
    preloader.predict_next(task_class, recent_files)
}

/// Record an externally observed file read in the shared predictor.
pub fn on_file_read(path: &str) {
    if let Ok(mut preloader) = global_preloader().lock() {
        preloader.on_file_read(path);
    } else {
        tracing::debug!("predictive preload lock poisoned; skipping read record");
    }
}

/// Returns the proportion of emitted predictions that were later read.
#[must_use]
pub fn accuracy() -> f32 {
    global_preloader()
        .lock()
        .map_or(0.0, |preloader| preloader.accuracy())
}

/// Primes the OS file cache. The bounded read deliberately avoids allocating a
/// second copy of a large source file in the MCP process.
pub fn warm_paths(project_root: Option<&Path>, paths: &[String]) {
    for path in paths {
        if !is_safe_relative_path(path) {
            continue;
        }
        let full_path = project_root.map_or_else(|| PathBuf::from(path), |root| root.join(path));
        let Ok(file) = fs::File::open(full_path) else {
            continue;
        };
        let mut reader = file.take(MAX_WARM_BYTES);
        let mut contents = Vec::new();
        let _ = reader.read_to_end(&mut contents);
    }
}

fn global_preloader() -> &'static Mutex<PredictivePreloader> {
    PRELOADER.get_or_init(|| Mutex::new(PredictivePreloader::default()))
}

fn default_history_path() -> PathBuf {
    dirs::home_dir().map_or_else(
        || PathBuf::from(".local/share/lean-ctx/predictions.jsonl"),
        |home| home.join(".local/share/lean-ctx/predictions.jsonl"),
    )
}

fn normalize_recent_files(recent_files: &[&str]) -> Vec<String> {
    recent_files
        .iter()
        .rev()
        .take(MAX_RECENT_FILES)
        .rev()
        .map(|path| normalize_path(path))
        .filter(|path| !path.is_empty())
        .collect()
}

fn normalize_path(path: &str) -> String {
    path.trim().replace('\\', "/")
}

fn unique_predictions(predictions: Vec<String>, recent: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    predictions
        .into_iter()
        .map(|path| normalize_path(&path))
        .filter(|path| is_safe_relative_path(path) && !recent.contains(path))
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn file_predictions(path: &str) -> Vec<String> {
    let mut predictions = Vec::new();
    if path.ends_with(".md") {
        predictions.extend(documented_rust_paths(path));
    }
    if is_test_file(path) {
        predictions.extend(implementation_candidates(path));
    } else if path.ends_with(".rs") {
        predictions.extend(rust_source_predictions(path));
    }
    if path.starts_with("src/proxy/") {
        predictions.extend([
            "src/proxy/mod.rs".to_owned(),
            "src/proxy/forward/mod.rs".to_owned(),
        ]);
    }
    predictions
}

fn rust_source_predictions(path: &str) -> Vec<String> {
    let source = Path::new(path);
    let Some(stem) = source.file_stem().and_then(|stem| stem.to_str()) else {
        return Vec::new();
    };
    let mut predictions = vec![format!("tests/{stem}.rs"), "Cargo.toml".to_owned()];
    if let Some(parent) = source.parent() {
        let module = parent.join("mod.rs");
        if module != source {
            predictions.push(module.to_string_lossy().replace('\\', "/"));
        }
    }
    if path.starts_with("src/") && path != "src/lib.rs" {
        predictions.push("src/lib.rs".to_owned());
    }
    predictions
}

fn implementation_candidates(path: &str) -> Vec<String> {
    let normalized = normalize_path(path);
    let file_name = Path::new(&normalized)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let stem = file_name.trim_end_matches(".rs");
    let implementation = stem
        .trim_end_matches("_tests")
        .trim_end_matches("_test")
        .replace(".test", "");

    let mut predictions = Vec::new();
    if normalized.starts_with("tests/") {
        predictions.push(format!("src/{implementation}.rs"));
        predictions.push(format!("src/{implementation}/mod.rs"));
    } else if let Some(parent) = Path::new(&normalized).parent() {
        predictions.push(
            parent
                .join(format!("{implementation}.rs"))
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }
    predictions
}

fn is_test_file(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.contains(".test.")
        || path.ends_with("_test.rs")
        || path.ends_with("_tests.rs")
}

fn documented_rust_paths(path: &str) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .split(|character: char| character.is_whitespace() || "`'\"()[]{}<>,:;".contains(character))
        .map(|token| token.trim_end_matches(|character| matches!(character, '.' | ')' | ']')))
        .filter(|token| token.ends_with(".rs") && is_safe_relative_path(token))
        .map(str::to_owned)
        .collect()
}

fn is_debugging_task(task_class: &str) -> bool {
    let task = task_class.to_ascii_lowercase();
    task.contains("debug")
        || task.contains("diagnos")
        || task.contains("trace")
        || task.contains("fix")
}

fn debugging_predictions() -> Vec<String> {
    vec![
        "logs/error.log".to_owned(),
        ".lean-ctx/config.toml".to_owned(),
        "Cargo.toml".to_owned(),
    ]
}

fn is_safe_relative_path(path: &str) -> bool {
    let path = Path::new(path);
    path.is_relative()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::PredictivePreloader;
    use std::path::PathBuf;

    fn preloader() -> PredictivePreloader {
        PredictivePreloader::with_history_path(PathBuf::from("/dev/null/predictions.jsonl"))
    }

    #[test]
    fn predicts_test_file_from_implementation_file() {
        let mut preloader = preloader();
        let predictions = preloader.predict_next("coding_new", &["src/server/context_gate.rs"]);

        assert!(predictions.contains(&"tests/context_gate.rs".to_owned()));
        assert!(predictions.contains(&"Cargo.toml".to_owned()));
    }

    #[test]
    fn predicts_parent_mod_from_child_module() {
        let mut preloader = preloader();
        let predictions = preloader.predict_next("coding_new", &["src/server/context_gate.rs"]);

        assert!(predictions.contains(&"src/server/mod.rs".to_owned()));
    }

    #[test]
    fn tracks_prediction_accuracy() {
        let mut preloader = preloader();
        let predictions = preloader.predict_next("coding_new", &["src/lib.rs"]);
        let predicted = predictions.len() as f32;

        preloader.on_file_read("Cargo.toml");

        assert_eq!(preloader.accuracy(), 1.0 / predicted);
    }

    #[test]
    fn debugging_task_predicts_logs_tests_and_configuration() {
        let mut preloader = preloader();
        let predictions = preloader.predict_next("coding_fix", &["src/proxy/pre_optimize.rs"]);

        assert!(predictions.contains(&"logs/error.log".to_owned()));
        assert!(predictions.contains(&".lean-ctx/config.toml".to_owned()));
        assert!(predictions.contains(&"tests/pre_optimize.rs".to_owned()));
    }

    #[test]
    fn test_file_predicts_implementation() {
        let mut preloader = preloader();
        let predictions = preloader.predict_next("coding_fix", &["tests/context_gate.rs"]);

        assert!(predictions.contains(&"src/context_gate.rs".to_owned()));
    }
}
