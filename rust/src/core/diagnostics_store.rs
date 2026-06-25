//! Active compiler/linter diagnostics as a context-priority signal (#499).
//!
//! When `cargo`/`tsc`/`eslint` fail, the files they point at are the most
//! task-relevant files in the project — the agent will read them next to fix
//! the build. The shell layer already sees this output (CLI `lean-ctx -c` and
//! MCP `ctx_shell`); this store extracts the structured `(file, line)` pairs
//! and makes them available to auto-mode, relevance ranking and the triage.
//!
//! Persistence: `~/.lean-ctx/diagnostics.json` — the CLI runs as a separate
//! process from the MCP server, so an in-memory store would never be seen by
//! `ctx_read`'s auto-mode. Entries expire after `TTL_SECS`; a succeeding run
//! of the same tool clears its diagnostics.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

const STORE_FILE: &str = "diagnostics.json";
/// Diagnostics older than this are stale — builds move fast.
const TTL_SECS: u64 = 15 * 60;
/// Bound per snapshot; one broken refactor can emit hundreds of errors.
const MAX_DIAGNOSTICS: usize = 200;

static STORE: OnceLock<Mutex<DiagnosticsStore>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub path: String,
    pub line: Option<u32>,
    pub severity: Severity,
    pub tool: String,
    pub message: String,
    pub recorded_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiagnosticsStore {
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip)]
    dirty: bool,
}

/// Which diagnostic tool a shell command belongs to, if any.
fn tool_of_command(command: &str) -> Option<&'static str> {
    let c = command.to_ascii_lowercase();
    if c.contains("cargo build")
        || c.contains("cargo check")
        || c.contains("cargo clippy")
        || c.contains("cargo test")
    {
        return Some("cargo");
    }
    if c.contains("tsc") {
        return Some("tsc");
    }
    if c.contains("eslint") {
        return Some("eslint");
    }
    None
}

impl DiagnosticsStore {
    fn load_from_disk() -> Self {
        let Ok(raw) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        let mut store: Self = serde_json::from_str(&raw).unwrap_or_default();
        store.expire(now_unix());
        store
    }

    fn expire(&mut self, now: u64) {
        let before = self.diagnostics.len();
        self.diagnostics
            .retain(|d| now.saturating_sub(d.recorded_unix) <= TTL_SECS);
        if self.diagnostics.len() != before {
            self.dirty = true;
        }
    }

    pub fn clear_for_tool(&mut self, tool: &str) {
        let before = self.diagnostics.len();
        self.diagnostics.retain(|d| d.tool != tool);
        if self.diagnostics.len() != before {
            self.dirty = true;
        }
    }

    pub fn replace_for_tool(&mut self, tool: &str, mut fresh: Vec<Diagnostic>) {
        self.diagnostics.retain(|d| d.tool != tool);
        fresh.truncate(MAX_DIAGNOSTICS);
        self.diagnostics.extend(fresh);
        self.dirty = true;
    }

    #[must_use]
    pub fn has_error(&self, path: &str) -> bool {
        let norm = crate::core::pathutil::normalize_tool_path(path);
        self.diagnostics.iter().any(|d| {
            d.severity == Severity::Error && (norm.ends_with(&d.path) || d.path.ends_with(&norm))
        })
    }

    #[must_use]
    pub fn severity_for(&self, path: &str) -> Option<Severity> {
        let norm = crate::core::pathutil::normalize_tool_path(path);
        let mut found: Option<Severity> = None;
        for d in &self.diagnostics {
            if norm.ends_with(&d.path) || d.path.ends_with(&norm) {
                if d.severity == Severity::Error {
                    return Some(Severity::Error);
                }
                found = Some(Severity::Warning);
            }
        }
        found
    }

    #[must_use]
    pub fn for_path(&self, path: &str) -> Vec<&Diagnostic> {
        let norm = crate::core::pathutil::normalize_tool_path(path);
        self.diagnostics
            .iter()
            .filter(|d| norm.ends_with(&d.path) || d.path.ends_with(&norm))
            .collect()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(self)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }
}

fn store_path() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(STORE_FILE)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn global() -> &'static Mutex<DiagnosticsStore> {
    STORE.get_or_init(|| Mutex::new(DiagnosticsStore::load_from_disk()))
}

/// Shell-layer hook: parse diagnostics out of a finished command.
/// Success clears the tool's previous diagnostics; failure replaces them.
/// Cheap for non-diagnostic commands (one `contains` probe).
pub fn record_from_shell(command: &str, output: &str, exit_code: i32) {
    let Some(tool) = tool_of_command(command) else {
        return;
    };
    let Ok(mut store) = global().lock() else {
        return;
    };
    if exit_code == 0 {
        store.clear_for_tool(tool);
    } else {
        let parsed = parse_output(tool, output);
        // A failing exit with zero parsed file references (e.g. test assertion
        // failures) should not wipe real compile errors recorded earlier.
        if !parsed.is_empty() {
            store.replace_for_tool(tool, parsed);
        }
    }
    if store.dirty && store.save().is_ok() {
        store.dirty = false;
    }
}

/// Does any tracked file currently carry a compile error?
#[must_use]
pub fn has_error(path: &str) -> bool {
    global().lock().is_ok_and(|s| s.has_error(path))
}

#[must_use]
pub fn severity_for(path: &str) -> Option<Severity> {
    global().lock().ok().and_then(|s| s.severity_for(path))
}

/// Snapshot for ranking/triage consumers: `(path, severity)` pairs.
#[must_use]
pub fn snapshot() -> Vec<(String, Severity)> {
    global()
        .lock()
        .map(|s| {
            s.diagnostics
                .iter()
                .map(|d| (d.path.clone(), d.severity))
                .collect()
        })
        .unwrap_or_default()
}

/// Diagnostics for one path: `(line, severity, message)` triples.
#[must_use]
pub fn details_for(path: &str) -> Vec<(Option<u32>, Severity, String)> {
    global()
        .lock()
        .map(|s| {
            s.for_path(path)
                .into_iter()
                .map(|d| (d.line, d.severity, d.message.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_output(tool: &str, output: &str) -> Vec<Diagnostic> {
    match tool {
        "cargo" => parse_cargo(output),
        "tsc" => parse_tsc(output),
        "eslint" => parse_eslint(output),
        _ => Vec::new(),
    }
}

fn cap_message(msg: &str) -> String {
    let trimmed = msg.trim();
    if trimmed.len() <= 160 {
        trimmed.to_string()
    } else {
        let mut end = 157;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &trimmed[..end])
    }
}

/// Cargo/rustc: severity line (`error[E0308]: ...` / `warning: ...`) followed
/// by a location line (`  --> src/main.rs:12:5`).
fn parse_cargo(output: &str) -> Vec<Diagnostic> {
    let now = now_unix();
    let mut out = Vec::new();
    let mut pending: Option<(Severity, String)> = None;

    for line in output.lines() {
        let trimmed = line.trim_start();
        if let Some(msg) = trimmed.strip_prefix("error") {
            // `error[E0308]: ...`, `error: ...` — but not `error_count` etc.
            if let Some(rest) = msg.split_once(':').map(|(_, r)| r)
                && (msg.starts_with('[') || msg.starts_with(':'))
            {
                pending = Some((Severity::Error, cap_message(rest)));
                continue;
            }
        }
        if let Some(msg) = trimmed.strip_prefix("warning:") {
            // Skip cargo's summary lines ("warning: `x` generated 3 warnings").
            if !msg.contains("generated") {
                pending = Some((Severity::Warning, cap_message(msg)));
            }
            continue;
        }
        if let Some(loc) = trimmed.strip_prefix("--> ")
            && let Some((severity, message)) = pending.take()
        {
            let mut parts = loc.rsplitn(3, ':');
            let _col = parts.next();
            let line_no = parts.next().and_then(|l| l.parse::<u32>().ok());
            let path = parts.next().unwrap_or(loc).trim().to_string();
            if !path.is_empty() {
                out.push(Diagnostic {
                    path,
                    line: line_no,
                    severity,
                    tool: "cargo".into(),
                    message,
                    recorded_unix: now,
                });
            }
        }
    }
    out
}

/// tsc emits two formats:
/// `src/a.ts(12,5): error TS2304: ...` and `src/a.ts:12:5 - error TS2304: ...`
fn parse_tsc(output: &str) -> Vec<Diagnostic> {
    let now = now_unix();
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        let (is_error, marker) = if line.contains(": error TS") {
            (true, ": error TS")
        } else if line.contains("- error TS") {
            (true, "- error TS")
        } else if line.contains(": warning TS") {
            (false, ": warning TS")
        } else {
            continue;
        };
        let Some(loc_part) = line.split(marker).next() else {
            continue;
        };
        let message = line
            .split_once("TS")
            .and_then(|(_, rest)| rest.split_once(':'))
            .map(|(_, m)| cap_message(m))
            .unwrap_or_default();

        let (path, line_no) = if let Some((p, rest)) = loc_part.split_once('(') {
            let n = rest.split(',').next().and_then(|x| x.parse::<u32>().ok());
            (p.trim().to_string(), n)
        } else {
            let mut parts = loc_part.trim().rsplitn(3, ':');
            let _col = parts.next();
            let n = parts.next().and_then(|x| x.parse::<u32>().ok());
            (parts.next().unwrap_or("").trim().to_string(), n)
        };
        if path.is_empty() {
            continue;
        }
        out.push(Diagnostic {
            path,
            line: line_no,
            severity: if is_error {
                Severity::Error
            } else {
                Severity::Warning
            },
            tool: "tsc".into(),
            message,
            recorded_unix: now,
        });
    }
    out
}

/// eslint (stylish): file header line, then `  12:5  error  msg  rule`.
fn parse_eslint(output: &str) -> Vec<Diagnostic> {
    let now = now_unix();
    let mut out = Vec::new();
    let mut current_file: Option<String> = None;

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
        let is_header = !line.starts_with(' ')
            && !line.starts_with('✖')
            && (line.starts_with('/') || line.contains('/'))
            && std::path::Path::new(line.trim())
                .extension()
                .is_some_and(|e| {
                    matches!(
                        e.to_str().unwrap_or(""),
                        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "vue" | "svelte"
                    )
                });
        if is_header {
            current_file = Some(line.trim().to_string());
            continue;
        }
        let trimmed = line.trim_start();
        let Some(file) = &current_file else {
            continue;
        };
        let mut cols = trimmed.split_whitespace();
        let Some(loc) = cols.next() else { continue };
        let Some(line_no) = loc.split(':').next().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let severity = match cols.next() {
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => continue,
        };
        let message = cap_message(&cols.collect::<Vec<_>>().join(" "));
        out.push(Diagnostic {
            path: file.clone(),
            line: Some(line_no),
            severity,
            tool: "eslint".into(),
            message,
            recorded_unix: now,
        });
    }
    out
}

/// Ranking boost per path: errors dominate, warnings hint (#499).
#[must_use]
pub fn relevance_boost(path: &str) -> f64 {
    match severity_for(path) {
        Some(Severity::Error) => 0.35,
        Some(Severity::Warning) => 0.10,
        None => 0.0,
    }
}

/// Apply diagnostic boosts to a relevance ranking and re-sort.
pub fn apply_boost(scores: &mut [crate::core::task_relevance::RelevanceScore]) {
    let snap = snapshot();
    if snap.is_empty() {
        return;
    }
    let mut by_path: HashMap<&str, Severity> = HashMap::new();
    for (p, s) in &snap {
        let entry = by_path.entry(p.as_str()).or_insert(*s);
        if *s == Severity::Error {
            *entry = Severity::Error;
        }
    }
    for score in scores.iter_mut() {
        let hit = by_path
            .iter()
            .find(|(p, _)| score.path.ends_with(*p) || p.ends_with(&score.path))
            .map(|(_, s)| *s);
        match hit {
            Some(Severity::Error) => score.score = (score.score + 0.35).min(1.0),
            Some(Severity::Warning) => score.score = (score.score + 0.10).min(1.0),
            None => {}
        }
    }
    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO_OUT: &str = r#"
   Compiling lean-ctx v3.7.5
error[E0308]: mismatched types
  --> src/core/cache.rs:42:9
   |
42 |         "x"
   |         ^^^ expected `usize`, found `&str`
warning: unused variable: `foo`
  --> src/tools/ctx_read.rs:10:9
error: aborting due to 1 previous error
"#;

    #[test]
    fn cargo_error_paths_extracted() {
        let diags = parse_cargo(CARGO_OUT);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].path, "src/core/cache.rs");
        assert_eq!(diags[0].line, Some(42));
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[1].path, "src/tools/ctx_read.rs");
        assert_eq!(diags[1].severity, Severity::Warning);
    }

    #[test]
    fn tsc_both_formats_extracted() {
        let out = "src/app.ts(12,5): error TS2304: Cannot find name 'foo'.\n\
                   src/lib.ts:7:3 - error TS2345: Argument type mismatch.";
        let diags = parse_tsc(out);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].path, "src/app.ts");
        assert_eq!(diags[0].line, Some(12));
        assert_eq!(diags[1].path, "src/lib.ts");
        assert_eq!(diags[1].line, Some(7));
        assert!(diags[1].message.contains("Argument type mismatch"));
    }

    #[test]
    fn eslint_stylish_extracted() {
        let out = "/repo/src/index.ts\n  3:1  error  'x' is never used  no-unused-vars\n  9:5  warning  Unexpected console  no-console\n";
        let diags = parse_eslint(out);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].path, "/repo/src/index.ts");
        assert_eq!(diags[0].line, Some(3));
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[1].severity, Severity::Warning);
    }

    #[test]
    fn successful_run_clears_tool_diagnostics() {
        let mut store = DiagnosticsStore::default();
        store.replace_for_tool("cargo", parse_cargo(CARGO_OUT));
        assert!(store.has_error("src/core/cache.rs"));
        store.clear_for_tool("cargo");
        assert!(!store.has_error("src/core/cache.rs"));
    }

    #[test]
    fn expiry_drops_stale_entries() {
        let mut store = DiagnosticsStore::default();
        store.replace_for_tool("cargo", parse_cargo(CARGO_OUT));
        store.expire(now_unix() + TTL_SECS + 10);
        assert!(store.diagnostics.is_empty());
    }

    #[test]
    fn severity_prefers_error_over_warning() {
        let mut store = DiagnosticsStore::default();
        let now = now_unix();
        store.replace_for_tool(
            "cargo",
            vec![
                Diagnostic {
                    path: "src/a.rs".into(),
                    line: Some(1),
                    severity: Severity::Warning,
                    tool: "cargo".into(),
                    message: "w".into(),
                    recorded_unix: now,
                },
                Diagnostic {
                    path: "src/a.rs".into(),
                    line: Some(9),
                    severity: Severity::Error,
                    tool: "cargo".into(),
                    message: "e".into(),
                    recorded_unix: now,
                },
            ],
        );
        assert_eq!(store.severity_for("src/a.rs"), Some(Severity::Error));
    }

    #[test]
    fn tool_detection_gates_parsing() {
        assert_eq!(tool_of_command("cargo build --release"), Some("cargo"));
        assert_eq!(tool_of_command("npx tsc --noEmit"), Some("tsc"));
        assert_eq!(tool_of_command("eslint src/"), Some("eslint"));
        assert_eq!(tool_of_command("git status"), None);
    }

    #[test]
    fn test_failure_without_paths_keeps_existing() {
        let mut store = DiagnosticsStore::default();
        store.replace_for_tool("cargo", parse_cargo(CARGO_OUT));
        let n = store.diagnostics.len();
        // Simulates the record_from_shell guard: empty parse -> no replace.
        let parsed = parse_cargo("test result: FAILED. 1 passed; 1 failed");
        assert!(parsed.is_empty());
        assert_eq!(store.diagnostics.len(), n);
    }
}
