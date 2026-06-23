//! `lean-ctx doctor overhead` — honest fixed-cost accounting (#572).
//!
//! Shows what a session costs BEFORE lean-ctx saves anything:
//!  1. advertised MCP tool schemas (mirrors the live `tools/list` policy),
//!  2. the MCP server instructions block,
//!  3. every rules file a client auto-loads, with duplicate detection.
//!
//! Research context: fixed context costs both money and model attention
//! (context degradation starts well below typical window limits), so every
//! always-on token has to justify itself.

use std::path::{Path, PathBuf};

use crate::core::context_overhead::tool_tokens;
use crate::core::tokens::count_tokens;

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RST: &str = "\x1b[0m";

/// One rules file a client auto-loads into context.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct RulesFileCost {
    pub path: String,
    /// Tokens of the whole file (what the client actually injects).
    pub file_tokens: usize,
    /// Tokens inside lean-ctx marker blocks (our share of the file).
    pub lean_ctx_tokens: usize,
    /// True when the file carries a *full* lean-ctx payload (canonical rules or
    /// the compression block), as opposed to only the lightweight
    /// `<!-- lean-ctx -->` pointer. Pointer-only files cross-reference the
    /// canonical source and do not duplicate guidance (#684).
    pub carries_full: bool,
    /// Clients that auto-load this file.
    pub clients: Vec<&'static str>,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct OverheadReport {
    pub tool_count: usize,
    pub tool_schema_tokens: usize,
    pub lean_default_tool_count: usize,
    pub lean_default_tool_tokens: usize,
    pub tool_profile: String,
    pub instruction_tokens: usize,
    pub rules_files: Vec<RulesFileCost>,
    pub duplicate_clients: Vec<(String, usize)>,
}

impl OverheadReport {
    fn rules_tokens_total(&self) -> usize {
        self.rules_files.iter().map(|r| r.file_tokens).sum()
    }

    fn total_tokens(&self) -> usize {
        self.tool_schema_tokens + self.instruction_tokens + self.rules_tokens_total()
    }
}

/// Tokens of the lean-ctx-owned portions of a rules file: every block that
/// starts at a line containing `<!-- lean-ctx` or the canonical rules marker
/// and ends at `<!-- /lean-ctx... -->` (inclusive). Files without markers
/// contribute 0 lean-ctx tokens (they still cost their full size).
pub(crate) fn lean_ctx_block_tokens(content: &str) -> usize {
    let mut tokens = 0;
    let mut in_block = false;
    let mut block = String::new();
    for line in content.lines() {
        if !in_block
            && (line.contains("<!-- lean-ctx")
                || line.contains(crate::core::rules_canonical::START_MARK))
        {
            in_block = true;
        }
        if in_block {
            block.push_str(line);
            block.push('\n');
            if line.contains("<!-- /lean-ctx") {
                in_block = false;
                tokens += count_tokens(&block);
                block.clear();
            }
        }
    }
    if !block.is_empty() {
        // Unterminated block (e.g. whole-file rules like .mdc without an end
        // marker) — count what we collected.
        tokens += count_tokens(&block);
    }
    tokens
}

fn push_rules_file(out: &mut Vec<RulesFileCost>, path: &Path, clients: Vec<&'static str>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    if content.trim().is_empty() {
        return;
    }
    out.push(RulesFileCost {
        path: path.to_string_lossy().to_string(),
        file_tokens: count_tokens(&content),
        lean_ctx_tokens: lean_ctx_block_tokens(&content),
        carries_full: crate::core::rules_channel::carries_full_rules(&content),
        clients,
    });
}

fn scan_mdc_dir(out: &mut Vec<RulesFileCost>, dir: &Path, clients: &[&'static str]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("mdc") {
            push_rules_file(out, &path, clients.to_vec());
        }
    }
}

/// Collects every rules file that an agent auto-loads for work in `project`:
/// global per-client files, the project root, and the parent chain up to
/// `home` (Cursor merges parent `.cursor/rules/` and AGENTS.md in monorepos).
pub(crate) fn collect_rules_files(home: &Path, project: &Path) -> Vec<RulesFileCost> {
    let mut out = Vec::new();

    // Global, per-client.
    push_rules_file(
        out.as_mut(),
        &home.join(".claude/CLAUDE.md"),
        vec!["claude"],
    );
    push_rules_file(
        out.as_mut(),
        &home.join(".codebuddy/CODEBUDDY.md"),
        vec!["codebuddy"],
    );
    push_rules_file(out.as_mut(), &home.join(".codex/AGENTS.md"), vec!["codex"]);
    push_rules_file(
        out.as_mut(),
        &home.join(".gemini/GEMINI.md"),
        vec!["gemini"],
    );
    scan_mdc_dir(out.as_mut(), &home.join(".cursor/rules"), &["cursor"]);

    // Project root + parent chain (stop at home or filesystem root).
    let mut dir = Some(project.to_path_buf());
    while let Some(d) = dir {
        push_rules_file(out.as_mut(), &d.join(".cursorrules"), vec!["cursor"]);
        scan_mdc_dir(out.as_mut(), &d.join(".cursor/rules"), &["cursor"]);
        // AGENTS.md is the shared instruction file: Cursor, Codex and several
        // other agents auto-load it.
        push_rules_file(out.as_mut(), &d.join("AGENTS.md"), vec!["cursor", "codex"]);
        push_rules_file(out.as_mut(), &d.join("CLAUDE.md"), vec!["claude"]);
        push_rules_file(out.as_mut(), &d.join("CODEBUDDY.md"), vec!["codebuddy"]);
        push_rules_file(out.as_mut(), &d.join("GEMINI.md"), vec!["gemini"]);

        if d == *home {
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }

    // The parent walk can reach directories the global scan already covered
    // (e.g. ~/.cursor/rules when the walk ends at home) — count each file once.
    let mut seen = std::collections::HashSet::new();
    out.retain(|f| seen.insert(f.path.clone()));

    out
}

/// Clients that auto-load more than one file carrying a *full* lean-ctx
/// payload — the same guidance billed multiple times per session (#578/#684).
///
/// Pointer-only files (a thinned `AGENTS.md` / `.cursorrules` that merely
/// cross-references the canonical source) are not counted: they exist precisely
/// to avoid duplication and cost only a handful of tokens (#684).
pub(crate) fn duplicate_clients(files: &[RulesFileCost]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    for f in files {
        if f.lean_ctx_tokens == 0 || !f.carries_full {
            continue;
        }
        for c in &f.clients {
            *counts.entry(c).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(c, n)| (c.to_string(), n))
        .collect()
}

#[must_use]
pub(super) fn measure(home: &Path, project: &Path) -> OverheadReport {
    let cfg = crate::core::config::Config::load();
    let advertised = crate::server::tool_visibility::advertised_tool_defs_default();
    let lean_default = crate::tool_defs::lazy_tool_defs();

    let instructions = crate::instructions::build_instructions(crate::tools::CrpMode::effective());

    let rules_files = collect_rules_files(home, project);
    let duplicates = duplicate_clients(&rules_files);

    let pinned = crate::server::tool_visibility::explicit_profile(&cfg);
    let tool_profile = if pinned {
        cfg.tool_profile_effective().as_str().to_string()
    } else {
        "lean (default)".to_string()
    };

    OverheadReport {
        tool_count: advertised.len(),
        tool_schema_tokens: advertised.iter().map(tool_tokens).sum(),
        lean_default_tool_count: lean_default.len(),
        lean_default_tool_tokens: lean_default.iter().map(tool_tokens).sum(),
        tool_profile,
        instruction_tokens: count_tokens(&instructions),
        rules_files,
        duplicate_clients: duplicates,
    }
}

pub(super) fn run_overhead(json: bool) -> i32 {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    let project = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let report = measure(&home, &project);

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("doctor overhead: JSON serialization failed: {e}");
                return 2;
            }
        }
        return 0;
    }

    println!("{BOLD}Fixed context overhead per session{RST}");
    println!("{DIM}What every session pays before any compression saves a token.{RST}\n");

    // 1. Tool schemas
    println!(
        "  {BOLD}MCP tool schemas{RST}      {:>6} tok  {DIM}({} tools advertised, profile: {}){RST}",
        report.tool_schema_tokens, report.tool_count, report.tool_profile
    );
    if report.tool_count > report.lean_default_tool_count {
        let saving = report
            .tool_schema_tokens
            .saturating_sub(report.lean_default_tool_tokens);
        println!(
            "  {YELLOW}→ lean default advertises {} tools ({} tok) — `lean-ctx tools lean` saves ~{saving} tok/session{RST}",
            report.lean_default_tool_count, report.lean_default_tool_tokens
        );
    }

    // 2. Instructions
    println!(
        "  {BOLD}MCP instructions{RST}      {:>6} tok",
        report.instruction_tokens
    );

    // 3. Rules files
    println!(
        "  {BOLD}Rules files{RST}           {:>6} tok  {DIM}({} auto-loaded files){RST}",
        report.rules_tokens_total(),
        report.rules_files.len()
    );
    for f in &report.rules_files {
        let ours = if f.lean_ctx_tokens == 0 {
            String::new()
        } else if f.carries_full {
            format!(", {} tok lean-ctx", f.lean_ctx_tokens)
        } else {
            format!(", {} tok pointer", f.lean_ctx_tokens)
        };
        println!(
            "    {DIM}{:<58}{RST} {:>6} tok  {DIM}[{}{}]{RST}",
            shorten(&f.path, 58),
            f.file_tokens,
            f.clients.join("+"),
            ours
        );
    }

    if !report.duplicate_clients.is_empty() {
        println!();
        for (client, n) in &report.duplicate_clients {
            println!(
                "  {YELLOW}⚠ {client}: {n} files contain lean-ctx rules — the same guidance is billed {n}× per session.{RST}"
            );
        }
        println!(
            "  {DIM}Fix: `lean-ctx rules dedup --apply` keeps one canonical source per client (#578).{RST}"
        );
    }

    println!();
    let total = report.total_tokens();
    let color = if total > 8000 { YELLOW } else { GREEN };
    println!("  {BOLD}Total fixed cost{RST}      {color}{total:>6} tok / session{RST}");
    println!(
        "  {DIM}With provider prompt caching, repeated turns re-bill this at ~10% — but only if the prefix stays byte-stable.{RST}"
    );

    0
}

fn shorten(path: &str, max: usize) -> String {
    if path.len() <= max {
        return path.to_string();
    }
    let tail: String = path
        .chars()
        .rev()
        .take(max.saturating_sub(1))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rules_canonical::START_MARK;

    #[test]
    fn block_tokens_counts_only_marked_regions() {
        let content = format!(
            "\
# Some user rules
custom stuff here

{}
## lean-ctx
Prefer ctx_read over Read.
{}

more user stuff that is not ours
",
            crate::core::rules_canonical::START_MARK,
            crate::core::rules_canonical::END_MARK,
        );
        let ours = lean_ctx_block_tokens(&content);
        assert!(ours > 0, "must count the marked block");
        assert!(
            ours < count_tokens(&content),
            "must not count unmarked user content"
        );
    }

    #[test]
    fn block_tokens_zero_without_markers() {
        assert_eq!(lean_ctx_block_tokens("just user rules\nno markers"), 0);
    }

    #[test]
    fn block_tokens_handles_canonical_marker_without_end() {
        // Dedicated files start with the canonical header and the whole
        // remainder counts as lean-ctx content.
        let content = format!("{START_MARK}\n<!-- version: 1 -->\n\nrule body\nmore rules\n");
        assert!(lean_ctx_block_tokens(&content) > 0);
    }

    #[test]
    fn duplicates_flag_clients_with_multiple_lean_ctx_sources() {
        let files = vec![
            RulesFileCost {
                path: "a/.cursorrules".into(),
                file_tokens: 100,
                lean_ctx_tokens: 50,
                carries_full: true,
                clients: vec!["cursor"],
            },
            RulesFileCost {
                path: "a/.cursor/rules/lean-ctx.mdc".into(),
                file_tokens: 200,
                lean_ctx_tokens: 200,
                carries_full: true,
                clients: vec!["cursor"],
            },
            RulesFileCost {
                path: "a/CLAUDE.md".into(),
                file_tokens: 80,
                lean_ctx_tokens: 40,
                carries_full: true,
                clients: vec!["claude"],
            },
        ];
        let dups = duplicate_clients(&files);
        assert_eq!(dups, vec![("cursor".to_string(), 2)]);
    }

    #[test]
    fn duplicates_ignore_files_without_lean_ctx_content() {
        let files = vec![
            RulesFileCost {
                path: "a/.cursorrules".into(),
                file_tokens: 100,
                lean_ctx_tokens: 0,
                carries_full: false,
                clients: vec!["cursor"],
            },
            RulesFileCost {
                path: "a/.cursor/rules/user.mdc".into(),
                file_tokens: 200,
                lean_ctx_tokens: 0,
                carries_full: false,
                clients: vec!["cursor"],
            },
        ];
        assert!(duplicate_clients(&files).is_empty());
    }

    #[test]
    fn duplicates_ignore_pointer_only_files() {
        // #684: a thinned AGENTS.md keeps the `<!-- lean-ctx -->` pointer (so
        // lean_ctx_tokens > 0) but is not a second full source — Cursor's only
        // full carrier is the global mdc, so there is no duplication.
        let files = vec![
            RulesFileCost {
                path: "a/.cursor/rules/lean-ctx.mdc".into(),
                file_tokens: 200,
                lean_ctx_tokens: 200,
                carries_full: true,
                clients: vec!["cursor"],
            },
            RulesFileCost {
                path: "a/AGENTS.md".into(),
                file_tokens: 120,
                lean_ctx_tokens: 60,
                carries_full: false,
                clients: vec!["cursor", "codex"],
            },
        ];
        assert!(
            duplicate_clients(&files).is_empty(),
            "pointer-only AGENTS.md must not count as a duplicate source"
        );
    }

    #[test]
    fn collect_walks_parent_chain_and_dedups_nothing_silently() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let project = home.join("projects/app");
        std::fs::create_dir_all(project.join(".cursor/rules")).unwrap();
        std::fs::create_dir_all(home.join("projects/.cursor/rules")).unwrap();

        std::fs::write(
            project.join(".cursor/rules/lean-ctx.mdc"),
            format!("{START_MARK}\n<!-- version: 1 -->\n\nbody\n"),
        )
        .unwrap();
        std::fs::write(
            home.join("projects/.cursor/rules/lean-ctx.mdc"),
            format!("{START_MARK}\n<!-- version: 1 -->\n\nbody\n"),
        )
        .unwrap();
        std::fs::write(
            project.join("AGENTS.md"),
            format!(
                "{}\nx\n{}\n",
                crate::core::rules_canonical::AGENTS_BLOCK_START,
                crate::core::rules_canonical::AGENTS_BLOCK_END,
            ),
        )
        .unwrap();

        let files = collect_rules_files(home, &project);
        assert_eq!(
            files.len(),
            3,
            "project mdc + parent mdc + AGENTS.md: {files:?}"
        );

        // The two mdc files are full carriers; the AGENTS.md here holds only the
        // `<!-- lean-ctx -->` pointer, so it is NOT counted as a third source
        // (#684 — pointers cross-reference, they do not duplicate).
        let dups = duplicate_clients(&files);
        assert!(
            dups.iter().any(|(c, n)| c == "cursor" && *n == 2),
            "cursor loads 2 full lean-ctx sources (pointer AGENTS.md excluded): {dups:?}"
        );
    }

    #[test]
    fn collect_counts_each_file_once_when_walk_overlaps_globals() {
        // Project directly under home: the parent walk ends AT home, whose
        // .cursor/rules the global scan already covered.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let project = home.join("app");
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            format!("{START_MARK}\n<!-- version: 1 -->\n\nbody\n"),
        )
        .unwrap();

        let files = collect_rules_files(home, &project);
        let global_count = files
            .iter()
            .filter(|f| f.path.ends_with("lean-ctx.mdc"))
            .count();
        assert_eq!(
            global_count, 1,
            "global mdc must be counted once: {files:?}"
        );
    }
}
