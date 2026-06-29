//! `lean-ctx health` — project code-health report.
//!
//! Surfaces the navigability score (cognitive complexity + naming), the top
//! hotspots, and the estimated token "quality tax". `--json` emits machine
//! output; `--gate` turns it into a CI check (exit 1 when the score is below the
//! minimum), mirroring `doctor`/`conformance`.

use crate::core::code_health::{report, scan_project};
use std::path::Path;

/// Default minimum score for `--gate` (grade C boundary). Override with
/// `LEAN_CTX_HEALTH_MIN_SCORE`.
const DEFAULT_GATE_MIN_SCORE: u32 = 60;

/// Number of hotspots to surface.
const TOP_HOTSPOTS: usize = 15;

pub(crate) fn cmd_health(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let gate = args.iter().any(|a| a == "--gate");
    let root = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map_or(".", String::as_str);

    let cfg = crate::core::config::Config::load();
    let threshold = cfg.code_health.cognitive_threshold;
    let model = crate::core::gain::model_pricing::resolve_model_for_client("cli");

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
