// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx health` — project code-health report.
//!
//! Surfaces the navigability score (cognitive complexity + naming), the top
//! hotspots, and the estimated token "quality tax". `--json` emits machine
//! output; `--gate` turns it into a CI check (exit 1 when the score is below the
//! minimum), mirroring `doctor`/`conformance`.

use lean_ctx::core::code_health::{report, scan_project};
use std::path::Path;

/// Default minimum score for `--gate` (grade C boundary). Override with
/// `LEAN_CTX_HEALTH_MIN_SCORE`.
const DEFAULT_GATE_MIN_SCORE: u32 = 60;

/// Number of hotspots to surface.
const TOP_HOTSPOTS: usize = 15;

pub(crate) fn cmd_health(args: &[String]) -> i32 {
    if args.is_empty() {
        return cmd_workspace_health();
    }

    if args.iter().any(|a| a == "--watch") {
        return cmd_workspace_health_watch();
    }

    if args.first().is_some_and(|arg| arg == "workspace") {
        return cmd_workspace_health();
    }

    cmd_code_health(args)
}

fn cmd_workspace_health() -> i32 {
    print_workspace_health();
    0
}

fn cmd_workspace_health_watch() -> i32 {
    loop {
        print!("\x1b[2J\x1b[H");
        let now = chrono::Local::now().format("%H:%M:%S");
        println!("lean-ctx health --watch  [{now}]\n");
        print_workspace_health();
        println!("\nRefreshing every 10s — Ctrl+C to stop");
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}

fn print_workspace_health() {
    let health = crate::dashboard::routes::health::workspace_health();

    println!("Workspace health: {}", health.overall_status.to_uppercase());
    for check in health.checks {
        let status = match check.status {
            "OK" => "OK",
            "Stopped" => "STOPPED",
            _ => "DEGRADED",
        };
        println!("[{status}] {:<18} {}", check.name, check.message);
    }
}

fn cmd_code_health(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let gate = args.iter().any(|a| a == "--gate");
    let root = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map_or(".", String::as_str);

    let cfg = lean_ctx::core::config::Config::load();
    let threshold = cfg.code_health.cognitive_threshold;
    let model = lean_ctx::core::gain::model_pricing::resolve_model_for_client("cli");

    let health = scan_project(Path::new(root), threshold, Some(&model), TOP_HOTSPOTS);

    if json {
        let value = report::json(&health, root);
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!("{}", report::text(&health, root, threshold, &model));
    }

    if gate && health.score.score < gate_min_score() {
        return 1;
    }
    0
}

fn gate_min_score() -> u32 {
    std::env::var("LEAN_CTX_HEALTH_MIN_SCORE")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_GATE_MIN_SCORE)
}
