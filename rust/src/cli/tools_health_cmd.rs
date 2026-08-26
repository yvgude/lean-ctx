//! `lean-ctx tools health` — token-budget & rot report for MCP tools, injected
//! rules, and stored knowledge (#848).
//!
//! Renders [`crate::core::tool_health`]. Text by default (only rot candidates,
//! `--all` for every tool); `--json` emits the full deterministic report for the
//! dashboard / scripting.

use std::path::PathBuf;

use crate::core::tool_health::{ToolHealthReport, ToolStatus};

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const RST: &str = "\x1b[0m";

pub(crate) fn cmd_tools_health(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");
    let show_all = args.iter().any(|a| a == "--all");

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    let project = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let report = crate::core::tool_health::compute(&home, &project);

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("tools health: JSON serialization failed: {e}");
                std::process::exit(2);
            }
        }
        return;
    }

    print_report(&report, show_all);
}

fn print_report(r: &ToolHealthReport, show_all: bool) {
    println!("{BOLD}Tool & rule budget — does every token earn its place?{RST}");
    println!(
        "{DIM}Profile: {} · {} advertised tools · cross-references fixed cost with recorded usage (#848){RST}\n",
        r.tool_profile, r.advertised_tools
    );

    // ── Fixed cost ────────────────────────────────────────────────────
    println!("{BOLD}Fixed cost / session{RST}");
    println!("  MCP tool schemas   {:>6} tok", r.tool_schema_tokens);
    println!("  MCP instructions   {:>6} tok", r.instruction_tokens);
    println!("  Rules files        {:>6} tok", r.rules_tokens);
    let color = if r.fixed_total_tokens > 8000 {
        YELLOW
    } else {
        GREEN
    };
    println!(
        "  {BOLD}Total fixed        {color}{:>6} tok{RST}",
        r.fixed_total_tokens
    );

    // ── Usage ─────────────────────────────────────────────────────────
    println!();
    if r.has_usage_data {
        println!(
            "{BOLD}Usage{RST}  {} recorded tool calls",
            r.total_recorded_calls
        );

        // Rot candidates: unused first (heaviest schema first), then low-use.
        let mut unused: Vec<_> = r
            .tools
            .iter()
            .filter(|t| t.status == ToolStatus::Unused)
            .collect();
        unused.sort_by(|a, b| {
            b.schema_tokens
                .cmp(&a.schema_tokens)
                .then(a.name.cmp(&b.name))
        });

        let low_use: Vec<_> = r
            .tools
            .iter()
            .filter(|t| t.status == ToolStatus::LowUse)
            .collect();

        if unused.is_empty() && low_use.is_empty() {
            println!("  {GREEN}✓ no rot — every advertised tool was used at least once{RST}");
        } else {
            if !unused.is_empty() {
                println!(
                    "\n  {RED}{} unused tool(s){RST} cost {RED}{}{RST} tok every session:",
                    r.unused_tools, r.unused_tool_tokens
                );
                for t in &unused {
                    println!(
                        "    {:<22} {:>5} tok  {DIM}never called{RST}",
                        t.name, t.schema_tokens
                    );
                }
                println!(
                    "  {DIM}→ `lean-ctx tools lean` advertises only the lazy core; trimmed tools stay callable via ctx_call.{RST}"
                );
            }
            if !low_use.is_empty() {
                println!(
                    "\n  {YELLOW}{} low-use tool(s){RST} {DIM}(heavy schema, <1% of calls):{RST}",
                    low_use.len()
                );
                for t in &low_use {
                    println!(
                        "    {:<22} {:>5} tok  {DIM}{}× calls{RST}",
                        t.name, t.schema_tokens, t.calls
                    );
                }
            }
        }
    } else {
        println!(
            "{YELLOW}No usage telemetry yet{RST} {DIM}— run lean-ctx-backed sessions to populate per-tool usage, then rerun for rot detection.{RST}"
        );
    }

    // ── Value recommendation (#961) ───────────────────────────────────
    if !r.disable_action.is_empty() {
        println!("\n  {YELLOW}→ {}{RST}", r.disable_action);
    }
    if let Some(note) = &r.footprint_note {
        println!("  {DIM}{note}{RST}");
    } else {
        println!(
            "  {DIM}→ run `lean-ctx eval footprint --suite <f>` to prove the tool-schema element earns its tokens.{RST}"
        );
    }

    if show_all {
        println!(
            "\n{BOLD}All advertised tools{RST} {DIM}(name · schema tok · calls · value/1k · status){RST}"
        );
        for t in &r.tools {
            let last = t
                .last_used
                .as_deref()
                .map_or_else(|| "—".to_string(), |s| s.get(..10).unwrap_or(s).to_string());
            println!(
                "  {:<22} {:>5} tok  {:>6} calls  {:>7.1}  {:<8} {DIM}{}{RST}",
                t.name,
                t.schema_tokens,
                t.calls,
                t.value_per_1k_tokens,
                t.status.label(),
                last
            );
        }
    }

    // ── Rules ─────────────────────────────────────────────────────────
    let cross_layer_rules: Vec<_> = r
        .rules
        .iter()
        .filter(|e| !e.action.is_empty() && !e.action.starts_with("duplicate"))
        .collect();
    if !r.duplicate_clients.is_empty() || !cross_layer_rules.is_empty() {
        println!("\n{BOLD}Rules{RST}");
        for (client, n) in &r.duplicate_clients {
            println!(
                "  {YELLOW}⚠ {client}: {n} files carry full lean-ctx rules — billed {n}× per session{RST}"
            );
        }
        for e in &cross_layer_rules {
            println!("  {YELLOW}⚠ {}{RST}\n    {DIM}{}{RST}", e.path, e.action);
        }
        println!(
            "  {DIM}→ `lean-ctx rules dedup --apply` keeps one canonical source per client.{RST}"
        );
    }

    // ── Foreign MCP servers ───────────────────────────────────────────
    if !r.foreign_servers.is_empty() {
        println!(
            "\n{BOLD}Foreign MCP servers{RST}  {DIM}(their schemas load every session; size not measurable locally){RST}"
        );
        for s in &r.foreign_servers {
            if s.action.is_empty() {
                println!(
                    "  {GREEN}✓{RST} {:<24} {:>5} calls in {} session(s)  {DIM}{}{RST}",
                    s.name, s.calls, s.sessions_with_use, s.source
                );
            } else {
                println!(
                    "  {YELLOW}⚠ {:<24}{RST} {}  {DIM}{}{RST}",
                    s.name, s.action, s.source
                );
            }
        }
    }

    // ── Behavior (turn economy) ───────────────────────────────────────
    let b = &r.behavior;
    if b.total_mode_reads > 0 || b.chains_detected > 0 {
        println!("\n{BOLD}Behavior{RST}");
        if let Some(pct) = b.heavy_read_share_pct {
            let color = if pct >= 40 { YELLOW } else { GREEN };
            println!(
                "  explicit full/raw reads  {color}{pct}%{RST} of {} recorded read modes",
                b.total_mode_reads
            );
        }
        println!(
            "  chains vs compose        {} search→read→search chain(s) · {} ctx_compose call(s) · {} hint(s) shown",
            b.chains_detected, b.compose_calls, b.hints_shown
        );
        if !b.action.is_empty() {
            println!("  {YELLOW}→ {}{RST}", b.action);
        }
    }

    // ── Knowledge ─────────────────────────────────────────────────────
    if r.knowledge.total_facts > 0 {
        println!(
            "\n{BOLD}Knowledge{RST}  {} facts ({} active)",
            r.knowledge.total_facts, r.knowledge.active_facts
        );
        if r.knowledge.stale_facts > 0 {
            println!("  {YELLOW}{}{RST}", r.knowledge.action);
        } else {
            println!("  {GREEN}✓ no stale facts{RST}");
        }
    }

    println!(
        "\n{DIM}Deterministic, local-only. JSON: `lean-ctx tools health --json` · full list: `--all`.{RST}"
    );
}
