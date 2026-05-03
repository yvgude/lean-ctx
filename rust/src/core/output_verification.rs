use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static STATS: OnceLock<VerificationStats> = OnceLock::new();

fn global_stats() -> &'static VerificationStats {
    STATS.get_or_init(VerificationStats::new)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationConfig {
    pub enabled: bool,
    pub strict_mode: bool,
    pub check_paths: bool,
    pub check_identifiers: bool,
    pub check_line_numbers: bool,
    pub check_structure: bool,
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strict_mode: false,
            check_paths: true,
            check_identifiers: true,
            check_line_numbers: false,
            check_structure: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WarningKind {
    MissingPath,
    MangledIdentifier,
    LineNumberDrift,
    TruncatedBlock,
}

impl std::fmt::Display for WarningKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingPath => write!(f, "missing_path"),
            Self::MangledIdentifier => write!(f, "mangled_identifier"),
            Self::LineNumberDrift => write!(f, "line_drift"),
            Self::TruncatedBlock => write!(f, "truncated_block"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationWarning {
    pub kind: WarningKind,
    pub detail: String,
    pub severity: WarningSeverity,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WarningSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub pass: bool,
    pub warnings: Vec<VerificationWarning>,
    pub info_loss_score: f64,
    pub paths_checked: usize,
    pub identifiers_checked: usize,
}

impl VerificationResult {
    pub fn ok() -> Self {
        Self {
            pass: true,
            warnings: Vec::new(),
            info_loss_score: 0.0,
            paths_checked: 0,
            identifiers_checked: 0,
        }
    }

    pub fn format_compact(&self) -> String {
        if self.warnings.is_empty() {
            return "PASS".to_string();
        }
        let status = if self.pass { "WARN" } else { "FAIL" };
        let counts: Vec<String> = self
            .warnings
            .iter()
            .fold(std::collections::HashMap::new(), |mut acc, w| {
                *acc.entry(w.kind.to_string()).or_insert(0u32) += 1;
                acc
            })
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        format!(
            "{status}({}) loss={:.1}%",
            counts.join(", "),
            self.info_loss_score * 100.0
        )
    }
}

pub fn verify_output(
    source: &str,
    compressed: &str,
    config: &VerificationConfig,
) -> VerificationResult {
    if !config.enabled || source.is_empty() || compressed.is_empty() {
        return VerificationResult::ok();
    }

    // No-op compression should never produce warnings.
    if source == compressed {
        return VerificationResult::ok();
    }

    let mut warnings = Vec::new();
    let mut paths_checked = 0;
    let mut identifiers_checked = 0;

    if config.check_paths {
        let (path_warnings, count) = check_paths(source, compressed);
        paths_checked = count;
        warnings.extend(path_warnings);
    }

    if config.check_identifiers {
        let (id_warnings, count) = check_identifiers(source, compressed);
        identifiers_checked = count;
        warnings.extend(id_warnings);
    }

    if config.check_line_numbers {
        warnings.extend(check_line_numbers(source, compressed));
    }

    if config.check_structure {
        warnings.extend(check_structure(source, compressed));
    }

    let total_checks = (paths_checked + identifiers_checked).max(1);
    let loss_items = warnings
        .iter()
        .filter(|w| w.severity == WarningSeverity::High)
        .count() as f64
        * 2.0
        + warnings
            .iter()
            .filter(|w| w.severity == WarningSeverity::Medium)
            .count() as f64;
    let info_loss_score = (loss_items / total_checks as f64).min(1.0);

    let pass = if config.strict_mode {
        !warnings
            .iter()
            .any(|w| w.severity == WarningSeverity::High || w.severity == WarningSeverity::Medium)
    } else {
        !warnings.iter().any(|w| w.severity == WarningSeverity::High)
    };

    let result = VerificationResult {
        pass,
        warnings,
        info_loss_score,
        paths_checked,
        identifiers_checked,
    };

    record_result(&result);
    result
}

fn check_paths(source: &str, compressed: &str) -> (Vec<VerificationWarning>, usize) {
    let paths = extract_file_paths(source);
    let mut warnings = Vec::new();

    for path in &paths {
        let basename = path.rsplit('/').next().unwrap_or(path);
        if !compressed.contains(basename) {
            warnings.push(VerificationWarning {
                kind: WarningKind::MissingPath,
                detail: format!("Path reference lost: {path}"),
                severity: WarningSeverity::Medium,
            });
        }
    }

    (warnings, paths.len())
}

fn check_identifiers(source: &str, compressed: &str) -> (Vec<VerificationWarning>, usize) {
    let identifiers = extract_identifiers(source);
    let mut warnings = Vec::new();
    let significant: Vec<&str> = identifiers
        .iter()
        .filter(|id| id.len() >= 4)
        .map(String::as_str)
        .collect();

    for id in &significant {
        if !compressed.contains(id) {
            warnings.push(VerificationWarning {
                kind: WarningKind::MangledIdentifier,
                detail: format!("Identifier lost: {id}"),
                severity: if id.len() >= 8 {
                    WarningSeverity::High
                } else {
                    WarningSeverity::Low
                },
            });
        }
    }

    (warnings, significant.len())
}

fn check_line_numbers(source: &str, compressed: &str) -> Vec<VerificationWarning> {
    let source_max = source.lines().count();
    let mut warnings = Vec::new();

    let re_like = Regex::new(r"(?:line\s+|L|:)(\d{1,6})")
        .ok()
        .or_else(|| Regex::new(r"(\d+)").ok());

    if let Some(re_like) = re_like {
        for cap in re_like.captures_iter(compressed) {
            if let Some(m) = cap.get(1) {
                if let Ok(n) = m.as_str().parse::<usize>() {
                    if n > source_max && n < 999_999 {
                        warnings.push(VerificationWarning {
                            kind: WarningKind::LineNumberDrift,
                            detail: format!("Line {n} exceeds source max {source_max}"),
                            severity: WarningSeverity::Low,
                        });
                    }
                }
            }
        }
    }

    warnings
}

fn check_structure(source: &str, compressed: &str) -> Vec<VerificationWarning> {
    let mut warnings = Vec::new();

    let src_opens: usize = source.chars().filter(|&c| c == '{').count();
    let src_closes: usize = source.chars().filter(|&c| c == '}').count();
    let src_diff = (src_opens as i64 - src_closes as i64).unsigned_abs();

    let opens: usize = compressed.chars().filter(|&c| c == '{').count();
    let closes: usize = compressed.chars().filter(|&c| c == '}').count();
    if opens > 0 || closes > 0 {
        let diff = (opens as i64 - closes as i64).unsigned_abs();
        // Only warn if compression materially worsened structural balance.
        if diff > (src_diff + 2) && diff > 2 {
            warnings.push(VerificationWarning {
                kind: WarningKind::TruncatedBlock,
                detail: format!("Brace mismatch: {{ {opens} vs }} {closes}"),
                severity: WarningSeverity::Medium,
            });
        }
    }

    let src_parens_open: usize = source.chars().filter(|&c| c == '(').count();
    let src_parens_close: usize = source.chars().filter(|&c| c == ')').count();
    let src_parens_diff = (src_parens_open as i64 - src_parens_close as i64).unsigned_abs();

    let parens_open: usize = compressed.chars().filter(|&c| c == '(').count();
    let parens_close: usize = compressed.chars().filter(|&c| c == ')').count();
    if parens_open > 0 || parens_close > 0 {
        let diff = (parens_open as i64 - parens_close as i64).unsigned_abs();
        if diff > (src_parens_diff + 3) && diff > 3 {
            warnings.push(VerificationWarning {
                kind: WarningKind::TruncatedBlock,
                detail: format!("Paren mismatch: ( {parens_open} vs ) {parens_close}"),
                severity: WarningSeverity::Low,
            });
        }
    }

    warnings
}

fn extract_file_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let re = Regex::new(
        r#"(?:^|[\s"'`(,])([a-zA-Z0-9_./-]{2,}\.(?:rs|ts|tsx|js|jsx|py|go|java|rb|cpp|c|h|toml|yaml|yml|json|md))\b"#
    )
    .ok()
    .or_else(|| Regex::new(r"(\S+\.\w+)").ok());

    if let Some(re) = re {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let p = m.as_str().to_string();
                if !paths.contains(&p) && p.len() < 200 {
                    paths.push(p);
                }
            }
        }
    }
    paths
}

fn extract_identifiers(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let re = Regex::new(
        r"\b(fn|struct|enum|trait|type|class|function|const|let|var|def|pub)\s+([a-zA-Z_][a-zA-Z0-9_]*)"
    )
    .ok()
    .or_else(|| Regex::new(r"([a-zA-Z_]\w+)").ok());

    if let Some(re) = re {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(2) {
                let id = m.as_str().to_string();
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

struct VerificationStats {
    pass_count: AtomicU64,
    warn_run_count: AtomicU64,
    warn_item_count: AtomicU64,
    total_count: AtomicU64,
    sum_info_loss_score_ppm: AtomicU64,
    last_info_loss_score_ppm: AtomicU64,
    recent_warnings: Mutex<Vec<VerificationWarning>>,
}

impl VerificationStats {
    fn new() -> Self {
        Self {
            pass_count: AtomicU64::new(0),
            warn_run_count: AtomicU64::new(0),
            warn_item_count: AtomicU64::new(0),
            total_count: AtomicU64::new(0),
            sum_info_loss_score_ppm: AtomicU64::new(0),
            last_info_loss_score_ppm: AtomicU64::new(0),
            recent_warnings: Mutex::new(Vec::new()),
        }
    }
}

fn record_result(result: &VerificationResult) {
    let stats = global_stats();
    stats.total_count.fetch_add(1, Ordering::Relaxed);
    if result.warnings.is_empty() {
        stats.pass_count.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.warn_run_count.fetch_add(1, Ordering::Relaxed);
        stats
            .warn_item_count
            .fetch_add(result.warnings.len() as u64, Ordering::Relaxed);
    }
    let ppm = (result.info_loss_score.clamp(0.0, 1.0) * 1_000_000.0).round() as u64;
    stats
        .sum_info_loss_score_ppm
        .fetch_add(ppm, Ordering::Relaxed);
    stats.last_info_loss_score_ppm.store(ppm, Ordering::Relaxed);

    if !result.warnings.is_empty() {
        if let Ok(mut recent) = stats.recent_warnings.lock() {
            for w in &result.warnings {
                recent.push(w.clone());
            }
            if recent.len() > 200 {
                let excess = recent.len() - 200;
                recent.drain(..excess);
            }
        }

        for w in &result.warnings {
            crate::core::events::emit_verification_warning(
                &w.kind.to_string(),
                &w.detail,
                &format!("{:?}", w.severity),
            );
        }
    }
}

pub fn stats_snapshot() -> VerificationSnapshot {
    let s = global_stats();
    let total = s.total_count.load(Ordering::Relaxed);
    let pass = s.pass_count.load(Ordering::Relaxed);
    let warn_runs = s.warn_run_count.load(Ordering::Relaxed);
    let warn_items = s.warn_item_count.load(Ordering::Relaxed);
    let sum_ppm = s.sum_info_loss_score_ppm.load(Ordering::Relaxed);
    let last_ppm = s.last_info_loss_score_ppm.load(Ordering::Relaxed);
    let recent = s
        .recent_warnings
        .lock()
        .map(|r| r.clone())
        .unwrap_or_default();
    VerificationSnapshot {
        total,
        pass,
        warn_runs,
        warn_items,
        pass_rate: if total > 0 {
            pass as f64 / total as f64
        } else {
            1.0
        },
        avg_info_loss_score: if total > 0 {
            (sum_ppm as f64 / total as f64) / 1_000_000.0
        } else {
            0.0
        },
        last_info_loss_score: (last_ppm as f64) / 1_000_000.0,
        recent_warnings: recent,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationSnapshot {
    pub total: u64,
    pub pass: u64,
    pub warn_runs: u64,
    pub warn_items: u64,
    pub pass_rate: f64,
    pub avg_info_loss_score: f64,
    pub last_info_loss_score: f64,
    pub recent_warnings: Vec<VerificationWarning>,
}

impl VerificationSnapshot {
    pub fn format_compact(&self) -> String {
        format!(
            "Verification: {}/{} pass ({:.0}%), warn_runs={}, warn_items={}, loss(avg)={:.1}%",
            self.pass,
            self.total,
            self.pass_rate * 100.0,
            self.warn_runs,
            self.warn_items,
            self.avg_info_loss_score * 100.0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VerificationConfig {
        VerificationConfig::default()
    }

    #[test]
    fn empty_input_passes() {
        let r = verify_output("", "", &cfg());
        assert!(r.pass);
    }

    #[test]
    fn identical_passes() {
        let src = "fn hello() { println!(\"world\"); }";
        let r = verify_output(src, src, &cfg());
        assert!(r.pass);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn detects_missing_path() {
        let src = "import { foo } from src/utils/helper.ts";
        let compressed = "import foo";
        let r = verify_output(src, compressed, &cfg());
        assert!(r
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::MissingPath));
    }

    #[test]
    fn detects_lost_identifier() {
        let src = "fn calculate_monthly_revenue(data: &[f64]) -> f64 { data.iter().sum() }";
        let compressed = "fn calc() -> f64 { sum }";
        let r = verify_output(src, compressed, &cfg());
        assert!(r
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::MangledIdentifier));
    }

    #[test]
    fn detects_brace_mismatch() {
        let src = "fn a() { if true { b(); } } fn c() { d(); } fn e() { f(); }";
        let compressed = "fn a() { if true { b(); fn c() { d(); fn e() { f();";
        let r = verify_output(src, compressed, &cfg());
        assert!(r
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::TruncatedBlock));
    }

    #[test]
    fn preserved_identifiers_pass() {
        let src = "fn process_data(input: Vec<u8>) -> Result<()> { Ok(()) }";
        let compressed = "fn process_data(input: Vec<u8>) -> Result<()>";
        let r = verify_output(src, compressed, &cfg());
        let mangled = r
            .warnings
            .iter()
            .filter(|w| w.kind == WarningKind::MangledIdentifier)
            .count();
        assert_eq!(mangled, 0);
    }

    #[test]
    fn extract_paths_finds_common_extensions() {
        let text = "see src/core/auth.rs and lib/utils.py for details";
        let paths = extract_file_paths(text);
        assert!(paths.iter().any(|p| p.contains("auth.rs")));
        assert!(paths.iter().any(|p| p.contains("utils.py")));
    }

    #[test]
    fn extract_identifiers_finds_functions() {
        let text = "fn calculate_total(x: i32) -> i32 { x }\nstruct UserProfile { name: String }";
        let ids = extract_identifiers(text);
        assert!(ids.contains(&"calculate_total".to_string()));
        assert!(ids.contains(&"UserProfile".to_string()));
    }

    #[test]
    fn info_loss_score_bounded() {
        let src = "fn very_long_function_name_here() {}\nfn another_significant_fn() {}";
        let compressed = "compressed";
        let r = verify_output(src, compressed, &cfg());
        assert!(r.info_loss_score >= 0.0);
        assert!(r.info_loss_score <= 1.0);
    }

    #[test]
    fn snapshot_starts_clean() {
        let snap = stats_snapshot();
        assert!(snap.pass_rate >= 0.0);
        assert!(snap.pass_rate <= 1.0);
    }

    #[test]
    fn disabled_config_passes() {
        let mut c = cfg();
        c.enabled = false;
        let r = verify_output("fn foo() {}", "bar", &c);
        assert!(r.pass);
    }
}
