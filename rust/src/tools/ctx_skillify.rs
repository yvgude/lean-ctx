//! `ctx_skillify` business logic — codify recurring session patterns into
//! versioned `.cursor/rules/skillify-*.mdc` files (#290).

use std::collections::BTreeMap;

use crate::core::skillify::{self, RuleNotice};

/// Dispatch a skillify action and return human-readable text (shared by the MCP
/// tool and the CLI).
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

/// The block for rule files lean-ctx reports instead of listing: path, reason
/// code and a fixed sentence — nothing of a file's content. The notices come
/// sorted by path, so identical calls print identical text (#498).
fn push_notices(out: &mut String, notices: &[RuleNotice]) {
    if notices.is_empty() {
        return;
    }
    out.push_str(&format!("Needs your attention ({}):\n", notices.len()));
    for notice in notices {
        out.push_str(&format!(
            "  {} — {} [{}]\n",
            notice.path, notice.reason, notice.code
        ));
    }
}

/// Why candidates were skipped, as `reason code → count`. The first element
/// of a `skipped` entry is a candidate's TITLE — a fact key or the first
/// sentence of its body — so it is never printed. The reasons are fixed
/// strings (`gate::judge`: "too short", "generic / one-off phrasing") or
/// typed gateway refusals (reason codes only); the one that carries numbers
/// of a candidate ("one-off (recurrence …)") is reduced to its code.
fn skip_reasons(skipped: &[(String, String)]) -> BTreeMap<&str, usize> {
    let mut reasons = BTreeMap::new();
    for (_, reason) in skipped {
        let code = if reason.starts_with("one-off") {
            "one-off"
        } else {
            reason.as_str()
        };
        *reasons.entry(code).or_insert(0) += 1;
    }
    reasons
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
            for (reason, count) in skip_reasons(&r.skipped) {
                out.push_str(&format!("  - skipped {count}: {reason}\n"));
            }
            push_notices(&mut out, &r.notices);
            out
        }
    }
}

fn render_list(project_root: &str) -> String {
    let listing = skillify::list_rules(project_root);
    if listing.rules.is_empty() && listing.notices.is_empty() {
        return "skillify: no generated rules yet — run `skillify mine`".to_string();
    }
    let mut out = format!("skillify rules ({}):\n", listing.rules.len());
    for r in &listing.rules {
        out.push_str(&format!("  {} v{} — {}\n", r.slug, r.version, r.title));
    }
    push_notices(&mut out, &listing.notices);
    out
}

fn render_status(project_root: &str) -> String {
    let cfg = skillify::current_config();
    let candidates = skillify::candidate::mine_candidates(project_root).len();
    let listing = skillify::list_rules(project_root);
    format!(
        "skillify status\n  enabled: {}\n  scope: {}\n  min_confidence: {:.2}\n  min_recurrence: {}\n  candidates available: {}\n  generated rules: {}\n  needs your attention: {}",
        cfg.enabled,
        cfg.scope,
        cfg.min_confidence,
        cfg.min_recurrence,
        candidates,
        listing.rules.len(),
        listing.notices.len()
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
