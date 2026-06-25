use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone)]
pub struct AutoFinding {
    pub file: Option<String>,
    pub summary: String,
}

struct RecentEntry {
    key: String,
    at: Instant,
}

static RECENT: Mutex<Vec<RecentEntry>> = Mutex::new(Vec::new());
const DEDUP_WINDOW_SECS: u64 = 60;
const MAX_SUMMARY_LEN: usize = 120;

/// Extract a finding from a tool call result. Returns `None` if the output
/// is not interesting or if a duplicate was emitted within the dedup window.
pub fn extract(tool_name: &str, output: &str) -> Option<AutoFinding> {
    let finding = match tool_name {
        "ctx_read" => extract_ctx_read(output),
        "ctx_search" => extract_ctx_search(output),
        "ctx_shell" => extract_ctx_shell(output),
        "ctx_graph" => extract_ctx_graph(output),
        "ctx_semantic_search" => extract_ctx_semantic_search(output),
        _ => None,
    }?;

    let dedup_key = format!(
        "{}:{}",
        finding.file.as_deref().unwrap_or(""),
        &finding.summary[..finding.summary.floor_char_boundary(80)]
    );

    if let Ok(mut recent) = RECENT.lock() {
        let now = Instant::now();
        recent.retain(|e| now.duration_since(e.at).as_secs() < DEDUP_WINDOW_SECS);

        if recent.iter().any(|e| e.key == dedup_key) {
            return None;
        }
        recent.push(RecentEntry {
            key: dedup_key,
            at: now,
        });
    }

    Some(finding)
}

fn extract_ctx_read(output: &str) -> Option<AutoFinding> {
    let first_line = output.lines().next().unwrap_or("");
    if first_line.is_empty() || output.len() < 20 {
        return None;
    }

    let raw_path = first_line
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches([':', ']']);

    let path = strip_cache_ref(raw_path);

    if path.is_empty() || path.starts_with('[') || path.starts_with("ERROR") {
        return None;
    }
    if is_noise_path(path) {
        return None;
    }

    // Extract line count from output
    let line_count = first_line
        .split_whitespace()
        .find(|w| w.ends_with('L') && w[..w.len() - 1].parse::<usize>().is_ok())
        .unwrap_or("");

    // Extract a content hint from first few meaningful lines
    let content_hint = extract_content_hint(output);

    let short_path = shorten_path(path);
    let summary = match (line_count.is_empty(), content_hint.is_empty()) {
        (true, true) => format!("Read {short_path}"),
        (false, true) => format!("Read {short_path} ({line_count})"),
        (true, false) => truncate(
            &format!("Read {short_path} — {content_hint}"),
            MAX_SUMMARY_LEN,
        ),
        (false, false) => truncate(
            &format!("Read {short_path} ({line_count}) — {content_hint}"),
            MAX_SUMMARY_LEN,
        ),
    };

    Some(AutoFinding {
        file: Some(path.to_string()),
        summary,
    })
}

fn extract_ctx_search(output: &str) -> Option<AutoFinding> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return None;
    }

    let last = lines.last().unwrap_or(&"");
    if last.contains("0 matches") || last.contains("No matches") {
        return None;
    }

    // Extract pattern from common output formats
    let pattern = extract_search_pattern(&lines);

    // Low-signal guard: if we could not identify a meaningful search pattern
    // (placeholder "?") or it is a single trivial character, the resulting
    // "Found `?` in N files" finding is pure noise — skip it.
    if pattern == "?" || pattern.trim().chars().count() < 2 {
        return None;
    }

    // Extract matched file names (lines with ':' that look like file:line matches),
    // excluding noise paths (VCS/deps/build/home dotfiles).
    let matched_files: Vec<&str> = lines
        .iter()
        .filter(|l| {
            l.contains(':')
                && !l.starts_with('[')
                && !l.starts_with("pattern")
                && !l.starts_with("Pattern")
        })
        .filter_map(|l| l.split(':').next())
        .filter(|p| !is_noise_path(p))
        .collect();

    // Deduplicate file paths
    let mut unique_files: Vec<&str> = Vec::new();
    for f in &matched_files {
        if !unique_files.contains(f) {
            unique_files.push(f);
        }
    }

    let match_count = matched_files.len();
    let file_count = unique_files.len();

    if match_count == 0 && file_count == 0 {
        return None;
    }

    // Build summary with actual file names (top 3)
    let file_list: String = if unique_files.len() <= 3 {
        unique_files
            .iter()
            .map(|f| shorten_path(f))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        let top3: Vec<String> = unique_files[..3].iter().map(|f| shorten_path(f)).collect();
        format!("{} +{} more", top3.join(", "), unique_files.len() - 3)
    };

    let summary = truncate(
        &format!("Found `{pattern}` in {file_count} files: {file_list}"),
        MAX_SUMMARY_LEN,
    );

    Some(AutoFinding {
        file: None,
        summary,
    })
}

fn extract_ctx_shell(output: &str) -> Option<AutoFinding> {
    let lines: Vec<&str> = output.lines().collect();
    let first_line = lines.first().unwrap_or(&"");

    // Extract command name
    let cmd = lines
        .iter()
        .find(|l| l.starts_with("$ ") || l.starts_with("cmd:"))
        .map_or("", |l| {
            l.trim_start_matches("$ ").trim_start_matches("cmd:").trim()
        });

    // Check for test results (cargo test, pytest, jest, etc.)
    if let Some(test_summary) = extract_test_result(&lines, cmd) {
        return Some(AutoFinding {
            file: None,
            summary: test_summary,
        });
    }

    // Check for build results (cargo build/clippy)
    if let Some(build_summary) = extract_build_result(&lines, cmd) {
        return Some(AutoFinding {
            file: None,
            summary: build_summary,
        });
    }

    // Failed commands
    if let Some(rest) = first_line.strip_prefix("exit:") {
        let code = rest.split_whitespace().next().unwrap_or("?");
        if code != "0" {
            let short_cmd = &cmd[..cmd.floor_char_boundary(50)];
            let error_hint = lines
                .iter()
                .find(|l| l.contains("error") || l.contains("Error") || l.contains("FAILED"))
                .map_or("", |l| l.trim());
            let error_short = &error_hint[..error_hint.floor_char_boundary(50)];

            let summary = if error_short.is_empty() {
                format!("FAILED (exit {code}): {short_cmd}")
            } else {
                truncate(
                    &format!("FAILED (exit {code}): {short_cmd} — {error_short}"),
                    MAX_SUMMARY_LEN,
                )
            };
            return Some(AutoFinding {
                file: None,
                summary,
            });
        }
    }

    None
}

fn extract_ctx_graph(output: &str) -> Option<AutoFinding> {
    let first_line = output.lines().next().unwrap_or("");

    if first_line.starts_with("Files related to") || first_line.starts_with("No files depend") {
        let file = first_line
            .split_whitespace()
            .last()
            .unwrap_or("")
            .trim_end_matches(':')
            .trim_end_matches(|c: char| c == '(' || c.is_ascii_digit() || c == ')')
            .to_string();

        let count = first_line
            .split('(')
            .nth(1)
            .and_then(|s| s.split(')').next())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);

        if count > 0 {
            return Some(AutoFinding {
                file: Some(file),
                summary: first_line.to_string(),
            });
        }
    }

    None
}

fn extract_ctx_semantic_search(output: &str) -> Option<AutoFinding> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return None;
    }

    // Count result entries (lines starting with a score or file path)
    let results: Vec<&&str> = lines
        .iter()
        .filter(|l| l.starts_with("  ") || l.contains("score:") || l.contains("→"))
        .collect();

    if results.is_empty() {
        return None;
    }

    // Try to get query from first line
    let query = lines
        .first()
        .and_then(|l| {
            l.strip_prefix("query:")
                .or_else(|| l.strip_prefix("Query:"))
        })
        .map_or("semantic search", str::trim);

    let summary = truncate(
        &format!("Semantic search `{}` — {} results", query, results.len()),
        MAX_SUMMARY_LEN,
    );

    Some(AutoFinding {
        file: None,
        summary,
    })
}

// --- Helpers ---

/// Returns true for paths whose findings are noise rather than signal:
/// VCS/dependency/build dirs, virtualenvs, caches, the user's home dotfiles
/// (e.g. `~/.ssh/config`), and binary/log files. Such findings polluted the
/// session and knowledge store (see EPIC 6 / #2363).
fn is_noise_path(path: &str) -> bool {
    let p = path.replace('\\', "/");
    const NOISE_SEGMENTS: &[&str] = &[
        ".git",
        "node_modules",
        ".ssh",
        ".gnupg",
        ".aws",
        ".cargo",
        ".rustup",
        "target",
        ".venv",
        "venv",
        "__pycache__",
        "site-packages",
        "dist-packages",
        ".next",
        ".cache",
        "dist",
        "build",
        "vendor",
        ".terraform",
    ];
    // Match a noise directory anywhere in the path (leading, middle, or with a
    // leading slash). Splitting on components handles relative paths too.
    if p.split('/').any(|c| NOISE_SEGMENTS.contains(&c)) {
        return true;
    }
    // Home dotfiles outside any workspace (e.g. ~/.ssh/config, ~/.zshrc).
    if let Some(home) = dirs::home_dir() {
        let home_s = home.to_string_lossy().replace('\\', "/");
        if let Some(rest) = p.strip_prefix(&home_s) {
            let rest = rest.trim_start_matches('/');
            if rest.starts_with('.') {
                return true;
            }
        }
    }
    const NOISE_EXTS: &[&str] = &[
        ".lock", ".log", ".min.js", ".map", ".png", ".jpg", ".jpeg", ".gif", ".pdf", ".zip",
        ".tar", ".gz", ".bin", ".so", ".dylib", ".dll", ".o", ".a", ".class", ".wasm",
    ];
    let lower = p.to_ascii_lowercase();
    NOISE_EXTS.iter().any(|ext| lower.ends_with(ext))
}

fn strip_cache_ref(raw: &str) -> &str {
    if raw.len() > 3
        && raw.starts_with('F')
        && raw[1..].starts_with(|c: char| c.is_ascii_digit())
        && raw.contains('=')
    {
        raw.split_once('=').map_or(raw, |(_, p)| p)
    } else {
        raw
    }
}

fn shorten_path(path: &str) -> String {
    if path.len() <= 40 {
        return path.to_string();
    }
    // Keep last 2 segments
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 2 {
        format!("…/{}", parts[parts.len() - 2..].join("/"))
    } else {
        path.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 1).collect();
        format!("{truncated}…")
    }
}

/// Extracts a one-line structural hint from file/tool output.
/// Shared between auto-findings and session file-summary generation.
#[must_use]
pub fn extract_content_hint(output: &str) -> String {
    let lines: Vec<&str> = output.lines().skip(1).take(20).collect();

    // Layer 1: deps/exports/module-level descriptions
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("deps:")
            || trimmed.starts_with("exports:")
            || trimmed.starts_with("//!")
        {
            return trimmed[..trimmed.floor_char_boundary(80)].to_string();
        }
    }

    // Layer 2: primary struct/fn/class/trait definitions
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("pub struct ")
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub enum ")
            || trimmed.starts_with("pub trait ")
            || trimmed.starts_with("impl ")
            || trimmed.starts_with("class ")
            || trimmed.starts_with("export ")
            || trimmed.starts_with("export default ")
            || trimmed.starts_with("export function ")
            || trimmed.starts_with("def ")
            || trimmed.starts_with("func ")
        {
            return trimmed[..trimmed.floor_char_boundary(70)].to_string();
        }
    }

    // Layer 3: doc comments / markdown headings
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("///") || trimmed.starts_with("# ") {
            return trimmed[..trimmed.floor_char_boundary(70)].to_string();
        }
    }

    String::new()
}

fn extract_search_pattern(lines: &[&str]) -> String {
    // Try explicit pattern line
    for line in lines.iter().take(3) {
        if let Some(p) = line
            .strip_prefix("pattern:")
            .or_else(|| line.strip_prefix("Pattern:"))
            .or_else(|| line.strip_prefix("query:"))
        {
            return p.trim().trim_matches('"').to_string();
        }
    }

    // Try to infer from search summary line (e.g. "[4 matches for `foo` in 2 files]")
    for line in lines.iter().rev().take(3) {
        if let Some(start) = line.find('`')
            && let Some(end) = line[start + 1..].find('`')
        {
            return line[start + 1..start + 1 + end].to_string();
        }
        if let Some(start) = line.find("for \"")
            && let Some(end) = line[start + 5..].find('"')
        {
            return line[start + 5..start + 5 + end].to_string();
        }
    }

    "?".to_string()
}

fn extract_test_result(lines: &[&str], cmd: &str) -> Option<String> {
    let is_test_cmd = cmd.contains("test")
        || cmd.contains("pytest")
        || cmd.contains("jest")
        || cmd.contains("vitest")
        || cmd.contains("mocha");

    if !is_test_cmd {
        return None;
    }

    // Look for test result summary lines
    for line in lines.iter().rev().take(10) {
        // Rust: "test result: ok. 2845 passed; 0 failed;"
        if line.contains("test result:") {
            let short_cmd = &cmd[..cmd.floor_char_boundary(30)];
            let result = line.trim();
            return Some(truncate(
                &format!("Test `{short_cmd}`: {result}"),
                MAX_SUMMARY_LEN,
            ));
        }
        // Python: "X passed, Y failed" or "X passed"
        if (line.contains(" passed") || line.contains(" failed"))
            && (line.contains("pytest") || line.contains("==="))
        {
            let short_cmd = &cmd[..cmd.floor_char_boundary(30)];
            let result = line.trim().trim_matches('=').trim();
            return Some(truncate(
                &format!("Test `{short_cmd}`: {result}"),
                MAX_SUMMARY_LEN,
            ));
        }
    }

    None
}

fn extract_build_result(lines: &[&str], cmd: &str) -> Option<String> {
    let is_build = cmd.contains("build")
        || cmd.contains("clippy")
        || cmd.contains("check")
        || cmd.contains("compile");

    if !is_build {
        return None;
    }

    // Look for Finished line (cargo)
    for line in lines.iter().rev().take(5) {
        if line.contains("Finished") {
            let short_cmd = &cmd[..cmd.floor_char_boundary(30)];
            // Count errors/warnings
            let errors = lines.iter().filter(|l| l.contains("error[")).count();
            let warnings = lines
                .iter()
                .filter(|l| l.contains("warning:") && !l.contains("generated"))
                .count();

            return if errors > 0 {
                Some(truncate(
                    &format!("Build `{short_cmd}`: {errors} errors, {warnings} warnings"),
                    MAX_SUMMARY_LEN,
                ))
            } else if warnings > 0 {
                Some(truncate(
                    &format!("Build `{short_cmd}`: OK, {warnings} warnings"),
                    MAX_SUMMARY_LEN,
                ))
            } else {
                Some(format!("Build `{short_cmd}`: OK"))
            };
        }
    }

    None
}

#[cfg(test)]
pub(crate) fn clear_recent() {
    if let Ok(mut recent) = RECENT.lock() {
        recent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn ctx_read_extracts_path_and_content() {
        let output = "src/server/mod.rs 1400L\n   deps: tokio, serde\n\npub struct Server {";
        let f = extract_ctx_read(output).unwrap();
        assert_eq!(f.file.as_deref(), Some("src/server/mod.rs"));
        assert!(f.summary.contains("1400L"));
        assert!(
            f.summary.contains("deps: tokio, serde"),
            "deps line should be preferred over struct: {}",
            f.summary
        );
    }

    #[test]
    fn ctx_read_with_bracket_info() {
        let output = "src/lib.rs [45L, full mode, 320 tok]\npub fn main() {}";
        let f = extract_ctx_read(output).unwrap();
        assert_eq!(f.file.as_deref(), Some("src/lib.rs"));
        assert!(f.summary.contains("pub fn main"));
    }

    #[test]
    fn ctx_read_ignores_errors() {
        assert!(extract_ctx_read("ERROR: file not found").is_none());
        assert!(extract_ctx_read("").is_none());
    }

    #[test]
    fn ctx_search_shows_files() {
        let output = "pattern: \"pub fn extract\"\nsrc/auto_findings.rs:19: pub fn extract\nsrc/core/mod.rs:5: pub fn extract_data\n[2 matches in 2 files]";
        let f = extract_ctx_search(output).unwrap();
        assert!(f.summary.contains("pub fn extract"));
        assert!(f.summary.contains("auto_findings.rs"));
        assert!(f.summary.contains("2 files"));
    }

    #[test]
    fn ctx_search_ignores_no_matches() {
        let output = "0 matches found";
        assert!(extract_ctx_search(output).is_none());
    }

    #[test]
    fn ctx_search_suppresses_unidentified_pattern() {
        // No pattern/Pattern/query line and no backtick hint → pattern resolves
        // to "?", which must not produce a "Found `?` in N files" noise finding.
        let output = "src/a.rs:10: something\nsrc/b.rs:20: other\n[2 matches in 2 files]";
        assert!(extract_ctx_search(output).is_none());
    }

    #[test]
    fn ctx_search_skips_noise_paths_only() {
        let output = "pattern: \"foo\"\nnode_modules/x/y.js:1: foo\n.git/config:2: foo\n[2 matches in 2 files]";
        assert!(
            extract_ctx_search(output).is_none(),
            "matches only in node_modules/.git should yield no finding"
        );
    }

    #[test]
    fn ctx_read_skips_dependency_path() {
        assert!(
            extract_ctx_read("node_modules/react/index.js 50L\nexport default React;").is_none()
        );
        assert!(extract_ctx_read("project/target/debug/build.rs 10L\nfn main() {}").is_none());
    }

    #[test]
    fn noise_path_detects_home_dotfiles() {
        if let Some(home) = dirs::home_dir() {
            let ssh = format!("{}/.ssh/config", home.display());
            assert!(is_noise_path(&ssh));
        }
        assert!(is_noise_path("a/node_modules/b.js"));
        assert!(is_noise_path("pkg/foo.min.js"));
        assert!(!is_noise_path("src/server/mod.rs"));
    }

    #[test]
    fn ctx_shell_captures_test_results() {
        let output = "exit: 0\n$ cargo test --lib\nrunning 2845 tests\ntest result: ok. 2845 passed; 0 failed; 1 ignored;";
        let f = extract_ctx_shell(output).unwrap();
        assert!(f.summary.contains("2845 passed"));
        assert!(f.summary.contains("cargo test"));
    }

    #[test]
    fn ctx_shell_captures_build_ok() {
        let output = "exit: 0\n$ cargo build --release\n   Compiling lean-ctx v3.6.17\n    Finished `release` profile in 2m 15s";
        let f = extract_ctx_shell(output).unwrap();
        assert!(f.summary.contains("Build"));
        assert!(f.summary.contains("OK"));
    }

    #[test]
    fn ctx_shell_captures_failed_with_error() {
        let output = "exit: 1\n$ cargo clippy\nerror[E0425]: cannot find value `x`";
        let f = extract_ctx_shell(output).unwrap();
        assert!(f.summary.contains("FAILED"));
        assert!(f.summary.contains("clippy"));
        assert!(f.summary.contains("E0425"));
    }

    #[test]
    fn ctx_shell_ignores_plain_success() {
        let output = "exit: 0\n$ echo hello\nhello";
        assert!(extract_ctx_shell(output).is_none());
    }

    #[test]
    fn ctx_graph_extracts_related() {
        let output = "Files related to mod.rs (15):";
        let f = extract_ctx_graph(output).unwrap();
        assert!(f.summary.contains("related"));
    }

    #[test]
    #[serial]
    fn dedup_prevents_duplicate_within_window() {
        clear_recent();
        let f1 = extract("ctx_read", "src/dedup_test.rs 100L\npub fn test() {}");
        assert!(f1.is_some());
        let f2 = extract("ctx_read", "src/dedup_test.rs 100L\npub fn test() {}");
        assert!(f2.is_none());
    }

    #[test]
    #[serial]
    fn different_files_not_deduped() {
        clear_recent();
        let f1 = extract("ctx_read", "src/unique_a.rs 50L\nstruct A;");
        assert!(f1.is_some());
        let f2 = extract("ctx_read", "src/unique_b.rs 50L\nstruct B;");
        assert!(f2.is_some());
    }

    #[test]
    fn ctx_read_strips_cache_ref_prefix() {
        let output = "F1=main.rs 10L\nfn main() {}";
        let f = extract_ctx_read(output).unwrap();
        assert_eq!(f.file.as_deref(), Some("main.rs"));
        assert!(f.summary.starts_with("Read main.rs"));
    }

    #[test]
    fn ctx_read_strips_multi_digit_ref() {
        let output = "F12=src/lib.rs 120L\npub mod core;";
        let f = extract_ctx_read(output).unwrap();
        assert_eq!(f.file.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn unknown_tool_returns_none() {
        assert!(extract("ctx_compile", "some output").is_none());
        assert!(extract("ctx_overview", "overview data").is_none());
    }

    #[test]
    fn truncation_works() {
        let long = "a".repeat(200);
        let result = truncate(&long, 120);
        assert_eq!(result.chars().count(), 120);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn session_watermark_filters_old_findings() {
        use crate::core::session::SessionState;
        use chrono::Utc;

        let mut session = SessionState::new();
        session.add_finding(Some("old.rs"), None, "old finding");

        let watermark = Utc::now();
        session.last_consolidate_ts = Some(watermark);

        std::thread::sleep(std::time::Duration::from_millis(10));
        session.add_finding(Some("new.rs"), None, "new finding");

        let new_findings: Vec<_> = session
            .findings
            .iter()
            .filter(|f| f.timestamp > watermark)
            .collect();

        assert_eq!(new_findings.len(), 1);
        assert_eq!(new_findings[0].summary, "new finding");
    }

    #[test]
    fn watermark_none_includes_all() {
        use crate::core::session::SessionState;

        let mut session = SessionState::new();
        session.add_finding(Some("a.rs"), None, "first");
        session.add_finding(Some("b.rs"), None, "second");

        assert!(session.last_consolidate_ts.is_none());

        let new_findings: Vec<_> = session
            .findings
            .iter()
            .filter(|f| match session.last_consolidate_ts {
                Some(ts) => f.timestamp > ts,
                None => true,
            })
            .collect();

        assert_eq!(new_findings.len(), 2);
    }

    #[test]
    #[serial]
    fn extract_content_hint_survives_multibyte_byte_budget() {
        // Regression for #379: a matched line whose byte-budget cut (70/80) lands
        // inside a 2-byte Cyrillic char must snap to a char boundary, not panic.
        // `extract_content_hint` skips the first line, so each input has a header.
        clear_recent();

        // Layer 3 — markdown / doc heading (the reported auto_findings.rs:424,
        // budget 70). "# _" is 3 bytes, so byte 70 lands mid-char in the run.
        let line = format!("# _{}", "я".repeat(40));
        let hint = extract_content_hint(&format!("header\n{line}"));
        assert!(
            line.starts_with(&hint),
            "hint must be a valid prefix: {hint}"
        );
        assert!(hint.starts_with("# _") && (60..=70).contains(&hint.len()));

        // Layer 2 — definition line (budget 70).
        let def = format!("def _{}", "я".repeat(40));
        let hint = extract_content_hint(&format!("header\n{def}"));
        assert!(def.starts_with(&hint) && hint.starts_with("def _") && hint.len() <= 70);

        // Layer 1 — module doc comment (budget 80).
        let doc = format!("//!{}", "я".repeat(40));
        let hint = extract_content_hint(&format!("header\n{doc}"));
        assert!(doc.starts_with(&hint) && hint.starts_with("//!") && hint.len() <= 80);
    }

    #[test]
    #[serial]
    fn extract_shell_survives_multibyte_byte_budget() {
        // Failed-command path slices cmd@50 and error_hint@50; both cross a 2-byte
        // char boundary here (#379 class). Must return a finding, not panic.
        clear_recent();
        let output = format!("exit: 1\n$ x{}\nerror: {}", "я".repeat(60), "ж".repeat(60));
        let f = extract("ctx_shell", &output).expect("failed-command finding");
        assert!(f.summary.contains("FAILED"), "got: {}", f.summary);

        // Test-result path slices cmd@30.
        clear_recent();
        let test_out = format!(
            "$ cargo test {}\n   test result: ok. 5 passed; 0 failed;",
            "я".repeat(40)
        );
        let t = extract("ctx_shell", &test_out).expect("test-result finding");
        assert!(t.summary.contains("Test"), "got: {}", t.summary);
    }

    #[test]
    #[serial]
    fn extract_search_dedup_survives_multibyte_summary() {
        // The dedup key slices finding.summary@80 (auto_findings.rs:34). A long
        // Cyrillic search pattern yields a >80-byte multibyte summary; the cut
        // must be char-boundary safe.
        clear_recent();
        let output = format!("pattern: \"{}\"\nsrc/main.rs:10: match", "я".repeat(50));
        let f = extract("ctx_search", &output).expect("search finding");
        assert!(f.summary.contains("Found"), "got: {}", f.summary);
    }
}
