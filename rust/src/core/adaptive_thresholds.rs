use std::collections::HashMap;
use std::path::Path;

use super::entropy::kolmogorov_proxy;

#[derive(Debug, Clone)]
pub struct CompressionThresholds {
    pub bpe_entropy: f64,
    pub jaccard: f64,
    pub auto_delta: f64,
}

impl Default for CompressionThresholds {
    fn default() -> Self {
        Self {
            bpe_entropy: 1.0,
            jaccard: 0.7,
            auto_delta: 0.6,
        }
    }
}

static LANGUAGE_THRESHOLDS: &[(&str, CompressionThresholds)] = &[
    // Python: English-like syntax, significant whitespace → higher entropy baseline
    (
        "py",
        CompressionThresholds {
            bpe_entropy: 1.2,
            jaccard: 0.65,
            auto_delta: 0.55,
        },
    ),
    // Rust: Repetitive keywords (fn, pub, impl, let, mut) → lower threshold catches more
    (
        "rs",
        CompressionThresholds {
            bpe_entropy: 0.85,
            jaccard: 0.72,
            auto_delta: 0.6,
        },
    ),
    // TypeScript/JavaScript: Type annotations are predictable
    (
        "ts",
        CompressionThresholds {
            bpe_entropy: 0.95,
            jaccard: 0.68,
            auto_delta: 0.58,
        },
    ),
    (
        "tsx",
        CompressionThresholds {
            bpe_entropy: 0.95,
            jaccard: 0.68,
            auto_delta: 0.58,
        },
    ),
    (
        "js",
        CompressionThresholds {
            bpe_entropy: 1.0,
            jaccard: 0.68,
            auto_delta: 0.58,
        },
    ),
    (
        "jsx",
        CompressionThresholds {
            bpe_entropy: 1.0,
            jaccard: 0.68,
            auto_delta: 0.58,
        },
    ),
    // Go: Verbose but highly structured → aggressive threshold
    (
        "go",
        CompressionThresholds {
            bpe_entropy: 0.9,
            jaccard: 0.72,
            auto_delta: 0.55,
        },
    ),
    // Java/Kotlin: Very verbose, lots of boilerplate
    (
        "java",
        CompressionThresholds {
            bpe_entropy: 0.8,
            jaccard: 0.65,
            auto_delta: 0.5,
        },
    ),
    (
        "kt",
        CompressionThresholds {
            bpe_entropy: 0.85,
            jaccard: 0.68,
            auto_delta: 0.55,
        },
    ),
    // C/C++: Headers are highly repetitive
    (
        "c",
        CompressionThresholds {
            bpe_entropy: 0.9,
            jaccard: 0.7,
            auto_delta: 0.6,
        },
    ),
    (
        "h",
        CompressionThresholds {
            bpe_entropy: 0.75,
            jaccard: 0.65,
            auto_delta: 0.5,
        },
    ),
    (
        "cpp",
        CompressionThresholds {
            bpe_entropy: 0.9,
            jaccard: 0.7,
            auto_delta: 0.6,
        },
    ),
    (
        "hpp",
        CompressionThresholds {
            bpe_entropy: 0.75,
            jaccard: 0.65,
            auto_delta: 0.5,
        },
    ),
    // Ruby: English-like, high entropy
    (
        "rb",
        CompressionThresholds {
            bpe_entropy: 1.15,
            jaccard: 0.65,
            auto_delta: 0.55,
        },
    ),
    // Config/data files: highly repetitive
    (
        "json",
        CompressionThresholds {
            bpe_entropy: 0.6,
            jaccard: 0.6,
            auto_delta: 0.4,
        },
    ),
    (
        "yaml",
        CompressionThresholds {
            bpe_entropy: 0.7,
            jaccard: 0.62,
            auto_delta: 0.45,
        },
    ),
    (
        "yml",
        CompressionThresholds {
            bpe_entropy: 0.7,
            jaccard: 0.62,
            auto_delta: 0.45,
        },
    ),
    (
        "toml",
        CompressionThresholds {
            bpe_entropy: 0.7,
            jaccard: 0.62,
            auto_delta: 0.45,
        },
    ),
    (
        "xml",
        CompressionThresholds {
            bpe_entropy: 0.6,
            jaccard: 0.6,
            auto_delta: 0.4,
        },
    ),
    // Markdown/docs: natural language, high entropy
    (
        "md",
        CompressionThresholds {
            bpe_entropy: 1.3,
            jaccard: 0.6,
            auto_delta: 0.55,
        },
    ),
    // CSS: very repetitive selectors/properties
    (
        "css",
        CompressionThresholds {
            bpe_entropy: 0.7,
            jaccard: 0.6,
            auto_delta: 0.45,
        },
    ),
    (
        "scss",
        CompressionThresholds {
            bpe_entropy: 0.75,
            jaccard: 0.62,
            auto_delta: 0.48,
        },
    ),
    // SQL: repetitive keywords
    (
        "sql",
        CompressionThresholds {
            bpe_entropy: 0.8,
            jaccard: 0.65,
            auto_delta: 0.5,
        },
    ),
    // Shell scripts
    (
        "sh",
        CompressionThresholds {
            bpe_entropy: 1.0,
            jaccard: 0.68,
            auto_delta: 0.55,
        },
    ),
    (
        "bash",
        CompressionThresholds {
            bpe_entropy: 1.0,
            jaccard: 0.68,
            auto_delta: 0.55,
        },
    ),
    // Swift/C#
    (
        "swift",
        CompressionThresholds {
            bpe_entropy: 0.9,
            jaccard: 0.68,
            auto_delta: 0.55,
        },
    ),
    (
        "cs",
        CompressionThresholds {
            bpe_entropy: 0.85,
            jaccard: 0.65,
            auto_delta: 0.52,
        },
    ),
    // PHP
    (
        "php",
        CompressionThresholds {
            bpe_entropy: 0.95,
            jaccard: 0.68,
            auto_delta: 0.55,
        },
    ),
];

fn language_map() -> HashMap<&'static str, &'static CompressionThresholds> {
    LANGUAGE_THRESHOLDS
        .iter()
        .map(|(ext, t)| (*ext, t))
        .collect()
}

#[must_use]
pub fn thresholds_for_path(path: &str) -> CompressionThresholds {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let map = language_map();
    if let Some(t) = map.get(ext) {
        return (*t).clone();
    }

    CompressionThresholds::default()
}

#[must_use]
pub fn adaptive_thresholds(path: &str, content: &str) -> CompressionThresholds {
    let mut base = thresholds_for_path(path);

    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let feedback = super::feedback::FeedbackStore::load();
    if let Some(learned_entropy) = feedback.get_learned_entropy(ext) {
        base.bpe_entropy = base.bpe_entropy * 0.6 + learned_entropy * 0.4;
    }
    if let Some(learned_jaccard) = feedback.get_learned_jaccard(ext) {
        base.jaccard = base.jaccard * 0.6 + learned_jaccard * 0.4;
    }

    // Quality-signal loop (#538): bounce/edit-fail history shifts the threshold
    // additively, clamped to ±0.15 inside threshold_learning. Applied before the
    // Kolmogorov adjustment so the final clamp below still bounds everything.
    base.bpe_entropy =
        (base.bpe_entropy + super::threshold_learning::learned_delta(ext)).clamp(0.4, 2.0);

    if content.len() > 500 {
        let k = kolmogorov_proxy(content);
        let k_adjustment = (k - 0.45) * 0.5;
        base.bpe_entropy = (base.bpe_entropy + k_adjustment).clamp(0.4, 2.0);
        base.jaccard = (base.jaccard - k_adjustment * 0.3).clamp(0.5, 0.85);
    }

    if let Some(project_root) =
        crate::core::session::SessionState::load_latest().and_then(|s| s.project_root)
    {
        let bandit_key = format!("{ext}_{}", token_bucket_label(content));
        let mut store = super::bandit::BanditStore::load(&project_root);
        let bandit = store.get_or_create(&bandit_key);
        // #4: deterministic argmax-of-mean by default (Thompson only under the
        // stochastic flag). The chosen arm drives both the entropy/jaccard
        // thresholds AND the learned FieldWeights that feed Phi.
        let arm = bandit.choose_arm();
        base.bpe_entropy = base.bpe_entropy * 0.5 + arm.entropy_threshold * 0.5;
        base.jaccard = base.jaccard * 0.5 + arm.jaccard_threshold * 0.5;
        let arm_name = arm.name.clone();
        super::context_field::set_active_weights(super::context_field::FieldWeights::from_arm(arm));
        record_selected_arm(path, project_root, bandit_key, arm_name);
    }

    base
}

/// The bandit arm selected for a file's most recent threshold-driven read, kept
/// so a *deferred* real outcome (bounce, edit-fail) can penalize the arm that
/// actually produced the compression — instead of a hardcoded success (#593).
#[derive(Clone)]
struct SelectedArm {
    project_root: String,
    bandit_key: String,
    arm_name: String,
}

/// Bounded far above the bounce (5) and edit-force (10) detection windows, so
/// evicting the oldest entry can never drop an arm a still-pending signal needs.
const ARM_REGISTRY_CAP: usize = 64;

static SELECTED_ARMS: std::sync::Mutex<Option<SelectedArmRegistry>> = std::sync::Mutex::new(None);

#[derive(Default)]
struct SelectedArmRegistry {
    /// Insertion order of distinct paths, for O(1) oldest-first eviction.
    order: std::collections::VecDeque<String>,
    by_path: std::collections::HashMap<String, SelectedArm>,
}

fn record_selected_arm(path: &str, project_root: String, bandit_key: String, arm_name: String) {
    let norm = crate::core::pathutil::normalize_tool_path(path);
    let mut guard = SELECTED_ARMS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let reg = guard.get_or_insert_with(SelectedArmRegistry::default);
    let arm = SelectedArm {
        project_root,
        bandit_key,
        arm_name,
    };
    if reg.by_path.insert(norm.clone(), arm).is_none() {
        reg.order.push_back(norm);
        while reg.order.len() > ARM_REGISTRY_CAP {
            if let Some(old) = reg.order.pop_front() {
                reg.by_path.remove(&old);
            }
        }
    }
}

/// Record a real downstream quality signal (#538/#593). It always feeds the
/// online threshold learner; `Bounce`/`EditFail` additionally penalize the
/// bandit arm that produced this file's compression, replacing the old
/// savings-only / hardcoded reward. `CleanCompressed`/`WastedFull` stay
/// learner-only — the positive bandit signal comes from realized savings
/// (`report_bandit_outcome_for_path` in entropy mode), so we avoid a bandit
/// disk write on every compressed read.
pub fn record_quality_signal(path: &str, signal: crate::core::threshold_learning::QualitySignal) {
    use crate::core::threshold_learning::QualitySignal;
    crate::core::threshold_learning::record_signal(path, signal);
    match signal {
        QualitySignal::Bounce | QualitySignal::EditFail => {
            report_bandit_outcome_for_path(path, false);
        }
        QualitySignal::CleanCompressed | QualitySignal::WastedFull => {}
    }
}

/// Reward (`success=true`) or penalize (`false`) the bandit arm recorded for
/// `path`. No-op when no arm was registered (e.g. the file was never read in a
/// threshold-driven mode), so attribution is never guessed.
pub fn report_bandit_outcome_for_path(path: &str, success: bool) {
    let norm = crate::core::pathutil::normalize_tool_path(path);
    let selected = {
        let guard = SELECTED_ARMS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.as_ref().and_then(|r| r.by_path.get(&norm).cloned())
    };
    if let Some(sel) = selected {
        let mut store = super::bandit::BanditStore::load(&sel.project_root);
        store
            .get_or_create(&sel.bandit_key)
            .update(&sel.arm_name, success);
        let _ = store.save(&sel.project_root);
    }
}

fn token_bucket_label(content: &str) -> &'static str {
    let len = content.len();
    match len {
        0..=2000 => "sm",
        2001..=10000 => "md",
        10001..=50000 => "lg",
        _ => "xl",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_has_lower_threshold_than_python() {
        let rs = thresholds_for_path("src/main.rs");
        let py = thresholds_for_path("src/main.py");
        assert!(rs.bpe_entropy < py.bpe_entropy);
    }

    #[test]
    fn json_has_lowest_threshold() {
        let json = thresholds_for_path("config.json");
        let rs = thresholds_for_path("main.rs");
        assert!(json.bpe_entropy < rs.bpe_entropy);
    }

    #[test]
    fn unknown_ext_uses_default() {
        let t = thresholds_for_path("file.xyz");
        assert!((t.bpe_entropy - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn adaptive_adjusts_for_compressibility() {
        let repetitive = "use std::io;\n".repeat(200);
        let diverse = (0..200).fold(String::new(), |mut s, i| {
            use std::fmt::Write;
            let _ = writeln!(s, "let var_{i} = compute_{i}(arg_{i});");
            s
        });

        let base_rep = thresholds_for_path("main.rs");
        let base_div = thresholds_for_path("main.rs");
        assert!(
            (base_rep.bpe_entropy - base_div.bpe_entropy).abs() < f64::EPSILON,
            "same path should get same base thresholds"
        );

        let k_rep = kolmogorov_proxy(&repetitive);
        let k_div = kolmogorov_proxy(&diverse);
        assert!(
            k_rep < k_div,
            "repetitive content should have lower Kolmogorov proxy: {k_rep} vs {k_div}"
        );
    }

    use crate::core::threshold_learning::QualitySignal;

    fn arm_mean(project_root: &str, key: &str, arm: &str) -> f64 {
        let mut store = crate::core::bandit::BanditStore::load(project_root);
        store
            .get_or_create(key)
            .arms
            .iter()
            .find(|a| a.name == arm)
            .map_or(0.5, crate::core::bandit::BanditArm::mean)
    }

    #[test]
    fn real_failure_signal_penalizes_selected_arm() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = "/fix1/penalize";
        record_selected_arm(
            "src/foo.rs",
            root.into(),
            "rs_md".into(),
            "aggressive".into(),
        );

        let before = arm_mean(root, "rs_md", "aggressive");
        for _ in 0..15 {
            record_quality_signal("src/foo.rs", QualitySignal::Bounce);
        }
        record_quality_signal("src/foo.rs", QualitySignal::EditFail);
        let after = arm_mean(root, "rs_md", "aggressive");

        assert!(
            after < before,
            "bounce/edit-fail must lower the selected arm mean: {before} -> {after}"
        );
    }

    #[test]
    fn clean_and_wasted_signals_leave_bandit_untouched() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = "/fix1/untouched";
        record_selected_arm("a.rs", root.into(), "rs_sm".into(), "balanced".into());

        let before = arm_mean(root, "rs_sm", "balanced");
        record_quality_signal("a.rs", QualitySignal::CleanCompressed);
        record_quality_signal("a.rs", QualitySignal::WastedFull);
        let after = arm_mean(root, "rs_sm", "balanced");

        assert!(
            (before - after).abs() < f64::EPSILON,
            "clean/wasted are learner-only: bandit mean must not move ({before} -> {after})"
        );
    }

    #[test]
    fn outcome_without_registered_arm_does_not_register_path() {
        let _data = crate::core::data_dir::isolated_data_dir();
        // Attribution must never be guessed for a path we never selected an arm for.
        report_bandit_outcome_for_path("fix1/never-seen-xyz.rs", false);
        let norm = crate::core::pathutil::normalize_tool_path("fix1/never-seen-xyz.rs");
        let guard = SELECTED_ARMS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let registered = guard
            .as_ref()
            .is_some_and(|r| r.by_path.contains_key(&norm));
        assert!(!registered, "no-op report must not create a registry entry");
    }

    #[test]
    fn registry_evicts_oldest_beyond_cap() {
        let _data = crate::core::data_dir::isolated_data_dir();
        for i in 0..(ARM_REGISTRY_CAP + 5) {
            record_selected_arm(
                &format!("evict/f{i}.rs"),
                "/fix1/evict".into(),
                "rs_md".into(),
                "balanced".into(),
            );
        }
        let guard = SELECTED_ARMS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let reg = guard.as_ref().expect("registry initialized");
        assert!(reg.order.len() <= ARM_REGISTRY_CAP);
        assert_eq!(reg.order.len(), reg.by_path.len());
    }
}
