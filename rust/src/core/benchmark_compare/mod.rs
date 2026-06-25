pub mod competitors;
pub mod metrics;
pub mod report;
pub mod system_info;

use std::path::Path;

use report::CompareReport;

#[must_use]
pub fn run_compare(root: &Path, output_path: Option<&str>) -> CompareReport {
    let metrics = metrics::measure_all(root);
    let system = system_info::collect();
    let competitors = competitors::all_competitors();

    let report = CompareReport {
        metrics,
        system,
        competitors,
    };

    if let Some(out_path) = output_path {
        let md = report::generate_markdown(&report);
        if let Err(e) = std::fs::write(out_path, &md) {
            eprintln!("Failed to write {out_path}: {e}");
        } else {
            eprintln!("Wrote benchmark report to {out_path}");
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::benchmark_compare::metrics::create_synthetic_benchmark_dir;

    #[test]
    fn run_compare_produces_valid_report() {
        let dir = create_synthetic_benchmark_dir();
        let report = run_compare(dir.path(), None);
        assert!(report.metrics.project_benchmark.files_measured > 0);
        assert!(!report.competitors.is_empty());
        assert!(!report.system.lean_ctx_version.is_empty());
    }

    #[test]
    fn run_compare_writes_output_file() {
        let dir = create_synthetic_benchmark_dir();
        let out_dir = tempfile::tempdir().unwrap();
        let out_path = out_dir.path().join("test_benchmarks.md");
        let out_str = out_path.to_string_lossy().to_string();

        let report = run_compare(dir.path(), Some(&out_str));
        assert!(out_path.exists());

        let content = std::fs::read_to_string(&out_path).unwrap();
        assert!(content.contains("Head-to-Head"));
        assert!(report.metrics.project_benchmark.files_measured > 0);
    }
}
