use super::calibration::CalibrationAccuracy;
use super::fidelity::FidelityClassV1;
use crate::core::tokens::{TokenizerFamily, count_tokens};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub(super) struct CorpusSample {
    pub name: &'static str,
    pub category: CorpusCategory,
    pub original: &'static str,
    pub compressed: &'static str,
    pub ext: &'static str,
    pub expected_fidelity: super::fidelity::FidelityClassV1,
    pub min_savings_pct: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CorpusCategory {
    RustCode,
    TypeScriptCode,
    Json,
    Markdown,
    ShellOutput,
}

pub(super) fn golden_corpus() -> Vec<CorpusSample> {
    vec![
        CorpusSample {
            name: "rust_exact_identity",
            category: CorpusCategory::RustCode,
            original: "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
            compressed: "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
            ext: "rs",
            expected_fidelity: FidelityClassV1::Exact,
            min_savings_pct: 0.0,
        },
        CorpusSample {
            name: "rust_structural_signatures",
            category: CorpusCategory::RustCode,
            original: concat!(
                "pub struct Config {\n",
                "    pub host: String,\n",
                "    pub port: u16,\n",
                "    pub max_connections: usize,\n",
                "}\n\n",
                "impl Config {\n",
                "    pub fn new(host: String, port: u16) -> Self {\n",
                "        Self { host, port, max_connections: 100 }\n",
                "    }\n\n",
                "    pub fn with_max_connections(mut self, max: usize) -> Self {\n",
                "        self.max_connections = max;\n",
                "        self\n",
                "    }\n\n",
                "    pub fn validate(&self) -> Result<(), String> {\n",
                "        if self.port == 0 { return Err(\"port must be non-zero\".into()); }\n",
                "        if self.host.is_empty() { return Err(\"host required\".into()); }\n",
                "        Ok(())\n",
                "    }\n",
                "}\n",
            ),
            compressed: concat!(
                "pub struct Config { pub host: String, pub port: u16, ",
                "pub max_connections: usize }\n",
                "impl Config {\n",
                "    pub fn new(host: String, port: u16) -> Self { ... }\n",
                "    pub fn with_max_connections(mut self, max: usize) -> Self { ... }\n",
                "    pub fn validate(&self) -> Result<(), String> { ... }\n",
                "}\n",
            ),
            ext: "rs",
            expected_fidelity: FidelityClassV1::Structural,
            min_savings_pct: 30.0,
        },
        CorpusSample {
            name: "json_structural_compact",
            category: CorpusCategory::Json,
            original: concat!(
                "{\n",
                "  \"name\": \"lean-ctx\",\n",
                "  \"version\": \"3.9.0\",\n",
                "  \"dependencies\": {\n",
                "    \"serde\": \"1.0\",\n",
                "    \"tokio\": \"1.0\",\n",
                "    \"hyper\": \"1.0\"\n",
                "  }\n",
                "}\n",
            ),
            compressed: concat!(
                "{\"name\":\"lean-ctx\",\"version\":\"3.9.0\",\"dependencies\":",
                "{\"serde\":\"1.0\",\"tokio\":\"1.0\",\"hyper\":\"1.0\"}}",
            ),
            ext: "json",
            expected_fidelity: FidelityClassV1::Structural,
            min_savings_pct: 20.0,
        },
        CorpusSample {
            name: "markdown_lossy_summary",
            category: CorpusCategory::Markdown,
            original: concat!(
                "# Architecture Guide\n\n",
                "## Overview\n\n",
                "The system consists of three main components: the proxy, the daemon, ",
                "and the MCP server.\n",
                "Each component has a specific responsibility in the request pipeline.\n\n",
                "## Proxy\n\n",
                "The proxy intercepts HTTP requests and applies context compression.\n",
                "It supports multiple providers including OpenAI, Anthropic, and Google.\n\n",
                "## Daemon\n\n",
                "The daemon manages session state and coordinates between components.\n",
                "It maintains the session cache and handles file watching.\n",
            ),
            compressed: concat!(
                "# Architecture Guide\n",
                "- 3 components: proxy, daemon, MCP server\n",
                "- Proxy: HTTP interception + compression (OpenAI/Anthropic/Google)\n",
                "- Daemon: session state + file watching\n",
            ),
            ext: "md",
            expected_fidelity: FidelityClassV1::Lossy,
            min_savings_pct: 50.0,
        },
        CorpusSample {
            name: "shell_git_status_compressed",
            category: CorpusCategory::ShellOutput,
            original: concat!(
                "On branch main\n",
                "Your branch is up to date with 'origin/main'.\n\n",
                "Changes not staged for commit:\n",
                "  (use \"git add <file>...\" to update what will be committed)\n",
                "  (use \"git restore <file>...\" to discard changes in working directory)\n",
                "        modified:   src/proxy/mod.rs\n",
                "        modified:   src/core/quality.rs\n\n",
                "Untracked files:\n",
                "  (use \"git add <file>...\" to include in what will be committed)\n",
                "        src/core/quality_lab/\n\n",
                "no changes added to commit (use \"git add\" and/or \"git commit -a\")\n",
            ),
            compressed: concat!(
                "branch main (up to date)\n",
                "modified: src/proxy/mod.rs, src/core/quality.rs\n",
                "untracked: src/core/quality_lab/\n",
            ),
            ext: "txt",
            expected_fidelity: FidelityClassV1::Lossy,
            min_savings_pct: 60.0,
        },
        CorpusSample {
            name: "typescript_structural_exports",
            category: CorpusCategory::TypeScriptCode,
            original: concat!(
                "export interface RequestOptions {\n",
                "  endpoint: string;\n",
                "  timeoutMs: number;\n",
                "  retries: number;\n",
                "}\n\n",
                "export async function requestJson(\n",
                "  options: RequestOptions,\n",
                "): Promise<Record<string, unknown>> {\n",
                "  const controller = new AbortController();\n",
                "  const timer = setTimeout(() => controller.abort(), options.timeoutMs);\n",
                "  try {\n",
                "    const response = await fetch(options.endpoint, ",
                "{ signal: controller.signal });\n",
                "    if (!response.ok) throw new Error(`HTTP ${response.status}`);\n",
                "    return await response.json();\n",
                "  } finally {\n",
                "    clearTimeout(timer);\n",
                "  }\n",
                "}\n",
            ),
            compressed: concat!(
                "export interface RequestOptions { endpoint: string; timeoutMs: number; ",
                "retries: number }\n",
                "export async function requestJson(options: RequestOptions): ",
                "Promise<Record<string, unknown>> { ... }\n",
            ),
            ext: "ts",
            expected_fidelity: FidelityClassV1::Lossy,
            min_savings_pct: 50.0,
        },
        CorpusSample {
            name: "json_structural_large_array",
            category: CorpusCategory::Json,
            original: concat!(
                "{\n",
                "  \"project\": \"quality-lab\",\n",
                "  \"runs\": [\n",
                "    {\n",
                "      \"id\": \"run-001\",\n",
                "      \"mode\": \"signatures\",\n",
                "      \"input_tokens\": 1842,\n",
                "      \"output_tokens\": 612,\n",
                "      \"passed\": true\n",
                "    },\n",
                "    {\n",
                "      \"id\": \"run-002\",\n",
                "      \"mode\": \"map\",\n",
                "      \"input_tokens\": 2310,\n",
                "      \"output_tokens\": 721,\n",
                "      \"passed\": true\n",
                "    },\n",
                "    {\n",
                "      \"id\": \"run-003\",\n",
                "      \"mode\": \"reference\",\n",
                "      \"input_tokens\": 1550,\n",
                "      \"output_tokens\": 488,\n",
                "      \"passed\": true\n",
                "    }\n",
                "  ]\n",
                "}\n",
            ),
            compressed: concat!(
                "{\"project\":\"quality-lab\",\"runs\":[",
                "{\"id\":\"run-001\",\"mode\":\"signatures\",\"input_tokens\":1842,",
                "\"output_tokens\":612,\"passed\":true},",
                "{\"id\":\"run-002\",\"mode\":\"map\",\"input_tokens\":2310,",
                "\"output_tokens\":721,\"passed\":true},",
                "{\"id\":\"run-003\",\"mode\":\"reference\",\"input_tokens\":1550,",
                "\"output_tokens\":488,\"passed\":true}]}",
            ),
            ext: "json",
            expected_fidelity: FidelityClassV1::Structural,
            min_savings_pct: 25.0,
        },
        CorpusSample {
            name: "empty_unknown",
            category: CorpusCategory::Markdown,
            original: "",
            compressed: "",
            ext: "md",
            expected_fidelity: FidelityClassV1::Exact,
            min_savings_pct: 0.0,
        },
        CorpusSample {
            name: "rust_lossy_architecture_summary",
            category: CorpusCategory::RustCode,
            original: concat!(
                "use std::collections::HashMap;\n\n",
                "pub struct SessionStore {\n",
                "    entries: HashMap<String, Vec<String>>,\n",
                "    capacity: usize,\n",
                "}\n\n",
                "impl SessionStore {\n",
                "    pub fn new(capacity: usize) -> Self {\n",
                "        Self { entries: HashMap::new(), capacity }\n",
                "    }\n\n",
                "    pub fn insert(&mut self, session: String, values: Vec<String>) {\n",
                "        if self.entries.len() >= self.capacity {\n",
                "            if let Some(key) = self.entries.keys().next().cloned() {\n",
                "                self.entries.remove(&key);\n",
                "            }\n",
                "        }\n",
                "        self.entries.insert(session, values);\n",
                "    }\n\n",
                "    pub fn get(&self, session: &str) -> Option<&[String]> {\n",
                "        self.entries.get(session).map(Vec::as_slice)\n",
                "    }\n",
                "}\n",
            ),
            compressed: "SessionStore: bounded session-to-values cache with insert/get operations.\n",
            ext: "rs",
            expected_fidelity: FidelityClassV1::Lossy,
            min_savings_pct: 80.0,
        },
        CorpusSample {
            name: "shell_cargo_test_compressed",
            category: CorpusCategory::ShellOutput,
            original: concat!(
                "   Compiling lean-ctx v3.9.0 (/workspace/lean-ctx/rust)\n",
                "    Finished `test` profile [unoptimized + debuginfo] target(s) in 8.42s\n",
                "     Running unittests src/lib.rs (target/debug/deps/lean_ctx-a19f2c)\n\n",
                "running 4 tests\n",
                "test core::quality::tests::exact_content_passes ... ok\n",
                "test core::quality::tests::signatures_preserve_structure ... ok\n",
                "test core::tokens::tests::counts_code_tokens ... ok\n",
                "test core::preservation::tests::imports_are_preserved ... ok\n\n",
                "test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; ",
                "112 filtered out; finished in 0.03s\n",
            ),
            compressed: concat!(
                "cargo test: 4 passed, 0 failed\n",
                "quality::exact_content_passes ✓\n",
                "quality::signatures_preserve_structure ✓\n",
                "tokens::counts_code_tokens ✓\n",
                "preservation::imports_are_preserved ✓\n",
            ),
            ext: "txt",
            expected_fidelity: FidelityClassV1::Lossy,
            min_savings_pct: 60.0,
        },
    ]
}

fn savings_pct(original: &str, compressed: &str) -> f64 {
    let original_tokens = count_tokens(original) as f64;
    if original_tokens == 0.0 {
        return 0.0;
    }
    let compressed_tokens = count_tokens(compressed) as f64;
    ((original_tokens - compressed_tokens) / original_tokens) * 100.0
}

#[test]
fn test_golden_corpus_fidelity_classification() {
    for sample in golden_corpus() {
        let assessment =
            super::fidelity::assess_fidelity(sample.original, sample.compressed, sample.ext);
        assert_eq!(
            assessment.class, sample.expected_fidelity,
            "fidelity mismatch for {}",
            sample.name
        );
    }
}

#[test]
fn test_golden_corpus_savings_thresholds() {
    for sample in golden_corpus() {
        let actual = savings_pct(sample.original, sample.compressed);
        assert!(
            actual + f64::EPSILON >= sample.min_savings_pct,
            "{} saved {actual:.2}%, expected at least {:.2}%",
            sample.name,
            sample.min_savings_pct
        );
    }
}

#[test]
fn test_golden_corpus_quality_gate() {
    for sample in golden_corpus() {
        let assessment =
            super::fidelity::assess_fidelity(sample.original, sample.compressed, sample.ext);
        match sample.expected_fidelity {
            FidelityClassV1::Exact | FidelityClassV1::Structural => assert!(
                assessment.passed_quality_gate,
                "{} must pass the quality gate",
                sample.name
            ),
            FidelityClassV1::Lossy => assert!(
                !assessment.passed_quality_gate,
                "{} must fail the quality gate",
                sample.name
            ),
            FidelityClassV1::Unknown => {}
        }
    }
}

#[test]
fn test_corpus_calibrated_counts() {
    for sample in golden_corpus()
        .into_iter()
        .filter(|sample| !sample.original.is_empty())
    {
        let count = super::calibration::count_tokens_with_calibration(
            sample.original,
            TokenizerFamily::O200kBase,
        );
        assert!(count.tokens > 0, "{} must have tokens", sample.name);
        assert_ne!(
            count.accuracy,
            CalibrationAccuracy::CharFallback,
            "{} must use a tokenizer",
            sample.name
        );
    }
}

#[test]
fn test_corpus_orchestrator_integration() {
    let sample = golden_corpus()
        .into_iter()
        .find(|sample| sample.name == "rust_structural_signatures")
        .expect("structural corpus sample must exist");
    let report =
        super::orchestrator::run_quality_lab(sample.original, sample.compressed, sample.ext);
    assert_eq!(report.schema_version, "lean-ctx.quality-lab/v2");
    assert!(report.input_compression.quality_gate_passed);
}

#[test]
fn test_corpus_all_categories_covered() {
    let categories: Vec<_> = golden_corpus()
        .into_iter()
        .map(|sample| sample.category)
        .collect();
    for category in [
        CorpusCategory::RustCode,
        CorpusCategory::TypeScriptCode,
        CorpusCategory::Json,
        CorpusCategory::Markdown,
        CorpusCategory::ShellOutput,
    ] {
        assert!(categories.contains(&category), "missing {category:?}");
    }
}

#[test]
fn test_corpus_no_duplicate_names() {
    let mut names = HashSet::new();
    for sample in golden_corpus() {
        assert!(names.insert(sample.name), "duplicate name: {}", sample.name);
    }
}

#[test]
fn test_corpus_round_trip_fidelity() {
    for sample in golden_corpus()
        .into_iter()
        .filter(|sample| sample.expected_fidelity == FidelityClassV1::Exact)
    {
        let assessment =
            super::fidelity::assess_fidelity(sample.original, sample.compressed, sample.ext);
        assert_eq!(
            assessment.preservation_score, 1.0,
            "{} must preserve exact content",
            sample.name
        );
    }
}
