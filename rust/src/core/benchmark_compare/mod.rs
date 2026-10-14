pub mod competitors;
pub mod metrics;
pub mod report;
pub mod system_info;

use std::path::Path;

use report::CompareReport;

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
pub(crate) mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// Small multi-language project shared by every `benchmark_compare` test.
    /// Measuring the real `src/` tree took ~3 min per test and set the tail of
    /// the lib suite; the report plumbing is identical on a fixture.
    pub(crate) fn fixture_root() -> &'static Path {
        static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
        DIR.get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let files: &[(&str, &str)] = &[
                (
                    "config.rs",
                    "use std::collections::HashMap;\n\n/// Parsed configuration.\npub struct Config {\n    pub values: HashMap<String, String>,\n}\n\nimpl Config {\n    pub fn parse(input: &str) -> Result<Self, String> {\n        let mut values = HashMap::new();\n        for line in input.lines() {\n            let (k, v) = line.split_once('=').ok_or_else(|| format!(\"bad line: {line}\"))?;\n            values.insert(k.trim().to_string(), v.trim().to_string());\n        }\n        Ok(Self { values })\n    }\n}\n",
                ),
                (
                    "errors.rs",
                    "#[derive(Debug)]\npub enum AppError {\n    Io(std::io::Error),\n    Parse(String),\n}\n\n/// Error handling helper used by every function in the fixture.\npub fn describe(err: &AppError) -> String {\n    match err {\n        AppError::Io(e) => format!(\"io: {e}\"),\n        AppError::Parse(msg) => format!(\"parse: {msg}\"),\n    }\n}\n\n#[cfg(test)]\nmod test {\n    #[test]\n    fn describes_parse() {}\n}\n",
                ),
                (
                    "client.ts",
                    "export interface Options { retries: number; timeoutMs: number }\n\nexport async function fetchWithRetry(url: string, opts: Options): Promise<string> {\n  let lastError: unknown;\n  for (let i = 0; i < opts.retries; i++) {\n    try {\n      const res = await fetch(url);\n      return await res.text();\n    } catch (err) {\n      lastError = err; // error handling: retry\n    }\n  }\n  throw lastError;\n}\n",
                ),
                (
                    "parse.py",
                    "def parse(text):\n    \"\"\"Parse key=value configuration lines.\"\"\"\n    result = {}\n    for line in text.splitlines():\n        if '=' not in line:\n            raise ValueError(f'bad line: {line}')\n        key, value = line.split('=', 1)\n        result[key.strip()] = value.strip()\n    return result\n\n\ndef test_parse():\n    assert parse('a=1') == {'a': '1'}\n",
                ),
            ];
            for (name, body) in files {
                std::fs::write(dir.path().join(name), body).unwrap();
            }
            dir
        })
        .path()
    }

    /// One shared report: every consumer only renders or inspects it.
    pub(crate) fn fixture_report() -> &'static CompareReport {
        static REPORT: OnceLock<CompareReport> = OnceLock::new();
        REPORT.get_or_init(|| run_compare(fixture_root(), None))
    }

    #[test]
    fn run_compare_produces_valid_report() {
        let report = fixture_report();
        assert!(report.metrics.project_benchmark.files_measured > 0);
        assert!(!report.competitors.is_empty());
        assert!(
            !report.system.lean_ctx_version.is_empty(),
            "`report.system.lean_ctx_version` is empty"
        );
    }

    #[test]
    fn run_compare_writes_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("test_benchmarks.md");
        let out_str = out_path.to_string_lossy().to_string();

        let report = run_compare(fixture_root(), Some(&out_str));
        assert!(out_path.exists());

        let content = std::fs::read_to_string(&out_path).unwrap();
        assert!(content.contains("Research-only representation comparison"));
        assert!(report.metrics.project_benchmark.files_measured > 0);
    }
}
