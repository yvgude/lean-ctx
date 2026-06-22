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

    // CCR proxy tee handle (#482): the proxy's prune / live-compression stubs
    // carry a content-addressed tee handle. When the lean-ctx retrieve tool is
    // attached the agent can pull back just the slice it needs (head / tail /
    // search / json_path / range) instead of re-injecting the whole original —
    // the surgical front-end the issue calls "preferred when available". The
    // same path also works with a plain native file read for proxy-only setups.
    if let Some(path) = crate::proxy::ccr::resolve_tee(id) {
        return expand_tee_file(&path, args);
    }

    // Route structured accessors by ID prefix. Archive IDs are hex-only, ref
    // IDs are `ref_+hex` — the prefix tells us the exact store to query.
    if id.starts_with("ref_") {
        return expand_reference(id, args);
    }
    expand_archive(id, args)
}

/// Expand a reference-store entry (`ref_`-prefixed ID). Resolves content from
/// the in-memory reference store and formats via `archive::format_*` — the same
/// gutter/JSON formatters the archive path uses — so output is consistent
/// regardless of which store backed the ID.
fn expand_reference(id: &str, args: &serde_json::Value) -> String {
    let Some(content) = crate::server::reference_store::resolve(id) else {
        return format!(
            "Reference '{id}' not found or expired (5-min TTL). \
             Use the HTTP proxy at /v1/references/{id} if available."
        );
    };
    let label = format!("reference {id}");

    if let Some(n) = args.get("head").and_then(serde_json::Value::as_u64) {
        let n = (n as usize).min(content.lines().count());
        return format!(
            "Reference {id} head {n}:\n{}",
            archive::format_range(&content, 1, n)
        );
    }
    if let Some(n) = args.get("tail").and_then(serde_json::Value::as_u64) {
        let n = n as usize;
        let total = content.lines().count();
        let start = if total > n { total - n + 1 } else { 1 };
        return format!(
            "Reference {id} tail {n}:\n{}",
            archive::format_range(&content, start, total)
        );
    }
    if args.get("json_keys").and_then(serde_json::Value::as_bool) == Some(true)
        || args.get("json_path").is_some()
    {
        let path = args.get("json_path").and_then(|v| v.as_str());
        match archive::format_json_keys(&content, path, &label) {
            Some(out) => return out,
            None => {
                return format!(
                    "Reference '{id}' is not valid JSON. Use ctx_expand(id=\"{id}\") for raw content."
                );
            }
        }
    }
    if let Some(pattern) = args.get("search").and_then(|v| v.as_str()) {
        return format!(
            "Reference {id}:\n{}",
            archive::format_search(&content, pattern, &label)
        );
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
        return format!(
            "Reference {id} lines {s}-{e}:\n{}",
            archive::format_range(&content, s, e)
        );
    }

    // Full content
    let lines = content.lines().count();
    let chars = content.len();
    format!("Reference {id} ({chars} chars, {lines} lines):\n{content}")
}

/// Expand an archive entry (hex-only ID). Delegates to `archive::retrieve*`
/// functions which handle on-disk lookup, cleanup-aware TTL checks, and
/// line-number-guttered output formatting.
fn expand_archive(id: &str, args: &serde_json::Value) -> String {
    if let Some(n) = args.get("head").and_then(serde_json::Value::as_u64) {
        let n = n as usize;
        return match archive::retrieve_head(id, n) {
            Some(result) => format!("Archive {id} head {n}:\n{result}"),
            None => format!("Archive '{id}' not found or expired."),
        };
    }
    if let Some(n) = args.get("tail").and_then(serde_json::Value::as_u64) {
        let n = n as usize;
        return match archive::retrieve_tail(id, n) {
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
            Some(result) => format!("Archive {id} lines {s}-{e}:\n{result}"),
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

/// Surgical retrieval over a CCR proxy tee file (#482). Mirrors the archive
/// selectors (head / tail / search / json_path / range / full) but operates on
/// the verbatim tee content on disk, so the agent pulls back only the slice it
/// needs rather than undoing the proxy's compression with a full re-inject.
fn expand_tee_file(path: &std::path::Path, args: &serde_json::Value) -> String {
    let Ok(content) = std::fs::read_to_string(path) else {
        return format!(
            "ERROR: CCR tee file is no longer available: {}",
            path.display()
        );
    };
    let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("ccr");

    if let Some(n) = args.get("head").and_then(serde_json::Value::as_u64) {
        return format!(
            "[ccr {label}] head {n}:\n{}",
            head_lines(&content, n as usize)
        );
    }
    if let Some(n) = args.get("tail").and_then(serde_json::Value::as_u64) {
        return format!(
            "[ccr {label}] tail {n}:\n{}",
            tail_lines(&content, n as usize)
        );
    }
    if args.get("json_keys").and_then(serde_json::Value::as_bool) == Some(true)
        || args.get("json_path").is_some()
    {
        let jp = args.get("json_path").and_then(|v| v.as_str());
        return match json_view(&content, jp) {
            Some(out) => format!("[ccr {label}] json {}:\n{out}", jp.unwrap_or("(keys)")),
            None => format!(
                "[ccr {label}] not valid JSON or path not found. Use ctx_expand(id=\"{label}\") for raw content."
            ),
        };
    }
    if let Some(pattern) = args.get("search").and_then(|v| v.as_str()) {
        return format!(
            "[ccr {label}] search \"{pattern}\":\n{}",
            search_lines(&content, pattern)
        );
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
        return format!(
            "[ccr {label}] lines {s}-{e}:\n{}",
            range_lines(&content, s, e)
        );
    }

    let lines = content.lines().count();
    format!(
        "[ccr {label}] ({} chars, {lines} lines):\n{content}",
        content.len()
    )
}

fn head_lines(s: &str, n: usize) -> String {
    s.lines().take(n).collect::<Vec<_>>().join("\n")
}

fn tail_lines(s: &str, n: usize) -> String {
    let v: Vec<&str> = s.lines().collect();
    let start = v.len().saturating_sub(n);
    v[start..].join("\n")
}

/// 1-indexed inclusive line range, clamped to the available lines.
fn range_lines(s: &str, start: usize, end: usize) -> String {
    let v: Vec<&str> = s.lines().collect();
    let a = start.saturating_sub(1).min(v.len());
    let b = end.min(v.len());
    if a >= b {
        return String::new();
    }
    v[a..b].join("\n")
}

fn search_lines(s: &str, pattern: &str) -> String {
    let hits: Vec<String> = s
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(pattern))
        .map(|(i, l)| format!("{}: {}", i + 1, l))
        .collect();
    if hits.is_empty() {
        format!("(no lines match \"{pattern}\")")
    } else {
        hits.join("\n")
    }
}

/// `json_path` navigation over the tee content: object segments by key, array
/// segments by numeric index, dot-separated. Empty path lists the root keys.
/// Objects render as their key list; scalars/arrays pretty-print.
fn json_view(s: &str, path: Option<&str>) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(s).ok()?;
    let target = match path {
        Some(p) if !p.is_empty() => navigate_json(&root, p)?,
        _ => &root,
    };
    if let Some(obj) = target.as_object() {
        Some(obj.keys().cloned().collect::<Vec<_>>().join("\n"))
    } else {
        serde_json::to_string_pretty(target).ok()
    }
}

fn navigate_json<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = v;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        cur = match seg.parse::<usize>() {
            Ok(idx) => cur.get(idx)?,
            Err(_) => cur.get(seg)?,
        };
    }
    Some(cur)
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

    #[test]
    fn text_selectors_slice_correctly() {
        let body = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(head_lines(&body, 2), "line 1\nline 2");
        assert_eq!(tail_lines(&body, 2), "line 9\nline 10");
        assert_eq!(range_lines(&body, 3, 4), "line 3\nline 4");
        assert!(search_lines(&body, "line 7").contains("7: line 7"));
        assert!(search_lines(&body, "zzz").contains("no lines match"));
    }

    #[test]
    fn json_view_lists_keys_and_navigates() {
        let doc = r#"{"a":{"b":[10,20,30]},"c":1}"#;
        assert_eq!(json_view(doc, None).unwrap(), "a\nc");
        assert_eq!(json_view(doc, Some("a")).unwrap(), "b");
        assert_eq!(json_view(doc, Some("a.b.1")).unwrap(), "20");
        assert!(json_view(doc, Some("a.missing")).is_none());
        assert!(json_view("not json", None).is_none());
    }

    #[test]
    fn ctx_expand_retrieves_proxy_tee_handle_surgically() {
        let _lock = crate::core::data_dir::test_env_lock();
        // Mimic what the proxy does: persist a verbatim original to the tee store
        // and hand the agent its content-addressed handle.
        let original = (1..=60)
            .map(|i| format!("output row {i:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(original.len() >= crate::proxy::ccr::MIN_TEE_BYTES);
        let tee_handle = crate::proxy::ccr::persist(&original).expect("tee handle");

        // Full content via the handle path (proxy-only fallback also reads this).
        let full = handle(&json!({"id": tee_handle}));
        assert!(full.contains("output row 001") && full.contains("output row 060"));

        // Surgical slices via the bare hash form the stub can also carry.
        let hash = crate::core::hasher::hash_short(&original);
        let head = handle(&json!({"id": hash, "head": 2}));
        assert!(head.contains("output row 001") && !head.contains("output row 010"));
        let search = handle(&json!({"id": hash, "search": "row 042"}));
        assert!(search.contains("output row 042") && !search.contains("output row 001"));
    }
}
