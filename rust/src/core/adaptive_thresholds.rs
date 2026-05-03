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
        let arm = bandit.select_arm();
        base.bpe_entropy = base.bpe_entropy * 0.5 + arm.entropy_threshold * 0.5;
        base.jaccard = base.jaccard * 0.5 + arm.jaccard_threshold * 0.5;
        LAST_BANDIT_ARM
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace((project_root, bandit_key, arm.name.clone()));
    }

    base
}

pub fn report_bandit_outcome(success: bool) {
    let data = LAST_BANDIT_ARM
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some((project_root, bandit_key, arm_name)) = data {
        let mut store = super::bandit::BanditStore::load(&project_root);
        store.get_or_create(&bandit_key).update(&arm_name, success);
        let _ = store.save(&project_root);
    }
}

static LAST_BANDIT_ARM: std::sync::Mutex<Option<(String, String, String)>> =
    std::sync::Mutex::new(None);

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
}
