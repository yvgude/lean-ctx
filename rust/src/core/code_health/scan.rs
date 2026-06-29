//! Project-wide code-health scan — the shared report behind `lean-ctx health`,
//! the `ctx_quality` tool, and the dashboard.
//!
//! Walks the repo once, analyzes each source file with the engine, and
//! aggregates into a [`NavigabilityScore`] plus focused per-file detail. The
//! "quality tax" is grounded in real data: the token count of the function
//! bodies that exceed the threshold — the tokens an agent must read in full
//! because the code cannot be navigated by signature.

use super::{Hotspot, NamingFinding, NavigabilityInputs, NavigabilityScore, analyze_file, grade};
use std::path::Path;

/// Source extensions the engine can analyze (mirrors `core::chunks_ts`).
const HEALTH_SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "cc", "cxx", "hpp",
];

/// Per-file health detail (only files with findings are retained in a report).
#[derive(Debug, Clone)]
pub struct FileReport {
    pub file: String,
    pub total_functions: usize,
    pub over_threshold: usize,
    pub worst_cognitive: u32,
    pub hotspots: Vec<Hotspot>,
    pub naming: Vec<NamingFinding>,
    /// Tokens locked inside over-threshold functions.
    pub wasted_tokens: u64,
}

/// Aggregated project health.
#[derive(Debug, Clone)]
pub struct ProjectHealth {
    pub score: NavigabilityScore,
    /// Files with at least one hotspot or naming finding, worst first.
    pub files: Vec<FileReport>,
    pub naming_count: usize,
}

impl ProjectHealth {
    /// Letter grade for the project score.
    pub fn grade(&self) -> char {
        grade(self.score.score)
    }
}

/// Scan `root` for code-health, pricing the quality tax with `model` (or the
/// blended fallback when `None`). `top_n` bounds the hotspot list in the score.
pub fn scan_project(
    root: &Path,
    threshold: u32,
    model: Option<&str>,
    top_n: usize,
) -> ProjectHealth {
    use rayon::prelude::*;

    let files = walk_sources(root);
    let mut reports: Vec<FileReport> = files
        .par_iter()
        .filter_map(|(path, content, ext)| analyze_one(path, content, ext, threshold))
        .collect();
    reports.sort_by(|a, b| {
        b.worst_cognitive
            .cmp(&a.worst_cognitive)
            .then_with(|| a.file.cmp(&b.file))
    });

    let functions_total: usize = reports.iter().map(|r| r.total_functions).sum();
    let over_threshold: usize = reports.iter().map(|r| r.over_threshold).sum();
    let worst_cognitive: u32 = reports.iter().map(|r| r.worst_cognitive).max().unwrap_or(0);
    let wasted_tokens: u64 = reports.iter().map(|r| r.wasted_tokens).sum();
    let naming_count: usize = reports.iter().map(|r| r.naming.len()).sum();

    let mut all_hotspots: Vec<Hotspot> = reports.iter().flat_map(|r| r.hotspots.clone()).collect();
    all_hotspots.sort_by(|a, b| {
        b.cognitive
            .cmp(&a.cognitive)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });

    let input_price_per_m = crate::core::gain::model_pricing::ModelPricing::load()
        .quote(model)
        .cost
        .input_per_m;

    let score = super::navigability(NavigabilityInputs {
        functions_total,
        over_threshold,
        worst_cognitive,
        import_cycles: 0,
        wasted_tokens,
        input_price_per_m,
        hotspots: &all_hotspots,
        top_n,
    });

    // Keep only files that actually have something to report.
    reports.retain(|r| r.over_threshold > 0 || !r.naming.is_empty());

    ProjectHealth {
        score,
        files: reports,
        naming_count,
    }
}

fn analyze_one(path: &str, content: &str, ext: &str, threshold: u32) -> Option<FileReport> {
    let health = analyze_file(content, ext)?;
    let lines: Vec<&str> = content.lines().collect();

    let mut hotspots = Vec::new();
    let mut wasted_tokens: u64 = 0;
    for f in &health.functions {
        if f.cognitive > threshold {
            hotspots.push(Hotspot {
                file: path.to_string(),
                symbol: f.name.clone(),
                line: f.line,
                cognitive: f.cognitive,
            });
            wasted_tokens += span_tokens(&lines, f.line, f.end_line);
        }
    }

    Some(FileReport {
        file: path.to_string(),
        total_functions: health.functions.len(),
        over_threshold: hotspots.len(),
        worst_cognitive: health.worst_cognitive(),
        hotspots,
        naming: health.naming,
        wasted_tokens,
    })
}

/// Token count of the source lines `start..=end` (1-based, inclusive).
fn span_tokens(lines: &[&str], start: usize, end: usize) -> u64 {
    if start == 0 || start > lines.len() {
        return 0;
    }
    let hi = end.min(lines.len());
    let body = lines[start - 1..hi].join("\n");
    crate::core::tokens::count_tokens(&body) as u64
}

/// Walk `root` for engine-supported source files. Returns `(rel_path, content,
/// ext)` sorted by path for deterministic aggregation.
fn walk_sources(root: &Path) -> Vec<(String, String, String)> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(crate::core::walk_filter::keep_entry)
        .build();

    let mut out: Vec<(String, String, String)> = Vec::new();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !HEALTH_SOURCE_EXTS.contains(&ext) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        // Skip vendored / minified / generated files (e.g. `*.min.js`, `vendor/`,
        // `dist/`): they are third-party or machine-emitted, so their complexity
        // is not the project's quality signal. Reuses the shared noise filter.
        if crate::core::auto_findings::is_noise_path(&rel) {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(path) {
            out.push((rel, content, ext.to_string()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(all(test, feature = "tree-sitter"))]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write fixture");
    }

    #[test]
    fn scan_aggregates_hotspots_and_score() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "clean.rs",
            "fn add_one(x: i32) -> i32 { x + 1 }\n",
        );
        write(
            tmp.path(),
            "messy.rs",
            "fn deep(a: bool) { if a { if a { if a { if a { if a { if a {} } } } } } }\n",
        );

        let health = scan_project(tmp.path(), 15, Some("gpt-5.4"), 10);
        assert_eq!(health.files.len(), 1, "only the messy file is reported");
        assert_eq!(health.files[0].file, "messy.rs");
        assert_eq!(health.score.over_threshold, 1);
        assert!(health.score.worst_cognitive >= 16);
        assert!(health.score.score < 100, "complexity lowers the score");
        assert!(
            health.score.estimated_waste_usd > 0.0,
            "tax priced from tokens"
        );
    }

    #[test]
    fn clean_project_is_perfect() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "ok.rs", "fn add_one(x: i32) -> i32 { x + 1 }\n");
        let health = scan_project(tmp.path(), 15, None, 10);
        assert_eq!(health.score.score, 100);
        assert_eq!(health.grade(), 'A');
        assert!(health.files.is_empty());
    }
}
