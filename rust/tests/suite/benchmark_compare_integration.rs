use std::path::Path;

use lean_ctx::core::benchmark_compare;
use lean_ctx::core::benchmark_compare::competitors;
use lean_ctx::core::benchmark_compare::metrics;
use lean_ctx::core::benchmark_compare::report;
use lean_ctx::core::benchmark_compare::system_info;

#[test]
fn end_to_end_compare_on_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path();

    create_fixture(fixture);

    let report = benchmark_compare::run_compare(fixture, None);

    assert!(
        report.metrics.project_benchmark.files_measured > 0,
        "should measure at least one file"
    );
    assert!(
        !report.metrics.mode_comparisons.is_empty(),
        "should produce mode comparisons"
    );
    assert!(
        !report.competitors.is_empty(),
        "should include competitor profiles"
    );
    assert!(
        report.metrics.feature_count > 10,
        "lean-ctx has many features"
    );
}

#[test]
fn compare_generates_valid_markdown() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path();
    create_fixture(fixture);

    let out_path = dir.path().join("BENCHMARKS.md");
    let out_str = out_path.to_string_lossy().to_string();

    let r = benchmark_compare::run_compare(fixture, Some(&out_str));

    assert!(out_path.exists(), "BENCHMARKS.md should be created");

    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(content.contains("# lean-ctx Benchmark: Head-to-Head Comparison"));
    assert!(content.contains("## Methodology"));
    assert!(content.contains("## Compression Comparison"));
    assert!(content.contains("## Feature Comparison"));
    assert!(content.contains("## Reproducibility"));
    assert!(content.contains("Repomix"));
    assert!(content.contains("lean-ctx"));

    let terminal = report::generate_terminal(&r);
    assert!(terminal.contains("Head-to-Head Benchmark"));
}

#[test]
fn competitor_profiles_are_consistent() {
    let comps = competitors::all_competitors();

    for c in &comps {
        assert!(!c.name.is_empty());
        assert!(!c.version.is_empty());
        assert!(!c.source.is_empty());
        if c.name != "Raw file read" {
            assert!(!c.url.is_empty(), "{} should have a URL", c.name);
        }
    }

    let baseline = comps.iter().find(|c| c.name == "Raw file read").unwrap();
    assert_eq!(baseline.compression_pct, Some(0.0));
    assert_eq!(baseline.feature_count, 1);
}

#[test]
fn system_info_is_populated() {
    let sys = system_info::collect();
    assert!(!sys.os.is_empty());
    assert!(!sys.arch.is_empty());
    assert!(!sys.lean_ctx_version.is_empty());
    assert!(sys.cpu_cores >= 1);
    assert!(sys.memory_gb > 0.0, "should detect system RAM");
}

#[test]
fn search_latency_measurement_works() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path();
    create_fixture(fixture);

    let m = metrics::measure_all(fixture);

    for sl in &m.search_latencies {
        assert!(!sl.query.is_empty());
    }
}

#[test]
fn mode_comparisons_include_expected_modes() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path();
    create_fixture(fixture);

    let m = metrics::measure_all(fixture);

    let mode_names: Vec<&str> = m.mode_comparisons.iter().map(|c| c.mode.as_str()).collect();
    assert!(mode_names.contains(&"full"), "should include full mode");
    assert!(mode_names.contains(&"map"), "should include map mode");

    for mc in &m.mode_comparisons {
        assert!(
            mc.avg_compression_pct >= 0.0 && mc.avg_compression_pct <= 100.0,
            "{}: compression {:.1}% out of range",
            mc.mode,
            mc.avg_compression_pct
        );
    }
}

fn create_fixture(root: &Path) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();

    std::fs::write(
        src.join("main.rs"),
        r#"
use std::collections::HashMap;

fn main() {
    let mut map: HashMap<String, Vec<i32>> = HashMap::new();
    map.insert("hello".to_string(), vec![1, 2, 3]);
    process(&map);
}

fn process(data: &HashMap<String, Vec<i32>>) {
    for (key, values) in data {
        let sum: i32 = values.iter().sum();
        println!("{key}: sum={sum}, count={}", values.len());
    }
}

pub struct Config {
    pub name: String,
    pub timeout: u64,
    pub retries: usize,
}

impl Config {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            timeout: 30,
            retries: 3,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("name cannot be empty".to_string());
        }
        if self.timeout == 0 {
            return Err("timeout must be positive".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation() {
        let c = Config::new("test");
        assert!(c.validate().is_ok());
    }
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("lib.rs"),
        r#"
pub mod utils;

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn multiply(a: i32, b: i32) -> i32 {
    a * b
}

pub trait Processor {
    fn process(&self, input: &str) -> String;
    fn name(&self) -> &str;
}

pub struct UpperCase;

impl Processor for UpperCase {
    fn process(&self, input: &str) -> String {
        input.to_uppercase()
    }

    fn name(&self) -> &str {
        "uppercase"
    }
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("utils.rs"),
        r"
use std::path::Path;

pub fn file_exists(path: &str) -> bool {
    Path::new(path).exists()
}

pub fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len]
    }
}

pub fn parse_key_value(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(2, '=');
    let key = parts.next()?.trim();
    let value = parts.next()?.trim();
    Some((key, value))
}
",
    )
    .unwrap();
}
