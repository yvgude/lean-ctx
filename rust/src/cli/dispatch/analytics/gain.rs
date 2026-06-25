//! `lean-ctx gain` — the savings dashboard and Wrapped share cards,
//! plus the `--opportunity` and `--raw` sub-views it absorbs.

use crate::{core, tools};

pub(in crate::cli::dispatch) fn cmd_gain(rest: &[String]) {
    // Keep the latest-version cache fresh (#563): non-blocking, 24h-TTL
    // guarded, opt-out respected. Without a CLI-side trigger the cache only
    // refreshes on MCP maintenance ticks and can freeze on old versions.
    core::version_check::check_background();
    if rest.iter().any(|a| a == "--reset") {
        core::stats::reset_all();
        println!("Stats reset. All token savings data cleared.");
        return;
    }
    if rest.iter().any(|a| a == "--live") {
        core::stats::gain_live();
        return;
    }
    let model = rest.iter().enumerate().find_map(|(i, a)| {
        if let Some(v) = a.strip_prefix("--model=") {
            return Some(v.to_string());
        }
        if a == "--model" {
            return rest.get(i + 1).cloned();
        }
        None
    });
    let period = rest
        .iter()
        .enumerate()
        .find_map(|(i, a)| {
            if let Some(v) = a.strip_prefix("--period=") {
                return Some(v.to_string());
            }
            if a == "--period" {
                return rest.get(i + 1).cloned();
            }
            None
        })
        .unwrap_or_else(|| "all".to_string());
    let limit = rest
        .iter()
        .enumerate()
        .find_map(|(i, a)| {
            if let Some(v) = a.strip_prefix("--limit=") {
                return v.parse::<usize>().ok();
            }
            if a == "--limit" {
                return rest.get(i + 1).and_then(|v| v.parse::<usize>().ok());
            }
            None
        })
        .unwrap_or(10);

    let copy = rest.iter().any(|a| a == "--copy");
    let open = rest.iter().any(|a| a == "--open");

    if let Some(req) = unpublish_request(rest) {
        let id = match &req {
            UnpublishReq::Id(s) => Some(s.as_str()),
            UnpublishReq::Latest => None,
        };
        crate::cli::wrapped_publish::unpublish(id);
        return;
    }
    if rest.iter().any(|a| a == "--publish") {
        let leaderboard = rest.iter().any(|a| a == "--leaderboard");
        crate::cli::wrapped_publish::publish(&period, name_arg(rest).as_deref(), leaderboard);
        return;
    }

    if let Some(svg_path) = svg_target(rest) {
        let report = core::wrapped::WrappedReport::generate(&period);
        match std::fs::write(&svg_path, report.to_svg()) {
            Ok(()) => println!(
                "Wrapped card written to {svg_path}\n\
                 Share it directly, open it in a browser, or convert to PNG with any SVG tool."
            ),
            Err(e) => {
                eprintln!("Failed to write {svg_path}: {e}");
                std::process::exit(1);
            }
        }
        share_side_effects(&report, Some(&svg_path), copy, open);
        return;
    }

    if let Some(html_path) = share_target(rest) {
        let report = core::wrapped::WrappedReport::generate(&period);
        let base = base_url_arg(rest);
        match std::fs::write(&html_path, report.to_share_html(base.as_deref())) {
            Ok(()) => {
                println!("Share page written to {html_path}");
                if base.is_some() {
                    println!(
                        "Host it at your base URL; for social preview cards, place a PNG \
                         (rasterise the SVG) at <base>/lean-ctx-wrapped.png."
                    );
                } else {
                    println!(
                        "Self-contained (SVG embedded) — host it anywhere to get a permalink. \
                         Pass --base-url=https://… to emit social preview meta."
                    );
                }
            }
            Err(e) => {
                eprintln!("Failed to write {html_path}: {e}");
                std::process::exit(1);
            }
        }
        share_side_effects(&report, Some(&html_path), copy, open);
        return;
    }

    if copy {
        let report = core::wrapped::WrappedReport::generate(&period);
        share_side_effects(&report, None, true, false);
        return;
    }

    if rest.iter().any(|a| a == "--graph") {
        println!("{}", core::stats::format_gain_graph());
    } else if rest.iter().any(|a| a == "--daily") {
        println!("{}", core::stats::format_gain_daily());
    } else if rest.iter().any(|a| a == "--json") {
        println!(
            "{}",
            tools::ctx_gain::handle("json", Some(&period), model.as_deref(), Some(limit))
        );
    } else if rest.iter().any(|a| a == "--score") {
        println!(
            "{}",
            tools::ctx_gain::handle("score", None, model.as_deref(), Some(limit))
        );
    } else if rest.iter().any(|a| a == "--cost") {
        println!(
            "{}",
            tools::ctx_gain::handle("cost", None, model.as_deref(), Some(limit))
        );
        print_measured_spend_hint();
    } else if rest.iter().any(|a| a == "--tasks") {
        println!(
            "{}",
            tools::ctx_gain::handle("tasks", None, None, Some(limit))
        );
    } else if rest.iter().any(|a| a == "--agents") {
        println!(
            "{}",
            tools::ctx_gain::handle("agents", None, None, Some(limit))
        );
    } else if rest.iter().any(|a| a == "--heatmap") {
        println!(
            "{}",
            tools::ctx_gain::handle("heatmap", None, None, Some(limit))
        );
    } else if rest.iter().any(|a| a == "--wrapped") {
        println!(
            "{}",
            tools::ctx_gain::handle("wrapped", Some(&period), model.as_deref(), Some(limit))
        );
        // Interactive publish prompt (if TTY and not already published)
        if !rest.iter().any(|a| a == "--publish")
            && std::io::IsTerminal::is_terminal(&std::io::stdin())
            && !crate::cli::wrapped_publish::has_published()
        {
            eprint!("\n  Publish this card? [y/N/leaderboard] ");
            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_ok() {
                let answer = input.trim().to_lowercase();
                match answer.as_str() {
                    "y" | "yes" => {
                        let cfg = crate::core::config::Config::load();
                        crate::cli::wrapped_publish::publish(
                            &period,
                            cfg.gain.display_name.as_deref(),
                            cfg.gain.leaderboard,
                        );
                    }
                    "l" | "leaderboard" => {
                        let cfg = crate::core::config::Config::load();
                        crate::cli::wrapped_publish::publish(
                            &period,
                            cfg.gain.display_name.as_deref(),
                            true,
                        );
                    }
                    _ => {}
                }
            }
        } else {
            crate::cli::wrapped_publish::maybe_auto_publish(&period);
        }
    } else if rest.iter().any(|a| a == "--pipeline") {
        let stats_path = crate::core::paths::state_dir()
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".lean-ctx"))
            .join("pipeline_stats.json");
        if let Ok(data) = std::fs::read_to_string(&stats_path) {
            if let Ok(stats) = serde_json::from_str::<core::pipeline::PipelineStats>(&data) {
                println!("{}", stats.format_summary());
            } else {
                println!("No pipeline stats available yet (corrupt data).");
            }
        } else {
            println!("No pipeline stats available yet. Use MCP tools to generate data.");
        }
    } else if rest.iter().any(|a| a == "--deep") {
        // Body first, then the extra themed sections, then the footer — so the
        // tips / Context OS panel land at the very end instead of mid-dashboard.
        println!("{}", core::stats::format_gain_body());
        print!(
            "{}",
            tools::ctx_gain::format_deep_themed(model.as_deref(), limit)
        );
        println!("{}", core::stats::format_gain_footer());
    } else if rest.iter().any(|a| a == "--opportunity") {
        cmd_opportunity();
    } else if rest.iter().any(|a| a == "--raw") {
        cmd_stats_raw(rest);
    } else {
        println!("{}", core::stats::format_gain_hero());
        // Surface available updates where users actually look (#563) — the
        // banner functions existed but had no caller, so terminals never
        // showed update hints at all.
        if let Some(banner) = core::version_check::get_update_banner() {
            println!("\n{banner}");
        }
        print_support_hint();
        print_bridge_warning();
        print_split_hint();
        crate::cli::wrapped_publish::maybe_auto_publish(&period);
        print_community_hint();
    }
}

/// `gain --cost` values savings with a *resolved* model (estimated). When the
/// proxy has recorded real provider usage, point at the measured `spend` view so
/// the user knows the exact bill is one command away.
fn print_measured_spend_hint() {
    if !crate::proxy::usage_meter::persisted_snapshot().is_empty() {
        eprintln!(
            "\n  \x1b[2m💡 Measured provider spend available — run `lean-ctx spend` for the real per-model bill.\x1b[0m"
        );
    }
}

/// When the bridge is not engaged, the proxy is no longer feeding live request
/// stats. Surface that caveat so `gain` never implies savings it cannot measure
/// (GitHub #361/#271) — **but only when there are genuinely no savings to show**.
///
/// `gain` also measures MCP-tool savings directly (reads/search/shell via the
/// savings ledger + stats), which need no proxy at all. Emitting a "proxy down —
/// savings cannot be measured" line above a real, MCP-measured headline wrongly
/// told MCP-only users their numbers were untrustworthy (#500). Gate on the
/// displayed total so the warning fires only for a true zero.
fn print_bridge_warning() {
    use crate::core::gain::bridge_status::{BridgeEngagement, BridgeStatus};
    let store = core::stats::load_for_display();
    let saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens);
    if saved > 0 {
        return;
    }
    let bridge = BridgeStatus::detect();
    if bridge.engagement != BridgeEngagement::Engaged {
        eprintln!("\n  \x1b[33m⚠ {}\x1b[0m", bridge.summary_line());
    }
}

/// When stats live in more than one auto-resolved data dir, `gain` now sums them
/// for display (#500) — but each process still *writes* to its own dir, so the
/// split persists. Nudge toward consolidation once, non-alarmingly, since the
/// headline is already correct. Suppressed when `LEAN_CTX_DATA_DIR` pins one dir.
fn print_split_hint() {
    if std::env::var_os("LEAN_CTX_DATA_DIR").is_some() {
        return;
    }
    let dirs = core::data_dir::all_data_dirs_with_stats();
    if dirs.len() >= 2 {
        eprintln!(
            "\n  \x1b[2m💡 Stats span {} data dirs (summed above). Consolidate them with `lean-ctx doctor`.\x1b[0m",
            dirs.len()
        );
    }
}

fn print_community_hint() {
    let s = core::savings_ledger::summary();
    if s.total_events == 0 {
        return;
    }

    let on_board = crate::cli::wrapped_publish::has_leaderboard_entry();
    let published = crate::cli::wrapped_publish::has_published();
    let has_name = crate::core::config::Config::load()
        .gain
        .display_name
        .is_some();

    // State-aware nudge so the path to https://leanctx.com/metrics — and how to set a
    // display name or unpublish — is always one copy-pasteable line away. Every state
    // names both the public command and the way back out (community ask: a clear
    // "how to publish/unpublish" line in the normal `gain` output).
    let body = if on_board && has_name {
        "💡 On the public leaderboard (https://leanctx.com/metrics).  \
         Refresh:  lean-ctx gain --publish --leaderboard   ·   Remove:  lean-ctx gain --unpublish"
            .to_string()
    } else if on_board {
        // Listed but nameless → shows as "anonymous"; spell out the exact missing step.
        "💡 You're on the leaderboard as \"anonymous\". Claim your handle:\n     \
         lean-ctx gain --publish --leaderboard --name=\"your handle\""
            .to_string()
    } else if published {
        "💡 You're published privately. List on the public leaderboard at https://leanctx.com/metrics:\n     \
         lean-ctx gain --publish --leaderboard --name=\"your handle\"   ·   Remove:  lean-ctx gain --unpublish"
            .to_string()
    } else {
        "💡 Join the public leaderboard at https://leanctx.com/metrics (opt-in — shares only 4 aggregate totals, never your code):\n     \
         Publish:  lean-ctx gain --publish --leaderboard --name=\"your handle\"   ·   Remove:  lean-ctx gain --unpublish"
            .to_string()
    };
    eprintln!("\n  \x1b[2m{body}\x1b[0m");
}

/// A prominent, friendly nudge to support lean-ctx financially. The engine is
/// free and never gated — this is the single place the default `gain` view asks
/// for support, rendered right under the savings hero so the people who benefit
/// most see the most natural moment to give back.
fn print_support_hint() {
    use crate::core::theme;
    let t = theme::load_theme(&crate::core::config::Config::load().theme);
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();
    let acc = t.accent.fg();
    let heart = t.danger.fg();
    let w = 57;
    let side = t.box_side();
    let row = |content: &str| -> String {
        let padded = theme::pad_right(content, w);
        format!("  {side}{padded}{side}")
    };

    println!();
    println!("  {}", t.box_top(w));
    println!(
        "{}",
        row(&format!(
            "  {heart}\u{2665}{rst}  {bold}Enjoying lean-ctx? Keep it free & independent.{rst}"
        ))
    );
    println!("{}", row(""));
    println!(
        "{}",
        row(&format!(
            "  {dim}Back development for the price of a coffee \u{2192}{rst}"
        ))
    );
    println!(
        "{}",
        row(&format!("  {acc}{bold}https://leanctx.com/support{rst}"))
    );
    println!("  {}", t.box_bottom(w));
}

/// Resolves the output path for the shareable SVG Wrapped card, or `None` when no
/// card was requested. Accepts `--svg`, `--svg=<path>`, `--card`, `--card=<path>`;
/// a bare flag falls back to `lean-ctx-wrapped.svg` in the current directory.
/// A requested `--unpublish`: either an explicit card id, or the most recent published card.
enum UnpublishReq {
    Latest,
    Id(String),
}

/// Parses `--unpublish[=<id>]`. `None` means it was not requested at all.
fn unpublish_request(rest: &[String]) -> Option<UnpublishReq> {
    for a in rest {
        if let Some(v) = a.strip_prefix("--unpublish=") {
            return Some(UnpublishReq::Id(v.to_string()));
        }
        if a == "--unpublish" {
            return Some(UnpublishReq::Latest);
        }
    }
    None
}

/// Parses `--name=<display>` / `--name <display>` for the optional publish display label.
fn name_arg(rest: &[String]) -> Option<String> {
    rest.iter().enumerate().find_map(|(i, a)| {
        if let Some(v) = a.strip_prefix("--name=") {
            return Some(v.to_string());
        }
        if a == "--name" {
            return rest.get(i + 1).cloned();
        }
        None
    })
}

fn svg_target(rest: &[String]) -> Option<String> {
    let mut requested = false;
    let mut path: Option<String> = None;
    for (i, a) in rest.iter().enumerate() {
        if let Some(v) = a
            .strip_prefix("--svg=")
            .or_else(|| a.strip_prefix("--card="))
        {
            requested = true;
            path = Some(v.to_string());
        } else if a == "--svg" || a == "--card" {
            requested = true;
            if let Some(next) = rest.get(i + 1)
                && !next.starts_with('-')
            {
                path = Some(next.clone());
            }
        }
    }
    requested.then(|| path.unwrap_or_else(|| "lean-ctx-wrapped.svg".to_string()))
}

/// Resolves the output path for the self-hostable HTML share page, or `None` when not
/// requested. Accepts `--share`, `--share=<path>`, `--page`, `--page=<path>`; a bare flag
/// falls back to `lean-ctx-wrapped.html`.
fn share_target(rest: &[String]) -> Option<String> {
    let mut requested = false;
    let mut path: Option<String> = None;
    for (i, a) in rest.iter().enumerate() {
        if let Some(v) = a
            .strip_prefix("--share=")
            .or_else(|| a.strip_prefix("--page="))
        {
            requested = true;
            path = Some(v.to_string());
        } else if a == "--share" || a == "--page" {
            requested = true;
            if let Some(next) = rest.get(i + 1)
                && !next.starts_with('-')
            {
                path = Some(next.clone());
            }
        }
    }
    requested.then(|| path.unwrap_or_else(|| "lean-ctx-wrapped.html".to_string()))
}

/// Reads `--base-url=<url>` / `--base-url <url>` (the location the share page will be
/// hosted at, used for absolute social-preview image meta).
fn base_url_arg(rest: &[String]) -> Option<String> {
    rest.iter().enumerate().find_map(|(i, a)| {
        if let Some(v) = a.strip_prefix("--base-url=") {
            return Some(v.to_string());
        }
        if a == "--base-url" {
            return rest.get(i + 1).cloned();
        }
        None
    })
}

/// Applies the optional `--copy` (clipboard) and `--open` (browser) side effects for a
/// Wrapped artifact. `path` is the just-written file to open, when one exists. Both
/// degrade gracefully: a clipboard miss falls back to printing the share line.
fn share_side_effects(
    report: &core::wrapped::WrappedReport,
    path: Option<&str>,
    copy: bool,
    open: bool,
) {
    if copy {
        let text = report.share_text(None);
        if core::share::copy_to_clipboard(&text) {
            println!("Copied to clipboard:  {text}");
        } else {
            println!("Share text (copy it): {text}");
        }
    }
    if open && let Some(p) = path {
        if core::share::open_in_browser(p) {
            println!("Opened {p}");
        } else {
            println!("Could not open {p} automatically — open it manually.");
        }
    }
}

/// `gain --opportunity` — merged discover + ghost (opportunity report).
fn cmd_opportunity() {
    use crate::core::theme;

    let t = {
        let cfg = crate::core::config::Config::load();
        theme::load_theme(&cfg.theme)
    };
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();
    let w = 57;

    let history = crate::cli::common::load_shell_history();
    let result = crate::tools::ctx_discover::analyze_history(&history, 10);

    let store = core::stats::load();
    let optimized_cmds = store.total_commands;

    if result.missed_commands.is_empty() {
        println!(
            "\n  {sc}{bold}All compressible commands are already using lean-ctx!{rst}\n  {dim}{optimized_cmds} commands optimized.{rst}\n",
            sc = t.success.fg(),
        );
        return;
    }

    let total_missed: u64 = result
        .missed_commands
        .iter()
        .map(|c| c.estimated_tokens as u64)
        .sum();
    let total_count: u64 = result
        .missed_commands
        .iter()
        .map(|c| u64::from(c.count))
        .sum();

    println!();
    println!("  {}", t.box_top(w));
    let side = t.box_side();
    let padded = |s: &str| -> String {
        let p = theme::pad_right(s, w);
        format!("  {side}{p}{side}")
    };
    println!(
        "{}",
        padded(&format!("  {bold}{}Opportunity Report{rst}", t.accent.fg()))
    );
    println!("{}", padded(""));
    println!(
        "{}",
        padded(&format!(
            "  {bold}~{}{rst} {dim}tokens/month going uncompressed{rst}",
            crate::core::wrapped::format_tokens(total_missed),
        ))
    );
    println!("{}", padded(""));

    for entry in result.missed_commands.iter().take(8) {
        let line = format!(
            "  {dim}{:<12}{rst} {:>4}x   {bold}~{}{rst} {dim}tokens recoverable{rst}",
            entry.prefix,
            entry.count,
            crate::core::wrapped::format_tokens(entry.estimated_tokens as u64),
        );
        println!("{}", padded(&line));
    }

    println!("{}", padded(""));
    let opt_line = format!(
        "  {dim}Already optimized: {optimized_cmds} commands ({pct}%){rst}",
        pct = total_count
            .checked_add(optimized_cmds)
            .and_then(|total| optimized_cmds.checked_mul(100)?.checked_div(total))
            .unwrap_or(100)
    );
    println!("{}", padded(&opt_line));
    println!("  {}", t.box_bottom(w));
    println!();
    let sec = t.secondary.fg();
    println!("  {sec}lean-ctx init --global{rst}   {dim}Compress everything{rst}");
    println!();
}

/// `gain --raw` — plain stats (absorbs former `lean-ctx stats` command).
fn cmd_stats_raw(rest: &[String]) {
    if rest.iter().any(|a| a == "--json" || a == "json") {
        let store = core::stats::load();
        println!(
            "{}",
            serde_json::to_string_pretty(&store).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        let store = core::stats::load();
        let input_saved = store
            .total_input_tokens
            .saturating_sub(store.total_output_tokens);
        let pct = if store.total_input_tokens > 0 {
            input_saved as f64 / store.total_input_tokens as f64 * 100.0
        } else {
            0.0
        };
        println!("Commands:    {}", store.total_commands);
        println!("Input:       {} tokens", store.total_input_tokens);
        println!("Output:      {} tokens", store.total_output_tokens);
        println!("Saved:       {input_saved} tokens ({pct:.1}%)");
        println!();
        println!("CEP sessions:  {}", store.cep.sessions);
        println!(
            "CEP tokens:    {} → {}",
            store.cep.total_tokens_original, store.cep.total_tokens_compressed
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{base_url_arg, share_target, svg_target};

    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_flag_means_no_card() {
        assert_eq!(svg_target(&args(&["--wrapped", "--period=month"])), None);
    }

    #[test]
    fn bare_flag_uses_default_path() {
        assert_eq!(
            svg_target(&args(&["--svg"])).as_deref(),
            Some("lean-ctx-wrapped.svg")
        );
        assert_eq!(
            svg_target(&args(&["--card"])).as_deref(),
            Some("lean-ctx-wrapped.svg")
        );
    }

    #[test]
    fn explicit_path_is_used() {
        assert_eq!(
            svg_target(&args(&["--svg=out.svg"])).as_deref(),
            Some("out.svg")
        );
        assert_eq!(
            svg_target(&args(&["--card=c.svg"])).as_deref(),
            Some("c.svg")
        );
        assert_eq!(
            svg_target(&args(&["--svg", "chosen.svg"])).as_deref(),
            Some("chosen.svg")
        );
    }

    #[test]
    fn following_flag_is_not_consumed_as_path() {
        assert_eq!(
            svg_target(&args(&["--svg", "--period=week"])).as_deref(),
            Some("lean-ctx-wrapped.svg")
        );
    }

    #[test]
    fn share_target_resolves_paths() {
        assert_eq!(share_target(&args(&["--wrapped"])), None);
        assert_eq!(
            share_target(&args(&["--share"])).as_deref(),
            Some("lean-ctx-wrapped.html")
        );
        assert_eq!(
            share_target(&args(&["--page=out.html"])).as_deref(),
            Some("out.html")
        );
        assert_eq!(
            share_target(&args(&["--share", "--base-url=https://x"])).as_deref(),
            Some("lean-ctx-wrapped.html"),
            "a following flag must not be eaten as the path"
        );
    }

    #[test]
    fn base_url_is_parsed_both_forms() {
        assert_eq!(
            base_url_arg(&args(&["--base-url=https://me.dev"])).as_deref(),
            Some("https://me.dev")
        );
        assert_eq!(
            base_url_arg(&args(&["--base-url", "https://me.dev"])).as_deref(),
            Some("https://me.dev")
        );
        assert_eq!(base_url_arg(&args(&["--share"])), None);
    }
}
