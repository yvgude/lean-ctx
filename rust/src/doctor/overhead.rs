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
//!
//! The rules-file enumeration lives in [`crate::core::rules_overhead`] so the
//! `lean-ctx tools health` report (#848) can reuse the exact same accounting.

use std::path::PathBuf;

use crate::core::context_overhead::tool_tokens;
use crate::core::rules_overhead::{
    DuplicateChannel, RulesFileCost, collect_rules_files, duplicate_channels, duplicate_clients,
};
use crate::core::tokens::count_tokens;

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RST: &str = "\x1b[0m";

#[derive(Debug, serde::Serialize)]
pub(super) struct OverheadReport {
    pub tool_count: usize,
    pub tool_schema_tokens: usize,
    pub lean_default_tool_count: usize,
    pub lean_default_tool_tokens: usize,
    pub tool_profile: String,
    pub instruction_tokens: usize,
    /// Tokens of the wakeup briefing (knowledge + session memory). With the
    /// default `minimal_overhead = true` it is delivered via the first tool
    /// call's AUTO CONTEXT block (instructions stay byte-stable, #498); with
    /// `false` it is injected at session start. Either way it bills once per
    /// session — its own source, not folded into `instruction_tokens` (#964).
    pub wakeup_tokens: usize,
    pub rules_files: Vec<RulesFileCost>,
    pub duplicate_clients: Vec<(String, usize)>,
    /// Hosts that receive the lean-ctx rules block through more than one
    /// channel (rules files and/or the full MCP instructions skeleton, A3).
    pub duplicate_channels: Vec<DuplicateChannel>,
    /// Configured budget (`[context] budget_tokens`); 0 disables the check (#964).
    pub budget_tokens: usize,
    /// Whether `total_tokens` exceeds a non-zero `budget_tokens` (#964).
    pub over_budget: bool,
    /// `default`, or the installed Claude Code governor arm (`governor`,
    /// `governor-mcp`), whose prefix is measured instead of the default one.
    pub setup: String,
    /// Governor arms: the managed rule block (the only rules text they add).
    pub governor_rules_tokens: usize,
}

impl OverheadReport {
    fn rules_tokens_total(&self) -> usize {
        self.rules_files.iter().map(|r| r.file_tokens).sum()
    }

    fn total_tokens(&self) -> usize {
        self.tool_schema_tokens
            + self.instruction_tokens
            + self.wakeup_tokens
            + self.rules_tokens_total()
            + self.governor_rules_tokens
    }
}

/// The installed governor setup's prefix: rule block only (`governor`), or
/// rule block plus deferred tool names (`governor-mcp`). No instructions, no
/// wakeup briefing (its hooks emit no session text), no other rules files.
fn governor_report(
    arm: crate::hooks::agents::claude_governor::GovernorArm,
    budget_tokens: usize,
) -> OverheadReport {
    let o = crate::core::context_overhead::ContextOverhead::measure_governor(arm);
    let lean_default = crate::tool_defs::lazy_tool_defs();
    let mut report = OverheadReport {
        tool_count: o.tool_count,
        tool_schema_tokens: o.tool_schema_tokens,
        lean_default_tool_count: lean_default.len(),
        lean_default_tool_tokens: lean_default.iter().map(tool_tokens).sum(),
        tool_profile: match arm {
            crate::hooks::agents::claude_governor::GovernorArm::NoMcp => "governor (no MCP server)",
            crate::hooks::agents::claude_governor::GovernorArm::McpDeferred => {
                "governor-mcp (deferred names only)"
            }
        }
        .to_string(),
        instruction_tokens: o.instruction_tokens,
        wakeup_tokens: 0,
        rules_files: Vec::new(),
        duplicate_clients: Vec::new(),
        // The governor block is the arm's only rules channel by construction.
        duplicate_channels: Vec::new(),
        budget_tokens,
        over_budget: false,
        setup: arm.label().to_string(),
        governor_rules_tokens: o.rules_block_tokens,
    };
    report.over_budget = budget_tokens > 0 && report.total_tokens() > budget_tokens;
    report
}

#[must_use]
pub(super) fn measure(home: &std::path::Path, project: &std::path::Path) -> OverheadReport {
    let cfg = crate::core::config::Config::load();
    if let Some(arm) = crate::hooks::agents::claude_governor::installed_arm(home) {
        return governor_report(arm, cfg.context_budget_tokens_effective());
    }
    let advertised = crate::server::tool_visibility::advertised_tool_defs_default();
    let lean_default = crate::tool_defs::lazy_tool_defs();

    let instructions = crate::instructions::build_instructions(crate::tools::CrpMode::effective());

    let rules_files = collect_rules_files(home, project);
    let duplicates = duplicate_clients(&rules_files);
    let channels = duplicate_channels(home, &rules_files, &|c| client_instruction_tokens(home, c));

    let pinned = crate::server::tool_visibility::explicit_profile(&cfg);
    let tool_profile = if pinned {
        cfg.tool_profile_effective().as_str().to_string()
    } else {
        "lean (default)".to_string()
    };

    // The wakeup briefing rides session start as its own source (#964). It reads
    // live knowledge + session state, so it reflects what this install actually
    // re-injects rather than a static estimate.
    let wakeup =
        crate::tools::ctx_overview::build_wakeup_briefing(&project.to_string_lossy(), None);

    let budget_tokens = cfg.context_budget_tokens_effective();

    let mut report = OverheadReport {
        tool_count: advertised.len(),
        tool_schema_tokens: advertised.iter().map(tool_tokens).sum(),
        lean_default_tool_count: lean_default.len(),
        lean_default_tool_tokens: lean_default.iter().map(tool_tokens).sum(),
        tool_profile,
        instruction_tokens: count_tokens(&instructions),
        wakeup_tokens: count_tokens(&wakeup),
        rules_files,
        duplicate_clients: duplicates,
        duplicate_channels: channels,
        budget_tokens,
        over_budget: false,
        setup: "default".to_string(),
        governor_rules_tokens: 0,
    };
    report.over_budget = budget_tokens > 0 && report.total_tokens() > budget_tokens;
    report
}

pub(super) fn run_overhead(json: bool, gate: bool) -> i32 {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    let project = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let report = measure(&home, &project);

    // `--gate` turns a budget breach into a non-zero exit for CI (#964); without
    // it the report is purely informational regardless of the breach.
    let exit_code = i32::from(gate && report.over_budget);

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("doctor overhead: JSON serialization failed: {e}");
                return 2;
            }
        }
        return exit_code;
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

    // 3. Wakeup briefing (knowledge + session memory re-injected on session start)
    println!(
        "  {BOLD}Wakeup briefing{RST}       {:>6} tok  {DIM}(knowledge + session memory){RST}",
        report.wakeup_tokens
    );

    if report.governor_rules_tokens > 0 {
        println!(
            "  {BOLD}Governor rule block{RST}   {:>6} tok  {DIM}(setup: {}){RST}",
            report.governor_rules_tokens, report.setup
        );
    }

    // 4. Rules files
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

    if !report.duplicate_channels.is_empty() {
        println!();
        for line in duplicate_channel_lines(&report.duplicate_channels) {
            println!("  {YELLOW}{line}{RST}");
        }
        println!("  {DIM}{CHANNEL_FIX_HINT}{RST}");
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
    let color = if report.over_budget { YELLOW } else { GREEN };
    println!("  {BOLD}Total fixed cost{RST}      {color}{total:>6} tok / session{RST}");
    if report.budget_tokens > 0 {
        if report.over_budget {
            // Machine-readable so CI/log scrapers can key on a stable token (#964).
            println!(
                "  {YELLOW}⚠ OVER_BUDGET: {total} tok > budget {} tok — trim tools/rules or raise [context] budget_tokens.{RST}",
                report.budget_tokens
            );
        } else {
            println!(
                "  {DIM}Within budget ({} / {} tok).{RST}",
                total, report.budget_tokens
            );
        }
    }
    println!(
        "  {DIM}With provider prompt caching, repeated turns re-bill this at ~10% — but only if the prefix stays byte-stable.{RST}"
    );

    exit_code
}

/// Tokens of the MCP instructions the named client receives right now; 0 for
/// a host without a lean-ctx MCP entry, which receives no instructions at all.
fn client_instruction_tokens(home: &std::path::Path, client_name: &str) -> usize {
    let location = match client_name {
        "claude-code" => "Claude Code",
        "cursor" => "Cursor",
        "codex" => "Codex",
        "gemini-cli" => "Gemini CLI",
        "opencode" => "OpenCode",
        "codebuddy" => "CodeBuddy",
        _ => return 0,
    };
    if !super::common::lean_ctx_mcp_location_names(home).contains(location) {
        return 0;
    }
    count_tokens(&crate::instructions::build_instructions_with_client(
        crate::tools::CrpMode::effective(),
        client_name,
    ))
}

/// Repair command for duplicate rules channels. `setup` and `doctor --fix`
/// run the same dedup automatically.
const CHANNEL_FIX_HINT: &str =
    "fix: `lean-ctx rules dedup --apply` (also run by `lean-ctx setup` / `lean-ctx doctor --fix`)";

fn duplicate_channel_lines(dups: &[DuplicateChannel]) -> Vec<String> {
    dups.iter()
        .map(|d| {
            let mut sources: Vec<String> = d
                .files
                .iter()
                .map(|(path, tok)| format!("{} (~{tok} tok)", tildify(path)))
                .collect();
            if d.instruction_tokens > 0 {
                sources.push(format!("MCP instructions (~{} tok)", d.instruction_tokens));
            }
            format!(
                "⚠ {}: lean-ctx rules arrive via {} channels — {}; ~{} tok/session duplicated",
                d.client,
                d.channel_count(),
                sources.join(", "),
                d.excess_tokens()
            )
        })
        .collect()
}

fn tildify(path: &str) -> String {
    match dirs::home_dir() {
        Some(home) => {
            let home = home.to_string_lossy();
            path.strip_prefix(home.as_ref())
                .map_or_else(|| path.to_string(), |rest| format!("~{rest}"))
        }
        None => path.to_string(),
    }
}

/// `lean-ctx doctor` board line (A3): exactly one lean-ctx rules channel per
/// host. Advisory (never fails the gate) — duplicates cost tokens, they do not
/// break anything.
pub(super) fn rules_channels_outcome() -> super::Outcome {
    use super::{BOLD, DIM, GREEN, RST, YELLOW};
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    let project = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let files = collect_rules_files(&home, &project);
    let dups = duplicate_channels(&home, &files, &|c| client_instruction_tokens(&home, c));
    let line = if dups.is_empty() {
        format!("{BOLD}Rules channels{RST}  {GREEN}one lean-ctx rules channel per host{RST}")
    } else {
        format!(
            "{BOLD}Rules channels{RST}  {YELLOW}{}{RST}  {DIM}({CHANNEL_FIX_HINT}; details: lean-ctx doctor overhead){RST}",
            duplicate_channel_lines(&dups).join("; ")
        )
    };
    super::Outcome { ok: true, line }
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

    fn report(schema: usize, instr: usize, wakeup: usize, budget: usize) -> OverheadReport {
        OverheadReport {
            tool_count: 1,
            tool_schema_tokens: schema,
            lean_default_tool_count: 1,
            lean_default_tool_tokens: schema,
            tool_profile: "lean (default)".into(),
            instruction_tokens: instr,
            wakeup_tokens: wakeup,
            rules_files: Vec::new(),
            duplicate_clients: Vec::new(),
            duplicate_channels: Vec::new(),
            budget_tokens: budget,
            over_budget: false,
            setup: "default".into(),
            governor_rules_tokens: 0,
        }
    }

    #[test]
    fn governor_install_reports_its_own_prefix() {
        use crate::hooks::agents::claude_governor::{GovernorArm, install};
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("CLAUDE_CONFIG_DIR");
        let default_total = {
            let home = tempfile::tempdir().unwrap();
            measure(home.path(), home.path()).total_tokens()
        };
        for (arm, label, tools) in [
            (GovernorArm::NoMcp, "governor", false),
            (GovernorArm::McpDeferred, "governor-mcp", true),
        ] {
            let home = tempfile::tempdir().unwrap();
            install(home.path(), arm);
            let r = measure(home.path(), home.path());
            let expected = crate::core::context_overhead::ContextOverhead::measure_governor(arm);
            assert_eq!(r.setup, label);
            assert_eq!(r.instruction_tokens, 0, "{label}: no instructions");
            assert_eq!(r.wakeup_tokens, 0);
            assert!(r.rules_files.is_empty());
            assert_eq!(r.governor_rules_tokens, expected.rules_block_tokens);
            assert_eq!(r.tool_count > 0, tools, "{label}: deferred tool names");
            assert_eq!(r.total_tokens(), expected.total_tokens());
            assert!(r.total_tokens() * 10 < default_total, "{label} ≪ default");
        }
    }

    #[test]
    fn total_includes_wakeup_source() {
        // #964: the wakeup briefing is its own fourth source in the total.
        let r = report(100, 200, 50, 0);
        assert_eq!(r.total_tokens(), 350);
    }

    #[test]
    fn over_budget_threshold_is_strict() {
        // Mirrors the check in `measure`: breach only above a non-zero budget.
        let mut r = report(100, 200, 50, 300);
        r.over_budget = r.budget_tokens > 0 && r.total_tokens() > r.budget_tokens;
        assert!(r.over_budget, "350 > 300 must breach");

        let mut under = report(100, 200, 50, 8000);
        under.over_budget = under.budget_tokens > 0 && under.total_tokens() > under.budget_tokens;
        assert!(!under.over_budget, "350 < 8000 is within budget");

        let mut disabled = report(100, 200, 50, 0);
        disabled.over_budget =
            disabled.budget_tokens > 0 && disabled.total_tokens() > disabled.budget_tokens;
        assert!(!disabled.over_budget, "budget 0 disables the check");
    }

    /// A3: a host without a lean-ctx MCP entry receives no instructions, so a
    /// stale rules file there must not count as a second (instructions) channel.
    #[test]
    fn instructions_channel_needs_a_lean_ctx_mcp_entry() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert_eq!(client_instruction_tokens(home, "cursor"), 0);
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        std::fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx"}}}"#,
        )
        .unwrap();
        assert!(client_instruction_tokens(home, "cursor") > 0);
    }
}
