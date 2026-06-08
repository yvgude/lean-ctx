//! Deterministic scenario matrix for the reproducible scorecard (#211).
//!
//! Each scenario materializes a synthetic, fully deterministic corpus (content
//! derived purely from the file index — no RNG) plus a labeled query set. The
//! same scenario always produces byte-identical files, so compression savings
//! and retrieval recall/MRR are reproducible across runs and machines.

use std::fs;
use std::path::Path;

/// A committed scorecard scenario.
pub(super) struct Scenario {
    pub(super) name: &'static str,
    /// Number of source files to generate.
    pub(super) files: usize,
    /// Emit one labeled query for every `query_step`-th file.
    pub(super) query_step: usize,
}

/// The committed scenario matrix (small / medium / large).
pub(super) const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "small",
        files: 12,
        query_step: 3,
    },
    Scenario {
        name: "medium",
        files: 48,
        query_step: 4,
    },
    Scenario {
        name: "large",
        files: 120,
        query_step: 5,
    },
];

/// Topic buckets. Each file belongs to one topic; queries combine the topic
/// with a per-file unique marker so the expected file is unambiguous.
const TOPICS: &[&str] = &[
    "authentication",
    "database",
    "caching",
    "networking",
    "serialization",
    "validation",
    "scheduling",
    "logging",
];

/// A labeled retrieval query with its single expected (relative) file path.
pub(super) struct LabeledQuery {
    pub(super) query: String,
    pub(super) expected_file: String,
}

/// Materialize `scenario` into `root`, returning the labeled query set.
pub(super) fn materialize(scenario: &Scenario, root: &Path) -> std::io::Result<Vec<LabeledQuery>> {
    let mut queries = Vec::with_capacity(scenario.files / scenario.query_step + 1);
    for j in 0..scenario.files {
        let topic = TOPICS[j % TOPICS.len()];
        let rel = format!("src/{topic}/file_{j:03}.rs");
        let path = root.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let marker = format!("marker_{j:04}");
        fs::write(&path, generate_file(topic, &marker, j))?;
        if j % scenario.query_step == 0 {
            queries.push(LabeledQuery {
                query: format!("{topic} {marker}"),
                expected_file: rel,
            });
        }
    }
    Ok(queries)
}

/// Deterministic, realistic-looking Rust source. File size grows strictly with
/// `j` so the benchmark's size-based file sampling is itself deterministic.
fn generate_file(topic: &str, marker: &str, j: usize) -> String {
    let body_lines = 6 + j; // strictly increasing → unique file sizes
    let mut s = String::new();
    s.push_str(&format!(
        "//! Module handling {topic} concerns (scenario file {j}).\n"
    ));
    s.push_str(&format!("//! Unique retrieval marker: {marker}.\n\n"));
    s.push_str(&format!("use crate::{topic}::Context;\n\n"));
    s.push_str(&format!(
        "/// Primary entry point for {topic}; see `{marker}`.\n"
    ));
    s.push_str(&format!(
        "pub fn {marker}(ctx: &Context) -> Result<usize, String> {{\n"
    ));
    s.push_str(&format!("    let mut total = {j};\n"));
    for k in 0..body_lines {
        s.push_str(&format!(
            "    let {topic}_value_{k} = compute_{topic}({k}, {j});\n"
        ));
        s.push_str(&format!("    total += {topic}_value_{k};\n"));
    }
    s.push_str("    Ok(total)\n}\n\n");
    s.push_str(&format!(
        "fn compute_{topic}(seed: usize, base: usize) -> usize {{ seed.wrapping_mul(base + 1) }}\n"
    ));
    s
}
