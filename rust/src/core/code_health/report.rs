//! Shared rendering of a [`ProjectHealth`] report (text + JSON).
//!
//! Used by both `lean-ctx health` (CLI) and `ctx_quality` (MCP) so the two
//! surfaces never drift. Deterministic given the report.

use super::ProjectHealth;
use serde_json::{Value, json};

/// Human-readable project report.
pub fn text(health: &ProjectHealth, root: &str, threshold: u32, model: &str) -> String {
    let s = &health.score;
    let mut out = String::new();
    out.push_str(&format!("Code Health — {root}\n"));
    out.push_str(&format!(
        "  score: {}/100 ({})   cognitive threshold={threshold}\n",
        s.score,
        health.grade()
    ));
    out.push_str(&format!(
        "  functions: {}   over-threshold: {}   worst cognitive: {}\n",
        s.total_functions, s.over_threshold, s.worst_cognitive
    ));
    out.push_str(&format!("  naming findings: {}\n", health.naming_count));
    out.push_str(&format!(
        "  quality tax (est.): ${:.2}   model={model}",
        s.estimated_waste_usd
    ));

    if s.hotspots.is_empty() {
        out.push_str("\n\n  no hotspots above threshold — clean.");
        return out;
    }
    out.push_str("\n\n  top hotspots (cognitive complexity):");
    for h in &s.hotspots {
        out.push_str(&format!(
            "\n    {}:{}  {}  cc={}",
            h.file, h.line, h.symbol, h.cognitive
        ));
    }
    out
}

/// Machine-readable project report.
pub fn json(health: &ProjectHealth, root: &str) -> Value {
    let s = &health.score;
    let hotspots: Vec<Value> = s
        .hotspots
        .iter()
        .map(|h| {
            json!({
                "file": h.file,
                "symbol": h.symbol,
                "line": h.line,
                "cognitive": h.cognitive,
            })
        })
        .collect();
    json!({
        "root": root,
        "score": s.score,
        "grade": health.grade().to_string(),
        "total_functions": s.total_functions,
        "over_threshold": s.over_threshold,
        "worst_cognitive": s.worst_cognitive,
        "naming_findings": health.naming_count,
        "estimated_waste_usd": s.estimated_waste_usd,
        "hotspots": hotspots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_health::scan::ProjectHealth;
    use crate::core::code_health::{Hotspot, NavigabilityScore};

    fn sample() -> ProjectHealth {
        ProjectHealth {
            score: NavigabilityScore {
                score: 72,
                total_functions: 40,
                over_threshold: 3,
                worst_cognitive: 28,
                import_cycles: 0,
                estimated_waste_usd: 1.234_5,
                hotspots: vec![Hotspot {
                    file: "src/a.rs".into(),
                    symbol: "do_it".into(),
                    line: 10,
                    cognitive: 28,
                }],
            },
            files: Vec::new(),
            naming_count: 2,
        }
    }

    #[test]
    fn text_is_deterministic_and_informative() {
        let h = sample();
        let a = text(&h, "repo", 15, "gpt-5.4");
        let b = text(&h, "repo", 15, "gpt-5.4");
        assert_eq!(a, b, "report text must be byte-stable (#498)");
        assert!(a.contains("score: 72/100 (C)"));
        assert!(a.contains("quality tax (est.): $1.23"));
        assert!(a.contains("src/a.rs:10  do_it  cc=28"));
    }

    #[test]
    fn json_is_deterministic() {
        let h = sample();
        let a = json(&h, "repo");
        let b = json(&h, "repo");
        assert_eq!(a, b, "report json must be byte-stable (#498)");
        assert_eq!(a["score"], 72);
        assert_eq!(a["grade"], "C");
        assert_eq!(a["hotspots"][0]["symbol"], "do_it");
    }

    #[test]
    fn clean_report_says_clean() {
        let mut h = sample();
        h.score.hotspots.clear();
        h.score.over_threshold = 0;
        let out = text(&h, "repo", 15, "gpt-5.4");
        assert!(out.contains("no hotspots above threshold"));
    }
}
