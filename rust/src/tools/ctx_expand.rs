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

    // Unified tee-store handle (#482 / #936). One resolver, fixed precedence:
    // the proxy's prune / live-compression stubs (`proxy_<hash>`), the JSON
    // crusher's lossy originals (`json_<hash>`), AND every compressed shell
    // command's already-teed verbatim output (`<slug>_<8hex>.log`) all live in
    // the shared content-addressed store and resolve here, before the reference
    // (`ref_`) and archive (hex) stores below. The agent pulls back just the
    // slice it needs (head / tail / search / json_path / range) instead of
    // re-injecting the whole original — the surgical front-end the issue calls
    // "preferred when available". Works with a plain native file read too.
    if let Some(path) = crate::proxy::ccr::resolve_tee(id) {
        return expand_tee_file(&path, args);
    }

    // Resolve the entry's content once, then run the shared selector ladder.
    // Archive IDs are hex-only; reference IDs are `ref_`-prefixed — the prefix
    // picks the exact store, so the two stores differ only in *how* content is
    // resolved, never in *how* selectors are dispatched (#498). Resolving up
    // front also drops the archive path's per-selector disk re-reads.
    if id.starts_with("ref_") {
        let Some(content) = crate::server::reference_store::resolve(id) else {
            return format!(
                "Reference '{id}' not found or expired (5-min TTL). \
                 Use the HTTP proxy at /v1/references/{id} if available."
            );
        };
        return dispatch_selectors(id, &content, "Reference", args);
    }
    let archive_id = if id.starts_with("shell_") {
        let Some(archive_id) = archive::resolve_alias(id) else {
            return format!(
                "Background job '{id}' has no retrievable output archive. \
                 The archive is unavailable or expired."
            );
        };
        archive_id
    } else {
        id.to_string()
    };
    let Some(content) = archive::retrieve(&archive_id) else {
        return format!(
            "Archive '{archive_id}' not found or expired. Use ctx_expand(action=\"list\") to see available archives."
        );
    };
    dispatch_selectors(&archive_id, &content, "Archive", args)
}

/// Apply the structured selector ladder (head / tail / json_keys / search /
/// range / full) to already-resolved `content`. Resolving once and formatting
/// in-memory lets the archive and reference stores share a single code path and
/// the same `archive::format_*` formatters, so output is byte-identical
/// regardless of which store backed the ID (#498). `noun` is the capitalised
/// store name used in messages — `"Archive"` or `"Reference"`.
fn dispatch_selectors(id: &str, content: &str, noun: &str, args: &serde_json::Value) -> String {
    let label = format!("{} {id}", noun.to_ascii_lowercase());

    if let Some(n) = args.get("head").and_then(serde_json::Value::as_u64) {
        let n = n as usize;
        return format!(
            "{noun} {id} head {n}:\n{}",
            archive::format_range(content, 1, n)
        );
    }
    if let Some(n) = args.get("tail").and_then(serde_json::Value::as_u64) {
        let n = n as usize;
        let total = content.lines().count();
        let start = if total > n { total - n + 1 } else { 1 };
        return format!(
            "{noun} {id} tail {n}:\n{}",
            archive::format_range(content, start, total)
        );
    }
    if args.get("json_keys").and_then(serde_json::Value::as_bool) == Some(true)
        || args.get("json_path").is_some()
    {
        let path = args.get("json_path").and_then(|v| v.as_str());
        return match archive::format_json_keys(content, path, &label) {
            Some(out) => out,
            None => format!(
                "{noun} '{id}' is not valid JSON. Use ctx_expand(id=\"{id}\") for raw content."
            ),
        };
    }
    if let Some(pattern) = args.get("search").and_then(|v| v.as_str()) {
        return archive::format_search(content, pattern, &label);
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
            "{noun} {id} lines {s}-{e}:\n{}",
            archive::format_range(content, s, e)
        );
    }

    let lines = content.lines().count();
    let chars = content.len();
    format!("{noun} {id} ({chars} chars, {lines} lines):\n{content}")
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

    #[test]
    fn ctx_expand_retrieves_shell_tee_output_surgically() {
        let _lock = crate::core::data_dir::test_env_lock();
        // Every compressed shell command already tees its verbatim output to the
        // shared store (#936). With the unified resolver, ctx_expand can slice
        // that tee surgically — the agent no longer has to re-read the whole file.
        let original = (1..=60)
            .map(|i| format!("api response row {i:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tee_path =
            crate::shell::save_tee("gh api /repos/foo/bar", &original).expect("shell tee saved");
        let name = std::path::Path::new(&tee_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap()
            .to_string();

        // Full content via the bare shell-tee basename the footer advertises.
        let full = handle(&json!({ "id": name.clone() }));
        assert!(full.contains("api response row 001") && full.contains("api response row 060"));

        // Surgical slices: head and search, same ladder as proxy/archive handles.
        let head = handle(&json!({ "id": name.clone(), "head": 2 }));
        assert!(head.contains("api response row 001") && !head.contains("api response row 010"));
        let search = handle(&json!({ "id": name, "search": "row 042" }));
        assert!(search.contains("api response row 042") && !search.contains("row 001"));
    }

    #[test]
    fn ctx_expand_retrieves_json_crush_original() {
        let _lock = crate::core::data_dir::test_env_lock();
        // The lossy JSON crusher persists its verbatim original under json_<hash>
        // (#936). The dropped columns must be recoverable through the same
        // ctx_expand path the stub/footer advertises.
        let original = (1..=50)
            .map(|i| format!(r#"{{"id":{i},"ts":"2026-06-22T10:00:{i:02}Z"}}"#))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(original.len() >= crate::proxy::ccr::MIN_TEE_BYTES);
        let handle_path = crate::proxy::ccr::persist_json(&original).expect("json tee handle");
        assert!(handle_path.contains("json_"), "json_ prefix: {handle_path}");

        let out = handle(&json!({ "id": handle_path }));
        assert!(
            out.contains("2026-06-22T10:00:42Z"),
            "dropped column recoverable: {out}"
        );
    }

    #[test]
    fn ctx_expand_resolves_reference_store_ids() {
        // #498: `ref_`-prefixed IDs route to the in-memory reference store, not
        // the on-disk archive. Exercises the resolve-then-dispatch ladder end to
        // end through the public `handle` entry point.
        let body = (1..=40)
            .map(|i| format!("ref row {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let id = crate::server::reference_store::store(body);
        assert!(id.starts_with("ref_"), "store must mint a ref_ id: {id}");

        let full = handle(&json!({"id": id}));
        assert!(
            full.contains("Reference") && full.contains("ref row 1") && full.contains("ref row 40"),
            "full: {full}"
        );

        let head = handle(&json!({"id": id, "head": 3}));
        assert!(head.contains("ref row 1") && head.contains("ref row 3"));
        assert!(!head.contains("ref row 4"), "head leaked row 4: {head}");

        let tail = handle(&json!({"id": id, "tail": 2}));
        assert!(tail.contains("ref row 39") && tail.contains("ref row 40"));
        assert!(!tail.contains("ref row 38"), "tail leaked row 38: {tail}");

        let search = handle(&json!({"id": id, "search": "ref row 7"}));
        assert!(search.contains("ref row 7") && !search.contains("ref row 1\n"));

        let range = handle(&json!({"id": id, "start_line": 5, "end_line": 6}));
        assert!(range.contains("ref row 5") && range.contains("ref row 6"));
        assert!(!range.contains("ref row 4") && !range.contains("ref row 7"));
    }

    #[test]
    fn ctx_expand_reference_json_keys_and_missing() {
        let id = crate::server::reference_store::store(r#"{"a":1,"b":[1,2,3]}"#.to_string());
        let keys = handle(&json!({"id": id, "json_keys": true}));
        assert!(keys.contains("object (2 keys)"), "got: {keys}");
        assert!(keys.contains("array(3)"), "got: {keys}");

        // An expired/unknown ref must explain the 5-min TTL, not fall through to
        // the archive's "not found" message.
        let missing = handle(&json!({"id": "ref_deadbeefcafef00d"}));
        assert!(
            missing.contains("not found or expired") && missing.contains("5-min TTL"),
            "got: {missing}"
        );
    }
}
