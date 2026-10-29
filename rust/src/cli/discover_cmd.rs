use std::fmt::Write as _;

use super::common::load_shell_history;

pub fn cmd_discover(args: &[String]) {
    let history = load_shell_history();
    if history.is_empty() {
        println!("No shell history found.");
        return;
    }

    let result = crate::tools::ctx_discover::analyze_history(&history, 20);

    if let Some(path) = card_target(args) {
        match std::fs::write(
            &path,
            crate::tools::ctx_discover::render_before_card(&result),
        ) {
            Ok(()) => println!(
                "Before-card written to {path}\n\
                 Share it, or open `lean-ctx cockpit` after a week for your real numbers \
                 (shareable card: `lean-ctx gain --wrapped`)."
            ),
            Err(e) => {
                eprintln!("Failed to write {path}: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let report = crate::tools::ctx_discover::format_cli_output(&result);
    super::common::print_released(&released_report(
        &super::common::detect_project_root(args),
        report,
    ));
}

/// Print a report that ends with its own `\n` (the former `print!`): it is
/// released without that final newline, which the one observed writer adds
/// back, so the bytes on stdout are the same and the hand-over is observed.
fn print_report(out: &str) {
    let report = out.strip_suffix('\n').unwrap_or(out).to_owned();
    super::common::print_released(&released_report(
        &super::common::detect_project_root(&[]),
        report,
    ));
}

/// The discover, ghost and first-run reports are statistics about the user's
/// own shell history (admitted where it was loaded). They name command
/// families — the first word of a command line — next to counters, so they
/// are reports for the person at the terminal: released like every output,
/// never recorded as a delivery of project sources.
fn released_report(project_root: &str, report: String) -> String {
    super::common::CliToolRun::new("ctx_discover", project_root, None)
        .human_facing()
        .deliver_text(|| report)
}

/// Resolves the `--card[=<path>]` output path for the shareable "before" SVG, or `None`
/// when not requested. A bare `--card` defaults to `lean-ctx-before.svg`.
fn card_target(args: &[String]) -> Option<String> {
    let mut requested = false;
    let mut path: Option<String> = None;
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix("--card=") {
            requested = true;
            path = Some(v.to_string());
        } else if a == "--card" {
            requested = true;
            if let Some(next) = args.get(i + 1)
                && !next.starts_with('-')
            {
                path = Some(next.clone());
            }
        }
    }
    requested.then(|| path.unwrap_or_else(|| "lean-ctx-before.svg".to_string()))
}

/// Path of the marker recording that the first-run "aha" was already shown.
fn first_run_marker() -> Option<std::path::PathBuf> {
    crate::core::paths::cache_dir()
        .ok()
        .map(|d| d.join(".first_run_wow_done"))
}

/// Shows the discover "you're leaving X tokens on the table" moment exactly once, right
/// after setup. Marker-guarded so re-running `setup` never repeats it. Stays silent (but
/// still marks done) when there is too little history to be meaningful, so it never nags.
/// Reads only local shell history and prints aggregate estimates — never command contents.
pub fn show_first_run_wow() {
    let Some(marker) = first_run_marker() else {
        return;
    };
    if marker.exists() {
        return;
    }
    // Mark immediately so any edge case never reshows on the next setup run.
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, "shown\n");

    let history = load_shell_history();
    if history.is_empty() {
        return;
    }
    let result = crate::tools::ctx_discover::analyze_history(&history, 20);
    let total_missed: u32 = result.missed_commands.iter().map(|m| m.count).sum();
    if total_missed == 0 {
        return;
    }

    let bold = "\x1b[1m";
    let green = "\x1b[32m";
    let yellow = "\x1b[33m";
    let dim = "\x1b[2m";
    let rst = "\x1b[0m";

    let top = result
        .missed_commands
        .iter()
        .take(3)
        .map(|m| format!("{} {}x", m.prefix, m.count))
        .collect::<Vec<_>>()
        .join("   ");
    let mut out = String::new();

    out.push('\n');
    let _ = writeln!(
        out,
        "  {bold}Here's what lean-ctx is about to start saving you{rst}"
    );
    let _ = writeln!(
        out,
        "  {dim}estimated from your shell history — nothing leaves your machine{rst}"
    );
    out.push('\n');
    if result.has_measured_data {
        // Returning user: project their own measured savings onto frequency.
        let monthly = result.potential_usd * 30.0;
        let saved = crate::core::wrapped::format_tokens(result.potential_tokens as u64);
        let _ = writeln!(
            out,
            "  {green}~{saved} tokens{rst}{dim}/month{rst} across {total_missed} uncompressed commands {dim}(~${monthly:.0}/mo, from your measured rate){rst}"
        );
    } else {
        // Fresh install: no measurements yet — show frequency, never a fake $.
        let _ = writeln!(
            out,
            "  {green}{total_missed} commands{rst} {dim}that lean-ctx will now compress automatically{rst}"
        );
    }
    if !top.is_empty() {
        let _ = writeln!(out, "  {dim}top: {top}{rst}");
    }
    out.push('\n');
    let _ = writeln!(
        out,
        "  {yellow}Open {bold}lean-ctx cockpit{rst}{yellow} after a week for your real numbers — {bold}lean-ctx gain --wrapped{rst}{yellow} makes a shareable card.{rst}"
    );
    out.push('\n');
    print_report(&out);
}

pub fn cmd_ghost(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");

    let history = load_shell_history();
    let discover = crate::tools::ctx_discover::analyze_history(&history, 20);

    let session = crate::core::session::SessionState::load_latest();
    let store = crate::core::stats::load();

    let unoptimized_tokens = discover.potential_tokens;
    let _unoptimized_usd = discover.potential_usd;

    // Redundant reads: every cache hit is a re-read lean-ctx served from cache
    // instead of re-sending the file. Value it at the user's *measured* average
    // read size (from the heatmap), never a guessed constant. No heatmap data
    // yet => 0, so we never invent a figure.
    let redundant_reads = store.cep.total_cache_hits as usize;
    let avg_read_tokens = crate::core::heatmap::HeatMap::load()
        .avg_original_tokens_per_access()
        .unwrap_or(0) as usize;
    let redundant_tokens = redundant_reads.saturating_mul(avg_read_tokens);

    // NOTE: the former "oversized contexts" category was dropped — it was a
    // fabricated wasted_original/3 heuristic applied to realized (not wasted)
    // compression savings. The JSON keeps `truncated_contexts: 0` for schema
    // stability; there is no honest data source to revive it.
    let total_ghost = unoptimized_tokens + redundant_tokens;
    let total_usd =
        total_ghost as f64 * crate::core::stats::DEFAULT_INPUT_PRICE_PER_M / 1_000_000.0;
    let monthly_usd = total_usd * 30.0;

    if json {
        let obj = serde_json::json!({
            "ghost_tokens": total_ghost,
            "breakdown": {
                "unoptimized_shells": unoptimized_tokens,
                "redundant_reads": redundant_tokens,
                "truncated_contexts": 0,
            },
            "estimated_usd": total_usd,
            "monthly_usd": monthly_usd,
            "session_active": session.is_some(),
            "history_commands": discover.total_commands,
            "already_optimized": discover.already_optimized,
        });
        // Counters only, but a stdout document of this command like the
        // report below: released as a document and receipted (G08).
        let document = serde_json::to_string_pretty(&obj).unwrap_or_default();
        let root = super::common::detect_project_root(&[]);
        match super::common::CliToolRun::new("ctx_discover", &root, None)
            .human_facing()
            .deliver_json(|| document)
        {
            Ok(document) => super::common::print_released(&document),
            Err(reason) => super::common::exit_withheld(&reason),
        }
        return;
    }

    let bold = "\x1b[1m";
    let green = "\x1b[32m";
    let yellow = "\x1b[33m";
    let red = "\x1b[31m";
    let dim = "\x1b[2m";
    let rst = "\x1b[0m";
    let white = "\x1b[97m";
    let mut out = String::new();

    out.push('\n');
    let _ = writeln!(out, "  {bold}{white}lean-ctx ghost report{rst}");
    let _ = writeln!(out, "  {dim}{}{rst}", "=".repeat(40));
    out.push('\n');

    if total_ghost == 0 {
        let _ = writeln!(out, "  {green}No ghost tokens detected!{rst}");
        let _ = writeln!(
            out,
            "  {dim}All {} commands optimized.{rst}",
            discover.total_commands
        );
        out.push('\n');
        print!(
            "{}",
            released_report(&super::common::detect_project_root(&[]), out)
        );
        return;
    }

    let severity = if total_ghost > 10000 {
        red
    } else if total_ghost > 3000 {
        yellow
    } else {
        green
    };

    let _ = writeln!(
        out,
        "  {bold}Ghost Tokens found:{rst}     {severity}{total_ghost:>8}{rst} tokens {dim}(~${total_usd:.2}){rst}"
    );
    out.push('\n');

    if unoptimized_tokens > 0 {
        let missed_count: u32 = discover.missed_commands.iter().map(|m| m.count).sum();
        let _ = writeln!(
            out,
            "  {dim}  Unoptimized shells:{rst}  {white}{unoptimized_tokens:>8}{rst} {dim}({missed_count} cmds without lean-ctx){rst}"
        );
    }
    if redundant_tokens > 0 {
        let _ = writeln!(
            out,
            "  {dim}  Redundant reads:{rst}     {white}{redundant_tokens:>8}{rst} {dim}({redundant_reads} cache hits = wasted re-reads){rst}"
        );
    }
    out.push('\n');
    let _ = writeln!(
        out,
        "  {bold}Monthly savings potential:{rst} {green}${monthly_usd:.2}{rst}"
    );

    if !discover.missed_commands.is_empty() {
        out.push('\n');
        let _ = writeln!(out, "  {bold}Top unoptimized commands:{rst}");
        for m in discover.missed_commands.iter().take(5) {
            let _ = writeln!(
                out,
                "    {dim}{:>4}x{rst}  {white}{:<12}{rst} {dim}{}{rst}",
                m.count, m.prefix, m.description
            );
        }
    }

    out.push('\n');
    if discover.already_optimized == 0 {
        let _ = writeln!(
            out,
            "  {yellow}Run '{bold}lean-ctx onboard{rst}{yellow}' to eliminate ghost tokens.{rst}"
        );
    } else {
        let _ = writeln!(
            out,
            "  {dim}Already optimized: {}/{} commands{rst}",
            discover.already_optimized, discover.total_commands
        );
    }
    out.push('\n');
    print_report(&out);
}

#[cfg(test)]
mod tests {
    use crate::cli::common::carrier_tests_support::{AWS_KEY, governed, lineage, project};

    /// The discover report is statistics about shell history: released,
    /// never registered as a delivery of project sources.
    #[test]
    fn the_discover_report_is_human_facing() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let (_project, root) = project();
        let report = "Analyzed 120 commands\n  git status  x14";
        let printed = super::released_report(&root, report.to_string());
        assert_eq!(printed, report, "the report keeps its bytes");
        assert_eq!(lineage(&root, &printed), None);
        let leaked = super::released_report(&root, format!("export KEY={AWS_KEY}  x2"));
        assert!(!leaked.contains(AWS_KEY), "{leaked}");
    }
}
