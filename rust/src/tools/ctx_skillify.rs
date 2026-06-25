//! `ctx_skillify` business logic — codify recurring session patterns into
//! versioned `.cursor/rules/skillify-*.mdc` files (#290).

use crate::core::skillify;

/// Dispatch a skillify action and return human-readable text (shared by the MCP
/// tool and the CLI).
#[must_use]
pub fn handle(project_root: &str, action: &str, slug: Option<&str>) -> String {
    match action.trim() {
        "" | "mine" => render_mine(project_root),
        "list" => render_list(project_root),
        "status" => render_status(project_root),
        "promote" => render_promote(project_root, slug),
        other => format!(
            "ERR: unknown skillify action '{other}'. Use: mine | list | status | promote <slug>"
        ),
    }
}

fn render_mine(project_root: &str) -> String {
    match skillify::mine(project_root) {
        Err(e) => format!("skillify: {e}"),
        Ok(r) => {
            let mut out = format!(
                "skillify mine → {} created, {} merged, {} unchanged, {} skipped ({} candidates)\n",
                r.created.len(),
                r.merged.len(),
                r.unchanged.len(),
                r.skipped.len(),
                r.candidates_seen,
            );
            out.push_str(&format!("output: {}\n", r.output_dir));
            for name in &r.created {
                out.push_str(&format!("  + {name}\n"));
            }
            for name in &r.merged {
                out.push_str(&format!("  ~ {name} (version bumped)\n"));
            }
            if r.created.is_empty() && r.merged.is_empty() {
                out.push_str("  (no new or changed rules)\n");
            }
            out
        }
    }
}

fn render_list(project_root: &str) -> String {
    let rules = skillify::list_rules(project_root);
    if rules.is_empty() {
        return "skillify: no generated rules yet — run `skillify mine`".to_string();
    }
    let mut out = format!("skillify rules ({}):\n", rules.len());
    for r in rules {
        out.push_str(&format!("  {} v{} — {}\n", r.slug, r.version, r.title));
    }
    out
}

fn render_status(project_root: &str) -> String {
    let cfg = skillify::current_config();
    let candidates = skillify::candidate::mine_candidates(project_root).len();
    let rules = skillify::list_rules(project_root).len();
    format!(
        "skillify status\n  enabled: {}\n  scope: {}\n  min_confidence: {:.2}\n  min_recurrence: {}\n  candidates available: {}\n  generated rules: {}",
        cfg.enabled, cfg.scope, cfg.min_confidence, cfg.min_recurrence, candidates, rules
    )
}

fn render_promote(project_root: &str, slug: Option<&str>) -> String {
    let Some(slug) = slug.filter(|s| !s.trim().is_empty()) else {
        return "ERR: promote requires a rule slug (see `skillify list`)".to_string();
    };
    match skillify::promote(project_root, slug.trim()) {
        Ok(dst) => format!("skillify: promoted `{slug}` → {dst}"),
        Err(e) => format!("skillify: {e}"),
    }
}
