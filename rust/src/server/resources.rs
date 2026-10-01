use rmcp::model::{Resource, ResourceContents};

const URI_SUMMARY: &str = "lean-ctx://context/summary";
const URI_PINNED: &str = "lean-ctx://context/pinned";
const URI_PRESSURE: &str = "lean-ctx://context/pressure";
const URI_PLAN: &str = "lean-ctx://context/plan";
const URI_BOUNCE: &str = "lean-ctx://context/bounce";

pub fn list_resources() -> Vec<Resource> {
    vec![
        make_resource(
            URI_SUMMARY,
            "Context Summary",
            "Ledger compact: items, pressure, budget",
        ),
        make_resource(
            URI_PINNED,
            "Pinned Items",
            "Pinned context items with compressed content",
        ),
        make_resource(
            URI_PRESSURE,
            "Context Pressure",
            "Budget utilization and recommendations",
        ),
        make_resource(
            URI_PLAN,
            "Context Plan",
            "Current context plan with modes per file",
        ),
        make_resource(
            URI_BOUNCE,
            "Bounce Stats",
            "Bounce detection statistics and wasted tokens",
        ),
    ]
}

pub(super) fn text_resource(text: &str, uri: &str) -> ResourceContents {
    ResourceContents::text(crate::server::policy_guard::redact_model_text(text), uri)
}

pub fn read_resource(
    uri: &str,
    ledger: &crate::core::context_ledger::ContextLedger,
) -> Option<Vec<ResourceContents>> {
    match uri {
        URI_SUMMARY => Some(vec![text_resource(&build_summary(ledger), uri)]),
        URI_PRESSURE => Some(vec![text_resource(&build_pressure(ledger), uri)]),
        URI_PLAN => Some(vec![text_resource(&build_plan(ledger), uri)]),
        URI_PINNED => Some(vec![text_resource(&build_pinned(ledger), uri)]),
        URI_BOUNCE => Some(vec![text_resource(&build_bounce(), uri)]),
        _ => None,
    }
}

/// Agent-facing explanation when `resources/read` is called with an unknown URI.
///
/// GH #1228: agents denied native Read for auto memory often retry via MCP
/// `resources/read` with `file://…/memory/MEMORY.md` and get a bare
/// "Unknown resource". Point them at the real surfaces instead.
pub fn unknown_resource_message(uri: &str) -> String {
    let looks_like_file = uri.starts_with("file:")
        || uri.starts_with('/')
        || (uri.len() > 2 && uri.as_bytes()[1] == b':' && uri.as_bytes()[0].is_ascii_alphabetic())
        || uri.contains("/.claude/projects/")
        || uri.contains("\\memory\\")
        || uri.contains("/memory/");

    if looks_like_file {
        format!(
            "Unknown resource: {uri}. File not readable — it may not exist, \
             be outside the project root, or be too large (>2 MiB). \
             For project files use ctx_read / ctx_patch. \
             For Claude auto memory (~/.claude/projects/<slug>/memory/) use native Read/Edit."
        )
    } else {
        format!("Unknown resource: {uri}")
    }
}

fn make_resource(uri: &str, name: &str, desc: &str) -> Resource {
    Resource::new(uri, name)
        .with_description(desc)
        .with_mime_type("text/plain")
}

fn build_summary(ledger: &crate::core::context_ledger::ContextLedger) -> String {
    let pressure = ledger.pressure();
    let adjusted = ledger.adjusted_total_saved();
    format!(
        "files:{} | sent:{} | saved:{} (adj:{}) | pressure:{:.0}% | action:{:?}",
        ledger.entries.len(),
        ledger.total_tokens_sent,
        ledger.total_tokens_saved,
        adjusted,
        pressure.utilization * 100.0,
        pressure.recommendation,
    )
}

fn build_pressure(ledger: &crate::core::context_ledger::ContextLedger) -> String {
    let p = ledger.pressure();
    let mut lines = vec![
        format!("utilization: {:.1}%", p.utilization * 100.0),
        format!("remaining: {} tokens", p.remaining_tokens),
        format!("entries: {}", p.entries_count),
        format!("action: {:?}", p.recommendation),
    ];

    if p.utilization > 0.8 {
        let evict = ledger.eviction_candidates_by_phi(3);
        if !evict.is_empty() {
            let names: Vec<_> = evict
                .iter()
                .take(5)
                .map(|p| crate::core::protocol::shorten_path(p))
                .collect();
            lines.push(format!("eviction_candidates: {}", names.join(", ")));
        }
    }

    lines.join("\n")
}

fn build_plan(ledger: &crate::core::context_ledger::ContextLedger) -> String {
    let mut lines = Vec::new();
    for entry in &ledger.entries {
        let short = crate::core::protocol::shorten_path(&entry.path);
        let phi_str = entry.phi.map_or("?".to_string(), |p| format!("{p:.2}"));
        let state_str = entry.state.as_ref().map_or("?", |s| match s {
            crate::core::context_field::ContextState::Included => "incl",
            crate::core::context_field::ContextState::Pinned => "pin",
            crate::core::context_field::ContextState::Excluded => "excl",
            crate::core::context_field::ContextState::Candidate => "cand",
            crate::core::context_field::ContextState::Stale => "stale",
            crate::core::context_field::ContextState::Shadowed => "shadow",
        });
        lines.push(format!(
            "{short} mode={} tok={} phi={phi_str} state={state_str}",
            entry.mode, entry.sent_tokens,
        ));
    }
    if lines.is_empty() {
        "No context items tracked yet.".to_string()
    } else {
        lines.join("\n")
    }
}

fn build_pinned(ledger: &crate::core::context_ledger::ContextLedger) -> String {
    let pinned: Vec<_> = ledger
        .entries
        .iter()
        .filter(|e| e.state == Some(crate::core::context_field::ContextState::Pinned))
        .collect();
    if pinned.is_empty() {
        return "No pinned items.".to_string();
    }
    let mut lines = Vec::new();
    for entry in pinned {
        let short = crate::core::protocol::shorten_path(&entry.path);
        lines.push(format!(
            "{short} mode={} tok={}",
            entry.mode, entry.sent_tokens
        ));
    }
    lines.join("\n")
}

fn build_bounce() -> String {
    match crate::core::bounce_tracker::global().lock() {
        Ok(bt) => bt.format_summary(),
        _ => "Bounce tracker unavailable.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_returns_five_resources() {
        let resources = list_resources();
        assert_eq!(resources.len(), 5);
    }

    #[test]
    fn read_summary_returns_content() {
        let ledger = crate::core::context_ledger::ContextLedger::new();
        let result = read_resource(URI_SUMMARY, &ledger);
        assert!(result.is_some());
    }

    #[test]
    fn read_unknown_uri_returns_none() {
        let ledger = crate::core::context_ledger::ContextLedger::new();
        let result = read_resource("lean-ctx://unknown", &ledger);
        assert!(result.is_none());
    }

    #[test]
    fn read_pressure_returns_content() {
        let ledger = crate::core::context_ledger::ContextLedger::new();
        let result = read_resource(URI_PRESSURE, &ledger);
        assert!(result.is_some());
    }

    #[test]
    fn read_bounce_returns_content() {
        let ledger = crate::core::context_ledger::ContextLedger::new();
        let result = read_resource(URI_BOUNCE, &ledger);
        assert!(result.is_some());
    }

    #[test]
    fn unknown_file_uri_message_guides_to_ctx_read_and_native_memory() {
        let msg = unknown_resource_message(
            "file:///home/jules/.claude/projects/-home-jules-Projects-blockposters/memory/MEMORY.md",
        );
        assert!(msg.contains("ctx_read"), "{msg}");
        assert!(msg.contains("native Read/Edit"), "{msg}");
        assert!(msg.contains("auto memory"), "{msg}");
    }

    #[test]
    fn unknown_lean_ctx_uri_stays_short() {
        let msg = unknown_resource_message("lean-ctx://unknown");
        assert_eq!(msg, "Unknown resource: lean-ctx://unknown");
    }
}
