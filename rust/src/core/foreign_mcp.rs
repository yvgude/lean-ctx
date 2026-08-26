//! Foreign MCP server audit — non-lean-ctx MCP servers the host client loads
//! into every session.
//!
//! lean-ctx measures its own fixed cost precisely, but every *other* configured
//! MCP server bills its tool schemas + instructions into the same context
//! budget — often more than lean-ctx's whole surface — and nothing reports
//! whether those servers are ever used. This module cross-references:
//!
//! * **configured** servers: `~/.claude.json` `mcpServers` (global and
//!   per-project) and the project `.mcp.json`, and
//! * **observed** servers: Claude Code per-project MCP log directories, which
//!   also capture claude.ai account connectors that appear in no local config,
//!
//! with **recorded usage**: `mcp__<server>__*` `tool_use` entries in the local
//! Claude Code transcripts (`~/.claude/projects/*/*.jsonl`).
//!
//! Deterministic, local-only, read-only — findings are suggestions the
//! operator acts on explicitly (mirrors [`crate::core::tool_health`]).
//!
//! Honesty note: a foreign server's schema size is not measurable locally
//! without connecting to it, so the report states *measured usage* only and
//! never invents a token estimate.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

/// Transcripts modified within this many days are scanned for usage.
const TRANSCRIPT_WINDOW_DAYS: u64 = 45;

/// One foreign (non-lean-ctx) MCP server the client loads.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ForeignServerEntry {
    pub name: String,
    /// Where the server was discovered (config scope or observed connection).
    pub source: String,
    /// `tool_use` calls recorded in scanned transcripts.
    pub calls: u64,
    /// Distinct transcripts containing at least one call.
    pub sessions_with_use: usize,
    /// Transcripts scanned for the window.
    pub transcripts_scanned: usize,
    /// Non-empty when the server looks like pure cost (never called).
    pub action: String,
}

/// Canonical comparison key: the `mcp__<server>__` tool-name form. Claude Code
/// replaces every non-alphanumeric byte with `_` (e.g. "claude.ai Figma" →
/// `claude_ai_Figma`), so config names, log-dir names, and tool prefixes all
/// normalize to the same key.
#[must_use]
fn tool_prefix_form(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Claude Code encodes paths and server names for cache directories by
/// replacing every non-alphanumeric byte with `-`.
#[must_use]
fn dir_form(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// True for lean-ctx itself, under any spelling a client config may use.
#[must_use]
fn is_lean_ctx(name: &str) -> bool {
    tool_prefix_form(name).eq_ignore_ascii_case("lean_ctx")
}

/// Inserts every server name under `servers` (a `mcpServers` JSON object).
fn insert_server_names(
    out: &mut BTreeMap<String, String>,
    servers: Option<&serde_json::Value>,
    source: &str,
) {
    if let Some(map) = servers.and_then(|s| s.as_object()) {
        for name in map.keys() {
            out.entry(name.clone())
                .or_insert_with(|| source.to_string());
        }
    }
}

/// Servers named in local Claude Code configuration: global + per-project
/// `mcpServers` in `~/.claude.json`, and the project `.mcp.json`.
fn configured_servers(home: &Path, project: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Ok(raw) = std::fs::read_to_string(home.join(".claude.json"))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        insert_server_names(
            &mut out,
            v.get("mcpServers"),
            "global config (~/.claude.json)",
        );
        if let Some(p) = v
            .get("projects")
            .and_then(|p| p.as_object())
            .and_then(|projects| projects.get(project.to_string_lossy().as_ref()))
        {
            insert_server_names(
                &mut out,
                p.get("mcpServers"),
                "project config (~/.claude.json)",
            );
        }
    }
    if let Ok(raw) = std::fs::read_to_string(project.join(".mcp.json"))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        insert_server_names(&mut out, v.get("mcpServers"), ".mcp.json");
    }
    out
}

/// Servers Claude Code actually connected to for this project, recovered from
/// its per-project MCP log directories. This is the only local trace of
/// claude.ai account connectors, which appear in no local config file.
fn observed_servers(home: &Path, project: &Path) -> BTreeMap<String, String> {
    let slug = dir_form(&project.to_string_lossy());
    let mut out = BTreeMap::new();
    for base in [
        home.join("Library/Caches/claude-cli-nodejs"),
        home.join(".cache/claude-cli-nodejs"),
    ] {
        let Ok(entries) = std::fs::read_dir(base.join(&slug)) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(server) = name.strip_prefix("mcp-logs-") {
                out.entry(server.to_string())
                    .or_insert_with(|| "observed connection (MCP logs)".to_string());
            }
        }
    }
    out
}

/// Pure: counts `tool_use` invocations per `mcp__<server>__` prefix across one
/// transcript's JSONL lines.
fn count_tool_use_by_server<I, S>(lines: I) -> BTreeMap<String, u64>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for line in lines {
        let line = line.as_ref();
        if !line.contains("\"mcp__") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for item in content {
            if item.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let Some(name) = item.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let Some((server, _tool)) = name
                .strip_prefix("mcp__")
                .and_then(|rest| rest.split_once("__"))
            else {
                continue;
            };
            *out.entry(server.to_string()).or_default() += 1;
        }
    }
    out
}

/// Scans recent Claude Code transcripts and aggregates per-server usage:
/// returns (transcripts scanned, server → (calls, transcripts with ≥1 call)).
fn transcript_usage(home: &Path) -> (usize, BTreeMap<String, (u64, usize)>) {
    use std::io::BufRead;

    let mut scanned = 0usize;
    let mut usage: BTreeMap<String, (u64, usize)> = BTreeMap::new();
    let cutoff = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(
        TRANSCRIPT_WINDOW_DAYS * 86_400,
    ));
    let Ok(dirs) = std::fs::read_dir(home.join(".claude/projects")) else {
        return (0, usage);
    };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let (Some(cutoff), Ok(meta)) = (cutoff, f.metadata())
                && meta.modified().is_ok_and(|m| m < cutoff)
            {
                continue;
            }
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            scanned += 1;
            let reader = std::io::BufReader::new(file);
            let counts = count_tool_use_by_server(reader.lines().map_while(Result::ok));
            for (server, calls) in counts {
                let e = usage.entry(server).or_default();
                e.0 += calls;
                e.1 += 1;
            }
        }
    }
    (scanned, usage)
}

/// Pure: joins discovered servers with recorded usage and renders verdicts.
/// Never-called servers sort first (they are the actionable rot).
#[must_use]
fn build_entries(
    discovered: Vec<(String, String)>,
    usage: &BTreeMap<String, (u64, usize)>,
    transcripts_scanned: usize,
) -> Vec<ForeignServerEntry> {
    let mut out: Vec<ForeignServerEntry> = discovered
        .into_iter()
        .map(|(name, source)| {
            let key = tool_prefix_form(&name);
            let (calls, sessions_with_use) = usage
                .iter()
                .find(|(s, _)| tool_prefix_form(s) == key)
                .map_or((0, 0), |(_, &(c, s))| (c, s));
            let action = if calls == 0 && transcripts_scanned > 0 {
                format!(
                    "never called in {transcripts_scanned} transcript(s) over {TRANSCRIPT_WINDOW_DAYS}d — its schemas still load every session; disable via `/mcp` (claude.ai connectors: claude.ai → Settings → Connectors)"
                )
            } else {
                String::new()
            };
            ForeignServerEntry {
                name,
                source,
                calls,
                sessions_with_use,
                transcripts_scanned,
                action,
            }
        })
        .collect();
    out.sort_by(|a, b| a.calls.cmp(&b.calls).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Gathers on-disk configuration, observed connections, and transcript usage,
/// and builds the foreign-server audit for `home`/`project`.
#[must_use]
pub(crate) fn audit(home: &Path, project: &Path) -> Vec<ForeignServerEntry> {
    // Merge keyed on the normalized tool-prefix form so "claude.ai Figma"
    // (config) and "claude-ai-Figma" (log dir) collapse into one entry;
    // configured names win the display slot.
    let mut merged: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (name, source) in configured_servers(home, project) {
        if is_lean_ctx(&name) {
            continue;
        }
        merged
            .entry(tool_prefix_form(&name))
            .or_insert((name, source));
    }
    for (name, source) in observed_servers(home, project) {
        if is_lean_ctx(&name) {
            continue;
        }
        merged
            .entry(tool_prefix_form(&name))
            .or_insert((name, source));
    }
    let (scanned, usage) = transcript_usage(home);
    build_entries(merged.into_values().collect(), &usage, scanned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_collapses_config_dir_and_tool_prefix_forms() {
        assert_eq!(tool_prefix_form("claude.ai Figma"), "claude_ai_Figma");
        assert_eq!(tool_prefix_form("claude-ai-Figma"), "claude_ai_Figma");
        assert_eq!(dir_form("claude.ai Figma"), "claude-ai-Figma");
        assert_eq!(dir_form("/a/b"), "-a-b");
    }

    #[test]
    fn lean_ctx_is_recognized_under_any_spelling() {
        assert!(is_lean_ctx("lean-ctx"));
        assert!(is_lean_ctx("lean_ctx"));
        assert!(is_lean_ctx("Lean Ctx"));
        assert!(!is_lean_ctx("claude.ai Figma"));
    }

    #[test]
    fn counts_tool_use_per_server_and_ignores_junk() {
        let lines = [
            r#"{"message":{"content":[{"type":"tool_use","name":"mcp__claude_ai_Figma__get_screenshot"}]}}"#,
            r#"{"message":{"content":[{"type":"tool_use","name":"mcp__lean-ctx__ctx_read"},{"type":"text","text":"mcp__decoy__x"}]}}"#,
            r#"{"message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
            "not json at all",
            r#"{"message":"no content array with mcp__ mention"}"#,
        ];
        let counts = count_tool_use_by_server(lines.iter().copied());
        assert_eq!(counts.get("claude_ai_Figma"), Some(&1));
        assert_eq!(counts.get("lean-ctx"), Some(&1));
        assert_eq!(
            counts.len(),
            2,
            "text blocks and native tools must not count"
        );
    }

    #[test]
    fn build_entries_flags_never_called_and_sorts_them_first() {
        let mut usage = BTreeMap::new();
        usage.insert("github".to_string(), (12u64, 3usize));
        let entries = build_entries(
            vec![
                ("github".to_string(), "global config".to_string()),
                (
                    "claude.ai Figma".to_string(),
                    "observed connection".to_string(),
                ),
            ],
            &usage,
            14,
        );
        assert_eq!(entries[0].name, "claude.ai Figma");
        assert_eq!(entries[0].calls, 0);
        assert!(
            entries[0]
                .action
                .contains("never called in 14 transcript(s)")
        );
        assert_eq!(entries[1].name, "github");
        assert_eq!(entries[1].calls, 12);
        assert_eq!(entries[1].sessions_with_use, 3);
        assert!(
            entries[1].action.is_empty(),
            "used servers carry no verdict"
        );
    }

    #[test]
    fn build_entries_matches_usage_across_name_forms() {
        let mut usage = BTreeMap::new();
        usage.insert("claude_ai_Figma".to_string(), (5u64, 2usize));
        let entries = build_entries(
            vec![("claude.ai Figma".to_string(), "config".to_string())],
            &usage,
            10,
        );
        assert_eq!(
            entries[0].calls, 5,
            "config name must match tool-prefix usage key"
        );
    }

    #[test]
    fn build_entries_without_telemetry_stays_neutral() {
        let entries = build_entries(
            vec![("figma".to_string(), "config".to_string())],
            &BTreeMap::new(),
            0,
        );
        assert!(
            entries[0].action.is_empty(),
            "no transcripts scanned → cannot judge, no verdict"
        );
    }
}
