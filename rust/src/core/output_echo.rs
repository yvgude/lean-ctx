//! Output-echo detection (#501).
//!
//! lean-ctx aggressively optimizes the *input* side, but never looked at the
//! agent's *output* — although output tokens cost 4-5x more. The most common
//! waste pattern is the code echo: the agent re-quotes file content that is
//! already in context. The `afterAgentResponse` hook delivers the reply text;
//! this module measures which share of its code lines were echoed from
//! recently read files (context radar tail) and feeds three consumers:
//!
//! 1. rolling stats (`~/.lean-ctx/output_echo.json`) for `ctx_metrics`,
//!    `ctx_session output_stats` and the dashboard,
//! 2. an automatic `LlmFeedbackEvent` so the adaptive mode policy finally
//!    receives continuous data instead of voluntary `ctx_feedback` calls,
//! 3. a stable, cache-friendly CEP nudge the MCP server appends to a tool
//!    result when echo stays high (cooldown-limited).

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const STATS_FILE: &str = "output_echo.json";
/// Rolling window of analyzed responses.
const MAX_REPORTS: usize = 50;
/// Echo lines shorter than this are noise (`}`, `);`, `end`).
const MIN_LINE_CHARS: usize = 12;
/// Nudge when the average echo ratio of the recent window exceeds this.
const NUDGE_THRESHOLD: f64 = 0.30;
/// Responses considered for the nudge decision.
const NUDGE_WINDOW: usize = 5;
/// Minimum analyzed responses between two nudges.
const NUDGE_COOLDOWN: usize = 20;
/// How much of the radar tail to scan for source content (bytes).
const RADAR_TAIL_BYTES: u64 = 262_144;
/// Cap on source documents compared against.
const MAX_SOURCES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EchoReport {
    pub response_lines: usize,
    pub code_lines: usize,
    pub echoed_lines: usize,
    pub echo_ratio: f64,
    pub recorded_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EchoStats {
    pub reports: Vec<EchoReport>,
    /// Index (count of analyzed responses) at the last nudge.
    pub last_nudge_at: u64,
    /// Total responses analyzed over the lifetime of the store.
    pub total_analyzed: u64,
}

impl EchoStats {
    #[must_use]
    pub fn avg_ratio(&self, window: usize) -> f64 {
        let recent: Vec<&EchoReport> = self.reports.iter().rev().take(window).collect();
        if recent.is_empty() {
            return 0.0;
        }
        recent.iter().map(|r| r.echo_ratio).sum::<f64>() / recent.len() as f64
    }

    /// Per-day `(YYYY-MM-DD, avg_echo_ratio, samples)` over the last `days`
    /// days, ascending by day — the dashboard's learning trend (#507).
    /// Days are UTC, consistent with the ledger's day slices.
    #[must_use]
    pub fn daily_trend(&self, days: u32) -> Vec<(String, f64, u64)> {
        use std::collections::BTreeMap;
        let cutoff = now_unix().saturating_sub(u64::from(days) * 86_400);
        let mut by_day: BTreeMap<String, (f64, u64)> = BTreeMap::new();
        for r in &self.reports {
            if r.recorded_unix < cutoff {
                continue;
            }
            let Some(dt) = chrono::DateTime::from_timestamp(r.recorded_unix as i64, 0) else {
                continue;
            };
            let day = dt.format("%Y-%m-%d").to_string();
            let entry = by_day.entry(day).or_default();
            entry.0 += r.echo_ratio;
            entry.1 += 1;
        }
        by_day
            .into_iter()
            .map(|(d, (sum, n))| (d, sum / n as f64, n))
            .collect()
    }
}

fn stats_path() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(STATS_FILE)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[must_use]
pub fn load_stats() -> EchoStats {
    std::fs::read_to_string(stats_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_stats(stats: &EchoStats) {
    let path = stats_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(stats) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Normalize a line for comparison: trim + collapse internal whitespace.
fn normalize_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last_space = false;
    for c in line.trim().chars() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out
}

/// Extract the code lines of a markdown response: fenced blocks plus
/// 4-space-indented lines. Prose is never counted as echo.
fn code_lines_of_response(response: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for raw in response.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || raw.starts_with("    ") {
            let norm = normalize_line(raw);
            if norm.chars().count() >= MIN_LINE_CHARS {
                lines.push(norm);
            }
        }
    }
    lines
}

/// Pure analysis: which share of the response's code lines appear verbatim
/// in any source document (recently read file contents)?
#[must_use]
pub fn analyze(response: &str, sources: &[String]) -> EchoReport {
    let code_lines = code_lines_of_response(response);
    let response_lines = response.lines().count();

    if code_lines.is_empty() {
        return EchoReport {
            response_lines,
            code_lines: 0,
            echoed_lines: 0,
            echo_ratio: 0.0,
            recorded_unix: now_unix(),
        };
    }

    let mut source_set: HashSet<String> = HashSet::new();
    for src in sources.iter().take(MAX_SOURCES) {
        for line in src.lines() {
            let norm = normalize_line(line);
            if norm.chars().count() >= MIN_LINE_CHARS {
                source_set.insert(norm);
            }
        }
    }

    let echoed = code_lines
        .iter()
        .filter(|l| source_set.contains(*l))
        .count();
    let ratio = echoed as f64 / code_lines.len() as f64;

    EchoReport {
        response_lines,
        code_lines: code_lines.len(),
        echoed_lines: echoed,
        echo_ratio: ratio,
        recorded_unix: now_unix(),
    }
}

/// Read the tail of the context radar log and collect recently delivered
/// content (file reads, MCP tool results, shell output) as echo sources,
/// plus the token sum of events since the last user message (turn input
/// approximation for the feedback event).
fn radar_tail_sources() -> (Vec<String>, u64) {
    let data_dir =
        crate::core::data_dir::lean_ctx_data_dir().unwrap_or_else(|_| PathBuf::from("."));
    let path = data_dir.join("context_radar.jsonl");
    let Ok(file) = std::fs::File::open(&path) else {
        return (Vec::new(), 0);
    };
    use std::io::{Read, Seek, SeekFrom};
    let mut file = file;
    let len = file.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(RADAR_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return (Vec::new(), 0);
    }
    let mut raw = String::new();
    if file.read_to_string(&mut raw).is_err() {
        return (Vec::new(), 0);
    }

    let mut sources: Vec<String> = Vec::new();
    let mut turn_tokens: u64 = 0;
    // Skip the first (possibly truncated) line when we started mid-file.
    let skip_first = start > 0;
    for (i, line) in raw.lines().enumerate() {
        if skip_first && i == 0 {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let event_type = v.get("event_type").and_then(|e| e.as_str()).unwrap_or("");
        let tokens = v
            .get("tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        match event_type {
            "user_message" => turn_tokens = 0,
            "agent_response" | "thinking" => {}
            _ => turn_tokens = turn_tokens.saturating_add(tokens),
        }
        if matches!(event_type, "file_read" | "mcp_call" | "shell")
            && let Some(content) = v.get("content").and_then(|c| c.as_str())
            && !content.is_empty()
        {
            sources.push(content.to_string());
        }
    }
    if sources.len() > MAX_SOURCES {
        let excess = sources.len() - MAX_SOURCES;
        sources.drain(..excess);
    }
    (sources, turn_tokens)
}

/// Hook entry point: analyze an agent response against the radar tail,
/// persist rolling stats and emit the automatic feedback event.
pub fn analyze_and_record(response: &str) {
    let (sources, turn_input_tokens) = radar_tail_sources();
    let report = analyze(response, &sources);

    let mut stats = load_stats();
    stats.total_analyzed = stats.total_analyzed.saturating_add(1);
    stats.reports.push(report.clone());
    if stats.reports.len() > MAX_REPORTS {
        let excess = stats.reports.len() - MAX_REPORTS;
        stats.reports.drain(..excess);
    }
    save_stats(&stats);

    emit_feedback_event(response, turn_input_tokens);
}

/// Automatic `LlmFeedbackEvent` (#501): the adaptive mode policy used to
/// depend on voluntary `ctx_feedback` calls that practically never happened.
/// Mode attribution comes from the session's `files_touched.last_mode`
/// aggregation — a session-window approximation, honest and available.
fn emit_feedback_event(response: &str, turn_input_tokens: u64) {
    let output_tokens = crate::core::tokens::count_tokens(response) as u64;
    if output_tokens == 0 {
        return;
    }

    let session = crate::core::session::SessionState::load_latest();
    let modes: Option<std::collections::BTreeMap<String, u64>> = session.as_ref().map(|s| {
        let mut m = std::collections::BTreeMap::new();
        for f in &s.files_touched {
            if !f.last_mode.is_empty() {
                *m.entry(f.last_mode.clone()).or_insert(0) += u64::from(f.read_count.max(1));
            }
        }
        m
    });
    let modes = modes.filter(|m| !m.is_empty());

    let model = crate::hook_handlers::load_detected_model().map(|(name, _)| name);

    let ev = crate::core::llm_feedback::LlmFeedbackEvent {
        agent_id: "output_echo_auto".to_string(),
        intent: session
            .as_ref()
            .and_then(|s| s.task.as_ref())
            .and_then(|t| t.intent.clone()),
        model,
        llm_input_tokens: turn_input_tokens.max(1),
        llm_output_tokens: output_tokens,
        latency_ms: None,
        note: None,
        ctx_read_last_mode: None,
        ctx_read_modes: modes,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    let mut policy = crate::core::adaptive_mode_policy::AdaptiveModePolicyStore::load();
    policy.update_from_feedback(&ev);
    let _ = policy.save();
    let _ = crate::core::llm_feedback::LlmFeedbackStore::record(ev);
}

/// CEP nudge for the MCP server to append to a tool result. Stable text in
/// 10%-steps (prompt-cache friendly, #498), cooldown-limited. Consuming the
/// nudge advances the cooldown marker.
#[must_use]
pub fn take_pending_nudge() -> Option<String> {
    let mut stats = load_stats();
    if stats.reports.len() < NUDGE_WINDOW {
        return None;
    }
    if stats.total_analyzed.saturating_sub(stats.last_nudge_at) < NUDGE_COOLDOWN as u64 {
        return None;
    }
    let avg = stats.avg_ratio(NUDGE_WINDOW);
    if avg < NUDGE_THRESHOLD {
        return None;
    }
    let rounded = ((avg * 10.0).round() * 10.0) as u32;
    stats.last_nudge_at = stats.total_analyzed;
    save_stats(&stats);
    Some(format!(
        "\n[CEP: ~{rounded}% of your recent replies echoed file content already in context — reference lines (F1:42-58) instead of re-quoting]"
    ))
}

/// Average echo ratio over the rolling window — CEP score input.
#[must_use]
pub fn current_avg_ratio() -> f64 {
    load_stats().avg_ratio(MAX_REPORTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_content() -> String {
        (0..30)
            .map(|i| format!("pub fn compute_value_{i}(input: u32) -> u32 {{ input * {i} }}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn echo_detected_for_quoted_file_content() {
        let src = file_content();
        let quoted: Vec<&str> = src.lines().take(10).collect();
        let response = format!(
            "Here is the relevant code:\n\n```rust\n{}\n```\n",
            quoted.join("\n")
        );
        let report = analyze(&response, &[src]);
        assert_eq!(report.code_lines, 10);
        assert_eq!(report.echoed_lines, 10);
        assert!(report.echo_ratio > 0.99);
    }

    #[test]
    fn prose_response_has_zero_echo() {
        let src = file_content();
        let response = "The cache works by storing entries keyed by path. \
                        No code needed here — the fix is a one-line change.";
        let report = analyze(response, &[src]);
        assert_eq!(report.code_lines, 0);
        assert!(report.echo_ratio.abs() < f64::EPSILON);
    }

    #[test]
    fn daily_trend_groups_by_utc_day_and_averages() {
        let now = now_unix();
        // Anchor both "today" samples to the start of the current UTC day. A naive
        // `now` / `now - 60` pair straddles midnight when the suite runs within 60s
        // of a UTC day rollover, yielding a spurious third day (flaky in CI).
        let day = now - (now % 86_400);
        let stats = EchoStats {
            reports: vec![
                // Two samples on the same UTC day (today): avg of 0.2 and 0.6 = 0.4.
                EchoReport {
                    echo_ratio: 0.2,
                    recorded_unix: day + 100,
                    ..Default::default()
                },
                EchoReport {
                    echo_ratio: 0.6,
                    recorded_unix: day + 200,
                    ..Default::default()
                },
                // One sample two UTC days ago.
                EchoReport {
                    echo_ratio: 1.0,
                    recorded_unix: day - 2 * 86_400 + 100,
                    ..Default::default()
                },
                // Outside the 14-day window — must be excluded.
                EchoReport {
                    echo_ratio: 1.0,
                    recorded_unix: day - 30 * 86_400,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let trend = stats.daily_trend(14);
        assert_eq!(trend.len(), 2, "two distinct days inside the window");
        // Ascending by day: the older day first.
        assert_eq!(trend[0].2, 1, "older day has one sample");
        assert!((trend[0].1 - 1.0).abs() < f64::EPSILON);
        assert_eq!(trend[1].2, 2, "today has two samples");
        assert!((trend[1].1 - 0.4).abs() < 1e-9);
    }

    #[test]
    fn short_lines_are_ignored() {
        let src = "}\n);\nend\nfn x() {}\n".to_string();
        let response = "```rust\n}\n);\nend\n```\n";
        let report = analyze(response, &[src]);
        assert_eq!(report.code_lines, 0, "sub-12-char lines never count");
    }

    #[test]
    fn novel_code_is_not_echo() {
        let src = file_content();
        let response = "```rust\npub fn completely_new_function(a: u64) -> u64 { a + 42 }\nlet result = completely_new_function(7);\n```";
        let report = analyze(response, &[src]);
        assert_eq!(report.echoed_lines, 0);
        assert!(report.echo_ratio.abs() < f64::EPSILON);
    }

    #[test]
    fn whitespace_differences_still_match() {
        let src = "pub fn   spaced_out(value:    u32)   -> u32 { value }".to_string();
        let response = "```rust\npub fn spaced_out(value: u32) -> u32 { value }\n```";
        let report = analyze(response, &[src]);
        assert_eq!(report.echoed_lines, 1);
    }

    #[test]
    fn avg_ratio_windows_correctly() {
        let mut stats = EchoStats::default();
        for ratio in [0.0, 0.2, 0.4, 0.6, 0.8] {
            stats.reports.push(EchoReport {
                response_lines: 10,
                code_lines: 10,
                echoed_lines: (ratio * 10.0) as usize,
                echo_ratio: ratio,
                recorded_unix: 0,
            });
        }
        assert!((stats.avg_ratio(5) - 0.4).abs() < 1e-9);
        assert!((stats.avg_ratio(2) - 0.7).abs() < 1e-9);
    }

    #[test]
    fn indented_code_outside_fences_counts() {
        let src = "    let total = items.iter().map(|i| i.price).sum::<f64>();".to_string();
        let response = "The sum is computed like this:\n\n    let total = items.iter().map(|i| i.price).sum::<f64>();\n";
        let report = analyze(response, &[src]);
        assert_eq!(report.code_lines, 1);
        assert_eq!(report.echoed_lines, 1);
    }
}
