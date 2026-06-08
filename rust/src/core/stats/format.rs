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
    // Must resolve the same directory the writer uses
    // (`server_metrics::write_mcp_live_stats` → `lean_ctx_data_dir()`). The MCP
    // config always sets `LEAN_CTX_DATA_DIR`, so the previous hardcoded
    // `~/.lean-ctx/mcp-live.json` read missed the live file under a custom/XDG
    // data dir — making `gain` / `/lean-ctx` report 0 despite live CEP data (#361).
    let path = crate::core::data_dir::lean_ctx_data_dir()
        .ok()?
        .join("mcp-live.json");
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

    let cep_w = 60;
    let cep_ss = theme.box_side_square();
    let cep_line = |content: &str| -> String {
        let padded = theme::pad_right(content, cep_w);
        format!("  {cep_ss}{padded}{cep_ss}")
    };

    out.push(String::new());
    out.push(format!("  {}", theme.box_top(cep_w)));
    let cep_side = theme.box_side();
    out.push(format!(
        "  {cep_side}{}{cep_side}",
        theme::pad_right(
            &format!(
                "  {icon}  {brand}  {dim}CEP Report{rst}",
                icon = theme.header_icon(),
                brand = theme.brand_title(),
            ),
            cep_w,
        )
    ));
    out.push(format!("  {}", theme.box_bottom(cep_w)));
    out.push(String::new());

    let score_ratio = (latest_score as f64 / 100.0).min(1.0);
    let score_bar = theme.gradient_bar(score_ratio, 20);
    let score_pc = theme.pct_color(latest_score as f64);

    out.push(format!("  {}", theme.box_top_labeled(cep_w, "CEP SCORE")));
    out.push(cep_line(&format!(
        "  {score_bar}  {score_pc}{bold}{latest_score}/100{rst}  {dim}avg: {avg_score:.0}{rst}"
    )));
    out.push(cep_line(&format!(
        "  {bold}{txt}Sessions{rst} {sec}{}{rst}  {bold}{txt}Cache{rst} {pc}{cache_hit_rate:.1}%{rst}  {bold}{txt}Compression{rst} {pc2}{overall_compression:.1}%{rst}",
        cep.sessions,
        pc = theme.pct_color(cache_hit_rate),
        pc2 = theme.pct_color(overall_compression),
    )));
    out.push(cep_line(&format!(
        "  {bold}{txt}Saved{rst} {sc}{}{rst} {dim}({} → {} · ≈ {}){rst}",
        format_big(total_saved),
        format_big(cep.total_tokens_original),
        format_big(cep.total_tokens_compressed),
        usd_estimate(total_saved),
    )));
    out.push(format!("  {}", theme.box_bottom_square(cep_w)));
    out.push(String::new());

    out.push(format!(
        "  {}",
        theme.box_top_labeled(cep_w, "SAVINGS BREAKDOWN")
    ));

    let bar_w = 26;
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
    // `cep_share` is already a percentage (0..100), so the shell share is its
    // complement — not `(1.0 - cep_share) * 100`, which produced absurd values.
    let shell_pct_display = format_pct_1dp(100.0 - cep_share);
    out.push(cep_line(&format!(
        "  {m}Shell Hook{rst}  {shell_bar} {bold}{:>6}{rst} {dim}({shell_pct_display}){rst}",
        format_big(shell_saved),
    )));
    let cep_bar = theme::pad_right(&theme.gradient_bar(cep_ratio, bar_w), bar_w);
    let cep_pct_display = format_pct_1dp(cep_share);
    out.push(cep_line(&format!(
        "  {m}MCP/CEP{rst}     {cep_bar} {bold}{:>6}{rst} {dim}({cep_pct_display}){rst}",
        format_big(total_saved),
    )));
    out.push(format!("  {}", theme.box_bottom_square(cep_w)));
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
        out.push(format!("  {}", theme.box_top_labeled(cep_w, "READ MODES")));

        let mut sorted_modes: Vec<_> = cep.modes.iter().collect();
        sorted_modes.sort_by_key(|item| std::cmp::Reverse(*item.1));
        let max_mode = (*sorted_modes.first().map_or(&1, |(_, c)| *c)).max(1);

        for (mode, count) in &sorted_modes {
            let ratio = **count as f64 / max_mode as f64;
            let bar = theme::pad_right(&theme.gradient_bar(ratio, 20), 20);
            let mode_disp = theme::truncate_visual(mode.as_str(), 16);
            out.push(cep_line(&format!(
                "  {sec}{mode_disp:<16}{rst} {count:>4}x  {bar}"
            )));
        }

        let total_mode_calls: u64 = sorted_modes.iter().map(|(_, c)| **c).sum();
        let full_count = cep.modes.get("full").copied().unwrap_or(0);
        let optimized = total_mode_calls.saturating_sub(full_count);
        let opt_pct = if total_mode_calls > 0 {
            optimized as f64 / total_mode_calls as f64 * 100.0
        } else {
            0.0
        };
        out.push(cep_line(&format!(
            "  {dim}{optimized}/{total_mode_calls} reads optimized \u{00b7} {opt_pct:.0}% non-full{rst}"
        )));
        out.push(format!("  {}", theme.box_bottom_square(cep_w)));
        out.push(String::new());
    }

    if cep.scores.len() >= 2 {
        out.push(format!("  {}", theme.box_top_labeled(cep_w, "SCORE TREND")));

        let score_values: Vec<u64> = cep.scores.iter().map(|s| s.score as u64).collect();
        // Cap to the most recent points so the sparkline fits inside the box.
        let spark_vals: Vec<u64> = score_values.iter().rev().take(54).rev().copied().collect();
        let spark = theme.gradient_sparkline(&spark_vals);
        out.push(cep_line(&format!("  {spark}")));

        let recent: Vec<_> = cep.scores.iter().rev().take(5).collect();
        for snap in recent.iter().rev() {
            let ts = snap.timestamp.get(..16).unwrap_or(&snap.timestamp);
            let pc = theme.pct_color(snap.score as f64);
            let cplx = theme::truncate_visual(&snap.complexity, 14);
            out.push(cep_line(&format!(
                "  {m}{ts}{rst}  {pc}{bold}{:>3}{rst}/100  {dim}cache {:>3}%  {cplx}{rst}",
                snap.score, snap.cache_hit_rate,
            )));
        }
        out.push(format!("  {}", theme.box_bottom_square(cep_w)));
        out.push(String::new());
    }

    out.push(format!("  {}", theme.box_top_labeled(cep_w, "IMPROVE")));
    let mut tips: Vec<String> = Vec::new();
    if cache_hit_rate < 50.0 {
        tips.push(format!(
            "  {wrn}\u{2191}{rst} Re-read files with ctx_read to leverage caching"
        ));
    }
    if cep.modes.len() < 3 {
        tips.push(format!(
            "  {wrn}\u{2191}{rst} Use map/signatures modes for context-only files"
        ));
    }
    if avg_score >= 70.0 {
        tips.push(format!(
            "  {sc}\u{2713}{rst} Great score! You're using lean-ctx effectively"
        ));
    }
    if tips.is_empty() {
        tips.push(format!(
            "  {sc}\u{2713}{rst} Solid usage \u{2014} keep leaning on cached, compressed reads"
        ));
    }
    for tip in tips {
        out.push(cep_line(&tip));
    }
    out.push(format!("  {}", theme.box_bottom_square(cep_w)));
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

/// Renders the concise "hero" gain output — 3 key metrics, gain score, trend, next actions.
pub fn format_gain_hero() -> String {
    format_gain_hero_themed(&active_theme())
}

/// Hero gain with specific theme.
pub fn format_gain_hero_themed(t: &Theme) -> String {
    let store = super::load();
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();

    if store.total_commands == 0 {
        return format_gain_themed_at(t, None);
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

    let engine = crate::core::gain::GainEngine::load();
    let score = engine.gain_score(None);

    let w = 57;
    let side = t.box_side();
    let box_line = |content: &str| -> String {
        let padded = theme::pad_right(content, w);
        format!("  {side}{padded}{side}")
    };

    let mut out = Vec::new();
    out.push(String::new());
    out.push(format!("  {}", t.box_top(w)));
    out.push(box_line(&format!(
        "  {icon}  {title}",
        icon = t.header_icon(),
        title = t.brand_title(),
    )));
    out.push(box_line(""));

    let c1 = t.success.fg();
    let c2 = t.secondary.fg();
    let c4 = t.accent.fg();
    let tok_val = format_big(input_saved);
    let pct_val = format!("{pct:.0}%");
    let usd_val = format_usd(cost.total_saved);

    let kw = 18;
    let v1 = theme::pad_right(&format!("{c1}{bold}{tok_val}{rst}"), kw);
    let v2 = theme::pad_right(&format!("{c2}{bold}{pct_val}{rst}"), kw);
    let v3 = theme::pad_right(&format!("{c4}{bold}{usd_val}{rst}"), kw);
    out.push(box_line(&format!("  {v1}{v2}{v3}")));

    let ul1 = theme::pad_right(&t.kpi_underline(tok_val.len(), &t.success), kw);
    let ul2 = theme::pad_right(&t.kpi_underline(pct_val.len(), &t.secondary), kw);
    let ul3 = theme::pad_right(&t.kpi_underline(usd_val.len(), &t.accent), kw);
    out.push(box_line(&format!("  {ul1}{ul2}{ul3}")));

    let l1 = theme::pad_right(&format!("{dim}tokens saved{rst}"), kw);
    let l2 = theme::pad_right(&format!("{dim}compression{rst}"), kw);
    let l3 = theme::pad_right(&format!("{dim}USD saved{rst}"), kw);
    out.push(box_line(&format!("  {l1}{l2}{l3}")));
    out.push(box_line(""));

    let score_bar_w = 30;
    let score_ratio = (score.total as f64 / 100.0).min(1.0);
    let bar = t.gradient_bar(score_ratio, score_bar_w);
    let sc_color = t.pct_color(score.total as f64);
    let lvl = score.level();
    out.push(box_line(&format!(
        "  {bar}  {sc_color}{bold}{}/100{rst}  Lv{} {dim}{}{rst}",
        score.total, lvl.level, lvl.title,
    )));
    out.push(box_line(""));

    if store.daily.len() >= 2 {
        let daily_savings: Vec<u64> = store
            .daily
            .iter()
            .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
            .collect();
        let spark = t.gradient_sparkline(&daily_savings);
        let trend_str = trend_string(&store, &c1, &t.warning.fg(), rst);
        out.push(box_line(&format!(
            "  {dim}trend:{rst} {spark}  {trend_str}"
        )));
    }

    if input_saved > 0 {
        let energy_str = crate::core::energy::format_for_tokens(input_saved);
        let charges = crate::core::energy::phone_charges_hint(input_saved)
            .map(|h| format!(" ({h})"))
            .unwrap_or_default();
        out.push(box_line(&format!(
            "  {dim}energy:{rst} {c1}{energy_str}{rst}{dim}{charges}{rst}"
        )));
    }

    out.push(format!("  {}", t.box_bottom(w)));
    out.push(String::new());

    // Weekly nudge: after 7 days of data, if user hasn't published, show a prominent card
    if store.daily.len() >= 7 && !crate::cli::wrapped_publish::has_published() {
        let week_saved: u64 = store
            .daily
            .iter()
            .rev()
            .take(7)
            .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
            .sum();
        if week_saved > 0 {
            let accent = t.accent.fg();
            out.push(format!("  {}", t.box_top(42)));
            let nside = t.box_side();
            out.push(format!(
                "  {nside} {accent}{bold}Your first week!{rst}                          {nside}"
            ));
            out.push(format!(
                "  {nside} You saved {c1}{bold}{}{rst} tokens this week.      {nside}",
                crate::core::wrapped::format_tokens(week_saved),
            ));
            out.push(format!(
                "  {nside} Share your card? {sec}lean-ctx gain --wrapped{rst} {nside}",
                sec = t.secondary.fg(),
            ));
            out.push(format!("  {}", t.box_bottom(42)));
            out.push(String::new());
        }
    }

    let sec = t.secondary.fg();
    out.push(format!(
        "  {sec}lean-ctx gain --deep{rst}     {dim}Full breakdown{rst}"
    ));
    out.push(format!(
        "  {sec}lean-ctx gain --wrapped{rst}  {dim}Shareable card{rst}"
    ));
    out.push(format!(
        "  {sec}lean-ctx watch{rst}           {dim}Live observatory{rst}"
    ));
    out.push(String::new());

    if let Some(tip) = contextual_tip(&store) {
        out.push(format!("  {dim}💡 {tip}{rst}"));
        out.push(String::new());
    }

    out.join("\n")
}

/// Renders the token savings dashboard at a specific animation tick (with footer).
pub fn format_gain_themed_at(t: &Theme, tick: Option<u64>) -> String {
    gain_dashboard(t, tick, true)
}

/// The dashboard body without the trailing footer (tips / Context OS / hints).
/// Used to compose `gain --deep`, where the extra themed sections must appear
/// before the footer instead of in the middle of the output.
pub fn format_gain_body() -> String {
    gain_dashboard(&active_theme(), None, false)
}

/// The standalone gain dashboard footer (contextual tip, Context OS, hints).
pub fn format_gain_footer() -> String {
    let store = super::load();
    let mut out = Vec::new();
    append_gain_footer(&mut out, &active_theme(), &store);
    out.join("\n")
}

#[allow(clippy::many_single_char_names)] // ANSI formatting: t=theme, r=reset, b=bold, d=dim
fn gain_dashboard(t: &Theme, tick: Option<u64>, with_footer: bool) -> String {
    let store = super::load();
    let mut out = Vec::new();
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();

    if store.total_commands == 0 {
        let data_dir = match crate::core::data_dir::lean_ctx_data_dir() {
            Ok(p) => p.display().to_string(),
            Err(_) => "~/.config/lean-ctx".into(),
        };
        let mcp_hint = if let Ok(live) =
            std::fs::read_to_string(std::path::Path::new(&data_dir).join("mcp-live.json"))
        {
            if live.contains("\"total_calls\"") {
                format!(
                    "\n{dim}MCP calls are tracked in mcp-live.json but stats.json is empty.{rst}\
                     \n{dim}This may indicate a data directory split. Run: lean-ctx doctor{rst}"
                )
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let split_dirs = crate::core::data_dir::all_data_dirs_with_stats();
        let split_hint = if split_dirs.len() >= 2 {
            format!(
                "\n{dim}⚠ Stats found in multiple locations:{rst}\
                 \n{dim}  {}{rst}\
                 \n{dim}Run: lean-ctx doctor{rst}",
                split_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            String::new()
        };
        return format!(
            "{bold}No savings recorded yet — and that's expected.{rst}\
             \n\n  {dim}Savings appear after your AI tool uses lean-ctx for the first time.{rst}\
             \n\n  Next:\
             \n    1. Make sure your AI tool is connected:  {cmd}lean-ctx doctor{rst}\
             \n    2. Fully restart your AI tool so it reconnects to lean-ctx.\
             \n    3. Ask it to read a file or run a command — then check back here.\
             \n\n  {dim}Tip: track a shell command yourself with {rst}{cmd}lean-ctx -c \"git status\"{rst}\
             \n\n  {dim}Stats path: {data_dir}{rst}{mcp_hint}{split_hint}",
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
    let _days_active = store.daily.len();

    let w = 70;
    let side = t.box_side();
    let ss = t.box_side_square();

    let box_line = |content: &str| -> String {
        let padded = theme::pad_right(content, w);
        format!("  {side}{padded}{side}")
    };
    let sec_line = |content: &str| -> String {
        let padded = theme::pad_right(content, w);
        format!("  {ss}{padded}{ss}")
    };

    out.push(String::new());
    out.push(format!("  {}", t.box_top(w)));
    out.push(box_line(""));

    let ver = env!("CARGO_PKG_VERSION");
    let header = format!(
        "     {icon}  {bold}{title}{rst}",
        icon = t.header_icon(),
        title = t.brand_title(),
    );
    let ver_part = format!("{dim}v{ver}{rst}");
    let header_padded = theme::pad_right(&header, w - ver.len() - 2);
    out.push(format!("  {side}{header_padded}{ver_part} {side}"));

    let subtitle = format!("     {dim}Token Savings Dashboard{rst}");
    out.push(box_line(&subtitle));
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

    let kw = 16;
    let v1 = theme::pad_right(&format!("{c1}{bold}{tok_val}{rst}"), kw);
    let v2 = theme::pad_right(&format!("{c2}{bold}{pct_val}{rst}"), kw);
    let v3 = theme::pad_right(&format!("{c3}{bold}{cmd_val}{rst}"), kw);
    let v4 = theme::pad_right(&format!("{c4}{bold}{usd_val}{rst}"), kw);
    out.push(box_line(&format!("     {v1}{v2}{v3}{v4}")));

    let ul1 = theme::pad_right(&t.kpi_underline(tok_val.len(), &t.success), kw);
    let ul2 = theme::pad_right(&t.kpi_underline(pct_val.len(), &t.secondary), kw);
    let ul3 = theme::pad_right(&t.kpi_underline(cmd_val.len(), &t.warning), kw);
    let ul4 = theme::pad_right(&t.kpi_underline(usd_val.len(), &t.accent), kw);
    out.push(box_line(&format!("     {ul1}{ul2}{ul3}{ul4}")));

    let l1 = theme::pad_right(&format!("{dim}tokens saved{rst}"), kw);
    let l2 = theme::pad_right(&format!("{dim}compression{rst}"), kw);
    let l3 = theme::pad_right(&format!("{dim}commands{rst}"), kw);
    let l4 = theme::pad_right(&format!("{dim}USD saved{rst}"), kw);
    out.push(box_line(&format!("     {l1}{l2}{l3}{l4}")));
    out.push(box_line(""));
    out.push(format!("  {}", t.box_bottom(w)));
    out.push(String::new());

    // -- GAIN SCORE section (labeled box) --
    {
        let engine = crate::core::gain::GainEngine::load();
        let score = engine.gain_score(None);
        let lvl = score.level();
        let score_ratio = (score.total as f64 / 100.0).min(1.0);
        let bar = t.gradient_bar(score_ratio, 30);
        let sc_color = t.pct_color(score.total as f64);

        out.push(format!("  {}", t.box_top_labeled(w, "GAIN SCORE")));
        out.push(sec_line(&format!(
            "  {bar}  {sc_color}{bold}{}/100{rst}  Lv{} {dim}{}{rst}",
            score.total, lvl.level, lvl.title,
        )));

        if store.daily.len() >= 2 {
            let daily_savings: Vec<u64> = store
                .daily
                .iter()
                .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
                .collect();
            let spark = t.gradient_sparkline(&daily_savings);
            let trend_str = trend_string(&store, &c1, &t.warning.fg(), rst);
            out.push(sec_line(&format!(
                "  {dim}trend:{rst} {spark}  {trend_str}"
            )));
        }

        if total_saved > 0 {
            let energy_str = crate::core::energy::format_for_tokens(total_saved);
            let charges = crate::core::energy::phone_charges_hint(total_saved)
                .map(|h| format!(" ({h})"))
                .unwrap_or_default();
            out.push(sec_line(&format!(
                "  {dim}energy:{rst} {c1}{energy_str}{rst}{dim}{charges}{rst}"
            )));
        }
        out.push(format!("  {}", t.box_bottom_square(w)));
    }

    // -- COMPANION section --
    {
        let cfg = crate::core::config::Config::load();
        if cfg.buddy_enabled {
            out.push(String::new());
            out.push(format!("  {}", t.box_top_labeled(w, "YOUR COMPANION")));
            let buddy = crate::core::buddy::BuddyState::compute();
            let block = crate::core::buddy::format_buddy_block_at(&buddy, t, tick);
            for line in block.lines() {
                out.push(sec_line(line));
            }
            out.push(format!("  {}", t.box_bottom_square(w)));
        }
    }

    out.push(String::new());

    // -- COST BREAKDOWN section --
    let price_label = format!(
        "@ ${:.2}/M input · ${:.2}/M output",
        cost_model.input_price_per_m, cost_model.output_price_per_m,
    );
    let cost_label = format!("COST BREAKDOWN ──── {price_label}");
    out.push(format!("  {}", t.box_top_labeled(w, &cost_label)));
    out.push(sec_line(""));
    let without_bar = t.gradient_bar(1.0, 26);
    let with_ratio = cost.total_cost_with / cost.total_cost_without.max(0.01);
    let with_bar = t.gradient_bar(with_ratio, 26);
    let saved_pct = if cost.total_cost_without > 0.0 {
        (1.0 - with_ratio) * 100.0
    } else {
        0.0
    };

    out.push(sec_line(&format!(
        "  {m}Without lean-ctx{rst}  {:>10}  {without_bar}",
        format_usd(cost.total_cost_without),
        m = t.muted.fg(),
    )));
    out.push(sec_line(&format!(
        "  {m}With lean-ctx{rst}      {:>10}  {with_bar}",
        format_usd(cost.total_cost_with),
        m = t.muted.fg(),
    )));
    out.push(sec_line(&format!(
        "  {c}{bold}You saved{rst}          {c}{bold}{:>10}{rst}  {dim}── {saved_pct:.1}% reduction ──{rst}",
        format_usd(cost.total_saved),
        c = t.success.fg(),
    )));
    out.push(format!("  {}", t.box_bottom_square(w)));

    out.push(String::new());

    // -- TOP COMMANDS section --
    if !store.commands.is_empty() {
        out.push(format!("  {}", t.box_top_labeled(w, "TOP COMMANDS")));

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
            let bar = theme::pad_right(&t.gradient_bar(ratio, 20), 20);
            let pc = t.pct_color(cmd_pct);
            let cmd_col = theme::pad_right(
                &format!("{m}{}{rst}", truncate_cmd(cmd, 14), m = t.muted.fg()),
                16,
            );
            let saved_col =
                theme::pad_right(&format!("{bold}{pc}{}{rst}", format_big(cmd_saved)), 7);
            let row = format!(
                " {cmd_col} {:>4}x {bar} {saved_col}{dim}{cmd_pct:>3.0}%{rst}",
                stats.count,
            );
            out.push(sec_line(&row));
        }

        if sorted.len() > 10 {
            out.push(sec_line(&format!(
                "  {dim}... +{} more commands{rst}",
                sorted.len() - 10
            )));
        }
        out.push(format!("  {}", t.box_bottom_square(w)));
    }

    // -- RECENT DAYS section --
    if store.daily.len() >= 2 {
        out.push(String::new());
        out.push(format!("  {}", t.box_top_labeled(w, "RECENT DAYS")));

        let max_day_saved = store
            .daily
            .iter()
            .rev()
            .take(7)
            .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
            .max()
            .unwrap_or(1)
            .max(1);

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
            let ratio = day_input_saved as f64 / max_day_saved as f64;
            let day_bar = t.gradient_bar(ratio, 20);
            let date_short = day.date.get(5..).unwrap_or(&day.date);
            let date_col = theme::pad_right(&format!("{m}{date_short}{rst}", m = t.muted.fg()), 7);
            let saved_col =
                theme::pad_right(&format!("{pc}{bold}{}{rst}", format_big(day_saved)), 9);
            out.push(sec_line(&format!(
                "  {date_col} {:>4} cmds  {saved_col} {pc}{day_pct:>5.1}%{rst}  {day_bar}",
                day.commands,
            )));
        }
        out.push(format!("  {}", t.box_bottom_square(w)));
    }

    if with_footer {
        append_gain_footer(&mut out, t, &store);
    }

    out.join("\n")
}

/// Appends the dashboard footer (contextual tip, Bug Memory, Context OS panel,
/// help hints). Kept separate so `gain --deep` can render it *after* the extra
/// themed sections instead of in the middle of the output.
fn append_gain_footer(out: &mut Vec<String>, t: &Theme, store: &StatsStore) {
    let rst = theme::rst();
    let bold = theme::bold();

    out.push(String::new());
    out.push(String::new());

    if let Some(tip) = contextual_tip(store) {
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

    {
        let project_root = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let a = t.accent.fg();
        let m = t.muted.fg();

        let mut ctx_items: Vec<String> = Vec::new();

        if let Some(session) =
            crate::core::session::SessionState::load_latest_for_project_root(&project_root)
        {
            let task_str = session
                .task
                .as_ref()
                .map_or("—", |tk| tk.description.as_str());
            let task_disp = if task_str.len() > 35 {
                format!("{}…", &task_str[..task_str.floor_char_boundary(32)])
            } else {
                task_str.to_string()
            };
            ctx_items.push(format!(
                "   Session: {bold}{task_disp}{rst}  {m}files={} findings={} terse={}{rst}",
                session.files_touched.len(),
                session.findings.len(),
                if session.terse_mode { "on" } else { "off" },
            ));
        }

        let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(&project_root);
        let active_facts = knowledge.facts.iter().filter(|f| f.is_current()).count();
        if active_facts > 0 {
            ctx_items.push(format!(
                "   Knowledge: {bold}{active_facts}{rst} active facts  {m}{} total{rst}",
                knowledge.facts.len(),
            ));
        }

        if let Some(open) = crate::core::graph_provider::open_best_effort(&project_root) {
            let nc = open.provider.node_count().unwrap_or(0);
            let ec = open.provider.edge_count().unwrap_or(0);
            if nc > 0 {
                let (unit, suffix) = match open.source {
                    crate::core::graph_provider::GraphProviderSource::PropertyGraph => {
                        ("nodes", "")
                    }
                    crate::core::graph_provider::GraphProviderSource::GraphIndex => {
                        let max_cfg = crate::core::config::Config::load().graph_index_max_files;
                        if max_cfg > 0 && nc >= max_cfg as usize {
                            ("files", " (limit reached)")
                        } else {
                            ("files", "")
                        }
                    }
                };
                ctx_items.push(format!(
                    "   Graph: {bold}{nc}{rst} {unit}  {bold}{ec}{rst} edges{suffix}",
                ));
            }
        }

        #[cfg(unix)]
        let daemon_running = crate::daemon::is_daemon_running();
        #[cfg(not(unix))]
        let daemon_running = false;

        if daemon_running {
            ctx_items.push(format!("   Daemon: {c}running{rst}", c = t.success.fg()));
        } else {
            ctx_items.push(format!(
                "   {w}Daemon: offline{rst} {m}(lean-ctx serve -d for persistent tracking){rst}",
                w = t.warning.fg()
            ));
        }

        if !ctx_items.is_empty() {
            out.push(format!("    {a}⚡ Context OS{rst}"));
            for item in &ctx_items {
                out.push(format!("    {item}"));
            }
            out.push(String::new());
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
}

fn trend_string(store: &StatsStore, up_color: &str, down_color: &str, rst: &str) -> String {
    if store.daily.len() < 14 {
        return String::new();
    }
    let recent_7: u64 = store
        .daily
        .iter()
        .rev()
        .take(7)
        .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
        .sum();
    let prev_7: u64 = store
        .daily
        .iter()
        .rev()
        .skip(7)
        .take(7)
        .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
        .sum();
    if prev_7 == 0 {
        return String::new();
    }
    let change = ((recent_7 as f64 / prev_7 as f64) - 1.0) * 100.0;
    if change >= 0.0 {
        format!("{up_color}+{change:.0}%{rst} vs last week")
    } else {
        format!("{down_color}{change:.0}%{rst} vs last week")
    }
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
        "Create a custom theme: write a TOML file and import it with lean-ctx theme import <file>"
            .into(),
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

    tracing::info!("Live mode (1s refresh) · Ctrl+C to exit");

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
    let w = 76;

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
        " {bold}{txt}{:<12} {:>6}  {:>10}  {:>10}  {:>7}  {:>8}  {:>8}{rst}",
        "Date",
        "Cmds",
        "Input",
        "Saved",
        "Rate",
        "USD",
        "Ver",
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
        let ver = if day.version.is_empty() {
            "—".to_string()
        } else {
            format!("v{}", day.version)
        };
        let row = format!(
            " {m}{:<12}{rst} {:>6}  {:>10}  {pc}{bold}{:>10}{rst}  {pc}{:>6.1}%{rst}  {dim}{:>8}{rst}  {dim}{:>8}{rst}",
            &day.date,
            day.commands,
            format_big(day.input_tokens),
            format_big(saved),
            pct,
            usd,
            ver,
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
        " {bold}{txt}{:<12}{rst} {:>6}  {:>10}  {sc}{bold}{:>10}{rst}  {sc}{bold}{:>6.1}%{rst}  {bold}{:>8}{rst}  {bold}{:>8}{rst}",
        "TOTAL",
        format_num(store.total_commands),
        format_big(total_input),
        format_big(total_saved),
        total_pct,
        total_usd,
        "",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// #361: the live-stats reader must look in `lean_ctx_data_dir()` (honoring
    /// `LEAN_CTX_DATA_DIR`, which the MCP config always sets), not a hardcoded
    /// `~/.lean-ctx`. Otherwise `gain` / `/lean-ctx` report 0 despite live data.
    #[test]
    fn load_mcp_live_reads_from_configured_data_dir() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join("lean_ctx_mcp_live_datadir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LEAN_CTX_DATA_DIR", &dir);
        std::fs::write(
            dir.join("mcp-live.json"),
            r#"{"cep_score":42,"tokens_saved":123}"#,
        )
        .unwrap();

        let loaded = load_mcp_live();

        std::env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);

        let live = loaded.expect("live stats must load from the configured data dir");
        assert_eq!(
            live.get("cep_score").and_then(serde_json::Value::as_u64),
            Some(42)
        );
    }

    #[test]
    fn load_mcp_live_none_when_file_absent() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join("lean_ctx_mcp_live_absent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("LEAN_CTX_DATA_DIR", &dir);

        let loaded = load_mcp_live();

        std::env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(loaded.is_none(), "no mcp-live.json → None");
    }
}
