use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::core::benchmark::{self, ModeSummary, ProjectBenchmark};
use crate::core::bm25_index::BM25Index;
use crate::core::tokens::count_tokens;

#[derive(Debug, Clone)]
pub struct SearchLatency {
    pub query: String,
    pub bm25_us: u64,
    pub result_count: usize,
}

#[derive(Debug, Clone)]
pub struct DiskFootprint {
    pub bm25_index_bytes: u64,
    pub total_index_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ColdStartTiming {
    pub scan_us: u64,
    pub bm25_build_us: u64,
    pub first_read_us: u64,
    pub total_us: u64,
}

#[derive(Debug, Clone)]
pub struct ModeComparison {
    pub mode: String,
    pub avg_compression_pct: f64,
    pub avg_latency_us: u64,
    pub avg_quality: f64,
    pub total_raw_tokens: usize,
    pub total_compressed_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct ComparativeMetrics {
    pub project_benchmark: ProjectBenchmark,
    pub mode_comparisons: Vec<ModeComparison>,
    pub search_latencies: Vec<SearchLatency>,
    pub disk_footprint: DiskFootprint,
    pub cold_start: ColdStartTiming,
    pub feature_count: usize,
}

const SEARCH_QUERIES: &[&str] = &[
    "function",
    "error handling",
    "configuration",
    "parse",
    "test",
];

pub fn measure_all(root: &Path) -> ComparativeMetrics {
    let root_str = root.to_string_lossy();

    let project_benchmark = benchmark::run_project_benchmark(&root_str);
    let mode_comparisons = build_mode_comparisons(&project_benchmark);
    let search_latencies = measure_search_latency(root);
    let disk_footprint = measure_disk_footprint(root);
    let cold_start = measure_cold_start(root);

    ComparativeMetrics {
        project_benchmark,
        mode_comparisons,
        search_latencies,
        disk_footprint,
        cold_start,
        feature_count: count_features(),
    }
}

fn build_mode_comparisons(bench: &ProjectBenchmark) -> Vec<ModeComparison> {
    let mode_names = ["full", "map", "signatures", "aggressive", "entropy"];

    mode_names
        .iter()
        .filter_map(|mode_name| {
            let summary = if *mode_name == "full" {
                Some(ModeSummary {
                    mode: "full".to_string(),
                    total_compressed_tokens: bench.total_raw_tokens,
                    avg_savings_pct: 0.0,
                    avg_latency_us: 0,
                    avg_preservation: 1.0,
                })
            } else {
                bench
                    .mode_summaries
                    .iter()
                    .find(|m| m.mode == *mode_name)
                    .cloned()
            };

            summary.map(|s| ModeComparison {
                mode: s.mode.clone(),
                avg_compression_pct: s.avg_savings_pct,
                avg_latency_us: s.avg_latency_us,
                avg_quality: if s.avg_preservation < 0.0 {
                    0.0
                } else {
                    s.avg_preservation
                },
                total_raw_tokens: bench.total_raw_tokens,
                total_compressed_tokens: s.total_compressed_tokens,
            })
        })
        .collect()
}

fn measure_search_latency(root: &Path) -> Vec<SearchLatency> {
    let index = crate::core::index_orchestrator::load_or_build_bm25(root);

    SEARCH_QUERIES
        .iter()
        .map(|query| {
            let start = Instant::now();
            let results = index.search(query, 10);
            let elapsed = start.elapsed();

            SearchLatency {
                query: (*query).to_string(),
                bm25_us: elapsed.as_micros() as u64,
                result_count: results.len(),
            }
        })
        .collect()
}

fn measure_disk_footprint(root: &Path) -> DiskFootprint {
    let bm25_path = BM25Index::index_file_path(root);
    let bm25_bytes = std::fs::metadata(&bm25_path).map_or(0, |m| m.len());

    let index_dir = root.join(".lean-ctx");
    let total_bytes = if index_dir.exists() {
        walkdir::WalkDir::new(&index_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.metadata().map_or(0, |m| m.len()))
            .sum()
    } else {
        bm25_bytes
    };

    DiskFootprint {
        bm25_index_bytes: bm25_bytes,
        total_index_bytes: total_bytes,
    }
}

fn measure_cold_start(root: &Path) -> ColdStartTiming {
    let scan_start = Instant::now();
    let files = list_text_files(root, 20);
    let scan_us = scan_start.elapsed().as_micros() as u64;

    let bm25_start = Instant::now();
    let _index = crate::core::index_orchestrator::load_or_build_bm25(root);
    let bm25_build_us = bm25_start.elapsed().as_micros() as u64;

    let read_start = Instant::now();
    if let Some(first_file) = files.first()
        && let Ok(content) = std::fs::read_to_string(first_file)
    {
        let _ = count_tokens(&content);
    }
    let first_read_us = read_start.elapsed().as_micros() as u64;

    ColdStartTiming {
        scan_us,
        bm25_build_us,
        first_read_us,
        total_us: scan_us + bm25_build_us + first_read_us,
    }
}

fn list_text_files(root: &Path, max: usize) -> Vec<PathBuf> {
    let code_exts = [
        "rs", "ts", "tsx", "js", "py", "go", "java", "c", "cpp", "rb",
    ];

    walkdir::WalkDir::new(root)
        .max_depth(6)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            if e.file_type().is_dir() {
                return !matches!(
                    name.as_ref(),
                    "node_modules" | ".git" | "target" | "dist" | "build" | "__pycache__"
                );
            }
            true
        })
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|ext| code_exts.contains(&ext))
        })
        .take(max)
        .map(walkdir::DirEntry::into_path)
        .collect()
}

fn count_features() -> usize {
    // Counted from lean-ctx capabilities:
    // 10 read modes + BM25 search + semantic search + shell compression
    // + session caching + CCP + knowledge + call graph + repomap
    // + pack + multi-repo
    // Each is a real, shipped feature.
    let read_modes = 10;
    let search = 2; // BM25 + semantic
    let compression = 3; // shell, aggressive, entropy
    let session = 3; // caching, CCP, session restore
    let analysis = 3; // knowledge, call graph, repomap
    let ops = 2; // pack, multi-repo
    read_modes + search + compression + session + analysis + ops
}

pub fn avg_search_latency_us(latencies: &[SearchLatency]) -> u64 {
    if latencies.is_empty() {
        return 0;
    }
    let total: u64 = latencies.iter().map(|l| l.bm25_us).sum();
    total / latencies.len() as u64
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_duration_us(us: u64) -> String {
    if us >= 1_000_000 {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    } else if us >= 1000 {
        format!("{:.1}ms", us as f64 / 1000.0)
    } else {
        format!("{us}μs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_ranges() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5_242_880), "5.0 MB");
    }

    #[test]
    fn format_duration_ranges() {
        assert_eq!(format_duration_us(500), "500μs");
        assert_eq!(format_duration_us(1500), "1.5ms");
        assert_eq!(format_duration_us(2_500_000), "2.50s");
    }

    #[test]
    fn count_features_is_reasonable() {
        let n = count_features();
        assert!(n >= 15, "lean-ctx has many features; got {n}");
        assert!(n <= 50, "feature count should be realistic; got {n}");
    }

    #[test]
    fn avg_search_latency_empty() {
        assert_eq!(avg_search_latency_us(&[]), 0);
    }

    #[test]
    fn build_mode_comparisons_includes_full() {
        let bench = crate::core::benchmark::run_project_benchmark("src");
        let comps = build_mode_comparisons(&bench);
        assert!(comps.iter().any(|c| c.mode == "full"));
        assert!(comps.iter().any(|c| c.mode == "map"));
    }

    #[test]
    fn measure_all_on_src() {
        let root = Path::new("src");
        let metrics = measure_all(root);
        assert!(metrics.project_benchmark.files_measured > 0);
        assert!(!metrics.mode_comparisons.is_empty());
        assert!(!metrics.search_latencies.is_empty());
        assert!(metrics.feature_count > 0);
    }
}
