use crate::core::theme::{self, Theme};

use super::model::{CommandStats, CostModel, DayStats, StatsStore};

fn active_theme() -> Theme {
    let cfg = crate::core::config::Config::load();
    theme::load_theme(&cfg.theme)
}

fn format_usd(amount: f64) -> String {
    if amount >= 0.01 {
        format!("${amount:.2}")
    } else {
        format!("${amount:.3}")
    }
}

fn usd_estimate(tokens: u64) -> String {
    let env_model = std::env::var("LEAN_CTX_MODEL")
        .or_else(|_| std::env::var("LCTX_MODEL"))
        .ok();
    let pricing = crate::core::gain::model_pricing::ModelPricing::load();
    let quote = pricing.quote(env_model.as_deref());
    let cost = tokens as f64 * quote.cost.input_per_m / 1_000_000.0;
    format_usd(cost)
}

pub(super) fn format_pct_1dp(val: f64) -> String {
    if val == 0.0 {
        "0.0%".to_string()
    } else if val > 0.0 && val < 0.1 {
        "<0.1%".to_string()
    } else {
        format!("{val:.1}%")
    }
}

pub(super) fn format_savings_pct(saved: u64, input: u64) -> String {
    if input == 0 {
        if saved > 0 {
            return "n/a".to_string();
        }
        return "0.0%".to_string();
    }
    let rate = saved as f64 / input as f64 * 100.0;
    format_pct_1dp(rate)
}

fn format_big(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

fn format_num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        format!("{n}")
    }
}

fn truncate_cmd(cmd: &str, max: usize) -> String {
    if cmd.len() <= max {
        cmd.to_string()
    } else {
        format!("{}…", &cmd[..max - 1])
    }
}

fn cmd_total_saved(s: &CommandStats, _cm: &CostModel) -> u64 {
    s.input_tokens.saturating_sub(s.output_tokens)
}

fn day_total_saved(d: &DayStats, _cm: &CostModel) -> u64 {
    d.input_tokens.saturating_sub(d.output_tokens)
}

pub(super) fn normalize_command(command: &str) -> String {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return command.to_string();
    }

    let base = std::path::Path::new(parts[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(parts[0]);

    match base {
        "git" => {
            if parts.len() > 1 {
                format!("git {}", parts[1])
            } else {
                "git".to_string()
            }
        }
        "cargo" => {
            if parts.len() > 1 {
                format!("cargo {}", parts[1])
            } else {
                "cargo".to_string()
            }
        }
        "npm" | "yarn" | "pnpm" => {
            if parts.len() > 1 {
                format!("{} {}", base, parts[1])
            } else {
                base.to_string()
            }
        }
        "docker" => {
            if parts.len() > 1 {
                format!("docker {}", parts[1])
            } else {
                "docker".to_string()
            }
        }
        _ => base.to_string(),
    }
}

#[allow(clippy::many_single_char_names)] // ANSI formatting: t=theme, r=reset, b=bold, d=dim
fn format_cep_live(lv: &serde_json::Value, t: &Theme) -> String {
    let mut out = Vec::new();
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();

    let score = lv["cep_score"].as_u64().unwrap_or(0) as u32;
    let cache_util = lv["cache_utilization"].as_u64().unwrap_or(0);
    let mode_div = lv["mode_diversity"].as_u64().unwrap_or(0);
    let comp_rate = lv["compression_rate"].as_u64().unwrap_or(0);
    let tok_saved = lv["tokens_saved"].as_u64().unwrap_or(0);
    let tok_orig = lv["tokens_original"].as_u64().unwrap_or(0);
    let tool_calls = lv["tool_calls"].as_u64().unwrap_or(0);
    let cache_hits = lv["cache_hits"].as_u64().unwrap_or(0);
    let total_reads = lv["total_reads"].as_u64().unwrap_or(0);
    let complexity = lv["task_complexity"].as_str().unwrap_or("Standard");

    out.push(String::new());
    out.push(format!(
        "  {icon} {brand} {cep}  {dim}Live Session (no historical data yet){rst}",
        icon = t.header_icon(),
        brand = t.brand_title(),
        cep = t.section_title("CEP"),
    ));
    out.push(format!("  {ln}", ln = t.border_line(56)));
    out.push(String::new());

    let txt = t.text.fg();
    let sc = t.success.fg();
    let sec = t.secondary.fg();

    out.push(format!(
        "  {bold}{txt}CEP Score{rst}         {bold}{pc}{score:>3}/100{rst}",
        pc = t.pct_color(score as f64),
    ));
    out.push(format!(
        "  {bold}{txt}Cache Hit Rate{rst}    {bold}{pc}{cache_util}%{rst}  {dim}({cache_hits} hits / {total_reads} reads){rst}",
        pc = t.pct_color(cache_util as f64),
    ));
    out.push(format!(
        "  {bold}{txt}Mode Diversity{rst}    {bold}{pc}{mode_div}%{rst}",
        pc = t.pct_color(mode_div as f64),
    ));
    out.push(format!(
        "  {bold}{txt}Compression{rst}       {bold}{pc}{comp_rate}%{rst}  {dim}({} → {}){rst}",
        format_big(tok_orig),
        format_big(tok_orig.saturating_sub(tok_saved)),
        pc = t.pct_color(comp_rate as f64),
    ));
    out.push(format!(
        "  {bold}{txt}Tokens Saved{rst}      {bold}{sc}{}{rst}  {dim}(≈ {}){rst}",
        format_big(tok_saved),
        usd_estimate(tok_saved),
    ));
    out.push(format!(
        "  {bold}{txt}Tool Calls{rst}        {bold}{sec}{tool_calls}{rst}"
    ));
    out.push(format!(
        "  {bold}{txt}Complexity{rst}        {dim}{complexity}{rst}"
    ));
    out.push(String::new());
    out.push(format!("  {ln}", ln = t.border_line(56)));
    out.push(format!(
        "  {dim}This is live data from the current MCP session.{rst}"
    ));
    out.push(format!(
        "  {dim}Historical CEP trends appear after more sessions.{rst}"
    ));
    out.push(String::new());

    out.join("\n")
}

fn load_mcp_live() -> Option<serde_json::Value> {
    let path = dirs::home_dir()?.join(".lean-ctx/mcp-live.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Renders the full CEP (Cognitive Efficiency Protocol) report with themes.
#[allow(clippy::many_single_char_names)] // ANSI formatting: t=theme, r=reset, b=bold, d=dim
pub fn format_cep_report() -> String {
    let theme = active_theme();
    let store = super::load();
    let cep = &store.cep;
    let live = load_mcp_live();
    let mut out = Vec::new();
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();

    if cep.sessions == 0 && live.is_none() {
        return format!(
            "{dim}No CEP sessions recorded yet.{rst}\n\
             Use lean-ctx as an MCP server in your editor to start tracking.\n\
             CEP metrics are recorded automatically during MCP sessions."
        );
    }

    if cep.sessions == 0 {
        if let Some(ref lv) = live {
            return format_cep_live(lv, &theme);
        }
    }

    let total_saved = cep
        .total_tokens_original
        .saturating_sub(cep.total_tokens_compressed);
    let overall_compression = if cep.total_tokens_original > 0 {
        total_saved as f64 / cep.total_tokens_original as f64 * 100.0
    } else {
        0.0
    };
    let cache_hit_rate = if cep.total_cache_reads > 0 {
        cep.total_cache_hits as f64 / cep.total_cache_reads as f64 * 100.0
    } else {
        0.0
    };
    let avg_score = if cep.scores.is_empty() {
        0.0
    } else {
        cep.scores.iter().map(|s| s.score as f64).sum::<f64>() / cep.scores.len() as f64
    };
    let latest_score = cep.scores.last().map_or(0, |s| s.score);

    let shell_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens)
        .saturating_sub(total_saved);
    let total_all_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens);
    let cep_share = if total_all_saved > 0 {
        total_saved as f64 / total_all_saved as f64 * 100.0
    } else {
        0.0
    };

    let txt = theme.text.fg();
    let sc = theme.success.fg();
    let sec = theme.secondary.fg();
    let wrn = theme.warning.fg();

    out.push(String::new());
    out.push(format!(
        "  {icon} {brand} {cep}  {dim}Cognitive Efficiency Protocol Report{rst}",
        icon = theme.header_icon(),
        brand = theme.brand_title(),
        cep = theme.section_title("CEP"),
    ));
    out.push(format!("  {ln}", ln = theme.border_line(56)));
    out.push(String::new());

    out.push(format!(
        "  {bold}{txt}CEP Score{rst}         {bold}{pc}{:>3}/100{rst}  {dim}(avg: {avg_score:.0}, latest: {latest_score}){rst}",
        latest_score,
        pc = theme.pct_color(latest_score as f64),
    ));
    out.push(format!(
        "  {bold}{txt}Sessions{rst}          {bold}{sec}{}{rst}",
        cep.sessions
    ));
    out.push(format!(
        "  {bold}{txt}Cache Hit Rate{rst}    {bold}{pc}{:.1}%{rst}  {dim}({} hits / {} reads){rst}",
        cache_hit_rate,
        cep.total_cache_hits,
        cep.total_cache_reads,
        pc = theme.pct_color(cache_hit_rate),
    ));
    out.push(format!(
        "  {bold}{txt}MCP Compression{rst}   {bold}{pc}{:.1}%{rst}  {dim}({} → {}){rst}",
        overall_compression,
        format_big(cep.total_tokens_original),
        format_big(cep.total_tokens_compressed),
        pc = theme.pct_color(overall_compression),
    ));
    out.push(format!(
        "  {bold}{txt}Tokens Saved{rst}      {bold}{sc}{}{rst}  {dim}(≈ {}){rst}",
        format_big(total_saved),
        usd_estimate(total_saved),
    ));
    out.push(String::new());

    out.push(format!("  {}", theme.section_title("Savings Breakdown")));
    out.push(format!("  {ln}", ln = theme.border_line(56)));

    let bar_w = 30;
    let shell_ratio = if total_all_saved > 0 {
        shell_saved as f64 / total_all_saved as f64
    } else {
        0.0
    };
    let cep_ratio = if total_all_saved > 0 {
        total_saved as f64 / total_all_saved as f64
    } else {
        0.0
    };
    let m = theme.muted.fg();
    let shell_bar = theme::pad_right(&theme.gradient_bar(shell_ratio, bar_w), bar_w);
    let shell_pct_val = (1.0 - cep_share) * 100.0;
    let shell_pct_display = format_pct_1dp(shell_pct_val);
    out.push(format!(
        "  {m}Shell Hook{rst}   {shell_bar} {bold}{:>6}{rst} {dim}({shell_pct_display}){rst}",
        format_big(shell_saved),
    ));
    let cep_bar = theme::pad_right(&theme.gradient_bar(cep_ratio, bar_w), bar_w);
    let cep_pct_display = format_pct_1dp(cep_share * 100.0);
    out.push(format!(
        "  {m}MCP/CEP{rst}      {cep_bar} {bold}{:>6}{rst} {dim}({cep_pct_display}){rst}",
        format_big(total_saved),
    ));
    out.push(String::new());

    if total_saved == 0 && cep.modes.is_empty() {
        if store.total_commands > 20 {
            out.push(format!(
                "  {wrn}⚠  MCP tools configured but not being used by your AI client.{rst}"
            ));
            out.push(
                "     Your AI client may be using native Read/Shell instead of ctx_read/ctx_shell."
                    .to_string(),
            );
            out.push(format!(
                "     Run {sec}lean-ctx init{rst} to update rules, then restart your AI session."
            ));
            out.push(format!(
                "     Run {sec}lean-ctx doctor{rst} for detailed adoption diagnostics."
            ));
        } else {
            out.push(format!(
                "  {wrn}⚠  MCP server not configured.{rst} Shell hook compresses output, but"
            ));
            out.push(
                "     full token savings require MCP tools (ctx_read, ctx_shell, ctx_search)."
                    .to_string(),
            );
            out.push(format!(
                "     Run {sec}lean-ctx setup{rst} to auto-configure your editors."
            ));
        }
        out.push(String::new());
    }

    if !cep.modes.is_empty() {
        out.push(format!("  {}", theme.section_title("Read Modes Used")));
        out.push(format!("  {ln}", ln = theme.border_line(56)));

        let mut sorted_modes: Vec<_> = cep.modes.iter().collect();
        sorted_modes.sort_by(|a, b2| b2.1.cmp(a.1));
        let max_mode = *sorted_modes.first().map_or(&1, |(_, c)| *c);
        let max_mode = max_mode.max(1);

        for (mode, count) in &sorted_modes {
            let ratio = **count as f64 / max_mode as f64;
            let bar = theme::pad_right(&theme.gradient_bar(ratio, 20), 20);
            out.push(format!("  {sec}{mode:<14}{rst} {count:>4}x  {bar}"));
        }

        let total_mode_calls: u64 = sorted_modes.iter().map(|(_, c)| **c).sum();
        let full_count = cep.modes.get("full").copied().unwrap_or(0);
        let optimized = total_mode_calls.saturating_sub(full_count);
        let opt_pct = if total_mode_calls > 0 {
            optimized as f64 / total_mode_calls as f64 * 100.0
        } else {
            0.0
        };
        out.push(format!(
            "  {dim}{optimized}/{total_mode_calls} reads used optimized modes ({opt_pct:.0}% non-full){rst}"
        ));
    }

    if cep.scores.len() >= 2 {
        out.push(String::new());
        out.push(format!("  {}", theme.section_title("CEP Score Trend")));
        out.push(format!("  {ln}", ln = theme.border_line(56)));

        let score_values: Vec<u64> = cep.scores.iter().map(|s| s.score as u64).collect();
        let spark = theme.gradient_sparkline(&score_values);
        out.push(format!("  {spark}"));

        let recent: Vec<_> = cep.scores.iter().rev().take(5).collect();
        for snap in recent.iter().rev() {
            let ts = snap.timestamp.get(..16).unwrap_or(&snap.timestamp);
            let pc = theme.pct_color(snap.score as f64);
            out.push(format!(
                "  {m}{ts}{rst}  {pc}{bold}{:>3}{rst}/100  cache:{:>3}%  modes:{:>3}%  {dim}{}{rst}",
                snap.score, snap.cache_hit_rate, snap.mode_diversity, snap.complexity,
            ));
        }
    }

    out.push(String::new());
    out.push(format!("  {ln}", ln = theme.border_line(56)));
    out.push(format!("  {dim}Improve your CEP score:{rst}"));
    if cache_hit_rate < 50.0 {
        out.push(format!(
            "    {wrn}↑{rst} Re-read files with ctx_read to leverage caching"
        ));
    }
    let modes_count = cep.modes.len();
    if modes_count < 3 {
        out.push(format!(
            "    {wrn}↑{rst} Use map/signatures modes for context-only files"
        ));
    }
    if avg_score >= 70.0 {
        out.push(format!(
            "    {sc}✓{rst} Great score! You're using lean-ctx effectively"
        ));
    }
    out.push(String::new());

    out.join("\n")
}

/// Renders the token savings dashboard using the active theme.
pub fn format_gain() -> String {
    format_gain_themed(&active_theme())
}

/// Renders the token savings dashboard with a specific theme.
pub fn format_gain_themed(t: &Theme) -> String {
    format_gain_themed_at(t, None)
}

/// Renders the token savings dashboard at a specific animation tick.
#[allow(clippy::many_single_char_names)] // ANSI formatting: t=theme, r=reset, b=bold, d=dim
pub fn format_gain_themed_at(t: &Theme, tick: Option<u64>) -> String {
    let store = super::load();
    let mut out = Vec::new();
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();

    if store.total_commands == 0 {
        return format!(
            "{dim}No commands recorded yet.{rst} Use {cmd}lean-ctx -c \"command\"{rst} to start tracking.",
            cmd = t.secondary.fg(),
        );
    }

    let input_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens);
    let pct = if store.total_input_tokens > 0 {
        input_saved as f64 / store.total_input_tokens as f64 * 100.0
    } else {
        0.0
    };
    let cost_model = CostModel::default();
    let cost = cost_model.calculate(&store);
    let total_saved = input_saved;
    let days_active = store.daily.len();

    let w = 62;
    let side = t.box_side();

    let box_line = |content: &str| -> String {
        let padded = theme::pad_right(content, w);
        format!("  {side}{padded}{side}")
    };

    out.push(String::new());
    out.push(format!("  {}", t.box_top(w)));
    out.push(box_line(""));

    let header = format!(
        "    {icon}  {bold}{title}{rst}   {dim}Token Savings Dashboard{rst}",
        icon = t.header_icon(),
        title = t.brand_title(),
    );
    out.push(box_line(&header));
    out.push(box_line(""));
    out.push(format!("  {}", t.box_mid(w)));
    out.push(box_line(""));

    let tok_val = format_big(total_saved);
    let pct_val = format!("{pct:.1}%");
    let cmd_val = format_num(store.total_commands);
    let usd_val = format_usd(cost.total_saved);

    let c1 = t.success.fg();
    let c2 = t.secondary.fg();
    let c3 = t.warning.fg();
    let c4 = t.accent.fg();

    let kw = 14;
    let v1 = theme::pad_right(&format!("{c1}{bold}{tok_val}{rst}"), kw);
    let v2 = theme::pad_right(&format!("{c2}{bold}{pct_val}{rst}"), kw);
    let v3 = theme::pad_right(&format!("{c3}{bold}{cmd_val}{rst}"), kw);
    let v4 = theme::pad_right(&format!("{c4}{bold}{usd_val}{rst}"), kw);
    out.push(box_line(&format!("    {v1}{v2}{v3}{v4}")));

    let l1 = theme::pad_right(&format!("{dim}tokens saved{rst}"), kw);
    let l2 = theme::pad_right(&format!("{dim}compression{rst}"), kw);
    let l3 = theme::pad_right(&format!("{dim}commands{rst}"), kw);
    let l4 = theme::pad_right(&format!("{dim}USD saved{rst}"), kw);
    out.push(box_line(&format!("    {l1}{l2}{l3}{l4}")));
    out.push(box_line(""));
    out.push(format!("  {}", t.box_bottom(w)));

    {
        let cfg = crate::core::config::Config::load();
        if cfg.buddy_enabled {
            let buddy = crate::core::buddy::BuddyState::compute();
            out.push(crate::core::buddy::format_buddy_block_at(&buddy, t, tick));
        }
    }

    out.push(String::new());

    let cost_title = t.section_title("Cost Breakdown");
    out.push(format!(
        "  {cost_title}  {dim}@ ${:.2}/M input · ${:.2}/M output{rst}",
        cost_model.input_price_per_m, cost_model.output_price_per_m,
    ));
    out.push(format!("  {ln}", ln = t.border_line(w)));
    out.push(String::new());
    let lbl_w = 20;
    let lbl_without = theme::pad_right(
        &format!("{m}Without lean-ctx{rst}", m = t.muted.fg()),
        lbl_w,
    );
    let lbl_with = theme::pad_right(&format!("{m}With lean-ctx{rst}", m = t.muted.fg()), lbl_w);
    let lbl_saved = theme::pad_right(
        &format!("{c}{bold}You saved{rst}", c = t.success.fg()),
        lbl_w,
    );

    out.push(format!(
        "    {lbl_without} {:>8}   {dim}{} input + {} output{rst}",
        format_usd(cost.total_cost_without),
        format_usd(cost.input_cost_without),
        format_usd(cost.output_cost_without),
    ));
    out.push(format!(
        "    {lbl_with} {:>8}   {dim}{} input + {} output{rst}",
        format_usd(cost.total_cost_with),
        format_usd(cost.input_cost_with),
        format_usd(cost.output_cost_with),
    ));
    out.push(String::new());
    out.push(format!(
        "    {lbl_saved} {c}{bold}{:>8}{rst}   {dim}input {} + output {}{rst}",
        format_usd(cost.total_saved),
        format_usd(cost.input_cost_without - cost.input_cost_with),
        format_usd(cost.output_cost_without - cost.output_cost_with),
        c = t.success.fg(),
    ));

    {
        let mut mcp_saved = 0u64;
        let mut mcp_input = 0u64;
        let mut mcp_calls = 0u64;
        let mut hook_saved = 0u64;
        let mut hook_input = 0u64;
        let mut hook_calls = 0u64;
        for (cmd, s) in &store.commands {
            let sv = s.input_tokens.saturating_sub(s.output_tokens);
            if cmd.starts_with("ctx_") {
                mcp_saved += sv;
                mcp_input += s.input_tokens;
                mcp_calls += s.count;
            } else {
                hook_saved += sv;
                hook_input += s.input_tokens;
                hook_calls += s.count;
            }
        }
        if mcp_calls > 0 || hook_calls > 0 {
            out.push(String::new());
            out.push(format!("  {}", t.section_title("Savings by Source")));
            out.push(format!("  {ln}", ln = t.border_line(w)));
            out.push(String::new());

            let total = (mcp_saved + hook_saved).max(1) as f64;
            let mcp_pct = mcp_saved as f64 / total * 100.0;
            let hook_pct = hook_saved as f64 / total * 100.0;
            let mcp_rate_str = format_savings_pct(mcp_saved, mcp_input);
            let hook_rate_str = format_savings_pct(hook_saved, hook_input);
            let mcp_pct_str = format_pct_1dp(mcp_pct);
            let hook_pct_str = format_pct_1dp(hook_pct);

            let mcp_bar = t.gradient_bar(mcp_saved as f64 / total, 18);
            let hook_bar = t.gradient_bar(hook_saved as f64 / total, 18);

            let mc = t.success.fg();
            let hc = t.secondary.fg();
            out.push(format!(
                "    {mc}{bold}MCP Tools{rst}      {:>5}x  {mcp_bar}  {bold}{:>6}{rst}  {dim}{mcp_rate_str:>6} rate · {mcp_pct_str:>6} of total{rst}",
                mcp_calls,
                format_big(mcp_saved),
            ));
            out.push(format!(
                "    {hc}{bold}Shell Hooks{rst}     {:>5}x  {hook_bar}  {bold}{:>6}{rst}  {dim}{hook_rate_str:>6} rate · {hook_pct_str:>6} of total{rst}",
                hook_calls,
                format_big(hook_saved),
            ));
        }
    }

    out.push(String::new());

    if let (Some(first), Some(_last)) = (&store.first_use, &store.last_use) {
        let first_short = first.get(..10).unwrap_or(first);
        let daily_savings: Vec<u64> = store
            .daily
            .iter()
            .map(|d2| day_total_saved(d2, &cost_model))
            .collect();
        let spark = t.gradient_sparkline(&daily_savings);
        out.push(format!(
            "    {dim}Since {first_short} · {days_active} day{plural}{rst}   {spark}",
            plural = if days_active == 1 { "" } else { "s" }
        ));
        out.push(String::new());
    }

    out.push(String::new());

    if !store.commands.is_empty() {
        out.push(format!("  {}", t.section_title("Top Commands")));
        out.push(format!("  {ln}", ln = t.border_line(w)));
        out.push(String::new());

        let mut sorted: Vec<_> = store
            .commands
            .iter()
            .filter(|(_, s)| s.input_tokens > s.output_tokens)
            .collect();
        sorted.sort_by(|a, b2| {
            let sa = cmd_total_saved(a.1, &cost_model);
            let sb = cmd_total_saved(b2.1, &cost_model);
            sb.cmp(&sa)
        });

        let max_cmd_saved = sorted
            .first()
            .map_or(1, |(_, s)| cmd_total_saved(s, &cost_model))
            .max(1);

        for (cmd, stats) in sorted.iter().take(10) {
            let cmd_saved = cmd_total_saved(stats, &cost_model);
            let cmd_input_saved = stats.input_tokens.saturating_sub(stats.output_tokens);
            let cmd_pct = if stats.input_tokens > 0 {
                cmd_input_saved as f64 / stats.input_tokens as f64 * 100.0
            } else {
                0.0
            };
            let ratio = cmd_saved as f64 / max_cmd_saved as f64;
            let bar = theme::pad_right(&t.gradient_bar(ratio, 22), 22);
            let pc = t.pct_color(cmd_pct);
            let cmd_col = theme::pad_right(
                &format!("{m}{}{rst}", truncate_cmd(cmd, 16), m = t.muted.fg()),
                18,
            );
            let saved_col =
                theme::pad_right(&format!("{bold}{pc}{}{rst}", format_big(cmd_saved)), 8);
            out.push(format!(
                "    {cmd_col} {:>5}x   {bar}  {saved_col} {dim}{cmd_pct:>3.0}%{rst}",
                stats.count,
            ));
        }

        if sorted.len() > 10 {
            out.push(format!(
                "    {dim}... +{} more commands{rst}",
                sorted.len() - 10
            ));
        }
    }

    if store.daily.len() >= 2 {
        out.push(String::new());
        out.push(String::new());
        out.push(format!("  {}", t.section_title("Recent Days")));
        out.push(format!("  {ln}", ln = t.border_line(w)));
        out.push(String::new());

        let recent: Vec<_> = store.daily.iter().rev().take(7).collect();
        for day in recent.iter().rev() {
            let day_saved = day_total_saved(day, &cost_model);
            let day_input_saved = day.input_tokens.saturating_sub(day.output_tokens);
            let day_pct = if day.input_tokens > 0 {
                day_input_saved as f64 / day.input_tokens as f64 * 100.0
            } else {
                0.0
            };
            let pc = t.pct_color(day_pct);
            let date_short = day.date.get(5..).unwrap_or(&day.date);
            let date_col = theme::pad_right(&format!("{m}{date_short}{rst}", m = t.muted.fg()), 7);
            let saved_col =
                theme::pad_right(&format!("{pc}{bold}{}{rst}", format_big(day_saved)), 9);
            out.push(format!(
                "    {date_col}  {:>5} cmds   {saved_col} saved   {pc}{day_pct:>5.1}%{rst}",
                day.commands,
            ));
        }
    }

    out.push(String::new());
    out.push(String::new());

    if let Some(tip) = contextual_tip(&store) {
        out.push(format!("    {w}💡 {tip}{rst}", w = t.warning.fg()));
        out.push(String::new());
    }

    {
        let project_root = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if !project_root.is_empty() {
            let gotcha_store = crate::core::gotcha_tracker::GotchaStore::load(&project_root);
            if gotcha_store.stats.total_errors_detected > 0 || !gotcha_store.gotchas.is_empty() {
                let a = t.accent.fg();
                out.push(format!("    {a}🧠 Bug Memory{rst}"));
                out.push(format!(
                    "    {m}   Active gotchas: {}{rst}   Bugs prevented: {}{rst}",
                    gotcha_store.gotchas.len(),
                    gotcha_store.stats.total_prevented,
                    m = t.muted.fg(),
                ));
                out.push(String::new());
            }
        }
    }

    let m = t.muted.fg();
    out.push(format!(
        "    {m}🐛 Found a bug? Run: lean-ctx report-issue{rst}"
    ));
    out.push(format!(
        "    {m}📊 Help improve lean-ctx: lean-ctx contribute{rst}"
    ));
    out.push(format!("    {m}🧠 View bug memory: lean-ctx gotchas{rst}"));

    out.push(String::new());
    out.push(String::new());

    out.join("\n")
}

fn contextual_tip(store: &StatsStore) -> Option<String> {
    let tips = build_tips(store);
    if tips.is_empty() {
        return None;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400;
    Some(tips[(seed as usize) % tips.len()].clone())
}

fn build_tips(store: &StatsStore) -> Vec<String> {
    let mut tips = Vec::new();

    if store.cep.modes.get("map").copied().unwrap_or(0) == 0 {
        tips.push("Try mode=\"map\" for files you only need as context — shows deps + exports, skips implementation.".into());
    }

    if store.cep.modes.get("signatures").copied().unwrap_or(0) == 0 {
        tips.push("Try mode=\"signatures\" for large files — returns only the API surface.".into());
    }

    if store.cep.total_cache_reads > 0
        && store.cep.total_cache_hits as f64 / store.cep.total_cache_reads as f64 > 0.8
    {
        tips.push(
            "High cache hit rate! Use ctx_compress periodically to keep context compact.".into(),
        );
    }

    if store.total_commands > 50 && store.cep.sessions == 0 {
        tips.push("Use ctx_session to track your task — enables cross-session memory.".into());
    }

    if store.cep.modes.get("entropy").copied().unwrap_or(0) == 0 && store.total_commands > 20 {
        tips.push("Try mode=\"entropy\" for maximum compression on large files.".into());
    }

    if store.daily.len() >= 7 {
        tips.push("Run lean-ctx gain --graph for a 30-day sparkline chart.".into());
    }

    tips.push("Run ctx_overview(task) at session start for a task-aware project map.".into());
    tips.push("Run lean-ctx dashboard for a live web UI with all your stats.".into());

    let cfg = crate::core::config::Config::load();
    if cfg.theme == "default" {
        tips.push(
            "Customize your dashboard! Try: lean-ctx theme set cyberpunk (or neon, ocean, sunset, monochrome)".into(),
        );
        tips.push(
            "Want a unique look? Run lean-ctx theme list to see all available themes.".into(),
        );
    } else {
        tips.push(format!(
            "Current theme: {}. Run lean-ctx theme list to explore others.",
            cfg.theme
        ));
    }

    tips.push(
        "Create your own theme with lean-ctx theme create <name> and set custom colors!".into(),
    );

    tips
}

/// Runs the live-updating gain dashboard (1s refresh loop, Ctrl+C to exit).
pub fn gain_live() {
    use std::io::Write;

    let interval = std::time::Duration::from_secs(1);
    let mut line_count = 0usize;
    let dim = theme::dim();
    let rst = theme::rst();

    eprintln!("  {dim}▸ Live mode (1s refresh) · Ctrl+C to exit{rst}");

    loop {
        if line_count > 0 {
            print!("\x1B[{line_count}A\x1B[J");
        }

        let tick = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64);
        let output = format_gain_themed_at(&active_theme(), tick);
        let footer = format!("\n  {dim}▸ Live · updates every 1s · Ctrl+C to exit{rst}\n");
        let full = format!("{output}{footer}");
        line_count = full.lines().count();

        print!("{full}");
        let _ = std::io::stdout().flush();

        std::thread::sleep(interval);
    }
}

/// Renders a 30-day token savings bar chart with sparkline.
#[allow(clippy::many_single_char_names)] // ANSI formatting: t=theme, r=reset, b=bold, d=dim
pub fn format_gain_graph() -> String {
    let theme = active_theme();
    let store = super::load();
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();

    if store.daily.is_empty() {
        return format!(
            "{dim}No daily data yet.{rst} Use lean-ctx for a few days to see the graph."
        );
    }

    let cm = CostModel::default();
    let days: Vec<_> = store
        .daily
        .iter()
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let savings: Vec<u64> = days.iter().map(|day| day_total_saved(day, &cm)).collect();

    let max_saved = *savings.iter().max().unwrap_or(&1);
    let max_saved = max_saved.max(1);

    let bar_width = 36;
    let mut out = Vec::new();

    out.push(String::new());
    out.push(format!(
        "  {icon} {title}  {dim}Token Savings Graph (last 30 days){rst}",
        icon = theme.header_icon(),
        title = theme.brand_title(),
    ));
    out.push(format!("  {ln}", ln = theme.border_line(58)));
    out.push(format!(
        "  {dim}{:>58}{rst}",
        format!("peak: {}", format_big(max_saved))
    ));
    out.push(String::new());

    for (i, day) in days.iter().enumerate() {
        let saved = savings[i];
        let ratio = saved as f64 / max_saved as f64;
        let bar = theme::pad_right(&theme.gradient_bar(ratio, bar_width), bar_width);

        let input_saved = day.input_tokens.saturating_sub(day.output_tokens);
        let pct = if day.input_tokens > 0 {
            input_saved as f64 / day.input_tokens as f64 * 100.0
        } else {
            0.0
        };
        let date_short = day.date.get(5..).unwrap_or(&day.date);

        out.push(format!(
            "  {m}{date_short}{rst} {brd}│{rst} {bar} {bold}{:>6}{rst} {dim}{pct:.0}%{rst}",
            format_big(saved),
            m = theme.muted.fg(),
            brd = theme.border.fg(),
        ));
    }

    let total_saved: u64 = savings.iter().sum();
    let total_cmds: u64 = days.iter().map(|day| day.commands).sum();
    let spark = theme.gradient_sparkline(&savings);

    out.push(String::new());
    out.push(format!("  {ln}", ln = theme.border_line(58)));
    out.push(format!(
        "  {spark}  {bold}{txt}{}{rst} saved across {bold}{}{rst} commands",
        format_big(total_saved),
        format_num(total_cmds),
        txt = theme.text.fg(),
    ));
    out.push(String::new());

    out.join("\n")
}

/// Renders a daily breakdown table of token savings with totals.
#[allow(clippy::many_single_char_names)] // ANSI formatting: t=theme, r=reset, b=bold, d=dim
pub fn format_gain_daily() -> String {
    let theme = active_theme();
    let store = super::load();
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();

    if store.daily.is_empty() {
        return format!("{dim}No daily data yet.{rst}");
    }

    let mut out = Vec::new();
    let w = 64;

    let side = theme.box_side();
    let daily_box = |content: &str| -> String {
        let padded = theme::pad_right(content, w);
        format!("  {side}{padded}{side}")
    };

    out.push(String::new());
    out.push(format!(
        "  {icon} {title}  {dim}Daily Breakdown{rst}",
        icon = theme.header_icon(),
        title = theme.brand_title(),
    ));
    out.push(format!("  {}", theme.box_top(w)));
    let hdr = format!(
        " {bold}{txt}{:<12} {:>6}  {:>10}  {:>10}  {:>7}  {:>6}{rst}",
        "Date",
        "Cmds",
        "Input",
        "Saved",
        "Rate",
        "USD",
        txt = theme.text.fg(),
    );
    out.push(daily_box(&hdr));
    out.push(format!("  {}", theme.box_mid(w)));

    let days: Vec<_> = store
        .daily
        .iter()
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .cloned()
        .collect();

    let cm = CostModel::default();
    for day in &days {
        let saved = day_total_saved(day, &cm);
        let input_saved = day.input_tokens.saturating_sub(day.output_tokens);
        let pct = if day.input_tokens > 0 {
            input_saved as f64 / day.input_tokens as f64 * 100.0
        } else {
            0.0
        };
        let pc = theme.pct_color(pct);
        let usd = usd_estimate(saved);
        let row = format!(
            " {m}{:<12}{rst} {:>6}  {:>10}  {pc}{bold}{:>10}{rst}  {pc}{:>6.1}%{rst}  {dim}{:>6}{rst}",
            &day.date,
            day.commands,
            format_big(day.input_tokens),
            format_big(saved),
            pct,
            usd,
            m = theme.muted.fg(),
        );
        out.push(daily_box(&row));
    }

    let total_input: u64 = store.daily.iter().map(|day| day.input_tokens).sum();
    let total_saved: u64 = store
        .daily
        .iter()
        .map(|day| day_total_saved(day, &cm))
        .sum();
    let total_pct = if total_input > 0 {
        let input_saved: u64 = store
            .daily
            .iter()
            .map(|day| day.input_tokens.saturating_sub(day.output_tokens))
            .sum();
        input_saved as f64 / total_input as f64 * 100.0
    } else {
        0.0
    };
    let total_usd = usd_estimate(total_saved);
    let sc = theme.success.fg();

    out.push(format!("  {}", theme.box_mid(w)));
    let total_row = format!(
        " {bold}{txt}{:<12}{rst} {:>6}  {:>10}  {sc}{bold}{:>10}{rst}  {sc}{bold}{:>6.1}%{rst}  {bold}{:>6}{rst}",
        "TOTAL",
        format_num(store.total_commands),
        format_big(total_input),
        format_big(total_saved),
        total_pct,
        total_usd,
        txt = theme.text.fg(),
    );
    out.push(daily_box(&total_row));
    out.push(format!("  {}", theme.box_bottom(w)));

    let daily_savings: Vec<u64> = days.iter().map(|day| day_total_saved(day, &cm)).collect();
    let spark = theme.gradient_sparkline(&daily_savings);
    out.push(format!("  {dim}Trend:{rst} {spark}"));
    out.push(String::new());

    out.join("\n")
}

/// Returns the full stats store as pretty-printed JSON.
pub fn format_gain_json() -> String {
    let store = super::load();
    serde_json::to_string_pretty(&store).unwrap_or_else(|_| "{}".to_string())
}
