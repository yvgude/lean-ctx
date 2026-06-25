//! CEP (Compact Efficiency Protocol) report: live MCP stats + scorecard.

use super::util::{active_theme, format_big, format_pct_1dp, usd_estimate};
use crate::core::theme::{self, Theme};

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
        pc = t.pct_color(f64::from(score)),
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
    // (`server_metrics::write_mcp_live_stats` → `state_dir()`). `mcp-live.json`
    // is runtime STATE (GH #408); `state_dir()` honors `LEAN_CTX_STATE_DIR` and
    // collapses to the legacy data dir for single-dir installs, so live CEP data
    // is found regardless of layout (#361).
    let path = crate::core::paths::state_dir().ok()?.join("mcp-live.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Renders the full CEP (Cognitive Efficiency Protocol) report with themes.
#[allow(clippy::many_single_char_names)] // ANSI formatting: t=theme, r=reset, b=bold, d=dim
#[must_use]
pub fn format_cep_report() -> String {
    let theme = active_theme();
    let store = crate::core::stats::load();
    let cep = &store.cep;
    let live = load_mcp_live();
    let mut out = Vec::new();
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();

    if cep.sessions == 0 && live.is_none() {
        // A short / phase-isolated bridge run can do real work (commands, proxy
        // turns, token savings) before any per-session CEP snapshot lands. Don't
        // mislabel that as "nothing recorded" — point at the meter that has the
        // real numbers (#361).
        let proxy_turns = crate::proxy::metrics::load_persisted().map_or(0, |m| m.requests_total);
        if store.total_commands > 0 || proxy_turns > 0 {
            return format!(
                "{dim}No per-session CEP snapshot yet, but lean-ctx is active \
                 ({cmds} commands, {proxy_turns} proxy turns).{rst}\n\
                 Run `lean-ctx gain` for token savings and net-of-injection bill impact.",
                cmds = store.total_commands,
            );
        }
        return format!(
            "{dim}No CEP sessions recorded yet.{rst}\n\
             Use lean-ctx as an MCP server in your editor to start tracking.\n\
             CEP metrics are recorded automatically during MCP sessions."
        );
    }

    if cep.sessions == 0
        && let Some(ref lv) = live
    {
        return format_cep_live(lv, &theme);
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
        cep.scores.iter().map(|s| f64::from(s.score)).sum::<f64>() / cep.scores.len() as f64
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

    let score_ratio = (f64::from(latest_score) / 100.0).min(1.0);
    let score_bar = theme.gradient_bar(score_ratio, 20);
    let score_pc = theme.pct_color(f64::from(latest_score));

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

        let score_values: Vec<u64> = cep.scores.iter().map(|s| u64::from(s.score)).collect();
        // Cap to the most recent points so the sparkline fits inside the box.
        let spark_vals: Vec<u64> = score_values.iter().rev().take(54).rev().copied().collect();
        let spark = theme.gradient_sparkline(&spark_vals);
        out.push(cep_line(&format!("  {spark}")));

        let recent: Vec<_> = cep.scores.iter().rev().take(5).collect();
        for snap in recent.iter().rev() {
            let ts = snap.timestamp.get(..16).unwrap_or(&snap.timestamp);
            let pc = theme.pct_color(f64::from(snap.score));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// #361: the live-stats reader must look in the same category the writer uses
    /// (now `state_dir()`, GH #408), honoring the configured dirs — not a
    /// hardcoded `~/.lean-ctx`. Otherwise `gain` / `/lean-ctx` report 0.
    #[test]
    fn load_mcp_live_reads_from_configured_state_dir() {
        let iso = crate::core::data_dir::isolated_data_dir();
        std::fs::write(
            iso.path().join("mcp-live.json"),
            r#"{"cep_score":42,"tokens_saved":123}"#,
        )
        .unwrap();

        let live = load_mcp_live().expect("live stats must load from the configured state dir");
        assert_eq!(
            live.get("cep_score").and_then(serde_json::Value::as_u64),
            Some(42)
        );
    }

    #[test]
    fn load_mcp_live_none_when_file_absent() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        assert!(load_mcp_live().is_none(), "no mcp-live.json → None");
    }
}
