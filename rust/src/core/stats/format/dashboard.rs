//! The `gain` hero dashboard: themed savings panels, footer, tips, live mode.

use super::util::{
    active_theme, cmd_total_saved, day_total_saved, format_big, format_num, format_usd,
    truncate_cmd,
};
use crate::core::theme::{self, Theme};

use super::super::model::{CostModel, StatsStore};

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
    let store = crate::core::stats::load();
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
    // One summary load powers both the score panel and the net-of-injection
    // reconciliation below (#361) — no double compute.
    let summary = engine.summary(None);
    let score = &summary.score;

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

    // Net-of-injection honesty (#361): the headline above is gross savings on
    // lean-ctx-touched traffic. lean-ctx also injects a fixed per-turn prefix
    // that, without provider prompt caching, is re-billed every turn — so the
    // honest bill impact is gross minus that tax. Show it in the default view,
    // not just in `--json` / `--deep`, so the hero never overstates the effect.
    if summary.turns > 0 {
        let net = summary.net_tokens_saved;
        let net_str = format_big(net.unsigned_abs());
        let sign = if net < 0 { "-" } else { "" };
        let net_col = if net < 0 { t.warning.fg() } else { c1.clone() };
        out.push(box_line(""));
        out.push(box_line(&format!(
            "  {dim}net of injection:{rst} {net_col}{bold}{sign}{net_str}{rst} {dim}(− {tax} tax · {turns} turns){rst}",
            tax = format_big(summary.injected_overhead_total_tokens),
            turns = summary.turns,
        )));
    } else if summary.injected_overhead_tokens_per_turn > 0 {
        out.push(box_line(""));
        out.push(box_line(&format!(
            "  {dim}injection:{rst} {dim}+{op}/turn fixed (net = gross; proxy not in path){rst}",
            op = format_big(summary.injected_overhead_tokens_per_turn),
        )));
    }

    out.push(format!("  {}", t.box_bottom(w)));
    // One-line methodology so the headline is never read as the whole bill.
    out.push(format!(
        "  {dim}savings = compression on lean-ctx-touched traffic, not your full provider bill · details: lean-ctx gain --deep{rst}"
    ));
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
    let store = crate::core::stats::load();
    let mut out = Vec::new();
    append_gain_footer(&mut out, &active_theme(), &store);
    out.join("\n")
}

#[allow(clippy::many_single_char_names)] // ANSI formatting: t=theme, r=reset, b=bold, d=dim
fn gain_dashboard(t: &Theme, tick: Option<u64>, with_footer: bool) -> String {
    let store = crate::core::stats::load();
    let mut out = Vec::new();
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();

    if store.total_commands == 0 {
        let data_dir = match crate::core::data_dir::lean_ctx_data_dir() {
            Ok(p) => p.display().to_string(),
            Err(_) => "~/.config/lean-ctx".into(),
        };
        // `mcp-live.json` is STATE (GH #408); read it from the state dir.
        let mcp_live = crate::core::paths::state_dir().map_or_else(
            |_| std::path::Path::new(&data_dir).join("mcp-live.json"),
            |d| d.join("mcp-live.json"),
        );
        let mcp_hint = if let Ok(live) = std::fs::read_to_string(&mcp_live) {
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
            // Pad the bar to a fixed width so the trailing version column lines up
            // (matches the BY COMMAND bar above; gradient_bar can return < width).
            let day_bar = theme::pad_right(&t.gradient_bar(ratio, 8), 8);
            let date_short = day.date.get(5..).unwrap_or(&day.date);
            let date_col = theme::pad_right(&format!("{m}{date_short}{rst}", m = t.muted.fg()), 7);
            // Per-day input volume (self-labeled "… in") makes the volume-weighted
            // nature of the % explicit: a lower day-% usually reflects a smaller /
            // less-compressible workload (e.g. fewer high-ratio grep/search calls),
            // not worse compression. Without it the % drop reads as a regression
            // when it is really composition (GL #622).
            let in_col = theme::pad_right(
                &format!(
                    "{m}{} in{rst}",
                    format_big(day.input_tokens),
                    m = t.muted.fg()
                ),
                11,
            );
            let saved_col =
                theme::pad_right(&format!("{pc}{bold}{}{rst}", format_big(day_saved)), 9);
            // Per-day version attributes a compression change to a specific
            // release (#307); pre-tracking days carry no version and show "—".
            let ver = if day.version.is_empty() {
                "—".to_string()
            } else {
                format!("v{}", day.version)
            };
            out.push(sec_line(&format!(
                "  {date_col} {:>4} cmds  {in_col} {saved_col} {pc}{day_pct:>5.1}%{rst}  {day_bar}  {dim}{ver}{rst}",
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

    {
        // Methodology disclosure (#361): the headline measures compression on
        // lean-ctx-touched traffic, not the full provider bill — and lean-ctx
        // itself injects a fixed per-turn prefix that, without provider prompt
        // caching, is re-billed every turn. State both so the number stays honest.
        let a = t.accent.fg();
        let m = t.muted.fg();
        let overhead = crate::core::context_overhead::ContextOverhead::cached();
        out.push(format!("    {a}📐 Methodology{rst}"));
        out.push(format!(
            "    {m}   Savings = compression on lean-ctx-touched traffic (reads + shell),{rst}"
        ));
        out.push(format!(
            "    {m}   not your full provider bill. lean-ctx adds ~{} tok/turn of context{rst}",
            overhead.total_tokens(),
        ));
        out.push(format!(
            "    {m}   ({} tool schemas + instructions + rules); without provider prompt{rst}",
            overhead.tool_count,
        ));
        out.push(format!(
            "    {m}   caching that rides every turn → net = savings − overhead × turns.{rst}"
        ));
        out.push(String::new());
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
