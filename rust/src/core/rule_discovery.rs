//! Rule discovery for ctx_read (#1325).
//!
//! When lean-ctx replaces native file-read tools, the IDE's rule-injection
//! contract must be honoured: reading a file should surface rules scoped to
//! that path (CLAUDE.md, .claude/rules, .cursor/rules, AGENTS.md).
//!
//! This module discovers applicable rules for a given file path, caches them
//! per directory, and formats them for appending to `ctx_read` output.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredRule {
    pub source: String,
    pub content: String,
}

// ── Per-session dedup ───────────────────────────────────────────────────────

static INJECTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn already_injected(key: &str) -> bool {
    let mut guard = INJECTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let set = guard.get_or_insert_with(HashSet::new);
    !set.insert(key.to_string())
}

/// Reset injection tracking (for tests).
#[cfg(test)]
pub(crate) fn reset_injection_cache() {
    let mut guard = INJECTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = None;
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Max tokens for the injected rules suffix. Prevents rules from dominating
/// the response and triggering turn-budget truncation of file content (#1406).
const MAX_RULES_SUFFIX_TOKENS: usize = 800;

/// Discover and format rules applicable to `file_path` that haven't been
/// injected yet in this session. Returns an empty string when no new rules
/// apply or when the client natively handles rule injection.
pub(crate) fn rules_suffix_for_read(
    file_path: &str,
    project_root: &str,
    client_id: &str,
) -> String {
    let cfg = crate::core::config::Config::load();
    if cfg.declines_rule_steering() || client_natively_injects_rules(client_id) {
        return String::new();
    }

    let rules = discover_rules(file_path, project_root, client_id);
    if rules.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    let mut token_count = 0;
    for rule in &rules {
        let key = format!("{}:{}", rule.source, blake3_short(&rule.content));
        if already_injected(&key) {
            continue;
        }
        let part = format!("[From: {}]\n{}", rule.source, rule.content.trim());
        let part_tokens = crate::core::tokens::count_tokens(&part);
        if token_count + part_tokens > MAX_RULES_SUFFIX_TOKENS && !parts.is_empty() {
            parts.push(format!(
                "[… {} more rule(s) omitted — use auto_inject_rules=false to disable]",
                rules.len() - parts.len()
            ));
            break;
        }
        token_count += part_tokens;
        parts.push(part);
    }

    if parts.is_empty() {
        return String::new();
    }

    format!(
        "\n\n--- Rules in scope for this file ---\n{}",
        parts.join("\n\n")
    )
}

// ── Client detection ────────────────────────────────────────────────────────

/// Returns true when the client's native harness already injects rules on
/// file reads, so lean-ctx should NOT duplicate them.
///
/// #1406: Claude Code loads CLAUDE.md + AGENTS.md + .claude/rules/ at session
/// start; Cursor loads .cursor/rules/ + AGENTS.md via workspace rules. Both
/// have the rules in their system prompt already — re-injecting them via
/// ctx_read wastes tokens and can trigger turn-budget truncation.
fn client_natively_injects_rules(client_id: &str) -> bool {
    let lower = client_id.to_lowercase();
    lower.contains("claude") || lower.contains("cursor")
}

// ── Discovery ───────────────────────────────────────────────────────────────

static DIR_CACHE: Mutex<Option<HashMap<String, Vec<DiscoveredRule>>>> = Mutex::new(None);

fn discover_rules(file_path: &str, project_root: &str, client_id: &str) -> Vec<DiscoveredRule> {
    let file = Path::new(file_path);
    let root = Path::new(project_root);
    let dir = file.parent().unwrap_or(root);

    let cache_key = format!("{client_id}:{}", dir.display());
    {
        let guard = DIR_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cache) = guard.as_ref()
            && let Some(cached) = cache.get(&cache_key)
        {
            return filter_by_path(cached, file_path, project_root);
        }
    }

    let mut rules = Vec::new();

    collect_hierarchy_rules(dir, root, &mut rules);
    collect_glob_rules(project_root, client_id, &mut rules);

    {
        let mut guard = DIR_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cache = guard.get_or_insert_with(HashMap::new);
        cache.insert(cache_key, rules.clone());
    }

    filter_by_path(&rules, file_path, project_root)
}

/// Walk from `dir` up to `root`, collecting CLAUDE.md and AGENTS.md files.
fn collect_hierarchy_rules(dir: &Path, root: &Path, rules: &mut Vec<DiscoveredRule>) {
    let mut current = Some(dir);
    while let Some(d) = current {
        for name in &["CLAUDE.md", "AGENTS.md"] {
            let candidate = d.join(name);
            if candidate.is_file()
                && let Ok(content) = std::fs::read_to_string(&candidate)
                && !content.trim().is_empty()
            {
                let relative = candidate.strip_prefix(root).map_or_else(
                    |_| candidate.display().to_string(),
                    |p| p.display().to_string(),
                );
                rules.push(DiscoveredRule {
                    source: relative,
                    content,
                });
            }
        }
        if d == root {
            break;
        }
        current = d.parent();
    }
}

/// Collect glob-scoped rules from .claude/rules and .cursor/rules.
fn collect_glob_rules(project_root: &str, client_id: &str, rules: &mut Vec<DiscoveredRule>) {
    let root = Path::new(project_root);

    let rule_dirs: Vec<PathBuf> = if client_id.contains("cursor") {
        vec![root.join(".cursor/rules")]
    } else if client_id.contains("claude") {
        vec![root.join(".claude/rules")]
    } else {
        vec![root.join(".cursor/rules"), root.join(".claude/rules")]
    };

    for rule_dir in rule_dirs {
        if !rule_dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&rule_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "md" | "mdc") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.trim().is_empty() {
                    continue;
                }
                let relative = path
                    .strip_prefix(root)
                    .map_or_else(|_| path.display().to_string(), |p| p.display().to_string());
                rules.push(DiscoveredRule {
                    source: relative,
                    content,
                });
            }
        }
    }
}

/// Filter rules to those applicable to the given file path.
///
/// Hierarchy rules (CLAUDE.md, AGENTS.md) always apply.
/// For `.cursor/rules` / `.claude/rules`:
///   - Rules with `alwaysApply: true` are skipped (already in system prompt)
///   - Rules without explicit `globs:` / `path:` patterns are skipped
///     (no frontmatter = system-level rule, also already in system prompt)
///   - Only rules with `globs:` / `path:` matching the file are injected
fn filter_by_path(
    rules: &[DiscoveredRule],
    file_path: &str,
    project_root: &str,
) -> Vec<DiscoveredRule> {
    let relative_file = Path::new(file_path)
        .strip_prefix(project_root)
        .map_or_else(|_| file_path.to_string(), |p| p.display().to_string());

    rules
        .iter()
        .filter(|r| {
            let src = &r.source;
            if src.ends_with("CLAUDE.md") || src.ends_with("AGENTS.md") {
                return true;
            }
            if is_always_apply(&r.content) {
                return false;
            }
            match extract_globs(&r.content) {
                Some(patterns) => patterns.iter().any(|p| glob_matches(p, &relative_file)),
                // No globs/path in frontmatter → system-level rule, skip
                None => false,
            }
        })
        .cloned()
        .collect()
}

/// Detect `alwaysApply: true` in rule frontmatter.
fn is_always_apply(content: &str) -> bool {
    if !content.starts_with("---") {
        return false;
    }
    let Some(end) = content[3..].find("---") else {
        return false;
    };
    let frontmatter = &content[3..3 + end];
    frontmatter
        .lines()
        .any(|l| l.trim().starts_with("alwaysApply:") && l.contains("true"))
}

/// Extract glob patterns from rule file frontmatter.
/// Supports both Cursor `.mdc` format (`globs: pattern`) and Claude Code
/// `.claude/rules/*.md` format (`path: pattern`).
fn extract_globs(content: &str) -> Option<Vec<String>> {
    if !content.starts_with("---") {
        return None;
    }
    let end = content[3..].find("---")?;
    let frontmatter = &content[3..3 + end];

    let mut patterns = Vec::new();
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("globs:")
            .or_else(|| trimmed.strip_prefix("path:"))
        {
            let pattern = rest.trim().trim_matches('"').trim_matches('\'');
            if !pattern.is_empty() {
                for p in pattern.split(',') {
                    let p = p.trim();
                    if !p.is_empty() {
                        patterns.push(p.to_string());
                    }
                }
            }
        }
    }

    if patterns.is_empty() {
        None
    } else {
        Some(patterns)
    }
}

/// Simple glob matching (supports `*` and `**`).
fn glob_matches(pattern: &str, path: &str) -> bool {
    if pattern == "*" || pattern == "**/*" || pattern == "**" {
        return true;
    }

    let pattern = pattern.replace('\\', "/");
    let path = path.replace('\\', "/");

    if let Some(suffix) = pattern.strip_prefix("**/") {
        if !suffix.contains('/') {
            let filename = path.rsplit('/').next().unwrap_or(&path);
            return glob_matches(suffix, filename);
        }
        let mut remaining = &path[..];
        loop {
            if glob_matches(suffix, remaining) {
                return true;
            }
            match remaining.find('/') {
                Some(pos) => remaining = &remaining[pos + 1..],
                None => return false,
            }
        }
    }

    if pattern.starts_with("*.") {
        let ext = &pattern[1..];
        return path.ends_with(ext);
    }

    if !pattern.contains('*') {
        return path == pattern || path.ends_with(&format!("/{pattern}"));
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 2 {
        let (prefix, suffix) = (parts[0], parts[1]);
        return (prefix.is_empty() || path.starts_with(prefix))
            && (suffix.is_empty() || path.ends_with(suffix));
    }

    path.contains(pattern.trim_matches('*'))
}

fn blake3_short(content: &str) -> String {
    let hash = blake3::hash(content.as_bytes());
    hash.to_hex()[..8].to_string()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_auto_inject_false_suppresses_ctx_read_rule_discovery() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config");
        let project = tmp.path().join("project");
        let source_dir = project.join("src");
        let file = source_dir.join("main.rs");

        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(&file, "fn main() {}\n").unwrap();
        std::fs::write(
            project.join("AGENTS.md"),
            "# Project rules\nAlways write regression tests.\n",
        )
        .unwrap();

        let _config_guard = crate::setup::EnvVarGuard::set(
            "LEAN_CTX_CONFIG_DIR",
            config_dir.to_string_lossy().as_ref(),
        );
        let _injection_guard = crate::setup::EnvVarGuard::set("LEAN_CTX_RULES_INJECTION", "shared");

        std::fs::write(
            config_dir.join("config.toml"),
            "[setup]\nauto_inject_rules = true\n",
        )
        .unwrap();

        reset_injection_cache();
        let enabled = rules_suffix_for_read(
            file.to_string_lossy().as_ref(),
            project.to_string_lossy().as_ref(),
            "opencode",
        );

        assert!(
            enabled.contains("Always write regression tests."),
            "test precondition failed: enabled rule discovery produced {enabled:?}"
        );

        std::fs::write(
            config_dir.join("config.toml"),
            "[setup]\nauto_inject_rules = false\n",
        )
        .unwrap();

        reset_injection_cache();
        let disabled = rules_suffix_for_read(
            file.to_string_lossy().as_ref(),
            project.to_string_lossy().as_ref(),
            "opencode",
        );

        assert!(
            disabled.is_empty(),
            "explicit auto_inject_rules=false must suppress ctx_read rule discovery, got: {disabled:?}"
        );
    }

    #[test]
    fn glob_matches_extension() {
        assert!(glob_matches("*.rs", "src/main.rs"));
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(!glob_matches("*.rs", "src/main.py"));
    }

    #[test]
    fn glob_matches_double_star() {
        assert!(glob_matches("**/test_*.rs", "src/tests/test_foo.rs"));
        assert!(glob_matches("**/test_*.rs", "test_foo.rs"));
        assert!(!glob_matches("**/test_*.rs", "src/foo.rs"));
    }

    #[test]
    fn glob_matches_exact() {
        assert!(glob_matches("src/main.rs", "src/main.rs"));
        assert!(!glob_matches("src/main.rs", "src/lib.rs"));
    }

    #[test]
    fn extract_globs_cursor_mdc() {
        let content = "---\nglobs: \"*.rs, *.toml\"\n---\nSome rule content";
        let patterns = extract_globs(content).unwrap();
        assert_eq!(patterns, vec!["*.rs", "*.toml"]);
    }

    #[test]
    fn extract_globs_claude_path() {
        let content = "---\npath: src/**/*.ts\n---\nRule for TS files";
        let patterns = extract_globs(content).unwrap();
        assert_eq!(patterns, vec!["src/**/*.ts"]);
    }

    #[test]
    fn extract_globs_no_frontmatter() {
        let content = "Just a plain markdown rule file";
        assert!(extract_globs(content).is_none());
    }

    #[test]
    fn format_suffix_empty_when_no_rules() {
        reset_injection_cache();
        let suffix = rules_suffix_for_read("/nonexistent/file.rs", "/nonexistent", "cursor");
        assert!(suffix.is_empty(), "{suffix:?}");
    }

    #[test]
    fn dedup_prevents_reinjection() {
        reset_injection_cache();
        let key = "test:abc12345";
        assert!(!already_injected(key));
        assert!(already_injected(key));
    }

    #[test]
    fn always_apply_detected() {
        let content = "---\ndescription: test\nalwaysApply: true\n---\nRule body";
        assert!(is_always_apply(content));
    }

    #[test]
    fn always_apply_false_not_detected() {
        let content = "---\ndescription: test\nalwaysApply: false\n---\nRule body";
        assert!(!is_always_apply(content));
    }

    #[test]
    fn no_frontmatter_not_always_apply() {
        let content = "Just a plain rule without frontmatter";
        assert!(!is_always_apply(content));
    }

    #[test]
    fn filter_skips_always_apply_rules() {
        let rules = vec![
            DiscoveredRule {
                source: ".cursor/rules/always.mdc".into(),
                content: "---\nalwaysApply: true\n---\nAlready in system prompt".into(),
            },
            DiscoveredRule {
                source: ".cursor/rules/scoped.mdc".into(),
                content: "---\nglobs: \"*.rs\"\n---\nRust-scoped rule".into(),
            },
            DiscoveredRule {
                source: ".cursor/rules/no-frontmatter.mdc".into(),
                content: "Rule without frontmatter".into(),
            },
            DiscoveredRule {
                source: "AGENTS.md".into(),
                content: "Agent instructions".into(),
            },
        ];
        let filtered = filter_by_path(&rules, "/project/src/main.rs", "/project");
        let sources: Vec<&str> = filtered.iter().map(|r| r.source.as_str()).collect();
        assert!(
            sources.contains(&"AGENTS.md"),
            "hierarchy rules always pass"
        );
        assert!(
            sources.contains(&".cursor/rules/scoped.mdc"),
            "glob-matched rules pass"
        );
        assert!(
            !sources.contains(&".cursor/rules/always.mdc"),
            "alwaysApply skipped"
        );
        assert!(
            !sources.contains(&".cursor/rules/no-frontmatter.mdc"),
            "no-frontmatter skipped"
        );
    }

    #[test]
    fn rules_suffix_completes_within_budget() {
        reset_injection_cache();
        let start = std::time::Instant::now();
        for _ in 0..100 {
            let _ =
                rules_suffix_for_read("/tmp/nonexistent/src/main.rs", "/tmp/nonexistent", "cursor");
        }
        let elapsed = start.elapsed();
        let per_call_us = elapsed.as_micros() / 100;
        assert!(
            per_call_us < 5_000,
            "rule discovery too slow: {per_call_us}μs per call (budget: 5ms)"
        );
    }

    #[test]
    fn real_project_overhead_acceptable() {
        reset_injection_cache();
        let manifest = env!("CARGO_MANIFEST_DIR");
        let project_root = std::path::Path::new(manifest)
            .parent()
            .unwrap()
            .to_str()
            .unwrap();
        let file = format!("{manifest}/src/core/rule_discovery.rs");

        // Cold call (no cache, does filesystem I/O)
        let start = std::time::Instant::now();
        let suffix = rules_suffix_for_read(&file, project_root, "cursor");
        let cold_us = start.elapsed().as_micros();

        // Warm calls (directory cache hit, only dedup check)
        let start = std::time::Instant::now();
        for _ in 0..100 {
            reset_injection_cache();
            let _ = rules_suffix_for_read(&file, project_root, "cursor");
        }
        let warm_us = start.elapsed().as_micros() / 100;

        eprintln!("\n=== Rule Discovery Performance ===");
        eprintln!(
            "Cold: {cold_us}μs | Warm: {warm_us}μs | Rules: {} bytes",
            suffix.len()
        );

        assert!(
            cold_us < 50_000,
            "cold call too slow: {cold_us}μs (budget: 50ms)"
        );
        assert!(
            warm_us < 5_000,
            "warm call too slow: {warm_us}μs (budget: 5ms)"
        );
    }
}
