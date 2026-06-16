use crate::core::archive;
use crate::core::context_handles::HandleRegistry;
use crate::core::context_ledger::ContextLedger;

pub fn handle(args: &serde_json::Value) -> String {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("retrieve");

    match action {
        "list" => handle_list(args),
        "search_all" => handle_search_all(args),
        _ => handle_retrieve(args),
    }
}

/// Try to resolve a handle reference (@F1, @K1, etc.) to a file path.
/// Returns None if the ID is not a handle reference.
pub fn resolve_handle_ref(id: &str) -> Option<String> {
    let clean = id.strip_prefix('@').unwrap_or(id);
    if clean.len() < 2 {
        return None;
    }
    let prefix = clean.chars().next()?;
    if !matches!(prefix, 'F' | 'S' | 'K' | 'M' | 'P') {
        return None;
    }
    if !clean[1..].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let ledger = ContextLedger::load();
    let mut registry = HandleRegistry::new();
    for entry in &ledger.entries {
        if let (Some(item_id), Some(kind)) = (&entry.id, &entry.kind) {
            let phi = entry.phi.unwrap_or(0.5);
            let view_costs = entry.view_costs.clone().unwrap_or_else(|| {
                crate::core::context_field::ViewCosts::from_full_tokens(entry.original_tokens)
            });
            registry.register(
                item_id.clone(),
                *kind,
                &entry.path,
                &format!("{} {}L", entry.path, entry.original_tokens),
                &view_costs,
                phi,
                entry
                    .state
                    .as_ref()
                    .is_some_and(|s| *s == crate::core::context_field::ContextState::Pinned),
            );
        }
    }

    registry.resolve(clean).map(|h| h.source_path.clone())
}

fn handle_retrieve(args: &serde_json::Value) -> String {
    let Some(id) = args.get("id").and_then(|v| v.as_str()) else {
        return "ERROR: 'id' parameter is required. Use ctx_expand(action=\"list\") to see available archives, or pass a handle ref like @F1.".to_string();
    };

    // Handle reference resolution: @F1, @K1, @S1, etc.
    if let Some(path) = resolve_handle_ref(id) {
        let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("full");
        return format!(
            "[handle:{id} -> {path}]\nUse ctx_read(path=\"{path}\", mode=\"{mode}\") to load content."
        );
    }

    // Structured drilldown selectors (head / tail / json_keys).
    if let Some(n) = args.get("head").and_then(serde_json::Value::as_u64) {
        return match archive::retrieve_head(id, n as usize) {
            Some(result) => format!("Archive {id} head {n}:\n{result}"),
            None => format!("Archive '{id}' not found or expired."),
        };
    }
    if let Some(n) = args.get("tail").and_then(serde_json::Value::as_u64) {
        return match archive::retrieve_tail(id, n as usize) {
            Some(result) => format!("Archive {id} tail {n}:\n{result}"),
            None => format!("Archive '{id}' not found or expired."),
        };
    }
    if args.get("json_keys").and_then(serde_json::Value::as_bool) == Some(true)
        || args.get("json_path").is_some()
    {
        let path = args.get("json_path").and_then(|v| v.as_str());
        return match archive::retrieve_json_keys(id, path) {
            Some(result) => result,
            None => format!(
                "Archive '{id}' not found or not valid JSON. Use ctx_expand(id=\"{id}\") for raw content."
            ),
        };
    }

    if let Some(pattern) = args.get("search").and_then(|v| v.as_str()) {
        return match archive::retrieve_with_search(id, pattern) {
            Some(result) => result,
            None => format!(
                "Archive '{id}' not found or expired. Use ctx_expand(action=\"list\") to see available archives."
            ),
        };
    }

    let start = args
        .get("start_line")
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as usize);
    let end = args
        .get("end_line")
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as usize);

    if let (Some(s), Some(e)) = (start, end) {
        return match archive::retrieve_with_range(id, s, e) {
            Some(result) => {
                format!("Archive {id} lines {s}-{e}:\n{result}")
            }
            None => format!("Archive '{id}' not found or expired."),
        };
    }

    match archive::retrieve(id) {
        Some(content) => {
            let lines = content.lines().count();
            let chars = content.len();
            format!("Archive {id} ({chars} chars, {lines} lines):\n{content}")
        }
        None => format!(
            "Archive '{id}' not found or expired. Use ctx_expand(action=\"list\") to see available archives."
        ),
    }
}

fn handle_search_all(args: &serde_json::Value) -> String {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.is_empty() => q,
        _ => return "ERROR: 'query' parameter required for search_all.".to_string(),
    };
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(10) as usize;

    let results = crate::core::archive_fts::search(query, limit);
    if results.is_empty() {
        return format!(
            "No archives match \"{query}\". Indexed: {} entries.",
            crate::core::archive_fts::entry_count()
        );
    }

    let mut out = format!("{} result(s) for \"{}\":\n", results.len(), query);
    for r in &results {
        out.push_str(&format!(
            "  {} | {} | {} | …{}…\n",
            r.archive_id, r.tool, r.command, r.snippet
        ));
    }
    out.push_str("\nRetrieve full: ctx_expand(id=\"<archive_id>\")");
    out
}

fn handle_list(args: &serde_json::Value) -> String {
    let session_id = args.get("session_id").and_then(|v| v.as_str());
    let entries = archive::list_entries(session_id);

    if entries.is_empty() {
        return "No archives found.".to_string();
    }

    let mut out = format!("{} archive(s):\n", entries.len());
    for e in &entries {
        out.push_str(&format!(
            "  {} | {} | {} | {} chars ({} tok) | {}\n",
            e.id,
            e.tool,
            e.command,
            e.size_chars,
            e.size_tokens,
            e.created_at.format("%H:%M:%S")
        ));
    }
    out.push_str("\nRetrieve: ctx_expand(id=\"<id>\")");
    out.push_str("\nSearch: ctx_expand(id=\"<id>\", search=\"ERROR\")");
    out.push_str("\nRange: ctx_expand(id=\"<id>\", start_line=10, end_line=50)");
    out.push_str("\nHead/Tail: ctx_expand(id=\"<id>\", head=120) | tail=40");
    out.push_str("\nJSON: ctx_expand(id=\"<id>\", json_keys=true) | json_path=\"data.items\"");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn handle_missing_id_returns_error() {
        let result = handle(&json!({}));
        assert!(result.contains("ERROR"));
        assert!(result.contains("id"));
    }

    #[test]
    fn handle_nonexistent_returns_not_found() {
        let result = handle(&json!({"id": "nonexistent_xyz"}));
        assert!(result.contains("not found"));
    }

    #[test]
    fn handle_list_empty() {
        let result = handle(&json!({"action": "list"}));
        assert!(
            result.contains("No archives") || result.contains("archive(s)"),
            "unexpected: {result}"
        );
    }
}
