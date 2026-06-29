//! `ctx_quality` — code-health surface for agents.
//!
//! The agent-facing twin of `lean-ctx health`: it reads the same engine
//! ([`crate::core::code_health`]) and surfaces the navigability score,
//! cognitive-complexity hotspots, and the estimated token "quality tax".
//!
//! Actions:
//!   - `report` (default): project-wide score + hotspots + quality tax.
//!   - `file`: per-function cognitive complexity + naming for one file.
//!   - `delta`: cognitive-complexity change of a file vs its git `HEAD`.
//!
//! `format=json` emits machine output. Rendering is deterministic for a given
//! repo state (#498); only the priced tax depends on the configured model.

use std::path::Path;
use std::time::Duration;

use serde_json::json;

use crate::core::code_health::{analyze_file, cognitive_delta, report, scan_project};

/// Hotspots to surface in a project report.
const TOP_HOTSPOTS: usize = 15;

/// Entry point used by the registered MCP wrapper. `path` is the resolved
/// (absolute) file path for `file`/`delta`; `root` is the project/repo root.
pub fn handle(action: &str, path: Option<&str>, root: &str, format: Option<&str>) -> String {
    let json = matches!(format, Some(f) if f.eq_ignore_ascii_case("json"));
    let threshold = crate::core::config::Config::load()
        .code_health
        .cognitive_threshold;

    match action {
        "report" | "summary" => report_action(root, threshold, json),
        "file" => file_action(path, threshold, json),
        "delta" => delta_action(path, root, threshold, json),
        other => format!("ctx_quality: unknown action '{other}'. Use: report | file | delta."),
    }
}

fn report_action(root: &str, threshold: u32, json: bool) -> String {
    let model = crate::core::gain::model_pricing::resolve_model_for_client("mcp");
    let health = scan_project(Path::new(root), threshold, Some(&model), TOP_HOTSPOTS);
    if json {
        let value = report::json(&health, root);
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
    } else {
        report::text(&health, root, threshold, &model)
    }
}

fn file_action(path: Option<&str>, threshold: u32, json: bool) -> String {
    let Some(p) = path else {
        return "ctx_quality file: 'path' is required.".to_string();
    };
    let content = match std::fs::read_to_string(p) {
        Ok(c) => c,
        Err(e) => return format!("ctx_quality file: cannot read {p}: {e}"),
    };
    let ext = Path::new(p)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let Some(health) = analyze_file(&content, ext) else {
        return format!("ctx_quality file: unsupported file type '{ext}' ({p}).");
    };

    let mut fns = health.functions.clone();
    fns.sort_by(|a, b| {
        b.cognitive
            .cmp(&a.cognitive)
            .then_with(|| a.line.cmp(&b.line))
    });

    if json {
        let functions: Vec<_> = fns
            .iter()
            .map(|f| {
                json!({
                    "name": f.name,
                    "line": f.line,
                    "cognitive": f.cognitive,
                    "over_threshold": f.cognitive > threshold,
                })
            })
            .collect();
        let naming: Vec<_> = health
            .naming
            .iter()
            .map(|n| json!({ "name": n.name, "line": n.line, "message": n.message }))
            .collect();
        let value = json!({
            "file": p,
            "threshold": threshold,
            "worst_cognitive": health.worst_cognitive(),
            "functions": functions,
            "naming": naming,
        });
        return serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    }

    let mut out = format!(
        "Code Health — {p}\n  worst cognitive: {}   threshold={threshold}\n",
        health.worst_cognitive()
    );
    if fns.is_empty() {
        out.push_str("  no functions analyzed.");
        return out;
    }
    out.push_str("  functions (cognitive complexity):");
    for f in &fns {
        let flag = if f.cognitive > threshold {
            "  (over)"
        } else {
            ""
        };
        out.push_str(&format!(
            "\n    L{}  {}  cc={}{flag}",
            f.line, f.name, f.cognitive
        ));
    }
    if !health.naming.is_empty() {
        out.push_str("\n  naming findings:");
        for n in &health.naming {
            out.push_str(&format!("\n    L{}  {}  — {}", n.line, n.name, n.message));
        }
    }
    out
}

fn delta_action(path: Option<&str>, root: &str, threshold: u32, json: bool) -> String {
    let Some(p) = path else {
        return "ctx_quality delta: 'path' is required.".to_string();
    };
    let new = match std::fs::read_to_string(p) {
        Ok(c) => c,
        Err(e) => return format!("ctx_quality delta: cannot read {p}: {e}"),
    };
    let ext = Path::new(p)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let Some(old) = git_head_content(root, p) else {
        return format!("ctx_quality delta: no git HEAD baseline for {p}.");
    };

    let deltas = cognitive_delta(&old, &new, ext);

    if json {
        let changes: Vec<_> = deltas
            .iter()
            .map(|d| {
                json!({
                    "name": d.name,
                    "before": d.before,
                    "after": d.after,
                    "increase": d.increase(),
                    "crosses_threshold": d.crosses_threshold(threshold),
                })
            })
            .collect();
        let value = json!({ "file": p, "threshold": threshold, "changes": changes });
        return serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    }

    if deltas.is_empty() {
        return format!("Code Health delta — {p}\n  no cognitive-complexity changes vs HEAD.");
    }
    let mut out = format!("Code Health delta — {p} (vs HEAD)");
    for d in &deltas {
        let cross = if d.crosses_threshold(threshold) {
            "  (crosses threshold)"
        } else {
            ""
        };
        out.push_str(&format!(
            "\n    {}: cognitive {}->{} ({:+}){cross}",
            d.name,
            d.before,
            d.after,
            d.increase()
        ));
    }
    out
}

/// File contents at git `HEAD`, or `None` when there is no committed baseline.
fn git_head_content(root: &str, abs_path: &str) -> Option<String> {
    let rel = Path::new(abs_path)
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    crate::core::git_cache::git_cached(
        &["show", &format!("HEAD:{rel}")],
        root,
        Duration::from_secs(2),
    )
}
